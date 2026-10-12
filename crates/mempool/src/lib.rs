//! # Mempool
//!
//! The transaction waiting room between RPC ingestion and block production.
//!
//! A mempool has to answer three questions cheaply and honestly:
//!
//! 1. **Have I seen this transaction before?** — dedup by signature hash, so a
//!    client that retries does not get charged twice.
//! 2. **Which transactions belong in this block?** — ordered by fee, so the
//!    block producer packs the most valuable work first (FIFO breaks ties).
//! 3. **What happens when it overflows?** — a bounded pool that evicts its
//!    cheapest entry rather than growing without limit, and drops entries that
//!    have aged past their blockhash window.
//!
//! Transactions are verified on the way in: an unsigned, malformed or
//! under-fee transaction never enters the pool.

use parking_lot::RwLock;
use solana_tx_processor::transaction::Transaction;
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};

/// Transaction identifier (the transaction hash covered by its signature).
pub type TxHash = [u8; 32];

/// Mempool tuning knobs.
#[derive(Debug, Clone)]
pub struct MempoolConfig {
    /// Maximum number of parked transactions before eviction kicks in.
    pub max_size: usize,
    /// Maximum age of an entry in milliseconds before it is pruned.
    pub max_age_ms: u64,
    /// Minimum fee (lamports) a transaction must pay to be admitted.
    pub min_fee: u64,
}

impl Default for MempoolConfig {
    fn default() -> Self {
        Self {
            max_size: 10_000,
            max_age_ms: 60_000,
            min_fee: 5_000,
        }
    }
}

/// Why a transaction was refused entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MempoolError {
    /// Signature missing, malformed or not matching the signer.
    InvalidSignature,
    /// Fee below [`MempoolConfig::min_fee`].
    FeeTooLow,
    /// An identical transaction (same hash) is already parked.
    Duplicate,
    /// The pool is full and this transaction is not valuable enough to evict
    /// an existing one.
    Full,
}

impl std::fmt::Display for MempoolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidSignature => write!(f, "invalid transaction signature"),
            Self::FeeTooLow => write!(f, "fee below mempool minimum"),
            Self::Duplicate => write!(f, "duplicate transaction"),
            Self::Full => write!(f, "mempool full"),
        }
    }
}

impl std::error::Error for MempoolError {}

/// One parked transaction with the metadata used for ordering and expiry.
#[derive(Debug, Clone)]
pub struct MempoolEntry {
    /// Transaction hash (dedup key).
    pub hash: TxHash,
    /// The transaction itself.
    pub tx: Transaction,
    /// Priority — copied from `tx.fee` at insertion time.
    pub fee: u64,
    /// Insertion time, Unix milliseconds.
    pub received_at: u64,
}

impl MempoolEntry {
    /// Age of this entry in milliseconds relative to `now`.
    pub fn age_ms(&self, now: u64) -> u64 {
        now.saturating_sub(self.received_at)
    }
}

struct Inner {
    /// hash → insertion sequence
    by_hash: HashMap<TxHash, u64>,
    /// sequence → entry (stable insertion order)
    by_seq: BTreeMap<u64, MempoolEntry>,
    next_seq: u64,
}

impl Inner {
    fn new() -> Self {
        Self {
            by_hash: HashMap::new(),
            by_seq: BTreeMap::new(),
            next_seq: 0,
        }
    }

    fn remove_seq(&mut self, seq: u64) -> Option<MempoolEntry> {
        let entry = self.by_seq.remove(&seq)?;
        self.by_hash.remove(&entry.hash);
        Some(entry)
    }
}

/// Bounded, deduplicating, fee-prioritised transaction pool.
pub struct Mempool {
    inner: RwLock<Inner>,
    config: MempoolConfig,
    total_accepted: AtomicU64,
    total_rejected: AtomicU64,
    total_evicted: AtomicU64,
    total_expired: AtomicU64,
}

