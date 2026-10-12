//! Tower — the vote tree that tracks validator voting history
//!
//! The Tower is Solana's fork choice rule. It maintains a tree of votes
//! and uses lockout-based switching to prevent long-range attacks.

use crate::vote::{ValidatorId, Vote, VoteState};
use dashmap::DashMap;
use parking_lot::RwLock;
use std::collections::{HashMap, HashSet};

/// Stake credited to a validator that voted without being explicitly
/// registered. Keeps equal-weight operation working out of the box while a
/// genesis registry is being wired up.
const DEFAULT_STAKE: u64 = 1;

/// A node in the tower tree
#[derive(Debug, Clone)]
pub struct TowerNode {
    /// Block hash at this node
    pub block_hash: [u8; 32],
    /// Slot
    pub slot: u64,
    /// Parent slot
    pub parent_slot: Option<u64>,
    /// Parent block hash, once linked
    pub parent_hash: Option<[u8; 32]>,
    /// Accumulated weight (stake of the validators that voted here)
    pub weight: u64,
    /// Validators that voted for this block
    pub voters: Vec<ValidatorId>,
    /// Children nodes
    pub children: Vec<[u8; 32]>,
}

/// The Tower — fork choice state machine
pub struct Tower {
    /// All known tower nodes (block_hash -> node)
    nodes: DashMap<[u8; 32], TowerNode>,
    /// Validator vote states
    validator_states: DashMap<ValidatorId, VoteState>,
    /// Registered stake per validator (genesis / staking program)
    stakes: DashMap<ValidatorId, u64>,
    /// Slot -> block hash, so a vote's parent can be linked into the tree
    slot_to_hash: DashMap<u64, [u8; 32]>,
    /// The current root (last confirmed slot)
    root_slot: RwLock<u64>,
    /// The current best fork head
    best_head: RwLock<Option<[u8; 32]>>,
    /// Total votes received
    total_votes: RwLock<u64>,
    /// Total fork switches
    total_switches: RwLock<u64>,
}

impl Tower {
    /// Create a new Tower
    pub fn new(genesis_slot: u64) -> Self {
        Self {
            nodes: DashMap::new(),
            validator_states: DashMap::new(),
            stakes: DashMap::new(),
            slot_to_hash: DashMap::new(),
            root_slot: RwLock::new(genesis_slot),
            best_head: RwLock::new(None),
            total_votes: RwLock::new(0),
            total_switches: RwLock::new(0),
        }
    }

    /// Register (or update) a validator's stake. Registered validators count
    /// toward the total stake even before they vote, which is what makes the
    /// 2/3 supermajority meaningful.
    pub fn register_validator(&self, validator: ValidatorId, stake: u64) {
        self.stakes.insert(validator, stake);
        self.validator_states
            .entry(validator)
            .or_insert_with(|| VoteState::new(validator, *self.root_slot.read()));
    }

    /// Stake credited to a validator.
    pub fn stake_of(&self, validator: &ValidatorId) -> u64 {
        self.stakes
            .get(validator)
            .map(|s| *s)
            .unwrap_or(DEFAULT_STAKE)
    }

    /// Total stake eligible to vote: every registered validator, plus a
    /// default-weight credit for any unregistered validator that has voted.
    pub fn total_stake(&self) -> u64 {
        let mut seen: HashSet<ValidatorId> = HashSet::new();
        let mut total: u64 = 0;

        for entry in self.stakes.iter() {
            seen.insert(*entry.key());
            total = total.saturating_add(*entry.value());
        }
        for entry in self.validator_states.iter() {
            if seen.insert(*entry.key()) {
                total = total.saturating_add(DEFAULT_STAKE);
            }
        }

        total
    }

