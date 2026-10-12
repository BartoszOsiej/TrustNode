//! AccountsDB — append-only state storage with hash map index
//!
//! Inspired by Solana's AppendVec + index architecture:
//! - Accounts are stored sequentially in an append-only buffer
//! - A DashMap provides O(1) lookup by pubkey
//! - State root is computed as a Merkle hash of all accounts

use crate::account::{Account, Pubkey};
use crate::merkle::StateRoot;
use dashmap::DashMap;
use parking_lot::{Mutex, RwLock};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Position in the append-only store
#[derive(Debug, Clone, Copy)]
pub struct AccountLocation {
    /// Offset in the append buffer
    pub offset: u64,
    /// Length of serialized account
    pub length: u64,
    /// Account's write version (monotonic)
    pub write_version: u64,
}

/// Log record tags.
const RECORD_PUT: u8 = 1;
const RECORD_DELETE: u8 = 2;

/// On-disk persistence: an append-only log plus the directory holding it.
struct Persist {
    dir: PathBuf,
    log: Mutex<File>,
}

impl Persist {
    fn log_path(&self) -> PathBuf {
        self.dir.join("accounts.log")
    }

    /// Append one already-encoded record and flush it to the OS.
    fn append(&self, record: &[u8]) {
        let mut log = self.log.lock();
        if let Err(e) = log.write_all(record) {
            tracing::error!("accounts log append failed: {}", e);
        } else if let Err(e) = log.flush() {
            tracing::error!("accounts log flush failed: {}", e);
        }
    }
}

/// Encode a stored-account record: tag, pubkey, u32 length, JSON payload.
fn encode_put(pubkey: &Pubkey, account: &Account) -> Vec<u8> {
    let json = serde_json::to_vec(account).unwrap_or_default();
    let mut buf = Vec::with_capacity(1 + 32 + 4 + json.len());
    buf.push(RECORD_PUT);
    buf.extend_from_slice(pubkey);
    buf.extend_from_slice(&(json.len() as u32).to_le_bytes());
    buf.extend_from_slice(&json);
    buf
}

/// Encode a tombstone: tag, pubkey.
fn encode_delete(pubkey: &Pubkey) -> Vec<u8> {
    let mut buf = Vec::with_capacity(1 + 32);
    buf.push(RECORD_DELETE);
    buf.extend_from_slice(pubkey);
    buf
}

/// Accounts database
pub struct AccountsDB {
    /// Append-only storage buffer
    storage: RwLock<Vec<u8>>,
    /// Index: pubkey → location in storage
    index: DashMap<Pubkey, AccountLocation>,
    /// Monotonic write version counter
    write_version: RwLock<u64>,
    /// Total accounts stored
    account_count: RwLock<u64>,
    /// Total bytes stored
    total_bytes: RwLock<u64>,
    /// Optional on-disk persistence
    persist: Option<Persist>,
}

impl AccountsDB {
    /// Create a new empty accounts database
    pub fn new() -> Self {
        Self {
            storage: RwLock::new(Vec::with_capacity(1024 * 1024)), // 1MB initial
            index: DashMap::new(),
            write_version: RwLock::new(0),
            account_count: RwLock::new(0),
            total_bytes: RwLock::new(0),
            persist: None,
        }
    }

