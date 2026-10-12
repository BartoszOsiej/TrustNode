//! # Net — UDP transport
//!
//! TrustNode validators exchange two kinds of datagrams on the wire:
//!
//! 1. **Gossip messages** — CRDS push/pull/prune traffic
//! 2. **Turbine shreds** — erasure-coded pieces of blocks
//!
//! Both are already authenticated at the application layer (Ed25519
//! signatures live inside `GossipMessage`/`Shred`). This crate only deals
//! with moving them over UDP: a tiny framing header, a length guard and a
//! socket wrapper. Keeping transport separate means the gossip and turbine
//! crates stay free of socket concerns and remain trivially testable.

use serde::{Deserialize, Serialize};
use solana_gossip::GossipMessage;
use solana_turbine::Shred;
use std::fmt;
use std::net::SocketAddr;

/// Maximum size of a single datagram we will send or accept. Shreds top out
/// at ~1232 bytes and gossip messages are small, so this is generous.
pub const MAX_DATAGRAM: usize = 64 * 1024;

/// Two-byte wire magic ("TN" for TrustNode) followed by the payload.
const MAGIC: [u8; 2] = [0x54, 0x4E];

const TAG_SHRED: u8 = 1;
const TAG_GOSSIP: u8 = 2;

/// A message that can travel over the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NetMessage {
    /// A Turbine shred.
    Shred(Shred),
    /// A CRDS gossip message.
    Gossip(Box<GossipMessage>),
}

impl NetMessage {
    /// RFC-style name used in logs.
    pub fn kind(&self) -> &'static str {
        match self {
            NetMessage::Shred(_) => "shred",
            NetMessage::Gossip(_) => "gossip",
        }
    }

    /// Serialize into a datagram: `magic | tag | json`.
    pub fn encode(&self) -> Result<Vec<u8>, NetError> {
        let tag = match self {
            NetMessage::Shred(_) => TAG_SHRED,
            NetMessage::Gossip(_) => TAG_GOSSIP,
        };
        let body = serde_json::to_vec(self).map_err(|e| NetError::Encode(e.to_string()))?;
        let total = MAGIC.len() + 1 + body.len();
        if total > MAX_DATAGRAM {
            return Err(NetError::TooLarge(total));
        }
        let mut buf = Vec::with_capacity(total);
        buf.extend_from_slice(&MAGIC);
        buf.push(tag);
        buf.extend_from_slice(&body);
        Ok(buf)
    }

    /// Parse a datagram produced by [`NetMessage::encode`].
    pub fn decode(bytes: &[u8]) -> Result<Self, NetError> {
        if bytes.len() < MAGIC.len() + 1 {
            return Err(NetError::Malformed("datagram too short".into()));
        }
        if bytes[..MAGIC.len()] != MAGIC {
            return Err(NetError::Malformed("bad magic".into()));
        }
        let tag = bytes[MAGIC.len()];
        let body = &bytes[MAGIC.len() + 1..];
        match tag {
            TAG_SHRED => {
                serde_json::from_slice(body).map_err(|e| NetError::Malformed(format!("shred: {e}")))
            }
            TAG_GOSSIP => serde_json::from_slice(body)
                .map_err(|e| NetError::Malformed(format!("gossip: {e}"))),
            other => Err(NetError::Malformed(format!("unknown tag {other}"))),
        }
    }
}

/// Errors from encoding, decoding or the socket layer.
#[derive(Debug)]
pub enum NetError {
    /// Failed to serialize a message.
    Encode(String),
    /// Datagram was malformed or could not be parsed.
    Malformed(String),
    /// Encoded message exceeded [`MAX_DATAGRAM`].
    TooLarge(usize),
    /// Underlying socket error.
    Io(std::io::Error),
}

impl fmt::Display for NetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NetError::Encode(e) => write!(f, "encode error: {e}"),
            NetError::Malformed(e) => write!(f, "malformed datagram: {e}"),
            NetError::TooLarge(n) => {
                write!(f, "datagram too large: {n} > {MAX_DATAGRAM} bytes")
            }
            NetError::Io(e) => write!(f, "io error: {e}"),
        }
    }
}

impl std::error::Error for NetError {}

impl From<std::io::Error> for NetError {
    fn from(e: std::io::Error) -> Self {
        NetError::Io(e)
    }
}

/// A UDP endpoint for exchanging [`NetMessage`]s.
///
/// Cheap to clone (the socket is shared), so it can be handed to several
/// tasks — e.g. one per gossip peer plus the Turbine tree.
#[derive(Clone, Debug)]
pub struct UdpTransport {
    socket: std::sync::Arc<tokio::net::UdpSocket>,
}

impl UdpTransport {
    /// Bind a UDP socket to `addr` (e.g. `"0.0.0.0:8001"`).
    pub async fn bind(addr: &str) -> Result<Self, NetError> {
        let socket = tokio::net::UdpSocket::bind(addr).await?;
        Ok(Self {
            socket: std::sync::Arc::new(socket),
        })
    }

    /// The local address this endpoint is bound to.
    pub fn local_addr(&self) -> Result<SocketAddr, NetError> {
        Ok(self.socket.local_addr()?)
    }

    /// Send a message to `peer`.
    pub async fn send_to(&self, msg: &NetMessage, peer: SocketAddr) -> Result<usize, NetError> {
        let bytes = msg.encode()?;
        self.send_raw(&bytes, peer).await
    }

