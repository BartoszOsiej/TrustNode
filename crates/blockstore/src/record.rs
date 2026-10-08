//! Block records — the unit of persistence in the ledger.
//!
//! Every produced block is captured as a [`BlockRecord`]. Records form a
//! hash chain: each record commits to its parent's `block_hash`, so any
//! tampering with history breaks every subsequent link.

use serde::{Deserialize, Serialize};

/// Parent hash of the first block in a fresh ledger.
pub const GENESIS_PARENT: [u8; 32] = [0u8; 32];

/// One persisted block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockRecord {
    /// Slot the block was produced in
    pub slot: u64,
    /// Block height (number of blocks before genesis)
    pub height: u64,
    /// `block_hash` of the previous record (`GENESIS_PARENT` for the first)
    pub parent_hash: [u8; 32],
    /// Hash of this record — commits to parent, slot, height and payload
    pub block_hash: [u8; 32],
    /// PoH hash at production time — lets a restarted validator resume the clock
    pub poh_hash: [u8; 32],
    /// Number of PoH entries in the block
    pub entry_count: u64,
    /// Number of transactions executed in the block
    pub tx_count: u64,
    /// Production time, Unix milliseconds
    pub timestamp_ms: u64,
}

impl BlockRecord {
    /// Build a record for a child of `parent_hash`.
    ///
    /// The `block_hash` is computed from every field, so two records with
    /// identical contents (same timestamp granularity) are interchangeable
    /// only if they describe the exact same block.
    pub fn new(
        parent_hash: [u8; 32],
        slot: u64,
        height: u64,
        poh_hash: [u8; 32],
        entry_count: u64,
        tx_count: u64,
    ) -> Self {
        let timestamp_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        let block_hash = Self::compute_hash(
            &parent_hash,
            slot,
            height,
            &poh_hash,
            entry_count,
            tx_count,
            timestamp_ms,
        );

        Self {
            slot,
            height,
            parent_hash,
            block_hash,
            poh_hash,
            entry_count,
            tx_count,
            timestamp_ms,
        }
    }

    /// Deterministic block hash over all record contents.
    pub fn compute_hash(
        parent_hash: &[u8; 32],
        slot: u64,
        height: u64,
        poh_hash: &[u8; 32],
        entry_count: u64,
        tx_count: u64,
        timestamp_ms: u64,
    ) -> [u8; 32] {
        use sha2::{Digest, Sha256};

        let mut hasher = Sha256::new();
        hasher.update(parent_hash);
        hasher.update(slot.to_le_bytes());
        hasher.update(height.to_le_bytes());
        hasher.update(poh_hash);
        hasher.update(entry_count.to_le_bytes());
        hasher.update(tx_count.to_le_bytes());
        hasher.update(timestamp_ms.to_le_bytes());
        hasher.finalize().into()
    }

    /// Check that `block_hash` matches the record contents.
    pub fn verify_hash(&self) -> bool {
        self.block_hash
            == Self::compute_hash(
                &self.parent_hash,
                self.slot,
                self.height,
                &self.poh_hash,
                self.entry_count,
                self.tx_count,
                self.timestamp_ms,
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_deterministic() {
        let a = BlockRecord::compute_hash(&[1; 32], 5, 4, &[2; 32], 10, 3, 1_700_000_000_000);
        let b = BlockRecord::compute_hash(&[1; 32], 5, 4, &[2; 32], 10, 3, 1_700_000_000_000);
        assert_eq!(a, b);
    }

    #[test]
    fn hash_changes_with_slot() {
        let a = BlockRecord::compute_hash(&[1; 32], 5, 4, &[2; 32], 10, 3, 1);
        let b = BlockRecord::compute_hash(&[1; 32], 6, 4, &[2; 32], 10, 3, 1);
        assert_ne!(a, b);
    }

    #[test]
    fn new_record_verifies() {
        let rec = BlockRecord::new(GENESIS_PARENT, 0, 0, [7; 32], 1, 0);
        assert!(rec.verify_hash());
        assert_eq!(rec.parent_hash, GENESIS_PARENT);
        assert!(rec.timestamp_ms > 0);
    }

    #[test]
    fn tampered_record_fails_verification() {
        let mut rec = BlockRecord::new(GENESIS_PARENT, 0, 0, [7; 32], 1, 0);
        rec.slot = 1;
        assert!(!rec.verify_hash());
    }
}
