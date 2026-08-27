//! PoH Entries — the building blocks of the PoH chain
//!
//! Each entry represents either a tick (heartbeat) or a transaction.
//! Entries are chained together via PoH hashes to create an immutable,
//! time-ordered log of all events.

use crate::hasher::PohHash;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// A single entry in the PoH chain
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PohEntry {
    /// PoH hash that proves this entry's position in time
    pub hash: PohHash,
    /// Hash of the previous entry (chains entries together)
    pub prev_hash: PohHash,
    /// Number of hashes since the previous entry
    pub num_hashes: u64,
    /// Slot this entry belongs to
    pub slot: u64,
    /// Index within the slot
    pub index: u64,
    /// Entry data (tick or transaction)
    pub data: EntryData,
}

/// What's inside an entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EntryData {
    /// Tick — heartbeat, proves time passing, no transactions
    Tick,
    /// Transaction — contains serialized transaction(s)
    Transactions(Vec<TransactionEntry>),
}

/// A transaction wrapped in a PoH entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionEntry {
    /// Serialized transaction bytes
    pub transaction: Vec<u8>,
    /// Signature over the transaction
    pub signature: Vec<u8>,
}

/// A completed PoH block (slot)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    /// Block hash (PoH hash at end of slot)
    pub hash: PohHash,
    /// Slot number
    pub slot: u64,
    /// Parent slot's hash (for chain linking)
    pub parent_hash: PohHash,
    /// All entries in this slot
    pub entries: Vec<PohEntry>,
    /// Block height
    pub height: u64,
    /// Timestamp (millis since epoch)
    pub timestamp: u64,
}

impl Entry {
    /// Create a new block
    pub fn new(
        hash: PohHash,
        slot: u64,
        parent_hash: PohHash,
        entries: Vec<PohEntry>,
        height: u64,
        timestamp: u64,
    ) -> Self {
        Self {
            hash,
            slot,
            parent_hash,
            entries,
            height,
            timestamp,
        }
    }

    /// Verify the PoH chain within this block
    pub fn verify_poh_chain(&self) -> bool {
        if self.entries.is_empty() {
            return false;
        }

        // Verify each entry's hash and chaining
        for (i, entry) in self.entries.iter().enumerate() {
            if !entry.verify() {
                tracing::warn!("Entry {} in slot {} failed verification", i, self.slot);
                return false;
            }

            // Verify chain: entry[i].prev_hash should equal entry[i-1].hash
            if i > 0 {
                let prev = &self.entries[i - 1];
                if entry.prev_hash != prev.hash {
                    tracing::warn!("Chain break at entry {}: prev_hash mismatch", i);
                    return false;
                }
            }
        }

        // Verify first entry's prev_hash matches block parent
        if let Some(first) = self.entries.first() {
            if first.prev_hash != self.parent_hash {
                return false;
            }
        }

        true
    }

    /// Get all transaction data from this block
    pub fn transactions(&self) -> Vec<&TransactionEntry> {
        self.entries
            .iter()
            .filter_map(|e| match &e.data {
                EntryData::Transactions(txs) => Some(txs.iter()),
                _ => None,
            })
            .flatten()
            .collect()
    }

    /// Calculate block hash from contents
    pub fn compute_hash(&self) -> PohHash {
        let mut hasher = Sha256::new();
        hasher.update(self.parent_hash.0);
        hasher.update(self.slot.to_le_bytes());
        hasher.update(self.height.to_le_bytes());

        for entry in &self.entries {
            hasher.update(entry.hash.0);
        }

        let result = hasher.finalize();
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(&result);
        PohHash(bytes)
    }
}

impl PohEntry {
    /// Compute the hash for this entry based on its contents.
    ///
    /// hash = SHA256(prev_hash || slot || index || num_hashes || data_bytes)
    pub fn compute_hash(&self) -> PohHash {
        let mut hasher = Sha256::new();
        hasher.update(self.prev_hash.0);
        hasher.update(self.slot.to_le_bytes());
        hasher.update(self.index.to_le_bytes());
        hasher.update(self.num_hashes.to_le_bytes());

        match &self.data {
            EntryData::Tick => {
                hasher.update(b"tick");
            }
            EntryData::Transactions(txs) => {
                for tx in txs {
                    hasher.update(&tx.transaction);
                    hasher.update(&tx.signature);
                }
            }
        }

        let result = hasher.finalize();
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(&result);
        PohHash(bytes)
    }

