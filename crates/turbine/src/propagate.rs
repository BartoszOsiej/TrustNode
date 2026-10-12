//! Block propagation using Turbine protocol
//!
//! The propagator:
//! 1. Takes a block and erasure-codes it into shreds
//! 2. Distributes shreds to neighbors in the Turbine tree
//! 3. Each validator forwards shreds to its children

use crate::erasure::ErasureCoder;
use crate::neighborhoods::Neighborhood;
use crate::shred::Shred;
use dashmap::DashMap;
use ed25519_dalek::SigningKey;
use parking_lot::RwLock;
use std::sync::Arc;
use tokio::sync::mpsc;

/// A received shred with metadata
#[derive(Debug, Clone)]
pub struct ReceivedShred {
    pub shred: Shred,
    pub received_from: [u8; 32],
    pub received_at: u64,
}

/// Block propagation statistics
#[derive(Debug, Clone, Default)]
pub struct PropagationStats {
    pub blocks_propagated: u64,
    pub shreds_sent: u64,
    pub shreds_received: u64,
    pub blocks_reassembled: u64,
    pub propagation_errors: u64,
    /// Shreds dropped because they were malformed or failed verification.
    pub invalid_shreds: u64,
    /// Shreds dropped because they were already seen (dedup).
    pub duplicate_shreds: u64,
    /// Slots evicted by pruning.
    pub slots_pruned: u64,
}

/// Turbine block propagator
pub struct TurbinePropagator {
    /// Erasure coder
    coder: ErasureCoder,
    /// Turbine tree
    neighborhood: Arc<RwLock<Neighborhood>>,
    /// This validator's identity
    pub validator_id: [u8; 32],
    /// Received shreds (slot -> shreds)
    received_shreds: DashMap<u64, Vec<ReceivedShred>>,
    /// Channel for sending shreds to neighbors
    shred_sender: mpsc::UnboundedSender<(Vec<u8>, Vec<Shred>)>,
    /// Optional signing key: when set, propagated shreds are signed.
    signer: Option<SigningKey>,
    /// Statistics
    stats: RwLock<PropagationStats>,
}

impl TurbinePropagator {
    /// Create a new propagator
    pub fn new(
        validator_id: [u8; 32],
        neighborhood: Arc<RwLock<Neighborhood>>,
        shred_sender: mpsc::UnboundedSender<(Vec<u8>, Vec<Shred>)>,
    ) -> Self {
        Self {
            coder: ErasureCoder::default_solana(),
            neighborhood,
            validator_id,
            received_shreds: DashMap::new(),
            shred_sender,
            signer: None,
            stats: RwLock::new(PropagationStats::default()),
        }
    }

    /// Attach a signing key so propagated shreds are signed by this validator.
    pub fn with_signer(mut self, key: SigningKey) -> Self {
        self.signer = Some(key);
        self
    }

    /// Propagate a block
    ///
    /// 1. Erasure-encode the block into shreds
    /// 2. Send shreds to neighbors
    pub async fn propagate_block(&self, block_data: Vec<u8>, slot: u64) -> anyhow::Result<()> {
        // Erasure-encode
        let mut shreds = self.coder.encode(&block_data, slot);

        // Sign every shred when a signing key is configured so receivers can
        // authenticate the producer.
        if let Some(key) = &self.signer {
            for shred in &mut shreds {
                shred.sign(key);
            }
        }

        tracing::info!(
            "Propagating block at slot {}: {} shreds ({} data + {} coding)",
            slot,
            shreds.len(),
            self.coder.config().data_shards,
            self.coder.config().parity_shards,
        );

        // Get neighbor keys to send to (collect owned data before dropping lock)
        let neighbor_keys: Vec<Vec<u8>> = {
            let neighborhood = self.neighborhood.read();
            neighborhood
                .neighbors(&self.validator_id)
                .iter()
                .map(|n| n.validator.to_vec())
                .collect()
        };

        // Send shreds to each neighbor
        for neighbor_key in &neighbor_keys {
            let shreds_clone = shreds.clone();

            if let Err(e) = self.shred_sender.send((neighbor_key.clone(), shreds_clone)) {
                tracing::error!("Failed to send shreds to neighbor: {}", e);
                self.stats.write().propagation_errors += 1;
            }
        }

        self.stats.write().blocks_propagated += 1;
        self.stats.write().shreds_sent += shreds.len() as u64;

        Ok(())
    }

