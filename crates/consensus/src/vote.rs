//! Vote data structures for Tower BFT

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Validator identity (public key)
pub type ValidatorId = [u8; 32];

/// A vote cast by a validator
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Vote {
    /// Validator who cast this vote
    pub validator: ValidatorId,
    /// Slot being voted for
    pub slot: u64,
    /// Hash of the block being voted for
    pub block_hash: [u8; 32],
    /// Parent slot of the voted block
    pub parent_slot: Option<u64>,
    /// Vote timestamp (millis since epoch)
    pub timestamp: u64,
    /// Signature over the vote
    pub signature: Vec<u8>,
    /// Vote state hash (for switch tracking)
    pub state_hash: [u8; 32],
}

impl Vote {
    /// Create a new vote
    pub fn new(
        validator: ValidatorId,
        slot: u64,
        block_hash: [u8; 32],
        parent_slot: Option<u64>,
    ) -> Self {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        let state_hash = Self::compute_state_hash(validator, slot, block_hash);

        Self {
            validator,
            slot,
            block_hash,
            parent_slot,
            timestamp,
            signature: Vec::new(),
            state_hash,
        }
    }

    /// Compute the vote state hash
    pub fn compute_state_hash(validator: ValidatorId, slot: u64, block_hash: [u8; 32]) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(validator);
        hasher.update(slot.to_le_bytes());
        hasher.update(block_hash);
        let result = hasher.finalize();
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(&result);
        bytes
    }

    /// Get the data to sign
    pub fn signable_data(&self) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(&self.validator);
        data.extend_from_slice(&self.slot.to_le_bytes());
        data.extend_from_slice(&self.block_hash);
        data.extend_from_slice(&self.timestamp.to_le_bytes());
        data
    }

    /// Verify vote signature (stub — in real impl, use Ed25519)
    pub fn verify_signature(&self) -> bool {
        !self.signature.is_empty()
    }
}

/// Vote state tracks a validator's voting history
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoteState {
    /// Validator ID
    pub validator: ValidatorId,
    /// Current root slot (last confirmed slot)
    pub root_slot: u64,
    /// Last voted slot
    pub last_vote_slot: u64,
    /// Last voted block hash
    pub last_vote_hash: [u8; 32],
    /// Number of consecutive votes on current fork
    pub consecutive_votes: u64,
    /// Lockout multiplier (increases with each switch)
    pub lockout_multiplier: u64,
    /// Total votes cast
    pub total_votes: u64,
    /// Vote history (recent slots voted)
    pub history: Vec<VoteHistoryEntry>,
}

/// A single entry in the vote history
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoteHistoryEntry {
    pub slot: u64,
    pub block_hash: [u8; 32],
    pub timestamp: u64,
}

impl VoteState {
    /// Create a new vote state for a validator
    pub fn new(validator: ValidatorId, genesis_slot: u64) -> Self {
        Self {
            validator,
            root_slot: genesis_slot,
            last_vote_slot: 0,
            last_vote_hash: [0u8; 32],
            consecutive_votes: 0,
            lockout_multiplier: 1,
            total_votes: 0,
            history: Vec::new(),
        }
    }

    /// Process a new vote
    pub fn process_vote(&mut self, vote: &Vote) {
        self.last_vote_slot = vote.slot;
        self.last_vote_hash = vote.block_hash;
        self.consecutive_votes += 1;
        self.total_votes += 1;

        self.history.push(VoteHistoryEntry {
            slot: vote.slot,
            block_hash: vote.block_hash,
            timestamp: vote.timestamp,
        });

        // Keep history bounded
        if self.history.len() > 32 {
            self.history.remove(0);
        }
    }

    /// Calculate lockout for switching forks
    ///
    /// Lockout increases exponentially: 2^consecutive_votes
    /// A validator must wait this many slots before voting on a different fork
    pub fn lockout_slots(&self) -> u64 {
        2u64.pow(self.consecutive_votes.min(31) as u32)
    }