impl Mempool {
    /// Create a pool with an explicit configuration.
    pub fn new(config: MempoolConfig) -> Self {
        Self {
            inner: RwLock::new(Inner::new()),
            config,
            total_accepted: AtomicU64::new(0),
            total_rejected: AtomicU64::new(0),
            total_evicted: AtomicU64::new(0),
            total_expired: AtomicU64::new(0),
        }
    }

    /// Create a pool with Solana-like defaults (10k entries, 60s, 5000 fee).
    pub fn default_pool() -> Self {
        Self::new(MempoolConfig::default())
    }

    /// Admit a transaction. Verifies the signature, enforces the minimum fee,
    /// rejects duplicates and, if the pool is full, evicts the cheapest entry
    /// when the newcomer is worth more.
    pub fn insert(&self, tx: &Transaction) -> Result<TxHash, MempoolError> {
        if !tx.verify_signature() {
            self.total_rejected.fetch_add(1, Ordering::Relaxed);
            return Err(MempoolError::InvalidSignature);
        }
        if tx.fee < self.config.min_fee {
            self.total_rejected.fetch_add(1, Ordering::Relaxed);
            return Err(MempoolError::FeeTooLow);
        }

        let hash = tx.hash();
        let now = now_ms();
        let mut inner = self.inner.write();

        if inner.by_hash.contains_key(&hash) {
            self.total_rejected.fetch_add(1, Ordering::Relaxed);
            return Err(MempoolError::Duplicate);
        }

        // Drop stale entries first so they never consume capacity.
        Self::expire_locked(&mut inner, self.config.max_age_ms, now, &self.total_expired);

        if inner.by_seq.len() >= self.config.max_size {
            let victim = inner
                .by_seq
                .iter()
                .min_by(|a, b| a.1.fee.cmp(&b.1.fee).then(a.0.cmp(b.0)))
                .map(|(seq, entry)| (*seq, entry.fee));

            match victim {
                Some((seq, victim_fee)) if tx.fee > victim_fee => {
                    inner.remove_seq(seq);
                    self.total_evicted.fetch_add(1, Ordering::Relaxed);
                }
                _ => {
                    self.total_rejected.fetch_add(1, Ordering::Relaxed);
                    return Err(MempoolError::Full);
                }
            }
        }

        let seq = inner.next_seq;
        inner.next_seq += 1;
        inner.by_hash.insert(hash, seq);
        inner.by_seq.insert(
            seq,
            MempoolEntry {
                hash,
                tx: tx.clone(),
                fee: tx.fee,
                received_at: now,
            },
        );

        self.total_accepted.fetch_add(1, Ordering::Relaxed);
        Ok(hash)
    }