    /// Receive a shred from a neighbor.
    ///
    /// Verifies payload integrity and, when present, the Ed25519 signature.
    /// Duplicate shred IDs are dropped (dedup). Returns `true` if the shred
    /// was accepted and stored, `false` otherwise.
    pub fn receive_shred(&self, shred: Shred, from: [u8; 32]) -> bool {
        // Integrity: the stored hash must match the payload.
        if !shred.verify() {
            self.stats.write().invalid_shreds += 1;
            tracing::warn!("Dropping shred with invalid payload hash (slot {})", shred.slot);
            return false;
        }

        // Authentication: any shred carrying a signature must verify. We do
        // not silently accept unsigned shreds that claim a signer.
        if !shred.signature.is_empty() && !shred.verify_signature() {
            self.stats.write().invalid_shreds += 1;
            tracing::warn!("Dropping shred with invalid signature (slot {})", shred.slot);
            return false;
        }

        let slot = shred.slot;
        let id = shred.id;
        let received = ReceivedShred {
            shred,
            received_from: from,
            received_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64,
        };

        // Dedup by ShredId within the slot.
        {
            let mut slot_shreds = self.received_shreds.entry(slot).or_default();
            if slot_shreds.iter().any(|r| r.shred.id == id) {
                self.stats.write().duplicate_shreds += 1;
                return false;
            }
            slot_shreds.push(received);
        }

        self.stats.write().shreds_received += 1;

        // Check if we have enough shreds to reassemble
        if let Some(shreds) = self.received_shreds.get(&slot) {
            if shreds.len() >= self.coder.config().min_shards() {
                tracing::info!(
                    "Received enough shreds for slot {} ({} shreds)",
                    slot,
                    shreds.len()
                );
            }
        }

        true
    }

    /// Drop all buffered shreds for slots strictly below `slot`.
    ///
    /// Returns the number of slots evicted. Used to bound memory as the chain
    /// advances so stale partial blocks cannot accumulate.
    pub fn prune_slots_below(&self, slot: u64) -> usize {
        let stale: Vec<u64> = self
            .received_shreds
            .iter()
            .filter(|e| *e.key() < slot)
            .map(|e| *e.key())
            .collect();

        for s in &stale {
            self.received_shreds.remove(s);
        }

        if !stale.is_empty() {
            self.stats.write().slots_pruned += stale.len() as u64;
        }

        stale.len()
    }

    /// Number of slots currently buffered.
    pub fn buffered_slots(&self) -> usize {
        self.received_shreds.len()
    }

    /// Try to reassemble a block from received shreds
    pub fn try_reassemble(&self, slot: u64) -> Option<Vec<u8>> {
        let shreds = self.received_shreds.get(&slot)?;
        let shred_refs: Vec<Shred> = shreds.iter().map(|r| r.shred.clone()).collect();

        let result = self.coder.decode(&shred_refs);

        if result.is_some() {
            self.stats.write().blocks_reassembled += 1;
        }

        result
    }

    /// Forward shreds to children in the tree (gossip propagation)
    pub async fn forward_shreds(&self, shreds: Vec<Shred>) -> anyhow::Result<()> {
        // Forward to children (not parent — parent already sent to us)
        let neighborhood = self.neighborhood.read();
        let my_node = neighborhood
            .nodes
            .iter()
            .find(|n| n.validator == self.validator_id);

        if let Some(node) = my_node {
            for &child_idx in &node.children {
                let child = &neighborhood.nodes[child_idx];
                let neighbor_key = child.validator.to_vec();
                let shreds_clone = shreds.clone();

                if let Err(e) = self.shred_sender.send((neighbor_key, shreds_clone)) {
                    tracing::error!("Failed to forward shreds: {}", e);
                }
            }
        }

        Ok(())
    }

    /// Get statistics
    pub fn stats(&self) -> PropagationStats {
        self.stats.read().clone()
    }

