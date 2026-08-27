//! Gossip message types for CRDS

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Types of gossip messages
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MessageType {
    /// Push data to a peer
    Push,
    /// Request data from peers
    PullRequest,
    /// Response to a pull request
    PullResponse,
    /// Tell a peer to stop pushing certain data
    Prune,
    /// Ping to check liveness
    Ping,
    /// Response to a ping
    Pong,
}

/// A gossip message
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GossipMessage {
    /// Sender's public key
    pub from: [u8; 32],
    /// Message type
    pub message_type: MessageType,
    /// Message payload
    pub payload: GossipPayload,
    /// Timestamp (millis since epoch)
    pub timestamp: u64,
    /// Signature over the message
    pub signature: Vec<u8>,
}

/// Gossip message payload
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GossipPayload {
    /// Push: new data to share
    Push(PushData),
    /// Pull request: ask for data newer than these timestamps
    PullRequest(PullRequestData),
    /// Pull response: data requested by a peer
    PullResponse(PullResponseData),
    /// Prune: stop sending this data type from this contact
    Prune(PruneData),
    /// Ping: liveness check
    Ping(PingData),
    /// Pong: response to ping
    Pong(PongData),
}

/// Data pushed to peers
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushData {
    /// Unique ID for deduplication
    pub id: [u8; 32],
    /// Data content
    pub content: Vec<u8>,
    /// Data kind (e.g., "vote", "block", "contact_info")
    pub kind: String,
}

/// Request for newer data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullRequestData {
    /// The highest timestamps we have for each data kind
    pub filters: Vec<(String, u64)>,
    /// Our own contact info
    pub contact_info: ContactInfo,
}

/// Response to a pull request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullResponseData {
    /// The data we're sending
    pub items: Vec<PushData>,
    /// Our contact info
    pub contact_info: ContactInfo,
}

/// Prune data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PruneData {
    /// Data kinds to stop sending
    pub kinds: Vec<String>,
    /// From which contact
    pub from: [u8; 32],
}

/// Ping data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PingData {
    /// Random nonce
    pub nonce: u64,
}

/// Pong data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PongData {
    /// Echo the nonce from ping
    pub nonce: u64,
}

/// Contact information for a validator
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContactInfo {
    /// Validator public key
    pub pubkey: [u8; 32],
    /// Gossip socket address
    pub gossip: SocketAddr,
    /// TPU (Transaction Processing Unit) address
    pub tpu: SocketAddr,
    /// RPC address
    pub rpc: SocketAddr,
    /// TVU (Transaction Validation Unit) address
    pub tvu: SocketAddr,
    /// Stake weight
    pub stake: u64,
    /// Version string
    pub version: String,
    /// Last timestamp
    pub wallclock: u64,
}

/// Simple socket address
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocketAddr {
    pub ip: [u8; 4],
    pub port: u16,
}

impl SocketAddr {
    pub fn new(ip: [u8; 4], port: u16) -> Self {
        Self { ip, port }
    }

    pub fn localhost(port: u16) -> Self {
        Self {
            ip: [127, 0, 0, 1],
            port,
        }
    }
}

impl GossipMessage {
    /// Create a new gossip message
    pub fn new(from: [u8; 32], message_type: MessageType, payload: GossipPayload) -> Self {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        Self {
            from,
            message_type,
            payload,
            timestamp,
            signature: Vec::new(),
        }
    }

    /// Compute message hash for deduplication
    pub fn hash(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(&self.from);
        hasher.update((self.message_type as u8).to_le_bytes());
        hasher.update(&self.timestamp.to_le_bytes());

        match &self.payload {
            GossipPayload::Push(data) => {
                hasher.update(&data.id);
                hasher.update(&data.content);
            }
            GossipPayload::PullRequest(data) => {
                for (kind, ts) in &data.filters {
                    hasher.update(kind.as_bytes());
                    hasher.update(ts.to_le_bytes());
                }
            }
            _ => {}
        }

        let result = hasher.finalize();
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(&result);
        bytes
    }

    /// Verify message signature (stub)
    pub fn verify_signature(&self) -> bool {
        // In real implementation, verify Ed25519 signature
        true
    }

    /// Get the data age in milliseconds
    pub fn age_ms(&self) -> u64 {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        now.saturating_sub(self.timestamp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_push_message() {
        let push = PushData {
            id: [1u8; 32],
            content: vec![1, 2, 3],
            kind: "vote".to_string(),
        };

        let msg = GossipMessage::new([42u8; 32], MessageType::Push, GossipPayload::Push(push));

        assert_eq!(msg.message_type, MessageType::Push);
        assert!(msg.verify_signature());
    }

    #[test]
    fn test_message_hash_deterministic() {
        let payload = GossipPayload::Ping(PingData { nonce: 12345 });
        let msg1 = GossipMessage::new([1u8; 32], MessageType::Ping, payload.clone());
        let msg2 = GossipMessage::new([1u8; 32], MessageType::Ping, payload);

        // Same from, type, timestamp → same hash
        assert_eq!(msg1.hash(), msg2.hash());
    }

    #[test]
    fn test_contact_info() {
        let info = ContactInfo {
            pubkey: [1u8; 32],
            gossip: SocketAddr::localhost(8000),
            tpu: SocketAddr::localhost(8001),
            rpc: SocketAddr::localhost(8002),
            tvu: SocketAddr::localhost(8003),
            stake: 1_000_000,
            version: "0.1.0".to_string(),
            wallclock: 1234567890,
        };

        assert_eq!(info.gossip.port, 8000);
        assert_eq!(info.stake, 1_000_000);
    }
}