    /// Process a vote
    pub fn process_vote(&self, vote: Vote) -> bool {
        // Byzantine guard: a vote that isn't Ed25519-signed by its claimed
        // validator identity is rejected before it touches any state.
        if !vote.verify_signature() {
            tracing::warn!(
                "Rejected unsigned or forged vote for slot {} from {}",
                vote.slot,
                hex::encode(&vote.validator[..8])
            );
            return false;
        }

        // Get or create validator state
        let mut validator_state = self
            .validator_states
            .entry(vote.validator)
            .or_insert_with(|| VoteState::new(vote.validator, *self.root_slot.read()));

        // A vote that builds directly on the current root re-roots this
        // validator's tower, so its accumulated lockout resets.
        let root = *self.root_slot.read();
        if vote.parent_slot == Some(root) && vote.slot > root {
            validator_state.reset_lockout();
        }

        // Check if validator can vote on this slot
        if vote.slot <= validator_state.last_vote_slot {
            tracing::warn!(
                "Vote for slot {} but last vote was {} — skipping",
                vote.slot,
                validator_state.last_vote_slot
            );
            return false;
        }

        // Check lockout
        let is_fork_switch = vote.parent_slot != Some(validator_state.last_vote_slot);
        if is_fork_switch && !validator_state.can_switch_fork(vote.slot) {
            tracing::warn!(
                "Validator {} cannot switch fork yet (lockout: {} slots)",
                hex::encode(&vote.validator[..8]),
                validator_state.lockout_slots()
            );
            return false;
        }

        // Record the fork switch if applicable
        if is_fork_switch {
            validator_state.switch_fork();
            *self.total_switches.write() += 1;
        }

        // Process the vote
        validator_state.process_vote(&vote);

        // Update tower node, weighting the vote by the validator's stake.
        let stake = self.stake_of(&vote.validator);
        let parent_hash = vote
            .parent_slot
            .and_then(|ps| self.slot_to_hash.get(&ps).map(|h| *h));
        let mut node = self
            .nodes
            .entry(vote.block_hash)
            .or_insert_with(|| TowerNode {
                block_hash: vote.block_hash,
                slot: vote.slot,
                parent_slot: vote.parent_slot,
                parent_hash,
                weight: 0,
                voters: Vec::new(),
                children: Vec::new(),
            });
        // A validator can only vote once per slot, but guard against
        // double-counting the same identity on the same block regardless.
        if !node.voters.contains(&vote.validator) {
            node.weight = node.weight.saturating_add(stake);
            node.voters.push(vote.validator);
        }
        drop(node);

        // Remember which block belongs to this slot so later votes can find
        // and link their parent.
        self.slot_to_hash.insert(vote.slot, vote.block_hash);

        // Link this node as a child of its parent, if the parent is known.
        if let Some(parent_slot) = vote.parent_slot {
            if let Some(parent_hash) = self.slot_to_hash.get(&parent_slot).map(|h| *h) {
                if let Some(mut parent) = self.nodes.get_mut(&parent_hash) {
                    if !parent.children.contains(&vote.block_hash) {
                        parent.children.push(vote.block_hash);
                    }
                }
            }
        }

        // Update best head
        self.update_best_head();

        *self.total_votes.write() += 1;

        tracing::debug!(
            "Vote processed: slot={}, validator={}, weight={}",
            vote.slot,
            hex::encode(&vote.validator[..8]),
            self.nodes
                .get(&vote.block_hash)
                .map(|n| n.weight)
                .unwrap_or(0),
        );

        true
    }

    /// Update the best head using the heaviest-chain rule: among the fork
    /// tips (leaves), pick the one whose chain from its fork root carries the
    /// most stake. Ties break on higher slot for determinism.
    fn update_best_head(&self) {
        let root = *self.root_slot.read();
        let leaves: Vec<[u8; 32]> = self
            .nodes
            .iter()
            .filter(|e| e.value().slot >= root && e.value().children.is_empty())
            .map(|e| *e.key())
            .collect();

        let mut memo: HashMap<[u8; 32], u64> = HashMap::new();
        let mut best: Option<([u8; 32], u64, u64)> = None; // (hash, chain weight, slot)

        for leaf in leaves {
            let weight = self.chain_weight(leaf, &mut memo);
            let slot = self.nodes.get(&leaf).map(|n| n.slot).unwrap_or(0);
            let better = match best {
                None => true,
                Some((_, best_weight, best_slot)) => {
                    weight > best_weight || (weight == best_weight && slot > best_slot)
                }
            };
            if better {
                best = Some((leaf, weight, slot));
            }
        }

        if let Some((hash, _, _)) = best {
            *self.best_head.write() = Some(hash);
        }
    }

    /// Stake along the chain from `hash` back to its fork root.
    fn chain_weight(&self, hash: [u8; 32], memo: &mut HashMap<[u8; 32], u64>) -> u64 {
        if let Some(weight) = memo.get(&hash) {
            return *weight;
        }
        let (weight, parent) = match self.nodes.get(&hash) {
            Some(node) => (node.weight, node.parent_hash),
            None => return 0,
        };

        let total = match parent {
            Some(parent_hash) => weight.saturating_add(self.chain_weight(parent_hash, memo)),
            None => weight,
        };
        memo.insert(hash, total);
        total
    }

