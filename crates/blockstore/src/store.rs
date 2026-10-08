//! Blockstore — append-only on-disk ledger.
//!
//! Frame layout (little-endian):
//!
//! ```text
//! | magic "TNBL" (4B) | payload_len (u32) | payload (JSON BlockRecord) | sha256(payload) (32B) |
//! ```
//!
//! Durability guarantees:
//! - every frame is checksummed (detects bit-rot / corruption)
//! - `block_hash` chains records to the genesis parent (detects tampering)
//! - a torn or corrupt tail is truncated on open — the valid prefix survives
//! - each append is `fdatasync`ed before it is acknowledged

use crate::record::{BlockRecord, GENESIS_PARENT};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// Frame magic bytes
const MAGIC: [u8; 4] = *b"TNBL";
/// Maximum accepted payload size (sanity bound against corrupt lengths)
const MAX_FRAME_PAYLOAD: u32 = 16 * 1024 * 1024;
/// sha256 output length
const HASH_LEN: usize = 32;
/// magic + len
const HEADER_LEN: usize = 8;

enum Chunk {
    Ok,
    CleanEof,
    Torn,
}

fn fill(file: &mut File, buf: &mut [u8]) -> Chunk {
    let mut filled = 0;
    while filled < buf.len() {
        match file.read(&mut buf[filled..]) {
            Ok(0) => {
                return if filled == 0 {
                    Chunk::CleanEof
                } else {
                    Chunk::Torn
                }
            }
            Ok(n) => filled += n,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return Chunk::Torn,
        }
    }
    Chunk::Ok
}

/// Persistent, replayable block ledger.
pub struct Blockstore {
    file: File,
    records: Vec<BlockRecord>,
    dir: PathBuf,
}

impl Blockstore {
    /// Open (or create) the ledger at `dir`, replaying and validating every
    /// frame already on disk. Corrupt or partial tails are truncated.
    pub fn open(dir: impl AsRef<Path>) -> anyhow::Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("blocks.log");

        let file = OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .open(&path)?;

        let (records, valid_len) = Self::replay(&path)?;
        let on_disk = file.metadata()?.len();
        if on_disk != valid_len {
            tracing::warn!(
                "ledger {}: truncating {} corrupt/torn bytes (keeping {} valid blocks)",
                path.display(),
                on_disk - valid_len,
                records.len()
            );
            file.set_len(valid_len)?;
            file.sync_data()?;
        }

        Ok(Self { file, records, dir })
    }

    /// Stream the file, returning validated records and the byte length of
    /// the valid prefix. Stops at the first torn, corrupt or foreign frame.
    fn replay(path: &Path) -> anyhow::Result<(Vec<BlockRecord>, u64)> {
        let mut file = File::open(path)?;
        let mut records: Vec<BlockRecord> = Vec::new();
        let mut valid_len: u64 = 0;

        loop {
            let mut magic = [0u8; 4];
            match fill(&mut file, &mut magic) {
                Chunk::Ok => {}
                Chunk::CleanEof => break,
                Chunk::Torn => return Ok((records, valid_len)),
            }
            if magic != MAGIC {
                return Ok((records, valid_len));
            }

            let mut len_buf = [0u8; 4];
            if !matches!(fill(&mut file, &mut len_buf), Chunk::Ok) {
                return Ok((records, valid_len));
            }
            let payload_len = u32::from_le_bytes(len_buf);
            if payload_len == 0 || payload_len > MAX_FRAME_PAYLOAD {
                return Ok((records, valid_len));
            }

            let mut payload = vec![0u8; payload_len as usize];
            if !matches!(fill(&mut file, &mut payload), Chunk::Ok) {
                return Ok((records, valid_len));
            }

            let mut expected_sum = [0u8; HASH_LEN];
            if !matches!(fill(&mut file, &mut expected_sum), Chunk::Ok) {
                return Ok((records, valid_len));
            }

            use sha2::{Digest, Sha256};
            let actual_sum: [u8; 32] = Sha256::digest(&payload).into();
            if actual_sum != expected_sum {
                tracing::warn!("ledger: checksum mismatch at offset {}", valid_len);
                return Ok((records, valid_len));
            }

            let record: BlockRecord = match serde_json::from_slice::<BlockRecord>(&payload) {
                Ok(record) => record,
                Err(_) => {
                    tracing::warn!("ledger: undecodable frame at offset {}", valid_len);
                    return Ok((records, valid_len));
                }
            };

            let expected_parent = records
                .last()
                .map(|r| r.block_hash)
                .unwrap_or(GENESIS_PARENT);
            if record.parent_hash != expected_parent || !record.verify_hash() {
                tracing::warn!("ledger: broken hash chain at height {}", record.height);
                return Ok((records, valid_len));
            }

            let frame_len = HEADER_LEN + payload.len() + HASH_LEN;
            valid_len += frame_len as u64;
            records.push(record);
        }

        Ok((records, valid_len))
    }

    /// Append the next block to the ledger.
    ///
    /// Parent linkage and hashes are derived by the store, so appending in
    /// order always produces a valid chain. Returns the persisted record.
    pub fn append(
        &mut self,
        slot: u64,
        height: u64,
        entry_count: u64,
        tx_count: u64,
        poh_hash: [u8; 32],
    ) -> anyhow::Result<BlockRecord> {
        let parent_hash = self
            .records
            .last()
            .map(|r| r.block_hash)
            .unwrap_or(GENESIS_PARENT);
        let record = BlockRecord::new(parent_hash, slot, height, poh_hash, entry_count, tx_count);
        self.append_record(record)?;
        Ok(self.records.last().expect("just appended").clone())
    }

    /// Append a caller-built record after validating parent linkage and hash.
    pub fn append_record(&mut self, record: BlockRecord) -> anyhow::Result<()> {
        let expected_parent = self
            .records
            .last()
            .map(|r| r.block_hash)
            .unwrap_or(GENESIS_PARENT);
        anyhow::ensure!(
            record.parent_hash == expected_parent,
            "record parent mismatch: got {}, expected {}",
            hex16(&record.parent_hash),
            hex16(&expected_parent)
        );
        anyhow::ensure!(record.verify_hash(), "record block_hash does not verify");

        use sha2::{Digest, Sha256};
        let payload = serde_json::to_vec(&record)
            .map_err(|err| anyhow::anyhow!("failed to encode block record: {}", err))?;
        let sum = Sha256::digest(&payload);

        let mut frame = Vec::with_capacity(HEADER_LEN + payload.len() + HASH_LEN);
        frame.extend_from_slice(&MAGIC);
        frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        frame.extend_from_slice(&payload);
        frame.extend_from_slice(&sum);

        self.file.write_all(&frame)?;
        self.file.sync_data()?;
        self.records.push(record);
        Ok(())
    }

    /// All validated records, oldest first.
    pub fn records(&self) -> &[BlockRecord] {
        &self.records
    }

    /// The most recent record, if the ledger is non-empty.
    pub fn last(&self) -> Option<&BlockRecord> {
        self.records.last()
    }

    /// Number of blocks in the ledger.
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// True when no blocks have been persisted yet.
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Directory backing this ledger.
    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

