//! # Proof of History (PoH)
//!
//! Solana's core innovation — a Verifiable Delay Function (VDF) that creates
//! a cryptographically verifiable ordering of events without requiring a
//! trusted timestamp server.
//!
//! ## How it works:
//! 1. Start with a random seed hash
//! 2. Repeatedly hash: `hash_n = SHA-256(hash_{n-1})`
//! 3. Each hash proves that `n` iterations occurred sequentially
//! 4. Anyone can verify by re-computing the chain
//!
//! This creates a **clock** that's embedded in the blockchain itself.
//! Transactions can reference PoH hashes to prove their temporal ordering.

pub mod entry;
pub mod hasher;
pub mod recorder;
pub mod verifier;

pub use entry::{Entry, PohEntry};
pub use hasher::{PohHash, PohHasher};
pub use recorder::PohRecorder;
pub use verifier::PohVerifier;

/// Default tick rate: ~400ms per tick (Solana targets 400ms slots)
pub const DEFAULT_TICKS_PER_SLOT: u64 = 64;
pub const DEFAULT_TARGET_TICKS_PER_SECOND: u64 = 160; // ~400ms per slot

/// PoH configuration
#[derive(Debug, Clone)]
pub struct PohConfig {
    /// Number of hashes between ticks (determines tick rate)
    pub hashes_per_tick: u64,
    /// Target tick rate (ticks per second)
    pub target_tick_rate: u64,
    /// Maximum number of hashes before auto-tick
    pub max_hashes_per_tick: u64,
}

impl Default for PohConfig {
    fn default() -> Self {
        Self {
            hashes_per_tick: 6_000,      // ~6k hashes per tick
            target_tick_rate: 160,       // 160 ticks/second ≈ 6.25ms/tick
            max_hashes_per_tick: 12_000, // max before forced tick
        }
    }
}
