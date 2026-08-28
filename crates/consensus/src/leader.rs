//! Leader Schedule — round-robin leader rotation based on PoH
//!
//! Solana uses a deterministic leader schedule: validators take turns
//! being the leader (block producer) based on their stake weight.

use crate::vote::ValidatorId;
use std::collections::HashMap;

/// A validator's stake
#[derive(Debug, Clone)]
pub struct ValidatorStake {
    pub validator: ValidatorId,
    pub lamports: u64,
    pub name: String,
}

/// Leader schedule — determines who produces which slot
pub struct LeaderSchedule {
    /// Ordered list of validator stakes (sorted by lamports descending)
    validators: Vec<ValidatorStake>,
    /// Slots per leader (how many consecutive slots each leader gets)
    slots_per_leader: u64,
    /// Epoch length in slots
    epoch_length: u64,
    /// Current epoch
    current_epoch: u64,
    /// Precomputed schedule: slot -> validator index
    schedule: Vec<usize>,
}

impl LeaderSchedule {
    /// Create a new leader schedule from validator stakes
    pub fn new(validators: Vec<ValidatorStake>, slots_per_leader: u64, epoch_length: u64) -> Self {
        assert!(!validators.is_empty(), "Must have at least one validator");

        let mut sorted = validators;
        sorted.sort_by_key(|b| std::cmp::Reverse(b.lamports));

        let mut schedule = Self {
            validators: sorted,
            slots_per_leader,
            epoch_length,
            current_epoch: 0,
            schedule: Vec::new(),
        };

        schedule.compute_schedule();
        schedule
    }

    /// Compute the leader schedule for the current epoch
    fn compute_schedule(&mut self) {
        self.schedule.clear();

        let total_stake: u64 = self.validators.iter().map(|v| v.lamports).sum();
        let total_slots = self.epoch_length;

        // Assign slots proportionally to stake
        for (idx, validator) in self.validators.iter().enumerate() {
            let proportion = validator.lamports as f64 / total_stake as f64;
            let num_slots = (proportion * total_slots as f64) as u64;

            for _ in 0..num_slots {
                self.schedule.push(idx);
            }
        }

        // Fill remaining slots with round-robin
        let mut idx = 0;
        while self.schedule.len() < total_slots as usize {
            self.schedule.push(idx % self.validators.len());
            idx += 1;
        }

        tracing::info!(
            "Leader schedule computed: {} validators, {} slots/epoch, {} slots/leader",
            self.validators.len(),
            self.epoch_length,
            self.slots_per_leader,
        );
    }

    /// Get the leader for a specific slot
    pub fn leader_for_slot(&self, slot: u64) -> &ValidatorStake {
        let idx = self.schedule[slot as usize % self.schedule.len()];
        &self.validators[idx]
    }

    /// Get the leader index for a specific slot
    pub fn leader_index_for_slot(&self, slot: u64) -> usize {
        self.schedule[slot as usize % self.schedule.len()]
    }

    /// Check if a validator is the leader for a slot
    pub fn is_leader(&self, validator: &ValidatorId, slot: u64) -> bool {
        self.leader_for_slot(slot).validator == *validator
    }

    /// Get all slots where a validator is leader
    pub fn slots_for_leader(&self, validator: &ValidatorId) -> Vec<u64> {
        self.schedule
            .iter()
            .enumerate()
            .filter(|(_, &idx)| self.validators[idx].validator == *validator)
            .map(|(slot, _)| slot as u64)
            .collect()
    }

    /// Get the next leader after a given slot
    pub fn next_leader(&self, after_slot: u64) -> &ValidatorStake {
        self.leader_for_slot(after_slot + 1)
    }

    /// Advance to the next epoch
    pub fn advance_epoch(&mut self) {
        self.current_epoch += 1;
        self.compute_schedule();
    }

    /// Get current epoch
    pub fn current_epoch(&self) -> u64 {
        self.current_epoch
    }

    /// Get total validators
    pub fn total_validators(&self) -> usize {
        self.validators.len()
    }

    /// Get epoch length
    pub fn epoch_length(&self) -> u64 {
        self.epoch_length
    }

    /// Get schedule as a debug map (slot -> validator name)
    pub fn debug_schedule(&self) -> HashMap<u64, String> {
        self.schedule
            .iter()
            .enumerate()
            .map(|(slot, &idx)| (slot as u64, self.validators[idx].name.clone()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_validator(id: u8, stake: u64) -> ValidatorStake {
        ValidatorStake {
            validator: {
                let mut v = [0u8; 32];
                v[0] = id;
                v
            },
            lamports: stake,
            name: format!("validator_{}", id),
        }
    }

    #[test]
    fn test_leader_schedule_creation() {
        let validators = vec![
            make_validator(1, 1000),
            make_validator(2, 500),
            make_validator(3, 500),
        ];

        let schedule = LeaderSchedule::new(validators, 4, 100);
        assert_eq!(schedule.total_validators(), 3);
        assert_eq!(schedule.epoch_length(), 100);
    }

    #[test]
    fn test_leader_for_slot() {
        let validators = vec![make_validator(1, 1000), make_validator(2, 500)];

        let schedule = LeaderSchedule::new(validators, 4, 100);

        // Validator 1 should get more slots (2x stake)
        let v1_slots = schedule.slots_for_leader(&make_validator(1, 0).validator);
        let v2_slots = schedule.slots_for_leader(&make_validator(2, 0).validator);

        assert!(v1_slots.len() >= v2_slots.len());
    }

    #[test]
    fn test_is_leader() {
        let validators = vec![make_validator(1, 1000)];
        let schedule = LeaderSchedule::new(validators, 4, 100);

        // With only one validator, they should be leader for all slots
        assert!(schedule.is_leader(&make_validator(1, 0).validator, 0));
        assert!(schedule.is_leader(&make_validator(1, 0).validator, 50));
    }

    #[test]
    fn test_advance_epoch() {
        let validators = vec![make_validator(1, 1000)];
        let mut schedule = LeaderSchedule::new(validators, 4, 100);

        assert_eq!(schedule.current_epoch(), 0);
        schedule.advance_epoch();
        assert_eq!(schedule.current_epoch(), 1);
    }
}