    /// Remove and return up to `limit` transactions, highest fee first (FIFO
    /// for equal fees). This is what the block producer packs.
    pub fn drain(&self, limit: usize) -> Vec<Transaction> {
        let mut inner = self.inner.write();
        if limit == 0 || inner.by_seq.is_empty() {
            return Vec::new();
        }

        let mut ordered: Vec<(u64, u64)> = inner
            .by_seq
            .iter()
            .map(|(seq, entry)| (entry.fee, *seq))
            .collect();
        // fee descending, then insertion order ascending
        ordered.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));

        let take = limit.min(ordered.len());
        let mut out = Vec::with_capacity(take);
        for (_, seq) in ordered.into_iter().take(take) {
            if let Some(entry) = inner.remove_seq(seq) {
                out.push(entry.tx);
            }
        }
        out
    }

    /// Remove and return a specific transaction by hash.
    pub fn remove(&self, hash: &TxHash) -> Option<Transaction> {
        let mut inner = self.inner.write();
        let seq = *inner.by_hash.get(hash)?;
        inner.remove_seq(seq).map(|entry| entry.tx)
    }

    /// Whether a transaction with this hash is parked.
    pub fn contains(&self, hash: &TxHash) -> bool {
        self.inner.read().by_hash.contains_key(hash)
    }

    /// Prune entries older than `max_age_ms`, returning how many were dropped.
    pub fn expire(&self, now: u64) -> usize {
        let mut inner = self.inner.write();
        Self::expire_locked(&mut inner, self.config.max_age_ms, now, &self.total_expired)
    }

    fn expire_locked(
        inner: &mut Inner,
        max_age_ms: u64,
        now: u64,
        counter: &AtomicU64,
    ) -> usize {
        let stale: Vec<u64> = inner
            .by_seq
            .iter()
            .filter(|(_, entry)| entry.age_ms(now) > max_age_ms)
            .map(|(seq, _)| *seq)
            .collect();
        let n = stale.len();
        for seq in stale {
            inner.remove_seq(seq);
        }
        if n > 0 {
            counter.fetch_add(n as u64, Ordering::Relaxed);
        }
        n
    }

    /// Number of parked transactions.
    pub fn len(&self) -> usize {
        self.inner.read().by_seq.len()
    }

    /// True when the pool is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Read-only view of the pool contents, highest fee first.
    pub fn entries(&self) -> Vec<MempoolEntry> {
        let inner = self.inner.read();
        let mut entries: Vec<MempoolEntry> = inner.by_seq.values().cloned().collect();
        entries.sort_by(|a, b| {
            b.fee
                .cmp(&a.fee)
                .then(a.received_at.cmp(&b.received_at))
        });
        entries
    }

    /// Configuration in use.
    pub fn config(&self) -> &MempoolConfig {
        &self.config
    }

    /// Snapshot of pool counters.
    pub fn stats(&self) -> MempoolStats {
        MempoolStats {
            len: self.len(),
            max_size: self.config.max_size,
            total_accepted: self.total_accepted.load(Ordering::Relaxed),
            total_rejected: self.total_rejected.load(Ordering::Relaxed),
            total_evicted: self.total_evicted.load(Ordering::Relaxed),
            total_expired: self.total_expired.load(Ordering::Relaxed),
        }
    }
}

impl Default for Mempool {
    fn default() -> Self {
        Self::default_pool()
    }
}