    /// Check if a slot is confirmed: a strict 2/3 supermajority of the total
    /// stake must have voted for a block at that slot. Strict (> 2/3) means the
    /// threshold is effectively rounded up, so exactly 2/3 is never enough.
    pub fn is_slot_confirmed(&self, slot: u64) -> bool {
        let total_stake = self.total_stake();
        if total_stake == 0 {
            return false;
        }

        for entry in self.nodes.iter() {
            if entry.value().slot == slot
                && entry.value().weight.saturating_mul(3) > total_stake.saturating_mul(2)
            {
                return true;
            }
        }

        false
    }

    /// Get the current best head
    pub fn best_head(&self) -> Option<[u8; 32]> {
        *self.best_head.read()
    }

    /// Get root slot
    pub fn root_slot(&self) -> u64 {
        *self.root_slot.read()
    }

    /// Advance the root (after confirmation)
    pub fn advance_root(&self, new_root: u64) {
        *self.root_slot.write() = new_root;

        // Update all validator states
        for mut state in self.validator_states.iter_mut() {
            state.update_root(new_root);
            // Once the root has reached a validator's last vote, its lockout is
            // settled and starts over.
            if state.last_vote_slot <= new_root {
                state.reset_lockout();
            }
        }

        // Prune old nodes and slot mappings
        self.nodes.retain(|_, node| node.slot >= new_root);
        self.slot_to_hash.retain(|slot, _| *slot >= new_root);
    }

    /// Get stats
    pub fn stats(&self) -> TowerStats {
        TowerStats {
            total_nodes: self.nodes.len(),
            total_validators: self.validator_states.len(),
            total_votes: *self.total_votes.read(),
            total_switches: *self.total_switches.read(),
            root_slot: self.root_slot(),
            best_head: *self.best_head.read(),
        }
    }

    /// Get the fork choice tree for debugging
    pub fn debug_tree(&self) -> Vec<TowerNode> {
        self.nodes.iter().map(|e| e.value().clone()).collect()
    }
}

/// Tower statistics
#[derive(Debug, Clone)]
pub struct TowerStats {
    pub total_nodes: usize,
    pub total_validators: usize,
    pub total_votes: u64,
    pub total_switches: u64,
    pub root_slot: u64,
    pub best_head: Option<[u8; 32]>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vote::Vote;

    fn test_validator(id: u8) -> ValidatorId {
        let mut v = [0u8; 32];
        v[0] = id;
        v
    }

    fn test_key(seed_byte: u8) -> ed25519_dalek::SigningKey {
        ed25519_dalek::SigningKey::from_bytes(&[seed_byte; 32])
    }

    fn signed_vote(
        sk: &ed25519_dalek::SigningKey,
        slot: u64,
        block_hash: [u8; 32],
        parent_slot: Option<u64>,
    ) -> Vote {
        let mut vote = Vote::new(sk.verifying_key().to_bytes(), slot, block_hash, parent_slot);
        vote.sign(sk);
        vote
    }

    #[test]
    fn test_tower_creation() {
        let tower = Tower::new(0);
        let stats = tower.stats();
        assert_eq!(stats.total_nodes, 0);
        assert_eq!(stats.total_validators, 0);
    }

    #[test]
    fn test_single_vote() {
        let tower = Tower::new(0);
        let sk = test_key(1);
        let vote = signed_vote(&sk, 10, [42u8; 32], None);

        assert!(tower.process_vote(vote));
        assert_eq!(tower.stats().total_votes, 1);
    }

    #[test]
    fn test_unsigned_and_forged_votes_rejected() {
        let tower = Tower::new(0);

        // Never signed — rejected
        let unsigned = Vote::new(test_validator(1), 10, [42u8; 32], None);
        assert!(!tower.process_vote(unsigned));
        assert_eq!(tower.stats().total_votes, 0);

        // Signed by key A but attributed to validator B — rejected
        let sk = test_key(1);
        let mut forged = signed_vote(&sk, 10, [42u8; 32], None);
        forged.validator = test_validator(9);
        assert!(!tower.process_vote(forged));
        assert_eq!(tower.stats().total_votes, 0);
        assert_eq!(tower.stats().total_validators, 0);

        // Correctly signed vote still goes through
        assert!(tower.process_vote(signed_vote(&sk, 10, [42u8; 32], None)));
        assert_eq!(tower.stats().total_votes, 1);
    }

    #[test]
    fn test_multiple_validators_vote_same_block() {
        let tower = Tower::new(0);

        for i in 0..5 {
            let sk = test_key(i);
            let vote = signed_vote(&sk, 10, [42u8; 32], None);
            tower.process_vote(vote);
        }

        // Block with 5 votes should be the best head
        assert_eq!(tower.best_head(), Some([42u8; 32]));

        // Not confirmed yet (need 2/3+ of 5 = 4)
        // Actually 5 > 4, so it IS confirmed
        assert!(tower.is_slot_confirmed(10));
    }

