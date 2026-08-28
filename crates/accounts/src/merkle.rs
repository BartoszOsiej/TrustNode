//! Merkle tree for state root computation
//!
//! The state root is a Merkle hash of all account hashes.
//! This allows efficient proof generation and verification.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// State root hash
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StateRoot(pub [u8; 32]);

impl StateRoot {
    /// Compute state root from a list of account hashes
    pub fn compute(hashes: &[[u8; 32]]) -> Self {
        if hashes.is_empty() {
            // Empty state root
            return Self([0u8; 32]);
        }

        if hashes.len() == 1 {
            return Self(hashes[0]);
        }

        // Build Merkle tree bottom-up
        let mut current_level = hashes.to_vec();

        while current_level.len() > 1 {
            let mut next_level = Vec::new();

            for chunk in current_level.chunks(2) {
                if chunk.len() == 2 {
                    // Hash pair
                    let mut hasher = Sha256::new();
                    hasher.update(chunk[0]);
                    hasher.update(chunk[1]);
                    let result = hasher.finalize();
                    let mut bytes = [0u8; 32];
                    bytes.copy_from_slice(&result);
                    next_level.push(bytes);
                } else {
                    // Odd element — promote to next level
                    next_level.push(chunk[0]);
                }
            }

            current_level = next_level;
        }

        Self(current_level[0])
    }

    /// Create a Merkle proof for a specific leaf
    pub fn prove(hashes: &[[u8; 32]], index: usize) -> MerkleProof {
        if hashes.is_empty() || index >= hashes.len() {
            return MerkleProof {
                leaf: [0u8; 32],
                siblings: vec![],
                index,
            };
        }

        let mut siblings = Vec::new();
        let mut current_level = hashes.to_vec();
        let mut current_index = index;

        while current_level.len() > 1 {
            let mut next_level = Vec::new();
            let next_index = current_index / 2;

            for (i, chunk) in current_level.chunks(2).enumerate() {
                if chunk.len() == 2 {
                    let mut hasher = Sha256::new();
                    hasher.update(chunk[0]);
                    hasher.update(chunk[1]);
                    let result = hasher.finalize();
                    let mut bytes = [0u8; 32];
                    bytes.copy_from_slice(&result);
                    next_level.push(bytes);

                    // If this pair contains our target, save the sibling
                    if i == current_index / 2 {
                        let sibling_idx = if current_index.is_multiple_of(2) { 1 } else { 0 };
                        if sibling_idx < chunk.len() {
                            // is_left = sibling is on the LEFT side (sibling hashes first)
                            let is_left = !current_index.is_multiple_of(2);
                            siblings.push((chunk[sibling_idx], is_left));
                        }
                    }
                } else {
                    next_level.push(chunk[0]);
                }
            }

            current_level = next_level;
            current_index = next_index;
        }

        MerkleProof {
            leaf: hashes[index],
            siblings,
            index,
        }
    }

    /// Convert to hex string
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// Get as bytes
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl std::fmt::Display for StateRoot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "StateRoot({})", &self.to_hex()[..16])
    }
}

/// Merkle proof for a single leaf
#[derive(Debug, Clone)]
pub struct MerkleProof {
    /// The leaf hash
    pub leaf: [u8; 32],
    /// Sibling hashes and their positions (hash, is_left)
    pub siblings: Vec<([u8; 32], bool)>,
    /// Index of the leaf in the original array
    pub index: usize,
}

impl MerkleProof {
    /// Verify this proof against a known root
    pub fn verify(&self, root: &StateRoot) -> bool {
        let mut current = self.leaf;

        for (sibling, is_left) in &self.siblings {
            let mut hasher = Sha256::new();
            if *is_left {
                hasher.update(sibling);
                hasher.update(current);
            } else {
                hasher.update(current);
                hasher.update(sibling);
            }
            let result = hasher.finalize();
            let mut bytes = [0u8; 32];
            bytes.copy_from_slice(&result);
            current = bytes;
        }

        current == root.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_root() {
        let root = StateRoot::compute(&[]);
        assert_eq!(root.0, [0u8; 32]);
    }

    #[test]
    fn test_single_element() {
        let hash = [1u8; 32];
        let root = StateRoot::compute(&[hash]);
        assert_eq!(root.0, hash);
    }

    #[test]
    fn test_two_elements() {
        let h1 = [1u8; 32];
        let h2 = [2u8; 32];

        let root = StateRoot::compute(&[h1, h2]);

        // Should be SHA256(h1 || h2)
        let mut hasher = Sha256::new();
        hasher.update(h1);
        hasher.update(h2);
        let expected: [u8; 32] = hasher.finalize().into();

        assert_eq!(root.0, expected);
    }

    #[test]
    fn test_deterministic() {
        let hashes: Vec<[u8; 32]> = (0..10)
            .map(|i| {
                let mut h = [0u8; 32];
                h[0] = i;
                h
            })
            .collect();

        let root1 = StateRoot::compute(&hashes);
        let root2 = StateRoot::compute(&hashes);
        assert_eq!(root1, root2);
    }

    #[test]
    fn test_merkle_proof() {
        let hashes: Vec<[u8; 32]> = (0..8)
            .map(|i| {
                let mut h = [0u8; 32];
                h[0] = i;
                h
            })
            .collect();

        let root = StateRoot::compute(&hashes);

        // Generate and verify proof for each leaf
        for i in 0..hashes.len() {
            let proof = StateRoot::prove(&hashes, i);
            assert!(proof.verify(&root), "Proof for index {} should verify", i);
        }
    }

    #[test]
    fn test_proof_fails_with_wrong_root() {
        let hashes: Vec<[u8; 32]> = (0..4)
            .map(|i| {
                let mut h = [0u8; 32];
                h[0] = i;
                h
            })
            .collect();

        let proof = StateRoot::prove(&hashes, 0);
        let wrong_root = StateRoot([99u8; 32]);
        assert!(!proof.verify(&wrong_root));
    }
}