/// Mempool counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MempoolStats {
    pub len: usize,
    pub max_size: usize,
    pub total_accepted: u64,
    pub total_rejected: u64,
    pub total_evicted: u64,
    pub total_expired: u64,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use solana_accounts::account::Pubkey;
    use solana_program_executor::instruction::SystemInstruction;
    use solana_tx_processor::transaction::{AccountMeta, Instruction};

    fn test_key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn transfer(sk: &SigningKey, recipient: Pubkey, lamports: u64, fee: u64) -> Transaction {
        let signer = sk.verifying_key().to_bytes();
        let ix = Instruction {
            program_id: [1u8; 32],
            accounts: vec![
                AccountMeta {
                    index: 0,
                    is_signer: true,
                    is_writable: true,
                },
                AccountMeta {
                    index: 1,
                    is_signer: false,
                    is_writable: true,
                },
            ],
            data: SystemInstruction::Transfer { lamports }.to_data(),
        };
        let mut tx = Transaction::new(signer, vec![ix], [0u8; 32]);
        tx.account_keys.push(recipient);
        tx.fee = fee;
        tx.sign(sk);
        tx
    }

    #[test]
    fn insert_and_contains() {
        let pool = Mempool::default_pool();
        let tx = transfer(&test_key(1), [9u8; 32], 100, 5_000);
        let hash = pool.insert(&tx).unwrap();

        assert_eq!(pool.len(), 1);
        assert!(pool.contains(&hash));
        assert_eq!(pool.stats().total_accepted, 1);
    }

    #[test]
    fn rejects_unsigned_transaction() {
        let pool = Mempool::default_pool();
        let mut tx = transfer(&test_key(1), [9u8; 32], 100, 5_000);
        tx.signature.clear();

        assert_eq!(pool.insert(&tx), Err(MempoolError::InvalidSignature));
        assert!(pool.is_empty());
        assert_eq!(pool.stats().total_rejected, 1);
    }

    #[test]
    fn rejects_low_fee() {
        let pool = Mempool::default_pool();
        let tx = transfer(&test_key(1), [9u8; 32], 100, 4_999);
        assert_eq!(pool.insert(&tx), Err(MempoolError::FeeTooLow));
        assert!(pool.is_empty());
    }

    #[test]
    fn rejects_duplicates() {
        let pool = Mempool::default_pool();
        let tx = transfer(&test_key(1), [9u8; 32], 100, 5_000);

        assert!(pool.insert(&tx).is_ok());
        assert_eq!(pool.insert(&tx), Err(MempoolError::Duplicate));
        assert_eq!(pool.len(), 1);
    }

    #[test]
    fn drain_returns_highest_fee_first() {
        let pool = Mempool::default_pool();
        let sk = test_key(1);
        // Same signer, different fees → different txs, distinct hashes because
        // the fee is covered by the signature.
        pool.insert(&transfer(&sk, [9u8; 32], 100, 5_000)).unwrap();
        pool.insert(&transfer(&sk, [9u8; 32], 100, 9_000)).unwrap();
        pool.insert(&transfer(&sk, [9u8; 32], 100, 7_000)).unwrap();

        let drained = pool.drain(2);
        assert_eq!(drained.len(), 2);
        assert_eq!(drained[0].fee, 9_000);
        assert_eq!(drained[1].fee, 7_000);
        assert_eq!(pool.len(), 1);
        assert_eq!(pool.entries()[0].fee, 5_000);
    }

    #[test]
    fn drain_limit_zero_is_noop() {
        let pool = Mempool::default_pool();
        let tx = transfer(&test_key(1), [9u8; 32], 100, 5_000);
        pool.insert(&tx).unwrap();
        assert!(pool.drain(0).is_empty());
        assert_eq!(pool.len(), 1);
    }

    #[test]
    fn evicts_cheapest_when_full() {
        let pool = Mempool::new(MempoolConfig {
            max_size: 2,
            max_age_ms: 60_000,
            min_fee: 5_000,
        });
        let sk = test_key(1);
        pool.insert(&transfer(&sk, [9u8; 32], 1, 5_000)).unwrap();
        pool.insert(&transfer(&sk, [9u8; 32], 1, 6_000)).unwrap();

        // A richer transaction displaces the 5_000 entry.
        pool.insert(&transfer(&sk, [9u8; 32], 1, 8_000)).unwrap();
        assert_eq!(pool.len(), 2);
        assert_eq!(pool.stats().total_evicted, 1);
        let fees: Vec<u64> = pool.entries().iter().map(|e| e.fee).collect();
        assert_eq!(fees, vec![8_000, 6_000]);

        // A poorer transaction is refused outright.
        assert_eq!(
            pool.insert(&transfer(&sk, [9u8; 32], 1, 5_500)),
            Err(MempoolError::Full)
        );
    }

    #[test]
    fn remove_by_hash() {
        let pool = Mempool::default_pool();
        let tx = transfer(&test_key(1), [9u8; 32], 100, 5_000);
        let hash = pool.insert(&tx).unwrap();

        assert!(pool.remove(&hash).is_some());
        assert!(pool.is_empty());
        assert!(!pool.contains(&hash));
        assert!(pool.remove(&hash).is_none());
    }

    #[test]
    fn expire_drops_stale_entries() {
        let pool = Mempool::new(MempoolConfig {
            max_size: 10,
            max_age_ms: 1_000,
            min_fee: 5_000,
        });
        let tx = transfer(&test_key(1), [9u8; 32], 100, 5_000);
        pool.insert(&tx).unwrap();
        assert_eq!(pool.len(), 1);

        // Pretend an hour passed.
        let future = now_ms() + 3_600_000;
        assert_eq!(pool.expire(future), 1);
        assert!(pool.is_empty());
        assert_eq!(pool.stats().total_expired, 1);
    }
}
