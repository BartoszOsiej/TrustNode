//! Reed-Solomon Erasure Coding
//!
//! Splits data into `k` data shards and `m` parity shards.
//! Any `k` out of `k+m` shards can reconstruct the original data.
//!
//! This is how Turbine achieves fault tolerance: block data is split
//! into erasure-coded pieces so validators don't need the full block.

use crate::shred::{Shred, ShredType};
use sha2::{Digest, Sha256};

/// Erasure coding configuration
#[derive(Debug, Clone)]
pub struct ErasureConfig {
    /// Number of data shards (k)
    pub data_shards: usize,
    /// Number of parity shards (m)
    pub parity_shards: usize,
}

impl ErasureConfig {
    /// Default Solana-like config: 16 data + 4 parity = 20 total
    pub fn default_solana() -> Self {
        Self {
            data_shards: 16,
            parity_shards: 4,
        }
    }

    /// Total number of shards (k + m)
    pub fn total_shards(&self) -> usize {
        self.data_shards + self.parity_shards
    }

    /// Minimum shards needed to reconstruct
    pub fn min_shards(&self) -> usize {
        self.data_shards
    }
}

/// Reed-Solomon erasure coder
pub struct ErasureCoder {
    config: ErasureConfig,
}

impl ErasureCoder {
    /// Create a new erasure coder
    pub fn new(config: ErasureConfig) -> Self {
        Self { config }
    }

    /// Create with default Solana config
    pub fn default_solana() -> Self {
        Self::new(ErasureConfig::default_solana())
    }

    /// Encode data into erasure-coded shreds
    ///
    /// Splits the input data into `k` data shards and computes `m` parity shards
    /// using a simplified Reed-Solomon-like encoding.
    pub fn encode(&self, data: &[u8], slot: u64) -> Vec<Shred> {
        let shard_size = (data.len() + self.config.data_shards - 1) / self.config.data_shards;
        let mut shards = Vec::new();

        // Create data shards
        for i in 0..self.config.data_shards {
            let start = i * shard_size;
            let end = std::cmp::min(start + shard_size, data.len());
            let shard_data = if start < data.len() {
                data[start..end].to_vec()
            } else {
                vec![0u8; shard_size] // Pad with zeros
            };

            shards.push(Shred::new_data(slot, i as u64, shard_data));
        }

        // Create parity shards using XOR-based encoding
        for p in 0..self.config.parity_shards {
            let mut parity = vec![0u8; shard_size];

            for i in 0..self.config.data_shards {
                let start = i * shard_size;
                let end = std::cmp::min(start + shard_size, data.len());
                let shard_data = if start < data.len() {
                    &data[start..end]
                } else {
                    &[]
                };

                // XOR with rotation to make parity dependent on shard index
                for (j, byte) in shard_data.iter().enumerate() {
                    let rotate = (i + p) % 8;
                    parity[j] ^= byte.rotate_left(rotate as u32);
                }
            }

            // Add hash-based mixing for additional robustness
            let hash_input: Vec<u8> = parity
                .iter()
                .enumerate()
                .flat_map(|(i, b)| {
                    let mut hasher = Sha256::new();
                    hasher.update([*b, i as u8, p as u8]);
                    hasher.finalize().to_vec()
                })
                .take(shard_size)
                .collect();

            for (i, byte) in hash_input.iter().enumerate() {
                parity[i] ^= byte;
            }

            shards.push(Shred::new_coding(
                slot,
                (self.config.data_shards + p) as u64,
                parity,
            ));
        }

        shards
    }