    /// Verify this entry's hash is correct
    pub fn verify(&self) -> bool {
        self.hash == self.compute_hash()
    }

    /// Create a tick entry with auto-computed hash
    pub fn tick(prev_hash: PohHash, num_hashes: u64, slot: u64, index: u64) -> Self {
        let mut entry = Self {
            hash: PohHash::zero(),
            prev_hash,
            num_hashes,
            slot,
            index,
            data: EntryData::Tick,
        };
        entry.hash = entry.compute_hash();
        entry
    }

    /// Create a transaction entry with auto-computed hash
    pub fn transaction(
        prev_hash: PohHash,
        num_hashes: u64,
        slot: u64,
        index: u64,
        transaction: Vec<u8>,
        signature: Vec<u8>,
    ) -> Self {
        let mut entry = Self {
            hash: PohHash::zero(),
            prev_hash,
            num_hashes,
            slot,
            index,
            data: EntryData::Transactions(vec![TransactionEntry {
                transaction,
                signature,
            }]),
        };
        entry.hash = entry.compute_hash();
        entry
    }

    /// Create an entry from explicit hash (for when hash comes from PoH chain)
    pub fn with_hash(
        hash: PohHash,
        prev_hash: PohHash,
        num_hashes: u64,
        slot: u64,
        index: u64,
        data: EntryData,
    ) -> Self {
        Self {
            hash,
            prev_hash,
            num_hashes,
            slot,
            index,
            data,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tick_entry() {
        let prev = PohHash::from_seed("prev");
        let entry = PohEntry::tick(prev, 100, 1, 0);
        assert!(entry.verify());
    }

    #[test]
    fn test_transaction_entry() {
        let prev = PohHash::from_seed("prev");
        let entry = PohEntry::transaction(
            prev,
            50,
            1,
            0,
            b"transfer 10 SOL".to_vec(),
            b"sig123".to_vec(),
        );
        assert!(entry.verify());
    }

    #[test]
    fn test_entry_hash_deterministic() {
        let prev = PohHash::from_seed("prev");
        let e1 = PohEntry::tick(prev, 42, 5, 3);
        let e2 = PohEntry::tick(prev, 42, 5, 3);
        assert_eq!(e1.hash, e2.hash);
    }

    #[test]
    fn test_entry_verify_rejects_tampered() {
        let prev = PohHash::from_seed("prev");
        let mut entry = PohEntry::tick(prev, 10, 1, 0);
        assert!(entry.verify());

        entry.num_hashes = 99;
        assert!(!entry.verify());
    }

    #[test]
    fn test_chain_linking() {
        let genesis = PohHash::from_seed("genesis");

        let e0 = PohEntry::tick(genesis, 1, 1, 0);
        let e1 = PohEntry::tick(e0.hash, 1, 1, 1);
        let e2 = PohEntry::tick(e1.hash, 1, 1, 2);

        // Each entry's prev_hash should match previous entry's hash
        assert_eq!(e0.prev_hash, genesis);
        assert_eq!(e1.prev_hash, e0.hash);
        assert_eq!(e2.prev_hash, e1.hash);

        // All should verify
        assert!(e0.verify());
        assert!(e1.verify());
        assert!(e2.verify());
    }

    #[test]
    fn test_block_creation() {
        let parent_hash = PohHash::from_seed("parent");

        let e0 = PohEntry::tick(parent_hash, 10, 1, 0);
        let e1 = PohEntry::tick(e0.hash, 10, 1, 1);

        let entries = vec![e0, e1];
        let block_hash = entries.last().unwrap().hash;

        let block = Entry::new(block_hash, 1, parent_hash, entries, 0, 1234567890);
        assert_eq!(block.slot, 1);
        assert_eq!(block.entries.len(), 2);
    }

    #[test]
    fn test_block_verify_poh_chain() {
        let parent_hash = PohHash::from_seed("parent");

        let e0 = PohEntry::tick(parent_hash, 10, 1, 0);
        let e1 = PohEntry::tick(e0.hash, 10, 1, 1);
        let block_hash = e1.hash;

        let block = Entry::new(block_hash, 1, parent_hash, vec![e0, e1], 0, 1234567890);

        assert!(block.verify_poh_chain());
    }
}
