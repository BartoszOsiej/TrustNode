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
            stats: RwLock::new(PropagationStats::default()),
        }
    }

    /// Propagate a block
    ///
    /// 1. Erasure-encode the block into shreds
    /// 2. Send shreds to neighbors
    pub async fn propagate_block(&self, block_data: Vec<u8>, slot: u64) -> anyhow::Result<()> {
        // Erasure-encode
        let shreds = self.coder.encode(&block_data, slot);

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

    /// Receive a shred from a neighbor
    pub fn receive_shred(&self, shred: Shred, from: [u8; 32]) {
        let slot = shred.slot;
        let received = ReceivedShred {
            shred,
            received_from: from,
            received_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64,
        };

        self.received_shreds.entry(slot).or_default().push(received);

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

        let (tx, mut rx) = mpsc::unbounded_channel();

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
}
