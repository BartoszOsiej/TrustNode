//! # Blockstore
//!
//! Persistent ledger for the TrustNode validator — the missing piece between
//! block production and block history.
//!
//! Every produced block becomes a [`BlockRecord`] chained by hash to its
//! parent, framed and checksummed on disk:
//!
//! ```text
//! | "TNBL" | len | JSON(BlockRecord) | sha256(payload) |
//! ```
//!
//! On startup the ledger is replayed: torn writes, corrupted frames and
//! tampered hash links are detected and truncated away, leaving a valid
//! prefix of history — the validator resumes from the last good block and
//! reseeds its PoH clock from the persisted `poh_hash`.

pub mod record;
pub mod store;

pub use record::{BlockRecord, GENESIS_PARENT};
pub use store::Blockstore;