fn hex16(bytes: &[u8; 32]) -> String {
    bytes[..8].iter().map(|b| format!("{:02x}", b)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct TempLedger(PathBuf);

    impl TempLedger {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "trustnode-blockstore-{}-{}",
                tag,
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            Self(dir)
        }

        fn open(&self) -> Blockstore {
            Blockstore::open(&self.0).expect("open ledger")
        }
    }

    impl Drop for TempLedger {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn open_creates_empty_ledger() {
        let tmp = TempLedger::new("empty");
        let store = tmp.open();
        assert!(store.is_empty());
        assert_eq!(store.len(), 0);
        assert!(store.last().is_none());
        assert!(tmp.0.join("blocks.log").exists());
    }

    #[test]
    fn append_then_replay_roundtrip() {
        let tmp = TempLedger::new("roundtrip");
        {
            let mut store = tmp.open();
            let a = store.append(0, 0, 1, 0, [1; 32]).unwrap();
            let b = store.append(1, 1, 2, 1, [2; 32]).unwrap();
            let c = store.append(2, 2, 1, 3, [3; 32]).unwrap();
            assert_eq!(b.parent_hash, a.block_hash);
            assert_eq!(c.parent_hash, b.block_hash);
            assert_eq!(store.len(), 3);
        }

        let store = tmp.open();
        assert_eq!(store.len(), 3);
        assert_eq!(store.records()[0].slot, 0);
        assert_eq!(store.records()[2].poh_hash, [3; 32]);
        assert!(store.records().iter().all(|r| r.verify_hash()));
    }

    #[test]
    fn replay_continues_chain_after_restart() {
        let tmp = TempLedger::new("restart");
        {
            let mut store = tmp.open();
            store.append(0, 0, 1, 0, [1; 32]).unwrap();
            store.append(1, 1, 1, 0, [2; 32]).unwrap();
        }
        {
            let mut store = tmp.open();
            let parent = store.last().unwrap().block_hash;
            let next = store.append(2, 2, 1, 0, [3; 32]).unwrap();
            assert_eq!(next.parent_hash, parent);
            assert_eq!(store.len(), 3);
        }

        let store = tmp.open();
        assert_eq!(store.len(), 3);
        let heights: Vec<u64> = store.records().iter().map(|r| r.height).collect();
        assert_eq!(heights, vec![0, 1, 2]);
    }

    #[test]
    fn append_record_rejects_wrong_parent() {
        let tmp = TempLedger::new("bad-parent");
        let mut store = tmp.open();
        store.append(0, 0, 1, 0, [1; 32]).unwrap();

        let rogue = BlockRecord::new([9; 32], 5, 5, [5; 32], 1, 0);
        let err = store.append_record(rogue).unwrap_err();
        assert!(err.to_string().contains("parent mismatch"));
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn append_record_rejects_tampered_hash() {
        let tmp = TempLedger::new("tampered");
        let mut store = tmp.open();

        let mut rogue = BlockRecord::new(GENESIS_PARENT, 0, 0, [1; 32], 1, 0);
        rogue.tx_count = 99;
        let err = store.append_record(rogue).unwrap_err();
        assert!(err.to_string().contains("does not verify"));
        assert!(store.is_empty());
    }

    fn raw_append(dir: &Path, bytes: &[u8]) {
        use std::io::Write;
        let mut file = OpenOptions::new()
            .append(true)
            .create(true)
            .open(dir.join("blocks.log"))
            .unwrap();
        file.write_all(bytes).unwrap();
    }

    fn wellformed_frame(record: &BlockRecord) -> Vec<u8> {
        use sha2::{Digest, Sha256};
        let payload = serde_json::to_vec(record).unwrap();
        let mut frame = Vec::new();
        frame.extend_from_slice(&MAGIC);
        frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        frame.extend_from_slice(&payload);
        frame.extend_from_slice(&Sha256::digest(&payload));
        frame
    }

    #[test]
    fn torn_tail_is_truncated() {
        let tmp = TempLedger::new("torn");
        let clean;
        {
            let mut store = tmp.open();
            store.append(0, 0, 1, 0, [1; 32]).unwrap();
            clean = std::fs::metadata(tmp.0.join("blocks.log")).unwrap().len();
        }
        // half of a frame header
        raw_append(&tmp.0, b"TNBL\x10\x00");

        let store = tmp.open();
        assert_eq!(store.len(), 1);
        let on_disk = std::fs::metadata(tmp.0.join("blocks.log")).unwrap().len();
        assert_eq!(on_disk, clean);
    }

    #[test]
    fn garbage_magic_stops_replay() {
        let tmp = TempLedger::new("garbage");
        let clean;
        {
            let mut store = tmp.open();
            store.append(0, 0, 1, 0, [1; 32]).unwrap();
            clean = std::fs::metadata(tmp.0.join("blocks.log")).unwrap().len();
        }
        raw_append(&tmp.0, b"XXXX-this-is-not-a-frame-at-all");

        let store = tmp.open();
        assert_eq!(store.len(), 1);
        let on_disk = std::fs::metadata(tmp.0.join("blocks.log")).unwrap().len();
        assert_eq!(on_disk, clean);
    }

    #[test]
    fn bad_checksum_stops_replay() {
        let tmp = TempLedger::new("badsum");
        let first;
        {
            let mut store = tmp.open();
            first = store.append(0, 0, 1, 0, [1; 32]).unwrap();
        }
        let mut frame = wellformed_frame(&BlockRecord::new(first.block_hash, 1, 1, [2; 32], 1, 0));
        let n = frame.len();
        frame[n - 1] ^= 0xff; // flip a checksum byte
        raw_append(&tmp.0, &frame);

        let store = tmp.open();
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn broken_chain_stops_replay() {
        let tmp = TempLedger::new("chain");
        let clean;
        {
            let mut store = tmp.open();
            store.append(0, 0, 1, 0, [1; 32]).unwrap();
            clean = std::fs::metadata(tmp.0.join("blocks.log")).unwrap().len();
        }
        // well-formed frame pointing at the wrong parent
        let orphan = BlockRecord::new([7; 32], 1, 1, [2; 32], 1, 0);
        raw_append(&tmp.0, &wellformed_frame(&orphan));

        let store = tmp.open();
        assert_eq!(store.len(), 1);
        let on_disk = std::fs::metadata(tmp.0.join("blocks.log")).unwrap().len();
        assert_eq!(on_disk, clean);
    }

    #[test]
    fn tampered_payload_stops_replay() {
        let tmp = TempLedger::new("tamper-frame");
        {
            let mut store = tmp.open();
            store.append(0, 0, 1, 0, [1; 32]).unwrap();
        }
        // valid checksum, but record contents edited after hashing
        let mut rogue = BlockRecord::new(
            BlockRecord::new(GENESIS_PARENT, 0, 0, [1; 32], 1, 0).block_hash,
            1,
            1,
            [2; 32],
            1,
            0,
        );
        rogue.slot = 42; // invalidates block_hash while frame checksum stays valid
        raw_append(&tmp.0, &wellformed_frame(&rogue));

        let store = tmp.open();
        assert_eq!(store.len(), 1);
        assert_eq!(store.last().unwrap().slot, 0);
    }
}
