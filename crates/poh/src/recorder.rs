//! PoH Recorder — records entries into the PoH chain
//!
//! The recorder is responsible for:
//! 1. Hashing the chain forward
//! 2. Recording transactions as they arrive
//! 3. Creating tick entries at regular intervals
//! 4. Emitting completed entries for block production

use crate::entry::{EntryData, PohEntry, TransactionEntry};
use crate::hasher::PohHash;
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
    /// Configuration
    config: PohConfig,
    /// Current slot
    slot: u64,
    /// Current tick height (total ticks since genesis)
    tick_height: u64,
    /// Total hashes performed (cumulative proof-of-history work)
    hash_count: u64,
    /// Entries recorded in current slot
    current_entries: Vec<PohEntry>,
    /// Index within current slot
    entry_index: u64,
    /// Channel to emit recorded entries
    entry_sender: mpsc::UnboundedSender<RecordedEntry>,
    /// Pending transactions to include
    pending_transactions: Vec<TransactionEntry>,
    /// Hash of the previous entry (the running PoH chain head)
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
            config,
            slot: 0,
            tick_height: 0,
            hash_count: 0,
            current_entries: Vec::new(),
            entry_index: 0,
            entry_sender,
            pending_transactions: Vec::new(),
            prev_hash: genesis_hash,
        }
    }

    /// Record a transaction into the PoH chain
    pub fn record_transaction(&mut self, transaction: Vec<u8>, signature: Vec<u8>) -> Result<()> {
        // A transaction entry commits one hash of sequential work.
        let prev = self.prev_hash;
        let entry =
            PohEntry::transaction(prev, 1, self.slot, self.entry_index, transaction, signature);

        self.hash_count += 1;
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
        // A tick commits hashes_per_tick steps of sequential work; the entry
        // hash is the chain result, so the work cannot be faked.
        self.tick_height += 1;

        let prev = self.prev_hash;
        let entry = PohEntry::tick(
            prev,
            self.config.hashes_per_tick,
            self.slot,
            self.entry_index,
        );

        self.hash_count += self.config.hashes_per_tick;
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

        // One hash step of sequential work per transaction in the batch; the
        // entry hash is the resulting chain head, so the work is committed.
        let mut entry = PohEntry {
            hash: PohHash::zero(),
            prev_hash: self.prev_hash,
            num_hashes: num_txs,
            slot: self.slot,
            index: self.entry_index,
            data: EntryData::Transactions(txs),
        };
        entry.hash = entry.compute_hash();

        self.hash_count += num_txs;
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
        // The chain continues from the previous slot — that continuity is
        // exactly what makes the history verifiable across slots.
        self.slot = slot;
        self.entry_index = 0;
        entries
    }

    /// Get the current chain head (the most recent entry hash)
    pub fn current_hash(&self) -> PohHash {
        self.prev_hash
    }

    /// Get tick height
    pub fn tick_height(&self) -> u64 {
        self.tick_height
    }

    /// Get current slot
    pub fn slot(&self) -> u64 {
        self.slot
    }

    /// Get number of hashes computed
    pub fn hash_count(&self) -> u64 {
        self.hash_count
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
