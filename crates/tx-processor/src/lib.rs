//! # Transaction Processor (Sealevel-like)
//!
//! Inspired by Solana's Sealevel runtime:
//! - Transactions declare which accounts they read/write
//! - The scheduler finds non-conflicting transactions for parallel execution
//! - Read-only accounts can be accessed concurrently
//! - Write-locked accounts serialize conflicting transactions

pub mod executor;
pub mod scheduler;
pub mod transaction;

pub use executor::Executor;
pub use scheduler::Scheduler;
pub use transaction::{Instruction, Transaction, TransactionResult};