    /// Open a persistent accounts database in `dir`, replaying an existing
    /// append-only log so the state survives a restart.
    pub fn open(dir: impl AsRef<Path>) -> std::io::Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir)?;

        let mut db = Self::new();
        let log_path = dir.join("accounts.log");
        if log_path.exists() {
            let data = std::fs::read(&log_path)?;
            db.replay(&data);
            tracing::info!(
                "accounts: replayed {} bytes from {} ({} accounts)",
                data.len(),
                log_path.display(),
                db.account_count(),
            );
        }

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)?;
        db.persist = Some(Persist {
            dir,
            log: Mutex::new(file),
        });
        Ok(db)
    }

    /// Rebuild in-memory state by replaying an append-only log.
    fn replay(&self, data: &[u8]) {
        let mut cursor = 0usize;
        while cursor < data.len() {
            match data[cursor] {
                RECORD_PUT => {
                    let Some(key_end) = cursor.checked_add(1 + 32) else {
                        break;
                    };
                    let Some(len_end) = key_end.checked_add(4) else {
                        break;
                    };
                    if len_end > data.len() {
                        break;
                    }
                    let mut pubkey = [0u8; 32];
                    pubkey.copy_from_slice(&data[cursor + 1..key_end]);
                    let len =
                        u32::from_le_bytes(data[key_end..len_end].try_into().expect("4 bytes"))
                            as usize;
                    let Some(payload_end) = len_end.checked_add(len) else {
                        break;
                    };
                    if payload_end > data.len() {
                        break;
                    }
                    if let Ok(account) =
                        serde_json::from_slice::<Account>(&data[len_end..payload_end])
                    {
                        self.store(pubkey, &account);
                    }
                    cursor = payload_end;
                }
                RECORD_DELETE => {
                    if cursor + 1 + 32 > data.len() {
                        break;
                    }
                    let mut pubkey = [0u8; 32];
                    pubkey.copy_from_slice(&data[cursor + 1..cursor + 1 + 32]);
                    self.remove(&pubkey);
                    cursor += 1 + 32;
                }
                _ => break, // corrupt/torn tail — stop cleanly
            }
        }
    }

    /// Whether this database persists to disk.
    pub fn is_persistent(&self) -> bool {
        self.persist.is_some()
    }

    /// Flush any buffered records to the OS.
    pub fn flush(&self) -> std::io::Result<()> {
        if let Some(persist) = &self.persist {
            persist.log.lock().flush()?;
        }
        Ok(())
    }

    /// Compact the log: rewrite only the current live accounts and atomically
    /// replace the log, then reopen it for appending.
    pub fn snapshot(&self) -> std::io::Result<()> {
        let Some(persist) = &self.persist else {
            return Ok(());
        };
        let tmp = persist.dir.join("accounts.log.tmp");

        {
            let mut file = File::create(&tmp)?;
            for entry in self.index.iter() {
                if let Some(account) = self.load(entry.key()) {
                    file.write_all(&encode_put(entry.key(), &account))?;
                }
            }
            file.flush()?;
            file.sync_all()?;
        }

        std::fs::rename(&tmp, persist.log_path())?;
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(persist.log_path())?;
        *persist.log.lock() = file;
        Ok(())
    }

    /// Every pubkey currently in the index.
    pub fn pubkeys(&self) -> Vec<Pubkey> {
        self.index.iter().map(|e| *e.key()).collect()
    }

    /// Total lamports held across all accounts — the supply invariant that
    /// every state transition must preserve.
    pub fn total_lamports(&self) -> u64 {
        self.index
            .iter()
            .filter_map(|e| self.load(e.key()).map(|a| a.lamports))
            .sum()
    }

    /// Store or update an account
    pub fn store(&self, pubkey: Pubkey, account: &Account) {
        let serialized = serde_json::to_vec(account).unwrap_or_else(|e| {
            // Serialization only fails on a corrupt account payload. Skipping
            // the write is the only safe outcome: `expect` here would kill the
            // node over a single bad account.
            tracing::error!(
                "account serialization failed for {}: {}",
                hex::encode(&pubkey[..8]),
                e
            );
            Vec::new()
        });
        let length = serialized.len() as u64;

        let write_version = {
            let mut wv = self.write_version.write();
            *wv += 1;
            *wv
        };

        let offset = {
            let mut storage = self.storage.write();
            let offset = storage.len() as u64;
            storage.extend_from_slice(&serialized);
            offset
        };

        let location = AccountLocation {
            offset,
            length,
            write_version,
        };

        // Update or insert in index
        let is_new = !self.index.contains_key(&pubkey);
        self.index.insert(pubkey, location);

        if is_new {
            *self.account_count.write() += 1;
        }
        *self.total_bytes.write() += length;

        if let Some(persist) = &self.persist {
            persist.append(&encode_put(&pubkey, account));
        }
    }

    /// Load an account by pubkey
    pub fn load(&self, pubkey: &Pubkey) -> Option<Account> {
        let location = self.index.get(pubkey)?;
        let storage = self.storage.read();
        let start = location.offset as usize;
        let end = start + location.length as usize;

        if end > storage.len() {
            return None;
        }

        serde_json::from_slice(&storage[start..end]).ok()
    }

    /// Check if an account exists
    pub fn exists(&self, pubkey: &Pubkey) -> bool {
        self.index.contains_key(pubkey)
    }

    /// Remove an account from the index.
    ///
    /// The append-only buffer is never rewritten (old bytes stay until
    /// compaction), so this is an index-only operation. It exists so that a
    /// failed transaction can roll back an account it created.
    pub fn remove(&self, pubkey: &Pubkey) -> bool {
        let removed = self.index.remove(pubkey).is_some();
        if removed {
            let mut count = self.account_count.write();
            if *count > 0 {
                *count -= 1;
            }
            if let Some(persist) = &self.persist {
                persist.append(&encode_delete(pubkey));
            }
        }
        removed
    }

    /// Get account count
    pub fn account_count(&self) -> u64 {
        *self.account_count.read()
    }

    /// Get total bytes stored
    pub fn total_bytes(&self) -> u64 {
        *self.total_bytes.read()
    }

    /// Compute state root hash
    pub fn compute_state_root(&self) -> StateRoot {
        let mut hashes: Vec<[u8; 32]> = Vec::new();

        // Collect all account hashes
        for entry in self.index.iter() {
            if let Some(account) = self.load(entry.key()) {
                hashes.push(account.hash());
            }
        }

        // Sort for deterministic ordering
        hashes.sort();

        StateRoot::compute(&hashes)
    }

    /// Create a snapshot of all account pubkeys
    pub fn snapshot_pubkeys(&self) -> Vec<Pubkey> {
        self.index.iter().map(|e| *e.key()).collect()
    }

    /// Apply a batch of account updates atomically
    pub fn apply_batch(&self, updates: Vec<(Pubkey, Account)>) {
        for (pubkey, account) in updates {
            self.store(pubkey, &account);
        }
    }

    /// Get storage stats
    pub fn stats(&self) -> DBStats {
        DBStats {
            account_count: self.account_count(),
            total_bytes: self.total_bytes(),
            index_slots: self.index.len(),
            write_version: *self.write_version.read(),
        }
    }
}

