//! Gossip protocol implementation
//!
//! Handles the push/pull gossip protocol for validator communication.

use crate::crds::Crds;
use crate::message::{
    ContactInfo, GossipMessage, GossipPayload, MessageType, PingData, PongData, PullRequestData,
    PullResponseData, PushData,
};
use dashmap::DashMap;
use ed25519_dalek::SigningKey;
use std::sync::Arc;
use tokio::sync::mpsc;

/// Gossip protocol handler
pub struct GossipProtocol {
    /// CRDS data store
    crds: Arc<Crds>,
    /// This validator's identity
    pub validator_id: [u8; 32],
    /// Our signing key — every outgoing message is Ed25519-signed
    signer: Arc<SigningKey>,
    /// Message sender
    msg_sender: mpsc::UnboundedSender<GossipMessage>,
    /// Received message count
    received_count: DashMap<MessageType, u64>,
    /// Ping nonce tracking (nonce -> sender)
    pending_pings: DashMap<u64, [u8; 32]>,
    /// Protocol stats
    stats: parking_lot::RwLock<ProtocolStats>,
}

impl GossipProtocol {
    /// Create a new gossip protocol handler. The validator identity is
    /// derived from the signing key's public key.
    pub fn new(
        signer: SigningKey,
        crds: Arc<Crds>,
        msg_sender: mpsc::UnboundedSender<GossipMessage>,
    ) -> Self {
        let validator_id = signer.verifying_key().to_bytes();
        Self {
            crds,
            validator_id,
            signer: Arc::new(signer),
            msg_sender,
            received_count: DashMap::new(),
            pending_pings: DashMap::new(),
            stats: parking_lot::RwLock::new(ProtocolStats::default()),
        }
    }

    /// Build a message with our identity and sign it with our key.
    fn signed(&self, message_type: MessageType, payload: GossipPayload) -> GossipMessage {
        let mut msg = GossipMessage::new(self.validator_id, message_type, payload);
        msg.sign(&self.signer);
        msg
    }

    /// Handle an incoming gossip message
    pub async fn handle_message(&self, msg: GossipMessage) -> anyhow::Result<()> {
        // Verify signature
        if !msg.verify_signature() {
            tracing::warn!(
                "Invalid gossip signature from {}",
                hex::encode(&msg.from[..8])
            );
            self.stats.write().invalid_messages += 1;
            return Ok(());
        }

        // Track received messages
        *self.received_count.entry(msg.message_type).or_insert(0) += 1;

        match &msg.message_type {
            MessageType::Push => {
                self.handle_push(&msg).await?;
            }
            MessageType::PullRequest => {
                self.handle_pull_request(&msg).await?;
            }
            MessageType::PullResponse => {
                self.handle_pull_response(&msg).await?;
            }
            MessageType::Prune => {
                self.handle_prune(&msg);
            }
            MessageType::Ping => {
                self.handle_ping(&msg).await?;
            }
            MessageType::Pong => {
                self.handle_pong(&msg);
            }
        }

        self.stats.write().messages_handled += 1;
        Ok(())
    }

    /// Handle push message
    async fn handle_push(&self, msg: &GossipMessage) -> anyhow::Result<()> {
        let inserted = self.crds.insert_push(msg);

        if inserted {
            tracing::debug!(
                "New push from {}: {:?}",
                hex::encode(&msg.from[..8]),
                msg.payload
            );

            // Forward to our peers (gossip)
            self.forward_push(msg).await?;
        }

        Ok(())
    }

    /// Forward push to random peers
    async fn forward_push(&self, msg: &GossipMessage) -> anyhow::Result<()> {
        let peers = self.crds.random_contacts(3); // Forward to 3 random peers

        for peer in peers {
            if peer.pubkey == self.validator_id || peer.pubkey == msg.from {
                continue; // Don't send to ourselves or back to sender
            }

            let forward = self.signed(MessageType::Push, msg.payload.clone());

            if let Err(e) = self.msg_sender.send(forward) {
                tracing::error!("Failed to forward push: {}", e);
            }
        }

        Ok(())
    }

