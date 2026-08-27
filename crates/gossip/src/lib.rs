//! # Gossip Protocol (CRDS)
//!
//! Solana's gossip protocol uses CRDS (Cluster Replicated Data Store)
//! for exchanging information between validators. It's based on:
//!
//! 1. **Push messages** — validators push data to random peers
//! 2. **Pull requests** — validators request missing data
//! 3. **Pull responses** — responding to pull requests with data
//! 4. **Prune messages** — telling peers to stop sending certain data
//!
//! CRDS stores data with timestamps and signatures for Byzantine fault tolerance.

pub mod crds;
pub mod message;
pub mod protocol;

pub use crds::Crds;
pub use message::{GossipMessage, MessageType};
pub use protocol::GossipProtocol;
