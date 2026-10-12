//! Validator networking.
//!
//! Two UDP sockets are bound at startup:
//!
//! * **gossip** — CRDS messages (push/pull/prune/ping) exchanged between
//!   validators, driven by [`GossipProtocol`].
//! * **TPU** — Turbine shreds for block propagation, driven by
//!   [`TurbinePropagator`].
//!
//! Each socket gets a receive loop that verifies and dispatches incoming
//! messages, plus an outbound path (a fan-out task for gossip, a per-peer
//! forward task for shreds). Shred history is pruned as slots advance so a
//! validator cannot be forced to buffer unbounded partial blocks.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use ed25519_dalek::SigningKey;
use solana_gossip::message::{ContactInfo, GossipMessage, SocketAddr as GossipAddr};
use solana_gossip::{Crds, GossipProtocol};
use solana_net::{NetMessage, UdpTransport};
use solana_turbine::{Neighborhood, Shred, TurbinePropagator};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// Slots of shred history retained before pruning.
pub const SHRED_RETENTION_SLOTS: u64 = 128;

/// Live network endpoints plus the background tasks that drive them.
pub struct Network {
    /// Turbine propagator, shared with the block producer and slot clock.
    pub propagator: Arc<TurbinePropagator>,
    /// Background receive/send/prune tasks; await these to keep the process up.
    pub tasks: Vec<JoinHandle<()>>,
}

impl Network {
    /// Bind the gossip and TPU sockets and spawn their runtime tasks.
    pub async fn start(
        identity: [u8; 32],
        signing_key: SigningKey,
        gossip_port: u16,
        tpu_port: u16,
        rpc_port: u16,
    ) -> anyhow::Result<Self> {
        let crds = Arc::new(Crds::default_store());

        let gossip = UdpTransport::bind(&format!("0.0.0.0:{gossip_port}")).await?;
        let gossip_addr = gossip.local_addr()?;
        let tpu = UdpTransport::bind(&format!("0.0.0.0:{tpu_port}")).await?;
        let tpu_addr = tpu.local_addr()?;

        tracing::info!(
            "Network: gossip {} | tpu {}",
            gossip_addr,
            tpu_addr
        );

        // Advertise ourselves so peers can find us via pull requests.
        let self_contact = ContactInfo {
            pubkey: identity,
            gossip: to_gossip_addr(gossip_addr),
            tpu: to_gossip_addr(tpu_addr),
            rpc: GossipAddr::new([127, 0, 0, 1], rpc_port),
            tvu: to_gossip_addr(tpu_addr),
            stake: 1,
            version: env!("CARGO_PKG_VERSION").to_string(),
            wallclock: now_ms(),
        };
        crds.set_self_info(self_contact.clone());
        crds.insert_contact(self_contact);

        let (gossip_tx, mut gossip_rx) = mpsc::unbounded_channel::<GossipMessage>();
        let protocol = Arc::new(GossipProtocol::new(
            signing_key.clone(),
            crds.clone(),
            gossip_tx,
        ));

        let mut tasks = Vec::new();

        // Gossip receive: verify, dedup and handle every datagram.
        {
            let gossip = gossip.clone();
            let protocol = protocol.clone();
            tasks.push(tokio::spawn(async move {
                loop {
                    match gossip.recv().await {
                        Ok((NetMessage::Gossip(msg), _peer)) => {
                            if let Err(e) = protocol.handle_message(*msg).await {
                                tracing::warn!("gossip handling failed: {e}");
                            }
                        }
                        Ok((NetMessage::Shred(_), peer)) => {
                            tracing::trace!("ignoring shred on gossip socket from {peer}");
                        }
                        Err(e) => tracing::warn!("gossip socket error: {e}"),
                    }
                }
            }));
        }

        // Gossip send: fan outgoing messages out to every known contact.
        {
            let gossip = gossip.clone();
            let crds = crds.clone();
            tasks.push(tokio::spawn(async move {
                while let Some(msg) = gossip_rx.recv().await {
                    let Ok(bytes) = NetMessage::Gossip(Box::new(msg)).encode() else {
                        continue;
                    };
                    for contact in crds.contacts() {
                        if contact.pubkey == identity {
                            continue;
                        }
                        let addr = to_socket_addr(contact.gossip);
                        if addr.ip().is_unspecified() {
                            continue;
                        }
                        if let Err(e) = gossip.send_raw(&bytes, addr).await {
                            tracing::debug!("gossip send to {addr} failed: {e}");
                        }
                    }
                }
            }));
        }

        // Turbine tree: single node for now — B4 seeds real peers.
        let neighborhood = Arc::new(parking_lot::RwLock::new(Neighborhood::build_tree(
            vec![(identity, 1u64)],
            1,
        )));
        let (shred_tx, mut shred_rx) = mpsc::unbounded_channel::<(Vec<u8>, Vec<Shred>)>();
        let propagator = Arc::new(
            TurbinePropagator::new(identity, neighborhood, shred_tx).with_signer(signing_key),
        );

        // TPU receive: verify shreds, dedup by id, buffer per slot.
        {
            let tpu = tpu.clone();
            let propagator = propagator.clone();
            tasks.push(tokio::spawn(async move {
                loop {
                    match tpu.recv().await {
                        Ok((NetMessage::Shred(shred), peer)) => {
                            if !propagator.receive_shred(shred, [0u8; 32]) {
                                tracing::trace!("dropped shred from {peer}");
                            }
                        }
                        Ok((NetMessage::Gossip(_), peer)) => {
                            tracing::trace!("ignoring gossip on TPU socket from {peer}");
                        }
                        Err(e) => tracing::warn!("tpu socket error: {e}"),
                    }
                }
            }));
        }

        // TPU send: forward buffered shreds to the target's advertised TPU.
        {
            let tpu = tpu.clone();
            let crds = crds.clone();
            tasks.push(tokio::spawn(async move {
                while let Some((dest, shreds)) = shred_rx.recv().await {
                    let Ok(dest_key) = <[u8; 32]>::try_from(dest.as_slice()) else {
                        continue;
                    };
                    let Some(contact) = crds
                        .contacts()
                        .into_iter()
                        .find(|c| c.pubkey == dest_key)
                    else {
                        continue;
                    };
                    let addr = to_socket_addr(contact.tpu);
                    for shred in shreds {
                        if let Err(e) = tpu.send_to(&NetMessage::Shred(shred), addr).await {
                            tracing::debug!("shred send to {addr} failed: {e}");
                        }
                    }
                }
            }));
        }

        Ok(Self { propagator, tasks })
    }
}

/// Convert our gossip-layer address type into a real socket address.
fn to_socket_addr(addr: GossipAddr) -> SocketAddr {
    SocketAddr::from((addr.ip, addr.port))
}

/// Convert a real socket address into the gossip-layer address type.
fn to_gossip_addr(addr: SocketAddr) -> GossipAddr {
    match addr.ip() {
        IpAddr::V4(v4) => GossipAddr::new(v4.octets(), addr.port()),
        IpAddr::V6(_) => GossipAddr::new([0, 0, 0, 0], addr.port()),
    }
}

/// Current time in milliseconds since the Unix epoch.
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
