//! # Accounts DB
//!
//! Solana-like append-only accounts database.
//!
//! Accounts are stored in an append-only log (AppendVec) with a hash map
//! index for fast lookups. The state root is a Merkle hash of all accounts.

pub mod account;
pub mod merkle;
pub mod store;

pub use account::{Account, AccountData};
pub use merkle::StateRoot;
pub use store::AccountsDB;