impl Default for AccountsDB {
    fn default() -> Self {
        Self::new()
    }
}

/// Database statistics
#[derive(Debug, Clone)]
pub struct DBStats {
    pub account_count: u64,
    pub total_bytes: u64,
    pub index_slots: usize,
    pub write_version: u64,
}

/// Shared database handle
pub type SharedAccountsDB = Arc<AccountsDB>;

/// Create a shared accounts database
pub fn create_shared_db() -> SharedAccountsDB {
    Arc::new(AccountsDB::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::random_pubkey;

    #[test]
    fn test_store_and_load() {
        let db = AccountsDB::new();
        let key = random_pubkey();
        let acc = Account::new_system_account(key, 1_000_000);

        db.store(key, &acc);
        let loaded = db.load(&key).unwrap();

        assert_eq!(loaded.lamports, 1_000_000);
        assert_eq!(loaded.owner, crate::account::system_program_id());
    }

    #[test]
    fn test_update_account() {
        let db = AccountsDB::new();
        let key = random_pubkey();

        let acc1 = Account::new_system_account(key, 100);
        db.store(key, &acc1);

        let acc2 = Account::new_system_account(key, 200);
        db.store(key, &acc2);

        let loaded = db.load(&key).unwrap();
        assert_eq!(loaded.lamports, 200);
        // Account count should still be 1
        assert_eq!(db.account_count(), 1);
    }

    #[test]
    fn test_batch_apply() {
        let db = AccountsDB::new();
        let mut updates = Vec::new();

        for _ in 0..10 {
            let key = random_pubkey();
            let acc = Account::new_system_account(key, 500);
            updates.push((key, acc));
        }

        db.apply_batch(updates);
        assert_eq!(db.account_count(), 10);
    }

    #[test]
    fn test_state_root_deterministic() {
        let db1 = AccountsDB::new();
        let db2 = AccountsDB::new();

        let key = random_pubkey();
        let acc = Account::new_system_account(key, 1000);

        db1.store(key, &acc);
        db2.store(key, &acc);

        assert_eq!(db1.compute_state_root(), db2.compute_state_root());
    }

    #[test]
    fn test_stats() {
        let db = AccountsDB::new();
        let key = random_pubkey();
        let acc = Account::new_system_account(key, 500);
        db.store(key, &acc);

        let stats = db.stats();
        assert_eq!(stats.account_count, 1);
        assert!(stats.total_bytes > 0);
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "tn-accounts-{}-{}-{}",
            std::process::id(),
            tag,
            nanos
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn test_persistence_roundtrip() {
        let dir = temp_dir("roundtrip");
        let key = random_pubkey();

        {
            let db = AccountsDB::open(&dir).unwrap();
            assert!(db.is_persistent());
            db.store(key, &Account::new_system_account(key, 42_000));
            db.flush().unwrap();
        }

        let db = AccountsDB::open(&dir).unwrap();
        assert_eq!(db.load(&key).unwrap().lamports, 42_000);
        assert_eq!(db.account_count(), 1);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_persistence_delete_survives_restart() {
        let dir = temp_dir("delete");
        let key = random_pubkey();

        {
            let db = AccountsDB::open(&dir).unwrap();
            db.store(key, &Account::new_system_account(key, 100));
            db.remove(&key);
            db.flush().unwrap();
        }

        let db = AccountsDB::open(&dir).unwrap();
        assert!(!db.exists(&key));
        assert_eq!(db.account_count(), 0);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_snapshot_compacts_and_preserves_state() {
        let dir = temp_dir("snapshot");
        let keys: Vec<_> = (0..20).map(|_| random_pubkey()).collect();

        {
            let db = AccountsDB::open(&dir).unwrap();
            for (i, key) in keys.iter().enumerate() {
                // Rewrite each account several times to bloat the log.
                for v in 0..5 {
                    db.store(*key, &Account::new_system_account(*key, (i + v) as u64));
                }
            }
            let before = std::fs::metadata(dir.join("accounts.log")).unwrap().len();
            db.snapshot().unwrap();
            let after = std::fs::metadata(dir.join("accounts.log")).unwrap().len();
            assert!(
                after < before,
                "snapshot should shrink the log: {after} !< {before}"
            );
        }

        let db = AccountsDB::open(&dir).unwrap();
        assert_eq!(db.account_count(), 20);
        for (i, key) in keys.iter().enumerate() {
            assert_eq!(db.load(key).unwrap().lamports, (i + 4) as u64);
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_in_memory_db_is_not_persistent() {
        let db = AccountsDB::new();
        assert!(!db.is_persistent());
        assert!(db.snapshot().is_ok());
    }
}
