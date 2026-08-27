//! # Turbine — Block Propagation
//!
//! Turbine is Solana's block propagation protocol. It uses:
//! 1. **Reed-Solomon erasure coding** — split blocks into shards
//! 2. **Neighborhoods** — validators form a tree for propagation
//! 3. **Gossip** — shreds are propagated via gossip protocol
//!
//! The key insight: instead of sending entire blocks to everyone,
//! Turbine sends small "shreds" (erasure-coded pieces) so that
//! any subset of validators can reconstruct the block.

pub mod erasure;
pub mod neighborhoods;
pub mod propagate;
pub mod shred;

pub use erasure::ErasureCoder;
pub use neighborhoods::Neighborhood;
pub use propagate::TurbinePropagator;
pub use shred::{Shred, ShredType};
