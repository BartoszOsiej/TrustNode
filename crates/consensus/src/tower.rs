//! Tower — the vote tree that tracks validator voting history
//!
//! The Tower is Solana's fork choice rule. It maintains a tree of votes
//! and uses lockout-based switching to prevent long-range attacks.

use crate::vote::{ValidatorId, Vote, VoteState};
use dashmap::DashMap;
use parking_lot::RwLock;

/// A node in the tower tree
#[derive(Debug, Clone)]
pub struct TowerNode {
    /// Block hash at this node
    pub block_hash: [u8; 32],
    /// Slot
    pub slot: u64,
    /// Parent slot
    pub parent_slot: Option<u64>,
    /// Accumulated weight (number of validator votes)
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
            root_slot: RwLock::new(genesis_slot),
            best_head: RwLock::new(None),
            total_votes: RwLock::new(0),
            total_switches: RwLock::new(0),
        }
    }

    /// Process a vote
    pub fn process_vote(&self, vote: Vote) -> bool {
        // Get or create validator state
        let mut validator_state = self
            .validator_states
            .entry(vote.validator)
            .or_insert_with(|| VoteState::new(vote.validator, *self.root_slot.read()));

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

        // Update tower node
        let mut node = self
            .nodes
            .entry(vote.block_hash)
            .or_insert_with(|| TowerNode {
                block_hash: vote.block_hash,
                slot: vote.slot,
                parent_slot: vote.parent_slot,
                weight: 0,
                voters: Vec::new(),
                children: Vec::new(),
            });
        node.weight += 1;
        node.voters.push(vote.validator);
        drop(node);

        // Update parent's children list
        if let Some(_parent_hash) = vote.parent_slot {
            // We need the parent's block hash — for now skip if not found
            // In a real impl, we'd look it up by slot
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

    /// Update the best head based on accumulated weight
    fn update_best_head(&self) {
        let mut best_hash = None;
        let mut best_weight = 0u64;

        for entry in self.nodes.iter() {
            if entry.value().weight > best_weight {
                best_weight = entry.value().weight;
                best_hash = Some(*entry.key());
            }
        }

        if let Some(hash) = best_hash {
            *self.best_head.write() = Some(hash);
        }
    }

    /// Check if a slot is confirmed (2/3+ validators voted for it)
    pub fn is_slot_confirmed(&self, slot: u64) -> bool {
        let total_validators = self.validator_states.len() as u64;
        if total_validators == 0 {
            return false;
        }

        let threshold = (total_validators * 2) / 3;

        // Find the block with this slot and check weight
        for entry in self.nodes.iter() {
            if entry.value().slot == slot && entry.value().weight >= threshold {
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
        }

        // Prune old nodes
        self.nodes.retain(|_, node| node.slot >= new_root);
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
        let validator = test_validator(1);
        let vote = Vote::new(validator, 10, [42u8; 32], None);

        assert!(tower.process_vote(vote));
        assert_eq!(tower.stats().total_votes, 1);
    }

    #[test]
    fn test_multiple_validators_vote_same_block() {
        let tower = Tower::new(0);

        for i in 0..5 {
            let validator = test_validator(i);
            let vote = Vote::new(validator, 10, [42u8; 32], None);
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
            let validator = test_validator(i);
            let vote = Vote::new(validator, 10, [1u8; 32], None);
            tower.process_vote(vote);
        }

        // Fork B: 5 votes
        for i in 3..8 {
            let validator = test_validator(i);
            let vote = Vote::new(validator, 10, [2u8; 32], None);
            tower.process_vote(vote);
        }

        // Fork B should win
        assert_eq!(tower.best_head(), Some([2u8; 32]));
    }
}