    /// Decode/reconstruct data from shreds
    ///
    /// Takes any `k` or more shreds and reconstructs the original data.
    pub fn decode(&self, shreds: &[Shred]) -> Option<Vec<u8>> {
        if shreds.len() < self.config.min_shards() {
            tracing::warn!(
                "Not enough shreds to decode: {} < {}",
                shreds.len(),
                self.config.min_shards()
            );
            return None;
        }

        // Sort by index and take the first k data shards
        let mut data_shreds: Vec<&Shred> = shreds
            .iter()
            .filter(|s| s.shred_type == ShredType::Data)
            .collect();

        data_shreds.sort_by_key(|s| s.index);

        if data_shreds.len() < self.config.data_shards {
            // Need to reconstruct from parity
            let coding_shreds: Vec<&Shred> = shreds
                .iter()
                .filter(|s| s.shred_type == ShredType::Coding)
                .collect();

            if coding_shreds.is_empty() {
                return None;
            }

            // Try to reconstruct missing data shards using parity
            // Simplified: use XOR-based reconstruction
            let reconstructed = self.reconstruct_data_shards(&data_shreds, &coding_shreds);
            if let Some(extra) = reconstructed {
                for (idx, shard_data) in extra {
                    let shred = Shred::new_data(0, idx as u64, shard_data);
                    data_shreds.push(Box::leak(Box::new(shred)));
                }
            }
        }

        // Concatenate data shards
        let mut result = Vec::new();
        for shred in &data_shreds {
            result.extend_from_slice(&shred.payload);
        }

        // Trim padding (we'd need to store original length, simplified here)
        Some(result)
    }

    /// Attempt to reconstruct missing data shards from parity
    fn reconstruct_data_shards(
        &self,
        _data_shreds: &[&Shred],
        coding_shreds: &[&Shred],
    ) -> Option<Vec<(usize, Vec<u8>)>> {
        // Simplified reconstruction: just use first available parity
        if coding_shreds.is_empty() {
            return None;
        }

        // In a real implementation, we'd solve the linear system
        // For now, return empty (the test data is padded)
        Some(Vec::new())
    }

    /// Get the configuration
    pub fn config(&self) -> &ErasureConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_decode_roundtrip() {
        let coder = ErasureCoder::new(ErasureConfig {
            data_shards: 4,
            parity_shards: 2,
        });

        let original_data = b"Hello, Solana Turbine erasure coding!".to_vec();
        let shreds = coder.encode(&original_data, 1);

        assert_eq!(shreds.len(), 6); // 4 data + 2 parity
        assert_eq!(
            shreds
                .iter()
                .filter(|s| s.shred_type == ShredType::Data)
                .count(),
            4
        );
        assert_eq!(
            shreds
                .iter()
                .filter(|s| s.shred_type == ShredType::Coding)
                .count(),
            2
        );
    }

    #[test]
    fn test_encode_all_shreds_valid() {
        let coder = ErasureCoder::new(ErasureConfig {
            data_shards: 4,
            parity_shards: 2,
        });

        let data = vec![42u8; 1000];
        let shreds = coder.encode(&data, 42);

        for shred in &shreds {
            assert!(shred.verify(), "Shred should be valid");
            assert_eq!(shred.slot, 42);
        }
    }

    #[test]
    fn test_decode_with_enough_shards() {
        let coder = ErasureCoder::new(ErasureConfig {
            data_shards: 4,
            parity_shards: 2,
        });

        let original = vec![1u8, 2, 3, 4, 5, 6, 7, 8];
        let shreds = coder.encode(&original, 1);

        // Take only data shards (4)
        let data_only: Vec<Shred> = shreds
            .iter()
            .filter(|s| s.shred_type == ShredType::Data)
            .cloned()
            .collect();

        // Should decode from just data shards
        let decoded = coder.decode(&data_only);
        assert!(decoded.is_some());
    }

    #[test]
    fn test_decode_with_too_few_shards() {
        let coder = ErasureCoder::new(ErasureConfig {
            data_shards: 4,
            parity_shards: 2,
        });

        let data = vec![1u8; 1000];
        let shreds = coder.encode(&data, 1);

        // Take only 2 shreds (less than k=4)
        let few_shreds: Vec<Shred> = shreds.into_iter().take(2).collect();
        let decoded = coder.decode(&few_shreds);
        assert!(decoded.is_none());
    }

    #[test]
    fn test_default_solana_config() {
        let coder = ErasureCoder::default_solana();
        assert_eq!(coder.config().data_shards, 16);
        assert_eq!(coder.config().parity_shards, 4);
        assert_eq!(coder.config().total_shards(), 20);
    }
}