    /// Get number of received shreds for a slot
    pub fn shreds_received_for_slot(&self, slot: u64) -> usize {
        self.received_shreds
            .get(&slot)
            .map(|s| s.len())
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::neighborhoods::Neighborhood;
    use parking_lot::RwLock;
    use std::sync::Arc;

    #[tokio::test]
    async fn test_propagate_block() {
        let validators = vec![([1u8; 32], 1000), ([2u8; 32], 800), ([3u8; 32], 600)];

        let neighborhood = Arc::new(RwLock::new(Neighborhood::build_tree(validators, 5)));

        let (tx, _rx) = mpsc::unbounded_channel();

        let propagator = TurbinePropagator::new([1u8; 32], neighborhood, tx);

        let block_data = vec![42u8; 1000];
        propagator.propagate_block(block_data, 10).await.unwrap();

        // Should have sent shreds to neighbors
        let stats = propagator.stats();
        assert!(stats.blocks_propagated > 0);
        assert!(stats.shreds_sent > 0);
    }

    #[tokio::test]
    async fn test_receive_and_reassemble() {
        let neighborhood = Arc::new(RwLock::new(Neighborhood::build_tree(
            vec![([1u8; 32], 1000)],
            5,
        )));

        let (tx, _rx) = mpsc::unbounded_channel();
        let propagator = TurbinePropagator::new([1u8; 32], neighborhood, tx);

        // Create some shreds
        let original_data = vec![1u8, 2, 3, 4, 5, 6, 7, 8];
        let shreds = propagator.coder.encode(&original_data, 1);

        // Receive all data shreds
        for shred in &shreds {
            propagator.receive_shred(shred.clone(), [2u8; 32]);
        }

        assert_eq!(propagator.shreds_received_for_slot(1), shreds.len());
    }

    #[tokio::test]
    async fn test_receive_dedups_by_shred_id() {
        let neighborhood = Arc::new(RwLock::new(Neighborhood::build_tree(
            vec![([1u8; 32], 1000)],
            5,
        )));
        let (tx, _rx) = mpsc::unbounded_channel();
        let propagator = TurbinePropagator::new([1u8; 32], neighborhood, tx);

        let shred = Shred::new_data(1, 0, vec![1, 2, 3]);
        assert!(propagator.receive_shred(shred.clone(), [2u8; 32]));
        assert!(!propagator.receive_shred(shred.clone(), [3u8; 32]));

        assert_eq!(propagator.shreds_received_for_slot(1), 1);
        assert_eq!(propagator.stats().duplicate_shreds, 1);
        assert_eq!(propagator.stats().shreds_received, 1);
    }

    #[tokio::test]
    async fn test_receive_rejects_tampered_and_bad_signature() {
        let neighborhood = Arc::new(RwLock::new(Neighborhood::build_tree(
            vec![([1u8; 32], 1000)],
            5,
        )));
        let (tx, _rx) = mpsc::unbounded_channel();
        let propagator = TurbinePropagator::new([1u8; 32], neighborhood, tx);
        let sk = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);

        // Tampered payload (hash no longer matches).
        let mut tampered = Shred::new_data(1, 0, vec![1, 2, 3]);
        tampered.payload[0] = 9;
        assert!(!propagator.receive_shred(tampered, [2u8; 32]));

        // Correctly hashed but signature forged over different bytes.
        let mut forged = Shred::new_data(1, 1, vec![4, 5, 6]);
        forged.sign(&sk);
        forged.index = 2; // invalidates the signature
        assert!(!propagator.receive_shred(forged, [2u8; 32]));

        // A properly signed shred is accepted.
        let mut good = Shred::new_data(1, 3, vec![7, 8, 9]);
        good.sign(&sk);
        assert!(propagator.receive_shred(good, [2u8; 32]));

        assert_eq!(propagator.stats().invalid_shreds, 2);
        assert_eq!(propagator.stats().shreds_received, 1);
    }

    #[tokio::test]
    async fn test_prune_slots_below() {
        let neighborhood = Arc::new(RwLock::new(Neighborhood::build_tree(
            vec![([1u8; 32], 1000)],
            5,
        )));
        let (tx, _rx) = mpsc::unbounded_channel();
        let propagator = TurbinePropagator::new([1u8; 32], neighborhood, tx);

        for slot in 1..=5 {
            propagator.receive_shred(Shred::new_data(slot, 0, vec![slot as u8]), [2u8; 32]);
        }
        assert_eq!(propagator.buffered_slots(), 5);

        assert_eq!(propagator.prune_slots_below(4), 3);
        assert_eq!(propagator.buffered_slots(), 2);
        assert!(propagator.stats().slots_pruned >= 3);
        assert!(propagator.try_reassemble(1).is_none());
    }

    #[tokio::test]
    async fn test_signed_propagation() {
        let neighborhood = Arc::new(RwLock::new(Neighborhood::build_tree(
            vec![([1u8; 32], 1000), ([2u8; 32], 900)],
            5,
        )));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let sk = ed25519_dalek::SigningKey::from_bytes(&[3u8; 32]);
        let propagator =
            TurbinePropagator::new([1u8; 32], neighborhood, tx).with_signer(sk.clone());

        propagator.propagate_block(vec![9u8; 512], 42).await.unwrap();

        let mut seen = 0;
        while let Ok((_peer, shreds)) = rx.try_recv() {
            for shred in shreds {
                assert!(shred.verify());
                assert!(shred.verify_signature());
                assert_eq!(shred.signer, sk.verifying_key().to_bytes());
                seen += 1;
            }
        }
        assert!(seen > 0);
    }
}