    /// Send an already-encoded datagram to `peer`.
    ///
    /// Useful when the same payload goes to several peers and should be
    /// serialized only once.
    pub async fn send_raw(&self, bytes: &[u8], peer: SocketAddr) -> Result<usize, NetError> {
        if bytes.len() > MAX_DATAGRAM {
            return Err(NetError::TooLarge(bytes.len()));
        }
        Ok(self.socket.send_to(bytes, peer).await?)
    }

    /// Receive one datagram, decoding and validating it.
    ///
    /// Datagrams that fail to decode are logged and skipped rather than
    /// aborting the receive loop, so a malformed peer cannot stall us.
    pub async fn recv(&self) -> Result<(NetMessage, SocketAddr), NetError> {
        let mut buf = vec![0u8; MAX_DATAGRAM];
        loop {
            let (n, peer) = self.socket.recv_from(&mut buf).await?;
            match NetMessage::decode(&buf[..n]) {
                Ok(msg) => return Ok((msg, peer)),
                Err(e) => tracing::warn!("dropping malformed datagram from {peer}: {e}"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use solana_gossip::message::{GossipPayload, MessageType, PingData};
    use solana_turbine::Shred;

    #[test]
    fn encode_decode_roundtrip_shred() {
        let sk = SigningKey::from_bytes(&[5u8; 32]);
        let mut shred = Shred::new_data(7, 1, vec![1, 2, 3, 4]);
        shred.sign(&sk);

        let bytes = NetMessage::Shred(shred.clone()).encode().unwrap();
        let decoded = NetMessage::decode(&bytes).unwrap();

        match decoded {
            NetMessage::Shred(s) => {
                assert_eq!(s.id, shred.id);
                assert_eq!(s.slot, 7);
                assert!(s.verify());
                assert!(s.verify_signature());
            }
            _ => panic!("expected shred"),
        }
    }

    #[test]
    fn encode_decode_roundtrip_gossip() {
        let sk = SigningKey::from_bytes(&[6u8; 32]);
        let mut msg = GossipMessage::new(
            [0u8; 32],
            MessageType::Ping,
            GossipPayload::Ping(PingData { nonce: 99 }),
        );
        msg.sign(&sk);

        let bytes = NetMessage::Gossip(Box::new(msg.clone())).encode().unwrap();
        let decoded = NetMessage::decode(&bytes).unwrap();

        match decoded {
            NetMessage::Gossip(m) => {
                assert_eq!(m.from, sk.verifying_key().to_bytes());
                assert_eq!(m.message_type, MessageType::Ping);
                assert!(m.verify_signature());
            }
            _ => panic!("expected gossip"),
        }
    }

    #[test]
    fn decode_rejects_garbage() {
        assert!(matches!(
            NetMessage::decode(&[]),
            Err(NetError::Malformed(_))
        ));
        assert!(matches!(
            NetMessage::decode(&[0x00, 0x00, 0x01, b'{']),
            Err(NetError::Malformed(_))
        ));

        // Correct magic but unknown tag.
        let mut buf = MAGIC.to_vec();
        buf.push(0xAB);
        buf.extend_from_slice(b"{}");
        assert!(matches!(
            NetMessage::decode(&buf),
            Err(NetError::Malformed(_))
        ));

        // Correct magic and tag but non-JSON body.
        let mut buf = MAGIC.to_vec();
        buf.push(TAG_SHRED);
        buf.extend_from_slice(b"not json");
        assert!(matches!(
            NetMessage::decode(&buf),
            Err(NetError::Malformed(_))
        ));
    }

    #[test]
    fn encode_rejects_oversized() {
        let payload = vec![0u8; MAX_DATAGRAM];
        let shred = Shred::new_data(1, 0, payload);
        assert!(matches!(
            NetMessage::Shred(shred).encode(),
            Err(NetError::TooLarge(_))
        ));
    }

    #[tokio::test]
    async fn udp_roundtrip_signed_shred() {
        let a = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let b = UdpTransport::bind("127.0.0.1:0").await.unwrap();

        let sk = SigningKey::from_bytes(&[9u8; 32]);
        let mut shred = Shred::new_data(21, 4, vec![10, 20, 30]);
        shred.sign(&sk);

        let sent = NetMessage::Shred(shred.clone());
        a.send_to(&sent, b.local_addr().unwrap()).await.unwrap();

        let (got, _from) = b.recv().await.unwrap();
        match got {
            NetMessage::Shred(s) => {
                assert_eq!(s.id, shred.id);
                assert!(s.verify_signature());
            }
            _ => panic!("expected shred"),
        }
    }

    #[tokio::test]
    async fn udp_skips_malformed_then_delivers_valid() {
        let a = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let b = UdpTransport::bind("127.0.0.1:0").await.unwrap();

        // Send raw garbage directly, then a valid message from the same peer.
        let garbage = b"not-a-valid-datagram";
        let raw = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        raw.send_to(garbage, b.local_addr().unwrap()).await.unwrap();

        let mut msg = GossipMessage::new(
            [0u8; 32],
            MessageType::Pong,
            GossipPayload::Pong(solana_gossip::message::PongData { nonce: 1 }),
        );
        msg.sign(&SigningKey::from_bytes(&[4u8; 32]));
        a.send_to(&NetMessage::Gossip(Box::new(msg)), b.local_addr().unwrap())
            .await
            .unwrap();

        // The garbage datagram is skipped; the next valid one comes through.
        let (got, _from) = b.recv().await.unwrap();
        assert!(matches!(got, NetMessage::Gossip(_)));
    }
}
