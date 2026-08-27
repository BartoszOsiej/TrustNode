//! Transaction Scheduler — Sealevel-like parallel execution scheduling
//!
//! The scheduler groups transactions into batches where non-conflicting
//! transactions can execute in parallel. This is the core innovation of
//! Sealevel — maximizing throughput by finding parallelism in tx ordering.

use crate::transaction::Transaction;
use std::collections::VecDeque;

/// A batch of transactions that can execute in parallel
#[derive(Debug, Clone)]
pub struct ExecutionBatch {
    /// Batch index
    pub index: u64,
    /// Transactions in this batch (non-conflicting)
    pub transactions: Vec<Transaction>,
    /// Total compute budget for the batch
    pub total_compute: u64,
}

/// Scheduler state
#[derive(Debug)]
pub struct Scheduler {
    /// Pending transactions
    pending: VecDeque<Transaction>,
    /// Scheduled batches ready for execution
    batches: Vec<ExecutionBatch>,
    /// Total transactions scheduled
    total_scheduled: u64,
    /// Maximum transactions per batch
    max_batch_size: usize,
    /// Maximum compute units per batch
    max_batch_compute: u64,
}

impl Scheduler {
    /// Create a new scheduler
    pub fn new(max_batch_size: usize, max_batch_compute: u64) -> Self {
        Self {
            pending: VecDeque::new(),
            batches: Vec::new(),
            total_scheduled: 0,
            max_batch_size,
            max_batch_compute,
        }
    }

    /// Create with default Solana-like limits
    pub fn default_solana() -> Self {
        Self::new(
            128,           // max 128 txs per batch
            200_000 * 128, // 200k CU per tx * 128 txs
        )
    }

    /// Add a transaction to the pending queue
    pub fn enqueue(&mut self, tx: Transaction) {
        self.pending.push_back(tx);
    }

    /// Add multiple transactions
    pub fn enqueue_batch(&mut self, txs: Vec<Transaction>) {
        for tx in txs {
            self.pending.push_back(tx);
        }
    }

    /// Schedule pending transactions into execution batches
    ///
    /// This is the core algorithm — it greedily finds non-conflicting
    /// transactions and groups them into parallelizable batches.
    pub fn schedule(&mut self) {
        let pending: Vec<Transaction> = self.pending.drain(..).collect();
        let mut batch_index = self.batches.len() as u64;

        let mut remaining: Vec<Transaction> = pending;

        while !remaining.is_empty() {
            let mut batch = ExecutionBatch {
                index: batch_index,
                transactions: Vec::new(),
                total_compute: 0,
            };

            // Track which accounts are used in this batch
            let mut batch_writes: Vec<u8> = Vec::new();
            let mut batch_reads: Vec<u8> = Vec::new();

            let mut still_remaining: Vec<Transaction> = Vec::new();

            for tx in remaining.drain(..) {
                if batch.transactions.len() >= self.max_batch_size {
                    still_remaining.push(tx);
                    continue;
                }

                let tx_compute = tx.compute_budget;

                // Check compute budget
                if batch.total_compute + tx_compute > self.max_batch_compute {
                    still_remaining.push(tx);
                    continue;
                }

                // Check for conflicts with current batch
                let tx_writes = tx.write_accounts();
                let tx_reads = tx.read_accounts();

                let mut conflicts = false;

                for w in &tx_writes {
                    if batch_writes.contains(w) || batch_reads.contains(w) {
                        conflicts = true;
                        break;
                    }
                }

                if !conflicts {
                    for r in &tx_reads {
                        if batch_writes.contains(r) {
                            conflicts = true;
                            break;
                        }
                    }
                }

                if !conflicts {
                    batch.transactions.push(tx.clone());
                    batch.total_compute += tx_compute;
                    batch_writes.extend(tx_writes);
                    batch_reads.extend(tx_reads);
                } else {
                    still_remaining.push(tx);
                }
            }

            remaining = still_remaining;

            if !batch.transactions.is_empty() {
                self.total_scheduled += batch.transactions.len() as u64;
                self.batches.push(batch);
                batch_index += 1;
            }
        }
    }