    /// Check if the validator can switch forks
    pub fn can_switch_fork(&self, current_slot: u64) -> bool {
        let lockout = self.lockout_slots();
        current_slot >= self.last_vote_slot + lockout
    }

    /// Record a fork switch
    pub fn switch_fork(&mut self) {
        self.consecutive_votes = 0;
        self.lockout_multiplier = self.lockout_multiplier.saturating_mul(2);
    }

    /// Update root (advance root after confirmation)
    pub fn update_root(&mut self, new_root: u64) {
        self.root_slot = new_root;

        // Remove history entries before root
        self.history.retain(|e| e.slot >= new_root);
    }

    /// Get the current tower hash (for fork choice)
    pub fn tower_hash(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(self.validator);
        hasher.update(self.last_vote_slot.to_le_bytes());
        hasher.update(self.last_vote_hash);
        hasher.update(self.consecutive_votes.to_le_bytes());
        let result = hasher.finalize();
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(&result);
        bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_validator() -> ValidatorId {
        let mut v = [0u8; 32];
        v[0] = 1;
        v
    }

    #[test]
    fn test_vote_creation() {
        let validator = test_validator();
        let vote = Vote::new(validator, 10, [42u8; 32], Some(9));

        assert_eq!(vote.slot, 10);
        assert_eq!(vote.validator, validator);
        assert_eq!(vote.parent_slot, Some(9));
    }

    #[test]
    fn test_vote_state_creation() {
        let validator = test_validator();
        let state = VoteState::new(validator, 0);

        assert_eq!(state.root_slot, 0);
        assert_eq!(state.consecutive_votes, 0);
        assert_eq!(state.total_votes, 0);
    }

    #[test]
    fn test_vote_state_processing() {
        let validator = test_validator();
        let mut state = VoteState::new(validator, 0);

        let vote = Vote::new(validator, 10, [1u8; 32], Some(9));
        state.process_vote(&vote);

        assert_eq!(state.last_vote_slot, 10);
        assert_eq!(state.consecutive_votes, 1);
        assert_eq!(state.total_votes, 1);
    }

    #[test]
    fn test_lockout_increases() {
        let validator = test_validator();
        let mut state = VoteState::new(validator, 0);

        // First vote: lockout = 2^1 = 2 slots
        let vote1 = Vote::new(validator, 10, [1u8; 32], Some(9));
        state.process_vote(&vote1);
        assert_eq!(state.lockout_slots(), 2);

        // Second consecutive vote: lockout = 2^2 = 4 slots
        let vote2 = Vote::new(validator, 20, [2u8; 32], Some(19));
        state.process_vote(&vote2);
        assert_eq!(state.lockout_slots(), 4);

        // Third: lockout = 2^3 = 8
        let vote3 = Vote::new(validator, 30, [3u8; 32], Some(29));
        state.process_vote(&vote3);
        assert_eq!(state.lockout_slots(), 8);
    }

    #[test]
    fn test_fork_switch() {
        let validator = test_validator();
        let mut state = VoteState::new(validator, 0);

        // Vote on fork A
        for i in 1..6 {
            let vote = Vote::new(validator, i * 10, [i as u8; 32], Some((i - 1) * 10));
            state.process_vote(&vote);
        }

        assert!(!state.can_switch_fork(50)); // Still locked

        // Switch fork — resets consecutive votes
        state.switch_fork();
        assert_eq!(state.consecutive_votes, 0);
        assert!(state.can_switch_fork(51)); // Can switch now (last_vote=50, lockout=1)
    }

    #[test]
    fn test_root_update() {
        let validator = test_validator();
        let mut state = VoteState::new(validator, 0);

        for i in 1..11 {
            let vote = Vote::new(validator, i, [i as u8; 32], Some(i - 1));
            state.process_vote(&vote);
        }

        assert_eq!(state.history.len(), 10);

        state.update_root(5);
        assert_eq!(state.root_slot, 5);
        // History should only contain entries with slot >= 5
        assert!(state.history.iter().all(|e| e.slot >= 5));
    }
}
