//! # Tower BFT Consensus
//!
//! Solana's Tower BFT is a PoH-based Byzantine Fault Tolerant consensus.
//! It combines:
//! 1. Proof of History — verifiable clock for ordering
//! 2. PBFT-style voting — 2/3+ supermajority for finality
//! 3. Leader rotation — round-robin based on PoH
//!
//! Key concepts:
//! - **Tower**: The vote tree that tracks validator voting history
//! - **Switch**: Changing fork choice requires a lockout period
//! - **Lockout**: Exponential increasing cooldown when switching forks
//! - **Finality**: Achieved when 2/3+ validators vote on the same fork

pub mod fork_choice;
pub mod leader;
pub mod tower;
pub mod vote;

pub use fork_choice::ForkChoice;
pub use leader::LeaderSchedule;
pub use tower::Tower;
pub use vote::{Vote, VoteState};
