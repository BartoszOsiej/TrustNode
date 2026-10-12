//! Cluster genesis.
//!
//! The genesis file pins the starting state every validator must agree on:
//! the initial accounts and the validator set with its stake weights. Two
//! validators booted from the same genesis and fed the same transactions
//! therefore derive the same state roots and the same block hashes.

use serde::{Deserialize, Serialize};

/// Genesis cluster definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenesisConfig {
    /// Human-readable cluster name.
    pub cluster: String,
    /// Accounts present at genesis.
    pub accounts: Vec<GenesisAccount>,
    /// Validators that make up the leader schedule.
    pub validators: Vec<GenesisValidator>,
}

/// A funded account at genesis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenesisAccount {
    pub pubkey: [u8; 32],
    pub lamports: u64,
}

/// A validator in the genesis leader schedule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenesisValidator {
    pub identity: [u8; 32],
    pub stake: u64,
    pub name: String,
}

impl GenesisConfig {
    /// A default single-validator cluster for local development. The system
    /// and token programs and a funded faucet mirror a fresh devnet.
    pub fn single(identity: [u8; 32]) -> Self {
        Self {
            cluster: "trustnode-devnet".to_string(),
            accounts: vec![
                GenesisAccount {
                    pubkey: [1u8; 32],
                    lamports: 0,
                },
                GenesisAccount {
                    pubkey: [2u8; 32],
                    lamports: 0,
                },
                GenesisAccount {
                    pubkey: [3u8; 32],
                    lamports: 1_000_000_000_000,
                },
            ],
            validators: vec![GenesisValidator {
                identity,
                stake: 10_000_000,
                name: "self".to_string(),
            }],
        }
    }

    /// Parse a genesis config from JSON.
    pub fn from_json(json: &str) -> anyhow::Result<Self> {
        serde_json::from_str(json).map_err(|e| anyhow::anyhow!("invalid genesis JSON: {e}"))
    }

    /// Render the genesis config as pretty JSON.
    pub fn to_json(&self) -> anyhow::Result<String> {
        serde_json::to_string_pretty(self)
            .map_err(|e| anyhow::anyhow!("genesis encode failed: {e}"))
    }

    /// Read a genesis config from disk.
    pub fn from_file(path: &str) -> anyhow::Result<Self> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("failed to read genesis {path}: {e}"))?;
        Self::from_json(&raw)
    }

    /// Write the genesis config to disk.
    pub fn write_file(&self, path: &str) -> anyhow::Result<()> {
        std::fs::write(path, self.to_json()?)
            .map_err(|e| anyhow::anyhow!("failed to write genesis {path}: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_json() {
        let cfg = GenesisConfig::single([9u8; 32]);
        let json = cfg.to_json().unwrap();
        let back = GenesisConfig::from_json(&json).unwrap();

        assert_eq!(back.cluster, cfg.cluster);
        assert_eq!(back.accounts.len(), cfg.accounts.len());
        assert_eq!(back.validators.len(), 1);
        assert_eq!(back.validators[0].identity, [9u8; 32]);
        assert_eq!(back.validators[0].stake, 10_000_000);
    }

    #[test]
    fn rejects_garbage() {
        assert!(GenesisConfig::from_json("not json").is_err());
    }
}
