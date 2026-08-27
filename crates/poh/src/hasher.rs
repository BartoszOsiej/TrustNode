//! Core PoH hasher — the SHA-256 hash chain engine
//!
//! This is the beating heart of Proof of History. It maintains a running
//! SHA-256 hash that proves sequential computation.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// A PoH hash — 32 bytes representing a position in the hash chain
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PohHash(pub [u8; 32]);

impl PohHash {
    /// Create a new PohHash from raw bytes
    pub fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Create a PohHash from a seed string
    pub fn from_seed(seed: &str) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(seed.as_bytes());
        let result = hasher.finalize();
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(&result);
        Self(bytes)
    }

    /// Get the inner bytes
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Convert to hex string
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// Zero hash (genesis)
    pub fn zero() -> Self {
        Self([0u8; 32])
    }
}

impl std::fmt::Display for PohHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", &self.to_hex()[..16])
    }
}

/// The PoH Hasher — maintains a running SHA-256 hash chain
///
/// Each call to `hash()` advances the chain by one step.
/// The hash chain proves that time has passed — you can't skip ahead
/// without doing the sequential computation.
#[derive(Debug)]
pub struct PohHasher {
    /// Current hash in the chain
    current_hash: PohHash,
    /// Number of hashes computed so far
    hash_count: u64,
    /// Slot (epoch segment) this hasher belongs to
    slot: u64,
}

impl PohHasher {
    /// Create a new PoH hasher with a given seed
    pub fn new(seed: PohHash, slot: u64) -> Self {
        Self {
            current_hash: seed,
            hash_count: 0,
            slot,
        }
    }

    /// Create a genesis hasher (starts from zero)
    pub fn genesis() -> Self {
        Self::new(PohHash::zero(), 0)
    }

    /// Perform a single hash iteration
    pub fn hash(&mut self) -> PohHash {
        let mut hasher = Sha256::new();
        hasher.update(self.current_hash.0);
        let result = hasher.finalize();
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(&result);
        self.current_hash = PohHash(bytes);
        self.hash_count += 1;
        self.current_hash
    }

    /// Perform N hash iterations and return the final hash
    pub fn hash_n(&mut self, n: u64) -> PohHash {
        for _ in 0..n {
            self.hash();
        }
        self.current_hash
    }

    /// Hash with a mix-in value (for transaction inclusion)
    pub fn hash_with(&mut self, data: &[u8]) -> PohHash {
        let mut hasher = Sha256::new();
        hasher.update(self.current_hash.0);
        hasher.update(data);
        let result = hasher.finalize();
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(&result);
        self.current_hash = PohHash(bytes);
        self.hash_count += 1;
        self.current_hash
    }

    /// Get the current hash
    pub fn current(&self) -> PohHash {
        self.current_hash
    }

    /// Get the number of hashes computed
    pub fn count(&self) -> u64 {
        self.hash_count
    }

    /// Get the current slot
    pub fn slot(&self) -> u64 {
        self.slot
    }

    /// Reset for a new slot
    pub fn new_slot(&mut self, slot: u64) {
        self.slot = slot;
        self.hash_count = 0;
        // Chain continues from current hash (this IS the proof of history!)
    }

    /// Verify that `steps` hashes from `start_hash` leads to `end_hash`
    pub fn verify(start_hash: PohHash, steps: u64, end_hash: PohHash) -> bool {
        let mut hasher = PohHasher::new(start_hash, 0);
        let computed = hasher.hash_n(steps);
        computed == end_hash
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hash_deterministic() {
        let seed = PohHash::from_seed("test");
        let mut h1 = PohHasher::new(seed, 0);
        let mut h2 = PohHasher::new(seed, 0);

        let r1 = h1.hash_n(100);
        let r2 = h2.hash_n(100);

        assert_eq!(r1, r2);
    }

    #[test]
    fn test_hash_chain_verification() {
        let seed = PohHash::from_seed("genesis");
        let mut hasher = PohHasher::new(seed, 0);
        let after_100 = hasher.hash_n(100);

        assert!(PohHasher::verify(seed, 100, after_100));
        assert!(!PohHasher::verify(seed, 99, after_100));
        assert!(!PohHasher::verify(seed, 101, after_100));
    }

    #[test]
    fn test_hash_with_mix_in() {
        let seed = PohHash::from_seed("test");
        let mut h1 = PohHasher::new(seed, 0);
        let mut h2 = PohHasher::new(seed, 0);

        h1.hash_n(50);
        h1.hash_with(b"transaction_data");

        h2.hash_n(50);
        h2.hash_with(b"different_data");

        assert_ne!(h1.current(), h2.current());
    }
}
