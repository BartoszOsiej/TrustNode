//! Shred — erasure-coded pieces of a block
//!
//! A "shred" is a small piece of a block that's been erasure-coded.
//! Multiple shreds can be combined to reconstruct the original block.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use solana_poh::hasher::PohHash;

/// Type of shred
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ShredType {
    /// Data shred — contains actual block data
    Data,
    /// Coding shred — erasure coding parity
    Coding,
}

/// A single shred
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Shred {
    /// Unique shred ID (slot + index + type)
    pub id: ShredId,
    /// Slot this shred belongs to
    pub slot: u64,
    /// Index within the slot
    pub index: u64,
    /// Type of shred
    pub shred_type: ShredType,
    /// Payload data
    pub payload: Vec<u8>,
    /// Hash of the payload for integrity verification
    pub hash: PohHash,
    /// Signature (Ed25519)
    pub signature: Vec<u8>,
}

/// Shred identifier
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ShredId {
    pub slot: u64,
    pub index: u64,
    pub shred_type: ShredType,
}

impl ShredId {
    pub fn data(slot: u64, index: u64) -> Self {
        Self {
            slot,
            index,
            shred_type: ShredType::Data,
        }
    }

    pub fn coding(slot: u64, index: u64) -> Self {
        Self {
            slot,
            index,
            shred_type: ShredType::Coding,
        }
    }
}

impl Shred {
    /// Create a new data shred
    pub fn new_data(slot: u64, index: u64, payload: Vec<u8>) -> Self {
        let hash = Self::compute_hash(&payload);
        Self {
            id: ShredId::data(slot, index),
            slot,
            index,
            shred_type: ShredType::Data,
            payload,
            hash,
            signature: Vec::new(),
        }
    }

    /// Create a new coding shred
    pub fn new_coding(slot: u64, index: u64, payload: Vec<u8>) -> Self {
        let hash = Self::compute_hash(&payload);
        Self {
            id: ShredId::coding(slot, index),
            slot,
            index,
            shred_type: ShredType::Coding,
            payload,
            hash,
            signature: Vec::new(),
        }
    }

    /// Compute hash of the payload
    pub fn compute_hash(payload: &[u8]) -> PohHash {
        let mut hasher = Sha256::new();
        hasher.update(payload);
        let result = hasher.finalize();
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(&result);
        PohHash(bytes)
    }

    /// Verify shred integrity
    pub fn verify(&self) -> bool {
        self.hash == Self::compute_hash(&self.payload)
    }

    /// Get size in bytes
    pub fn size(&self) -> usize {
        std::mem::size_of::<ShredId>()
            + 8 // slot
            + 8 // index
            + 1 // shred_type
            + 4 + self.payload.len() // payload (length-prefixed)
            + 32 // hash
            + 4 + self.signature.len() // signature (length-prefixed)
    }

    /// Get the maximum shred payload size (like Solana's ~1228 bytes)
    pub fn max_payload_size() -> usize {
        1228
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_data_shred_creation() {
        let shred = Shred::new_data(10, 0, vec![1, 2, 3, 4]);
        assert_eq!(shred.slot, 10);
        assert_eq!(shred.index, 0);
        assert_eq!(shred.shred_type, ShredType::Data);
        assert!(shred.verify());
    }

    #[test]
    fn test_coding_shred_creation() {
        let shred = Shred::new_coding(10, 0, vec![5, 6, 7, 8]);
        assert_eq!(shred.shred_type, ShredType::Coding);
        assert!(shred.verify());
    }

    #[test]
    fn test_shred_integrity() {
        let mut shred = Shred::new_data(10, 0, vec![1, 2, 3]);
        assert!(shred.verify());

        // Tamper with payload
        shred.payload[0] = 99;
        assert!(!shred.verify());
    }

    #[test]
    fn test_shred_id_equality() {
        let id1 = ShredId::data(10, 0);
        let id2 = ShredId::data(10, 0);
        assert_eq!(id1, id2);

        let id3 = ShredId::coding(10, 0);
        assert_ne!(id1, id3); // Different type
    }
}