    /// Handle pull request
    async fn handle_pull_request(&self, msg: &GossipMessage) -> anyhow::Result<()> {
        if let GossipPayload::PullRequest(request) = &msg.payload {
            // Find entries newer than each filter timestamp
            let mut items = Vec::new();

            for (kind, timestamp) in &request.filters {
                let entries: Vec<PushData> = self
                    .crds
                    .entries_newer_than(*timestamp)
                    .into_iter()
                    .filter(|e| e.data.kind == *kind)
                    .map(|e| e.data)
                    .collect();
                items.extend(entries);
            }

            // Send pull response
            let response = self.signed(
                MessageType::PullResponse,
                GossipPayload::PullResponse(PullResponseData {
                    items,
                    contact_info: self.crds.self_info().unwrap_or_else(|| ContactInfo {
                        pubkey: self.validator_id,
                        gossip: crate::message::SocketAddr::localhost(8000),
                        tpu: crate::message::SocketAddr::localhost(8001),
                        rpc: crate::message::SocketAddr::localhost(8002),
                        tvu: crate::message::SocketAddr::localhost(8003),
                        stake: 0,
                        version: "0.1.0".to_string(),
                        wallclock: 0,
                    }),
                }),
            );

            let _ = self.msg_sender.send(response);
        }

        Ok(())
    }

    /// Handle pull response
    async fn handle_pull_response(&self, msg: &GossipMessage) -> anyhow::Result<()> {
        if let GossipPayload::PullResponse(response) = &msg.payload {
            // Insert all received items
            for item in &response.items {
                let push_msg = GossipMessage::new(
                    msg.from,
                    MessageType::Push,
                    GossipPayload::Push(item.clone()),
                );
                self.crds.insert_push(&push_msg);
            }

            // Store contact info
            self.crds.insert_contact(response.contact_info.clone());
        }

        Ok(())
    }

    /// Handle prune message
    fn handle_prune(&self, _msg: &GossipMessage) {
        // In a real implementation, we'd track which data to stop sending
        self.stats.write().prunes_received += 1;
    }

    /// Handle ping
    async fn handle_ping(&self, msg: &GossipMessage) -> anyhow::Result<()> {
        if let GossipPayload::Ping(ping) = &msg.payload {
            // Respond with pong
            let pong = self.signed(
                MessageType::Pong,
                GossipPayload::Pong(PongData { nonce: ping.nonce }),
            );

            let _ = self.msg_sender.send(pong);
        }

        Ok(())
    }

    /// Handle pong
    fn handle_pong(&self, msg: &GossipMessage) {
        if let GossipPayload::Pong(pong) = &msg.payload {
            self.pending_pings.remove(&pong.nonce);
            self.stats.write().pongs_received += 1;
        }
    }

    /// Send a ping to a peer
    pub async fn send_ping(&self, peer: ContactInfo) -> anyhow::Result<()> {
        let nonce = rand::random::<u64>();
        self.pending_pings.insert(nonce, peer.pubkey);

        let ping = self.signed(MessageType::Ping, GossipPayload::Ping(PingData { nonce }));

        let _ = self.msg_sender.send(ping);
        Ok(())
    }

    /// Send a pull request to a peer
    pub async fn send_pull_request(
        &self,
        _peer: ContactInfo,
        filters: Vec<(String, u64)>,
    ) -> anyhow::Result<()> {
        let request = self.signed(
            MessageType::PullRequest,
            GossipPayload::PullRequest(PullRequestData {
                filters,
                contact_info: self.crds.self_info().unwrap_or_else(|| ContactInfo {
                    pubkey: self.validator_id,
                    gossip: crate::message::SocketAddr::localhost(8000),
                    tpu: crate::message::SocketAddr::localhost(8001),
                    rpc: crate::message::SocketAddr::localhost(8002),
                    tvu: crate::message::SocketAddr::localhost(8003),
                    stake: 0,
                    version: "0.1.0".to_string(),
                    wallclock: 0,
                }),
            }),
        );

        let _ = self.msg_sender.send(request);
        Ok(())
    }

    /// Get stats
    pub fn stats(&self) -> ProtocolStats {
        self.stats.read().clone()
    }

    /// Get CRDS reference
    pub fn crds(&self) -> &Arc<Crds> {
        &self.crds
    }
}

