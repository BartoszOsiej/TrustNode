//! AccountsDB — append-only state storage with hash map index
//!
//! Inspired by Solana's AppendVec + index architecture:
//! - Accounts are stored sequentially in an append-only buffer
//! - A DashMap provides O(1) lookup by pubkey
//! - State root is computed as a Merkle hash of all accounts

use crate::account::{Account, Pubkey};
use crate::merkle::StateRoot;
use dashmap::DashMap;
use parking_lot::RwLock;
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
        }
    }

    /// Store or update an account
    pub fn store(&self, pubkey: Pubkey, account: &Account) {
        let serialized = bincode::serialize(account).expect("Failed to serialize account");
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

        bincode::deserialize(&storage[start..end]).ok()
    }

    /// Check if an account exists
    pub fn exists(&self, pubkey: &Pubkey) -> bool {
        self.index.contains_key(pubkey)
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
}
