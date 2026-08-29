//! CRDS — Cluster Replicated Data Store
//!
//! CRDS is Solana's gossip data store. It stores data with:
//! - Timestamps for freshness
//! - Signatures for authenticity
//! - Deduplication via content hashing

use crate::message::{ContactInfo, GossipMessage, GossipPayload, PushData};
use dashmap::DashMap;
use parking_lot::RwLock;

/// A CRDS entry with metadata
#[derive(Debug, Clone)]
pub struct CrdsEntry {
    /// The message data
    pub data: PushData,
    /// Timestamp when this was inserted
    pub inserted_at: u64,
    /// Who sent this
    pub from: [u8; 32],
    /// Number of times this has been propagated
    pub propagation_count: u32,
}

/// CRDS data store
pub struct Crds {
    /// Stored entries indexed by content hash
    entries: DashMap<[u8; 32], CrdsEntry>,
    /// Contact info for known validators
    contacts: DashMap<[u8; 32], ContactInfo>,
    /// Our own contact info
    self_info: RwLock<Option<ContactInfo>>,
    /// Maximum entries before pruning
    max_entries: usize,
    /// Total entries inserted
    total_inserted: RwLock<u64>,
    /// Total entries pruned
    total_pruned: RwLock<u64>,
}

impl Crds {
    /// Create a new CRDS store
    pub fn new(max_entries: usize) -> Self {
        Self {
            entries: DashMap::new(),
            contacts: DashMap::new(),
            self_info: RwLock::new(None),
            max_entries,
            total_inserted: RwLock::new(0),
            total_pruned: RwLock::new(0),
        }
    }

    /// Create with default capacity
    pub fn default_store() -> Self {
        Self::new(100_000)
    }

    /// Insert a push message into CRDS
    pub fn insert_push(&self, msg: &GossipMessage) -> bool {
        if let GossipPayload::Push(data) = &msg.payload {
            let id = data.id;

            // Check if we already have this entry
            if self.entries.contains_key(&id) {
                return false;
            }

            let entry = CrdsEntry {
                data: data.clone(),
                inserted_at: self.now_ms(),
                from: msg.from,
                propagation_count: 0,
            };

            self.entries.insert(id, entry);
            *self.total_inserted.write() += 1;

            // Prune if necessary
            if self.entries.len() > self.max_entries {
                self.prune_old();
            }

            true
        } else {
            false
        }
    }

    /// Insert contact info
    pub fn insert_contact(&self, contact: ContactInfo) {
        self.contacts.insert(contact.pubkey, contact);
    }

    /// Set our own contact info
    pub fn set_self_info(&self, info: ContactInfo) {
        *self.self_info.write() = Some(info);
    }

    /// Get our contact info
    pub fn self_info(&self) -> Option<ContactInfo> {
        self.self_info.read().clone()
    }

    /// Get an entry by ID
    pub fn get(&self, id: &[u8; 32]) -> Option<CrdsEntry> {
        self.entries.get(id).map(|e| e.value().clone())
    }

    /// Get entries newer than a timestamp
    pub fn entries_newer_than(&self, timestamp: u64) -> Vec<CrdsEntry> {
        self.entries
            .iter()
            .filter(|e| e.inserted_at > timestamp)
            .map(|e| e.value().clone())
            .collect()
    }

    /// Get entries by kind
    pub fn entries_by_kind(&self, kind: &str) -> Vec<CrdsEntry> {
        self.entries
            .iter()
            .filter(|e| e.data.kind == kind)
            .map(|e| e.value().clone())
            .collect()
    }

    /// Get all known contacts
    pub fn contacts(&self) -> Vec<ContactInfo> {
        self.contacts.iter().map(|e| e.value().clone()).collect()
    }

    /// Get a random set of contacts (for gossip)
    pub fn random_contacts(&self, count: usize) -> Vec<ContactInfo> {
        use rand::seq::SliceRandom;
        let mut contacts: Vec<ContactInfo> = self.contacts();
        contacts.shuffle(&mut rand::rng());
        contacts.into_iter().take(count).collect()
    }

    /// Prune old entries (FIFO)
    fn prune_old(&self) {
        let mut entries: Vec<([u8; 32], u64)> = self
            .entries
            .iter()
            .map(|e| (*e.key(), e.inserted_at))
            .collect();

        entries.sort_by_key(|(_, ts)| *ts);

        let to_remove = entries.len().saturating_sub(self.max_entries * 9 / 10);
        for (id, _) in entries.into_iter().take(to_remove) {
            self.entries.remove(&id);
            *self.total_pruned.write() += 1;
        }
    }

    /// Get total entries
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Check if empty
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Get stats
    pub fn stats(&self) -> CrdsStats {
        CrdsStats {
            entries: self.entries.len(),
            contacts: self.contacts.len(),
            total_inserted: *self.total_inserted.read(),
            total_pruned: *self.total_pruned.read(),
        }
    }

    /// Current timestamp in milliseconds
    fn now_ms(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
    }
}

/// CRDS statistics
#[derive(Debug, Clone)]
pub struct CrdsStats {
    pub entries: usize,
    pub contacts: usize,
    pub total_inserted: u64,
    pub total_pruned: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{GossipMessage, GossipPayload, MessageType, PushData};

    #[test]
    fn test_crds_insert() {
        let crds = Crds::default_store();

        let data = PushData {
            id: [1u8; 32],
            content: vec![1, 2, 3],
            kind: "vote".to_string(),
        };

        let msg = GossipMessage::new([42u8; 32], MessageType::Push, GossipPayload::Push(data));
        assert!(crds.insert_push(&msg));
        assert_eq!(crds.len(), 1);
    }

    #[test]
    fn test_crds_dedup() {
        let crds = Crds::default_store();

        let data = PushData {
            id: [1u8; 32],
            content: vec![1, 2, 3],
            kind: "vote".to_string(),
        };

        let msg = GossipMessage::new([42u8; 32], MessageType::Push, GossipPayload::Push(data));
        assert!(crds.insert_push(&msg));
        assert!(!crds.insert_push(&msg)); // Duplicate → rejected
        assert_eq!(crds.len(), 1);
    }

    #[test]
    fn test_crds_pruning() {
        let crds = Crds::new(10); // Very small max

        for i in 0..20 {
            let data = PushData {
                id: [i; 32],
                content: vec![i],
                kind: "vote".to_string(),
            };
            let msg = GossipMessage::new([42u8; 32], MessageType::Push, GossipPayload::Push(data));
            crds.insert_push(&msg);
        }

        // Should have pruned some entries
        let stats = crds.stats();
        assert!(stats.total_pruned > 0);
        assert!(crds.len() <= 10);
    }

    #[test]
    fn test_contacts() {
        let crds = Crds::default_store();

        let contact = ContactInfo {
            pubkey: [1u8; 32],
            gossip: crate::message::SocketAddr::localhost(8000),
            tpu: crate::message::SocketAddr::localhost(8001),
            rpc: crate::message::SocketAddr::localhost(8002),
            tvu: crate::message::SocketAddr::localhost(8003),
            stake: 1_000_000,
            version: "0.1.0".to_string(),
            wallclock: 1234567890,
        };

        crds.insert_contact(contact);
        assert_eq!(crds.contacts().len(), 1);
    }
}
