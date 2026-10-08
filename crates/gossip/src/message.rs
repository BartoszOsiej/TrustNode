//! Gossip message types for CRDS

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
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

    /// Data covered by the Ed25519 signature: sender identity, message
    /// type, timestamp and the full serialized payload (every variant —
    /// nothing can be swapped or dropped without breaking the signature).
    pub fn signable_data(&self) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(&self.from);
        data.push(self.message_type as u8);
        data.extend_from_slice(&self.timestamp.to_le_bytes());
        let payload = serde_json::to_vec(&self.payload).expect("gossip payload always serializes");
        data.extend_from_slice(&payload);
        data
    }

    /// Compute message hash for deduplication — covers the exact same
    /// bytes as the signature.
    pub fn hash(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(self.signable_data());
        let result = hasher.finalize();
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(&result);
        bytes
    }

    /// Sign this message with the sender's Ed25519 key. Rebinds `from` to
    /// the key's public key.
    pub fn sign(&mut self, key: &SigningKey) {
        self.from = key.verifying_key().to_bytes();
        self.signature = key.sign(&self.signable_data()).to_bytes().to_vec();
    }

    /// Verify the message's Ed25519 signature against its sender identity.
    pub fn verify_signature(&self) -> bool {
        let Ok(vk) = VerifyingKey::from_bytes(&self.from) else {
            return false;
        };
        let Ok(sig) = Signature::from_slice(&self.signature) else {
            return false;
        };
        vk.verify_strict(&self.signable_data(), &sig).is_ok()
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

        let mut msg = GossipMessage::new([42u8; 32], MessageType::Push, GossipPayload::Push(push));
        msg.sign(&SigningKey::from_bytes(&[7u8; 32]));

        assert_eq!(msg.message_type, MessageType::Push);
        assert!(msg.verify_signature());
    }

    #[test]
    fn test_unsigned_message_rejected() {
        let msg = GossipMessage::new(
            [1u8; 32],
            MessageType::Ping,
            GossipPayload::Ping(PingData { nonce: 1 }),
        );
        assert!(!msg.verify_signature());
    }

    #[test]
    fn test_tampered_message_rejected() {
        let sk = SigningKey::from_bytes(&[7u8; 32]);
        let mut msg = GossipMessage::new(
            [1u8; 32],
            MessageType::Push,
            GossipPayload::Push(PushData {
                id: [1u8; 32],
                content: vec![1, 2, 3],
                kind: "vote".to_string(),
            }),
        );
        msg.sign(&sk);
        assert!(msg.verify_signature());

        // Swap the payload after signing
        if let GossipPayload::Push(data) = &mut msg.payload {
            data.content = vec![9, 9, 9];
        }
        assert!(!msg.verify_signature());

        // Swap the sender after signing
        msg.sign(&sk);
        msg.from = [3u8; 32];
        assert!(!msg.verify_signature());
    }

    #[test]
    fn test_message_hash_covers_signature_bytes() {
        let sk = SigningKey::from_bytes(&[7u8; 32]);
        let mk = |nonce| GossipPayload::Ping(PingData { nonce });
        let mut msg1 = GossipMessage::new([1u8; 32], MessageType::Ping, mk(1));
        let mut msg2 = GossipMessage::new([1u8; 32], MessageType::Ping, mk(1));
        msg1.sign(&sk);
        msg2.sign(&sk);

        // Same content, independently signed → same hash
        assert_eq!(msg1.hash(), msg2.hash());

        // Different nonce → different hash
        let mut msg3 = GossipMessage::new([1u8; 32], MessageType::Ping, mk(2));
        msg3.sign(&sk);
        assert_ne!(msg1.hash(), msg3.hash());
    }

    #[test]
    fn test_message_hash_deterministic() {
        let payload = GossipPayload::Ping(PingData { nonce: 12345 });
        let mut msg1 = GossipMessage::new([1u8; 32], MessageType::Ping, payload.clone());
        let mut msg2 = GossipMessage::new([1u8; 32], MessageType::Ping, payload);
        msg2.timestamp = msg1.timestamp;

        // Same from, type, timestamp → same hash
        assert_eq!(msg1.hash(), msg2.hash());
        msg1.timestamp += 1;
        assert_ne!(msg1.hash(), msg2.hash());
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
