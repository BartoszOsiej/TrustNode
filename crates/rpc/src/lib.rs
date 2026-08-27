//! # JSON-RPC Server
//!
//! Provides a JSON-RPC 2.0 API for interacting with the validator.
//! Compatible with Solana's RPC methods.

pub mod handler;
pub mod server;

pub use handler::RpcHandler;
pub use server::RpcServer;
