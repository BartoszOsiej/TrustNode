//! PoH Recorder — records entries into the PoH chain
//!
//! The recorder is responsible for:
//! 1. Hashing the chain forward
//! 2. Recording transactions as they arrive
//! 3. Creating tick entries at regular intervals
//! 4. Emitting completed entries for block production

use crate::entry::{EntryData, PohEntry, TransactionEntry};
use crate::hasher::{PohHash, PohHasher};
use crate::PohConfig;
use anyhow::Result;
use parking_lot::RwLock;
use std::sync::Arc;
use tokio::sync::mpsc;

/// A recorded entry ready for block production
#[derive(Debug, Clone)]
pub struct RecordedEntry {
    pub entry: PohEntry,
    pub tick_height: u64,
}

/// PoH Recorder — the main recording engine
pub struct PohRecorder {
    /// The hash chain engine
    hasher: PohHasher,
    /// Configuration
    config: PohConfig,
    /// Current tick height (total ticks since genesis)
    tick_height: u64,
    /// Entries recorded in current slot
    current_entries: Vec<PohEntry>,
    /// Index within current slot
    entry_index: u64,
    /// Channel to emit recorded entries
    entry_sender: mpsc::UnboundedSender<RecordedEntry>,
    /// Pending transactions to include
    pending_transactions: Vec<TransactionEntry>,
    /// Hash of the previous entry (for chain linking)
    prev_hash: PohHash,
}

impl PohRecorder {
    /// Create a new PohRecorder
    pub fn new(
        config: PohConfig,
        genesis_hash: PohHash,
        entry_sender: mpsc::UnboundedSender<RecordedEntry>,
    ) -> Self {
        Self {
            hasher: PohHasher::new(genesis_hash, 0),
            config,
            tick_height: 0,
            current_entries: Vec::new(),
            entry_index: 0,
            entry_sender,
            pending_transactions: Vec::new(),
            prev_hash: genesis_hash,
        }
    }

    /// Record a transaction into the PoH chain
    pub fn record_transaction(&mut self, transaction: Vec<u8>, signature: Vec<u8>) -> Result<()> {
        // Hash forward to create space for this transaction
        self.hasher.hash_n(1);

        let prev = self.prev_hash;
        let entry = PohEntry::transaction(
            prev,
            1,
            self.hasher.slot(),
            self.entry_index,
            transaction,
            signature,
        );

        self.prev_hash = entry.hash;
        self.entry_index += 1;
        self.current_entries.push(entry.clone());

        // Emit the entry
        let _ = self.entry_sender.send(RecordedEntry {
            entry,
            tick_height: self.tick_height,
        });

        Ok(())
    }

    /// Record a tick (heartbeat) into the PoH chain
    pub fn record_tick(&mut self) -> Result<()> {
        // Hash forward hashes_per_tick times
        self.hasher.hash_n(self.config.hashes_per_tick);
        self.tick_height += 1;

        let prev = self.prev_hash;
        let entry = PohEntry::tick(
            prev,
            self.config.hashes_per_tick,
            self.hasher.slot(),
            self.entry_index,
        );

        self.prev_hash = entry.hash;
        self.entry_index += 1;
        self.current_entries.push(entry.clone());

        // Emit the tick
        let _ = self.entry_sender.send(RecordedEntry {
            entry,
            tick_height: self.tick_height,
        });

        Ok(())
    }

    /// Flush all pending transactions as a single entry
    pub fn flush_transactions(&mut self) -> Result<()> {
        if self.pending_transactions.is_empty() {
            return Ok(());
        }

        let txs: Vec<TransactionEntry> = std::mem::take(&mut self.pending_transactions);
        let num_txs = txs.len() as u64;

        // Hash forward
        self.hasher.hash_n(num_txs);

        // Build combined hash from all transaction hashes
        let combined_hash = {
            use sha2::{Digest, Sha256};
            let mut hasher = Sha256::new();
            for tx in &txs {
                hasher.update(&tx.transaction);
                hasher.update(&tx.signature);
            }
            let result = hasher.finalize();
            let mut bytes = [0u8; 32];
            bytes.copy_from_slice(&result);
            PohHash(bytes)
        };

        let prev = self.prev_hash;
        let entry = PohEntry::with_hash(
            combined_hash,
            prev,
            num_txs,
            self.hasher.slot(),
            self.entry_index,
            EntryData::Transactions(txs),
        );

        self.prev_hash = entry.hash;
        self.entry_index += 1;
        self.current_entries.push(entry.clone());

        let _ = self.entry_sender.send(RecordedEntry {
            entry,
            tick_height: self.tick_height,
        });

        Ok(())
    }

