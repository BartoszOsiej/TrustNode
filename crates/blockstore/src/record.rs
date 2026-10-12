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
    /// Production time, Unix milliseconds.
    ///
    /// This is **metadata only** — it is deliberately excluded from
    /// [`BlockRecord::compute_hash`]. Wall-clock time is not deterministic
    /// across nodes, so folding it into the block hash would make honest
    /// validators disagree on the hash of an identical block.
    pub timestamp_ms: u64,
}

impl BlockRecord {
    /// Build a record for a child of `parent_hash`.
    ///
    /// The `block_hash` is a deterministic function of the consensus
    /// contents — parent hash, slot, height, PoH hash and entry/transaction
    /// counts. The wall-clock `timestamp_ms` is recorded as metadata but is
    /// not part of the hash, so two nodes producing the same block agree on
    /// the same `block_hash` regardless of when they ran.
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

    /// Deterministic, consensus-critical block hash.
    ///
    /// Commits to every field that two honest nodes must agree on. It
    /// intentionally excludes `timestamp_ms` — see [`BlockRecord::timestamp_ms`].
    pub fn compute_hash(
        parent_hash: &[u8; 32],
        slot: u64,
        height: u64,
        poh_hash: &[u8; 32],
        entry_count: u64,
        tx_count: u64,
    ) -> [u8; 32] {
        use sha2::{Digest, Sha256};

        let mut hasher = Sha256::new();
        hasher.update(parent_hash);
        hasher.update(slot.to_le_bytes());
        hasher.update(height.to_le_bytes());
        hasher.update(poh_hash);
        hasher.update(entry_count.to_le_bytes());
        hasher.update(tx_count.to_le_bytes());
        hasher.finalize().into()
    }

    /// Check that `block_hash` matches the record's consensus contents.
    ///
    /// `timestamp_ms` is metadata and is not covered by the hash.
    pub fn verify_hash(&self) -> bool {
        self.block_hash
            == Self::compute_hash(
                &self.parent_hash,
                self.slot,
                self.height,
                &self.poh_hash,
                self.entry_count,
                self.tx_count,
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_deterministic() {
        let a = BlockRecord::compute_hash(&[1; 32], 5, 4, &[2; 32], 10, 3);
        let b = BlockRecord::compute_hash(&[1; 32], 5, 4, &[2; 32], 10, 3);
        assert_eq!(a, b);
    }

    #[test]
    fn hash_changes_with_slot() {
        let a = BlockRecord::compute_hash(&[1; 32], 5, 4, &[2; 32], 10, 3);
        let b = BlockRecord::compute_hash(&[1; 32], 6, 4, &[2; 32], 10, 3);
        assert_ne!(a, b);
    }

    #[test]
    fn hash_ignores_timestamp() {
        // Same consensus contents, different wall-clock production time:
        // the block hash must be identical on both nodes.
        let mut a = BlockRecord::new(GENESIS_PARENT, 0, 0, [7; 32], 1, 0);
        let mut b = a.clone();
        a.timestamp_ms = 1_000;
        b.timestamp_ms = 9_999_999;
        assert_eq!(a.block_hash, b.block_hash);
        assert_eq!(
            BlockRecord::compute_hash(&a.parent_hash, a.slot, a.height, &a.poh_hash, a.entry_count, a.tx_count),
            a.block_hash
        );
        assert!(a.verify_hash());
        assert!(b.verify_hash());
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

    #[test]
    fn tampered_timestamp_still_verifies() {
        // timestamp_ms is metadata, not consensus — changing it must not
        // break the hash chain.
        let mut rec = BlockRecord::new(GENESIS_PARENT, 0, 0, [7; 32], 1, 0);
        rec.timestamp_ms = 42;
        assert!(rec.verify_hash());
    }
}