/// Protocol statistics
#[derive(Debug, Clone, Default)]
pub struct ProtocolStats {
    pub messages_handled: u64,
    pub invalid_messages: u64,
    pub prunes_received: u64,
    pub pongs_received: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_key(seed_byte: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed_byte; 32])
    }

    fn signed_incoming(message_type: MessageType, payload: GossipPayload) -> GossipMessage {
        let mut msg = GossipMessage::new([0u8; 32], message_type, payload);
        msg.sign(&test_key(2));
        msg
    }

    #[tokio::test]
    async fn test_handle_ping() {
        let crds = Arc::new(Crds::default_store());
        let (tx, mut rx) = mpsc::unbounded_channel();

        let protocol = GossipProtocol::new(test_key(1), crds, tx);

        let ping_msg = signed_incoming(
            MessageType::Ping,
            GossipPayload::Ping(PingData { nonce: 42 }),
        );

        protocol.handle_message(ping_msg).await.unwrap();

        // Should have sent a signed pong
        let pong = rx.recv().await.unwrap();
        assert_eq!(pong.message_type, MessageType::Pong);
        assert!(pong.verify_signature());
        assert_eq!(pong.from, protocol.validator_id);
        assert_eq!(protocol.stats().invalid_messages, 0);
    }

    #[tokio::test]
    async fn test_handle_push() {
        let crds = Arc::new(Crds::default_store());
        let (tx, _rx) = mpsc::unbounded_channel();

        let protocol = GossipProtocol::new(test_key(1), crds.clone(), tx);

        let push_data = PushData {
            id: [1u8; 32],
            content: vec![1, 2, 3],
            kind: "vote".to_string(),
        };

        let push_msg = signed_incoming(MessageType::Push, GossipPayload::Push(push_data));

        protocol.handle_message(push_msg).await.unwrap();

        // Should be in CRDS
        assert!(!crds.is_empty());
        assert_eq!(protocol.stats().invalid_messages, 0);
    }

    #[tokio::test]
    async fn test_unsigned_message_rejected() {
        let crds = Arc::new(Crds::default_store());
        let (tx, mut rx) = mpsc::unbounded_channel();

        let protocol = GossipProtocol::new(test_key(1), crds, tx);

        // Not signed at all — must be counted and dropped, no reply sent
        let unsigned = GossipMessage::new(
            [2u8; 32],
            MessageType::Ping,
            GossipPayload::Ping(PingData { nonce: 7 }),
        );
        assert!(!unsigned.verify_signature());

        protocol.handle_message(unsigned).await.unwrap();
        assert_eq!(protocol.stats().invalid_messages, 1);
        assert!(rx.try_recv().is_err());

        // Signed by the wrong key's message but with `from` overridden
        let mut forged = GossipMessage::new(
            [1u8; 32],
            MessageType::Ping,
            GossipPayload::Ping(PingData { nonce: 8 }),
        );
        forged.sign(&test_key(3));
        forged.from = protocol.validator_id; // impersonate us after signing
        assert!(!forged.verify_signature());

        protocol.handle_message(forged).await.unwrap();
        assert_eq!(protocol.stats().invalid_messages, 2);
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn test_handle_pull_request() {
        let crds = Arc::new(Crds::default_store());
        let (tx, mut rx) = mpsc::unbounded_channel();

        let protocol = GossipProtocol::new(test_key(1), crds, tx);

        let pull_msg = signed_incoming(
            MessageType::PullRequest,
            GossipPayload::PullRequest(PullRequestData {
                filters: vec![("vote".to_string(), 0)],
                contact_info: ContactInfo {
                    pubkey: [2u8; 32],
                    gossip: crate::message::SocketAddr::localhost(8000),
                    tpu: crate::message::SocketAddr::localhost(8001),
                    rpc: crate::message::SocketAddr::localhost(8002),
                    tvu: crate::message::SocketAddr::localhost(8003),
                    stake: 1000,
                    version: "0.1.0".to_string(),
                    wallclock: 0,
                },
            }),
        );

        protocol.handle_message(pull_msg).await.unwrap();

        // Should have sent a signed pull response
        let response = rx.recv().await.unwrap();
        assert_eq!(response.message_type, MessageType::PullResponse);
        assert!(response.verify_signature());
        assert_eq!(response.from, protocol.validator_id);
    }
}