    /// Get the next batch ready for execution
    pub fn next_batch(&mut self) -> Option<ExecutionBatch> {
        self.batches.drain(..1).next()
    }

    /// Peek at the next batch without consuming it
    pub fn peek_batch(&self) -> Option<&ExecutionBatch> {
        self.batches.first()
    }

    /// Get number of pending transactions
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// Get number of scheduled batches
    pub fn batch_count(&self) -> usize {
        self.batches.len()
    }

    /// Get total transactions scheduled
    pub fn total_scheduled(&self) -> u64 {
        self.total_scheduled
    }

    /// Check if scheduler is idle
    pub fn is_idle(&self) -> bool {
        self.pending.is_empty() && self.batches.is_empty()
    }

    /// Drain all remaining transactions (e.g., for block packing)
    pub fn drain_remaining(&mut self) -> Vec<Transaction> {
        self.pending.drain(..).collect()
    }

    /// Get stats
    pub fn stats(&self) -> SchedulerStats {
        SchedulerStats {
            pending: self.pending.len(),
            batches: self.batches.len(),
            total_scheduled: self.total_scheduled,
        }
    }
}

/// Scheduler statistics
#[derive(Debug, Clone)]
pub struct SchedulerStats {
    pub pending: usize,
    pub batches: usize,
    pub total_scheduled: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transaction::{AccountMeta, Instruction};

    fn make_tx(read_accounts: Vec<u8>, write_accounts: Vec<u8>) -> Transaction {
        let program = [1u8; 32];
        let signer = [2u8; 32];

        let mut accounts = Vec::new();
        for i in read_accounts {
            accounts.push(AccountMeta {
                index: i,
                is_signer: false,
                is_writable: false,
            });
        }
        for i in write_accounts {
            accounts.push(AccountMeta {
                index: i,
                is_signer: i == 0,
                is_writable: true,
            });
        }

        let ix = Instruction {
            program_id: program,
            accounts,
            data: vec![],
        };

        Transaction::new(signer, vec![ix], [0u8; 32])
    }

    #[test]
    fn test_parallel_no_conflict() {
        let mut scheduler = Scheduler::new(10, 1_000_000);

        // Three txs touching different accounts — should all be in one batch
        scheduler.enqueue(make_tx(vec![], vec![0]));
        scheduler.enqueue(make_tx(vec![], vec![1]));
        scheduler.enqueue(make_tx(vec![], vec![2]));

        scheduler.schedule();

        let batch = scheduler.next_batch().unwrap();
        assert_eq!(batch.transactions.len(), 3);
    }

    #[test]
    fn test_conflict_separates_batches() {
        let mut scheduler = Scheduler::new(10, 1_000_000);

        // tx1 writes account 0
        scheduler.enqueue(make_tx(vec![], vec![0]));
        // tx2 writes account 0 — conflicts with tx1
        scheduler.enqueue(make_tx(vec![], vec![0]));

        scheduler.schedule();

        // Should produce 2 batches (one tx each)
        assert_eq!(scheduler.batch_count(), 2);
    }

    #[test]
    fn test_read_write_conflict() {
        let mut scheduler = Scheduler::new(10, 1_000_000);

        // tx1 writes account 0
        scheduler.enqueue(make_tx(vec![], vec![0]));
        // tx2 reads account 0 — conflicts with tx1
        scheduler.enqueue(make_tx(vec![0], vec![]));

        scheduler.schedule();

        assert_eq!(scheduler.batch_count(), 2);
    }

    #[test]
    fn test_no_read_read_conflict() {
        let mut scheduler = Scheduler::new(10, 1_000_000);

        // Both txs read account 0 — no conflict
        scheduler.enqueue(make_tx(vec![0], vec![]));
        scheduler.enqueue(make_tx(vec![0], vec![]));

        scheduler.schedule();

        let batch = scheduler.next_batch().unwrap();
        assert_eq!(batch.transactions.len(), 2);
    }
}
