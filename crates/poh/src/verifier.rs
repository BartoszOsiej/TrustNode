//! PoH Verifier — verify the integrity of the PoH chain
//!
//! The verifier checks:
//! 1. Individual entry hashes
//! 2. Chain continuity between entries
//! 3. Full block PoH chains
//! 4. Cross-block chain linking

use crate::entry::{Entry, PohEntry};
use crate::hasher::PohHash;

/// PoH verification result
#[derive(Debug, Clone)]
pub struct VerificationResult {
    pub valid: bool,
    pub errors: Vec<String>,
    pub entries_verified: u64,
    pub total_hashes_verified: u64,
}

impl VerificationResult {
    pub fn ok(entries: u64, hashes: u64) -> Self {
        Self {
            valid: true,
            errors: Vec::new(),
            entries_verified: entries,
            total_hashes_verified: hashes,
        }
    }

    pub fn fail(error: String) -> Self {
        Self {
            valid: false,
            errors: vec![error],
            entries_verified: 0,
            total_hashes_verified: 0,
        }
    }

    pub fn add_error(&mut self, error: String) {
        self.errors.push(error);
        self.valid = false;
    }
}

/// PoH chain verifier
pub struct PohVerifier {
    /// The last known valid hash
    last_valid_hash: Option<PohHash>,
    /// Total entries verified
    entries_verified: u64,
    /// Total hashes verified
    hashes_verified: u64,
}

impl PohVerifier {
    pub fn new() -> Self {
        Self {
            last_valid_hash: None,
            entries_verified: 0,
            hashes_verified: 0,
        }
    }

    pub fn new_with_state(last_valid_hash: PohHash) -> Self {
        Self {
            last_valid_hash: Some(last_valid_hash),
            entries_verified: 0,
            hashes_verified: 0,
        }
    }

    /// Verify a single entry's hash
    pub fn verify_entry(&self, entry: &PohEntry) -> bool {
        entry.verify()
    }

    /// Verify a chain of entries within a slot
    pub fn verify_slot_entries(&self, entries: &[PohEntry]) -> VerificationResult {
        if entries.is_empty() {
            return VerificationResult::fail("Empty entries".to_string());
        }

        let mut total_hashes = 0u64;

        for (i, entry) in entries.iter().enumerate() {
            // Verify individual entry hash
            if !entry.verify() {
                return VerificationResult::fail(format!("Entry {} hash verification failed", i));
            }
            total_hashes += entry.num_hashes;
        }

        // Verify chaining between consecutive entries
        for window in entries.windows(2) {
            let current = &window[0];
            let next = &window[1];

            // Verify chain: next.prev_hash should equal current.hash
            if next.prev_hash != current.hash {
                return VerificationResult::fail(format!(
                    "Chain break: next.prev_hash {:?} != current.hash {:?}",
                    next.prev_hash, current.hash
                ));
            }
        }

        let mut result = VerificationResult::ok(entries.len() as u64, total_hashes);
        // Note: tracking entries_verified happens at a higher level
        result
    }

    /// Verify a full block
    pub fn verify_block(&self, block: &Entry) -> VerificationResult {
        // Verify entries
        let mut result = self.verify_slot_entries(&block.entries);
        if !result.valid {
            return result;
        }

        // Verify block hash matches last entry hash
        if !block.entries.is_empty() {
            let last_entry = block.entries.last().unwrap();
            if last_entry.hash != block.hash {
                result.add_error(format!(
                    "Block hash mismatch: expected {:?}, got {:?}",
                    block.hash, last_entry.hash
                ));
            }
        }

        result
    }

    /// Verify chain continuity between two blocks
    pub fn verify_chain_link(&self, parent: &Entry, child: &Entry) -> VerificationResult {
        // Child's parent_hash should match parent's hash
        if parent.hash != child.parent_hash {
            return VerificationResult::fail(format!(
                "Chain link broken: parent hash {:?} != child parent_hash {:?}",
                parent.hash, child.parent_hash
            ));
        }

        // Child slot should be parent slot + 1
        if child.slot != parent.slot + 1 {
            return VerificationResult::fail(format!(
                "Slot discontinuity: parent={}, child={}",
                parent.slot, child.slot
            ));
        }

        // Child height should be parent height + 1
        if child.height != parent.height + 1 {
            return VerificationResult::fail(format!(
                "Height discontinuity: parent={}, child={}",
                parent.height, child.height
            ));
        }

        VerificationResult::ok(0, 0)
    }

    /// Get total entries verified
    pub fn entries_verified(&self) -> u64 {
        self.entries_verified
    }

    /// Get total hashes verified
    pub fn hashes_verified(&self) -> u64 {
        self.hashes_verified
    }
}

impl Default for PohVerifier {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::{EntryData, PohEntry};

    /// Helper: create a chain of tick entries
    fn make_chained_entries(count: usize, slot: u64) -> Vec<PohEntry> {
        let mut entries = Vec::new();
        let mut prev = PohHash::from_seed(&format!("genesis_{}", slot));
        for i in 0..count {
            let entry = PohEntry::tick(prev, 1, slot, i as u64);
            prev = entry.hash;
            entries.push(entry);
        }
        entries
    }

    #[test]
    fn test_verify_single_tick() {
        let verifier = PohVerifier::new();
        let genesis = PohHash::from_seed("genesis");
        let entry = PohEntry::tick(genesis, 10, 1, 0);
        assert!(verifier.verify_entry(&entry));
    }

    #[test]
    fn test_verify_slot_entries() {
        let verifier = PohVerifier::new();
        let entries = make_chained_entries(5, 1);
        let result = verifier.verify_slot_entries(&entries);
        assert!(result.valid, "Chain should be valid: {:?}", result.errors);
    }

    #[test]
    fn test_verify_block() {
        let verifier = PohVerifier::new();
        let parent_hash = PohHash::from_seed("parent");

        let mut entries = Vec::new();
        let mut prev = parent_hash;
        for i in 0..3 {
            let entry = PohEntry::tick(prev, 1, 1, i);
            prev = entry.hash;
            entries.push(entry);
        }

        let block_hash = entries.last().unwrap().hash;
        let block = Entry::new(block_hash, 1, parent_hash, entries, 0, 1234567890);

        let result = verifier.verify_block(&block);
        assert!(result.valid, "Block should be valid: {:?}", result.errors);
    }

    #[test]
    fn test_chain_link_verification() {
        let verifier = PohVerifier::new();

        let parent = Entry::new(
            PohHash::from_seed("parent_hash"),
            1,
            PohHash::zero(),
            vec![PohEntry::tick(PohHash::from_seed("p_e1"), 1, 1, 0)],
            0,
            1000,
        );

        let child = Entry::new(
            PohHash::from_seed("child_hash"),
            2,
            PohHash::from_seed("parent_hash"),
            vec![PohEntry::tick(PohHash::from_seed("c_e1"), 1, 2, 0)],
            1,
            1400,
        );

        let result = verifier.verify_chain_link(&parent, &child);
        assert!(
            result.valid,
            "Chain link should be valid: {:?}",
            result.errors
        );
    }
}
