//! Shred — erasure-coded pieces of a block
//!
//! A "shred" is a small piece of a block that's been erasure-coded.
//! Multiple shreds can be combined to reconstruct the original block.

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
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
    /// Public key of the signer (the leader that produced this shred)
    pub signer: [u8; 32],
    /// Signature (Ed25519) over the shred's signable data
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
    /// Create a new data shred (unsigned).
    pub fn new_data(slot: u64, index: u64, payload: Vec<u8>) -> Self {
        let hash = Self::compute_hash(&payload);
        Self {
            id: ShredId::data(slot, index),
            slot,
            index,
            shred_type: ShredType::Data,
            payload,
            hash,
            signer: [0u8; 32],
            signature: Vec::new(),
        }
    }

    /// Create a new coding shred (unsigned).
    pub fn new_coding(slot: u64, index: u64, payload: Vec<u8>) -> Self {
        let hash = Self::compute_hash(&payload);
        Self {
            id: ShredId::coding(slot, index),
            slot,
            index,
            shred_type: ShredType::Coding,
            payload,
            hash,
            signer: [0u8; 32],
            signature: Vec::new(),
        }
    }

    /// Bytes covered by the shred signature: type, slot, index and the full
    /// payload (length-prefixed, so encoding is injective). `hash` is derived
    /// from the payload and therefore covered transitively.
    pub fn signable_data(&self) -> Vec<u8> {
        let mut data = Vec::with_capacity(1 + 8 + 8 + 4 + self.payload.len());
        data.push(match self.shred_type {
            ShredType::Data => 0,
            ShredType::Coding => 1,
        });
        data.extend_from_slice(&self.slot.to_le_bytes());
        data.extend_from_slice(&self.index.to_le_bytes());
        data.extend_from_slice(&(self.payload.len() as u32).to_le_bytes());
        data.extend_from_slice(&self.payload);
        data
    }

    /// Sign this shred. Binds `signer` to the key's public key.
    pub fn sign(&mut self, key: &SigningKey) {
        self.signer = key.verifying_key().to_bytes();
        self.signature = key.sign(&self.signable_data()).to_bytes().to_vec();
    }

    /// Verify the shred's Ed25519 signature against its embedded signer.
    ///
    /// Returns `false` for unsigned shreds, malformed signatures and any
    /// tampering with the signable data.
    pub fn verify_signature(&self) -> bool {
        let Ok(vk) = VerifyingKey::from_bytes(&self.signer) else {
            return false;
        };
        let Ok(sig) = Signature::from_slice(&self.signature) else {
            return false;
        };
        vk.verify_strict(&self.signable_data(), &sig).is_ok()
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
            + 32 // signer
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

    fn key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    #[test]
    fn test_sign_and_verify_shred() {
        let sk = key(1);
        let mut shred = Shred::new_data(10, 3, vec![1, 2, 3, 4]);
        assert!(!shred.verify_signature(), "unsigned shreds must not verify");

        shred.sign(&sk);
        assert_eq!(shred.signer, sk.verifying_key().to_bytes());
        assert!(shred.verify_signature());
        assert!(shred.verify());
    }

    #[test]
    fn test_tampered_shred_fails_signature() {
        let sk = key(1);
        let mut shred = Shred::new_data(10, 3, vec![1, 2, 3, 4]);
        shred.sign(&sk);
        assert!(shred.verify_signature());

        // Change the payload after signing — both integrity and signature fail.
        shred.payload[0] = 9;
        assert!(!shred.verify());
        assert!(!shred.verify_signature());

        // Recompute the payload hash but keep the old signature: the
        // signature still covers the payload bytes, so it must fail.
        shred.hash = Shred::compute_hash(&shred.payload);
        assert!(shred.verify());
        assert!(!shred.verify_signature());
    }

    #[test]
    fn test_signature_covers_shred_metadata() {
        let sk = key(1);
        let mut shred = Shred::new_data(10, 3, vec![1, 2, 3]);
        shred.sign(&sk);

        // Moving the shred to another slot/index invalidates the signature.
        shred.slot = 11;
        assert!(!shred.verify_signature());
        shred.slot = 10;
        shred.index = 4;
        assert!(!shred.verify_signature());
        shred.index = 3;
        shred.shred_type = ShredType::Coding;
        assert!(!shred.verify_signature());
    }

    #[test]
    fn test_wrong_signer_rejected() {
        let mut shred = Shred::new_data(10, 0, vec![7]);
        shred.sign(&key(1));

        // Impersonate another signer after signing.
        shred.signer = key(2).verifying_key().to_bytes();
        assert!(!shred.verify_signature());

        // Garbage signatures are rejected outright.
        shred.sign(&key(1));
        shred.signature = vec![1u8; 64];
        assert!(!shred.verify_signature());
        shred.signature = Vec::new();
        assert!(!shred.verify_signature());
    }
}