    #[test]
    fn test_fork_choice() {
        let tower = Tower::new(0);

        // Fork A: 3 votes
        for i in 0..3 {
            let sk = test_key(i);
            let vote = signed_vote(&sk, 10, [1u8; 32], None);
            tower.process_vote(vote);
        }

        // Fork B: 5 votes
        for i in 3..8 {
            let sk = test_key(i);
            let vote = signed_vote(&sk, 10, [2u8; 32], None);
            tower.process_vote(vote);
        }

        // Fork B should win
        assert_eq!(tower.best_head(), Some([2u8; 32]));
    }

    #[test]
    fn test_stake_weighted_supermajority() {
        let tower = Tower::new(0);
        let keys: Vec<_> = (0..4).map(test_key).collect();

        // Stakes 10, 10, 10, 100 -> total 130.
        tower.register_validator(keys[0].verifying_key().to_bytes(), 10);
        tower.register_validator(keys[1].verifying_key().to_bytes(), 10);
        tower.register_validator(keys[2].verifying_key().to_bytes(), 10);
        tower.register_validator(keys[3].verifying_key().to_bytes(), 100);
        assert_eq!(tower.total_stake(), 130);

        // Fork A (slot 10) gets the two small validators: weight 20.
        tower.process_vote(signed_vote(&keys[0], 10, [1u8; 32], None));
        tower.process_vote(signed_vote(&keys[1], 10, [1u8; 32], None));
        // Fork B (slot 20) gets the whale: weight 100.
        tower.process_vote(signed_vote(&keys[3], 20, [2u8; 32], None));

        // 20 * 3 = 60 is not > 130 * 2 = 260, but 100 * 3 = 300 is.
        assert!(!tower.is_slot_confirmed(10));
        assert!(tower.is_slot_confirmed(20));
        assert_eq!(tower.best_head(), Some([2u8; 32]));
    }

    #[test]
    fn test_strict_two_thirds_is_not_enough() {
        let tower = Tower::new(0);
        let keys: Vec<_> = (0..3).map(test_key).collect();
        for key in &keys {
            tower.register_validator(key.verifying_key().to_bytes(), 1);
        }

        tower.process_vote(signed_vote(&keys[0], 5, [7u8; 32], None));
        tower.process_vote(signed_vote(&keys[1], 5, [7u8; 32], None));
        // Exactly 2/3 of 3 validators — still short of a strict supermajority.
        assert!(!tower.is_slot_confirmed(5));

        tower.process_vote(signed_vote(&keys[2], 5, [7u8; 32], None));
        assert!(tower.is_slot_confirmed(5));
    }

    #[test]
    fn test_parent_links_and_heaviest_chain() {
        let tower = Tower::new(0);
        let k0 = test_key(0);
        let k1 = test_key(1);
        let k2 = test_key(2);

        // Fork A is a chain: A(slot1) -> A2(slot2), two votes deep.
        tower.process_vote(signed_vote(&k0, 1, [0xaau8; 32], Some(0)));
        tower.process_vote(signed_vote(&k1, 2, [0xa2u8; 32], Some(1)));
        // Fork B is a single block at slot 1 with equal per-block weight.
        tower.process_vote(signed_vote(&k2, 1, [0xbbu8; 32], Some(0)));

        let tree = tower.debug_tree();
        let a = tree.iter().find(|n| n.block_hash == [0xaau8; 32]).unwrap();
        assert_eq!(a.children, vec![[0xa2u8; 32]]);

        let a2 = tree.iter().find(|n| n.block_hash == [0xa2u8; 32]).unwrap();
        assert_eq!(a2.parent_hash, Some([0xaau8; 32]));

        // The deeper fork A carries more stake along its chain, so its tip wins.
        assert_eq!(tower.best_head(), Some([0xa2u8; 32]));
    }

    #[test]
    fn test_lockout_resets_when_extending_root() {
        let tower = Tower::new(0);
        let sk = test_key(5);

        // Build up several consecutive votes.
        for slot in 1..=5 {
            tower.process_vote(signed_vote(&sk, slot, [slot as u8; 32], Some(slot - 1)));
        }

        // Advance the root to the validator's last vote; lockout resets, so a
        // vote building straight on the new root is accepted immediately.
        tower.advance_root(5);
        let mut next = Vote::new(sk.verifying_key().to_bytes(), 6, [6u8; 32], Some(5));
        next.sign(&sk);
        assert!(tower.process_vote(next));
    }
}