    /// Start a new slot
    pub fn new_slot(&mut self, slot: u64) -> Vec<PohEntry> {
        let entries = std::mem::take(&mut self.current_entries);
        self.hasher.new_slot(slot);
        self.entry_index = 0;
        entries
    }

    /// Get current hash
    pub fn current_hash(&self) -> PohHash {
        self.hasher.current()
    }

    /// Get tick height
    pub fn tick_height(&self) -> u64 {
        self.tick_height
    }

    /// Get current slot
    pub fn slot(&self) -> u64 {
        self.hasher.slot()
    }

    /// Get number of hashes computed
    pub fn hash_count(&self) -> u64 {
        self.hasher.count()
    }

    /// Run the recorder loop (generates ticks at configured rate)
    pub async fn run_loop(mut self) -> Result<()> {
        let tick_interval = std::time::Duration::from_millis(1000 / self.config.target_tick_rate);

        tracing::info!(
            "PoH Recorder started — tick interval: {:?}, hashes_per_tick: {}",
            tick_interval,
            self.config.hashes_per_tick,
        );

        loop {
            tokio::time::sleep(tick_interval).await;

            if let Err(e) = self.record_tick() {
                tracing::error!("PoH tick error: {}", e);
            }

            // Log progress periodically
            if self.tick_height.is_multiple_of(1000) {
                tracing::info!(
                    "PoH tick_height={}, hashes={}, slot={}",
                    self.tick_height,
                    self.hash_count(),
                    self.slot(),
                );
            }
        }
    }
}

/// Shared recorder state for concurrent access
pub type SharedRecorder = Arc<RwLock<PohRecorder>>;

/// Create a shared recorder
pub fn create_shared_recorder(
    config: PohConfig,
    genesis_hash: PohHash,
    entry_sender: mpsc::UnboundedSender<RecordedEntry>,
) -> SharedRecorder {
    Arc::new(RwLock::new(PohRecorder::new(
        config,
        genesis_hash,
        entry_sender,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    #[test]
    fn test_recorder_tick() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let config = PohConfig::default();
        let genesis = PohHash::from_seed("genesis");

        let mut recorder = PohRecorder::new(config, genesis, tx);
        recorder.record_tick().unwrap();

        assert_eq!(recorder.tick_height(), 1);
        assert_eq!(recorder.current_entries.len(), 1);
    }

    #[test]
    fn test_recorder_transaction() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let config = PohConfig::default();
        let genesis = PohHash::from_seed("genesis");

        let mut recorder = PohRecorder::new(config, genesis, tx);
        recorder
            .record_transaction(b"transfer".to_vec(), b"sig".to_vec())
            .unwrap();

        assert_eq!(recorder.current_entries.len(), 1);
    }

    #[tokio::test]
    async fn test_recorder_loop() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let config = PohConfig {
            hashes_per_tick: 10,
            target_tick_rate: 100,
            max_hashes_per_tick: 100,
        };
        let genesis = PohHash::from_seed("genesis");

        let recorder = PohRecorder::new(config, genesis, tx);

        // Run for a short time
        tokio::spawn(async move {
            tokio::time::timeout(std::time::Duration::from_millis(200), recorder.run_loop())
                .await
                .ok();
        });

        // Should receive some ticks
        let mut count = 0;
        while let Some(_entry) = rx.recv().await {
            count += 1;
            if count >= 3 {
                break;
            }
        }
        assert!(count >= 1);
    }
}
