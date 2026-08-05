//! Write-Ahead Log (WAL) for crash recovery.
//!
//! Format: [length: u32][crc32: u32][payload: bytes]
//!
//! All writes are appended sequentially. On recovery, we replay the log
//! to reconstruct the MemTable state.

use onto_core::{CoreError, Entry, EntryKind, Result};
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};

/// Write-Ahead Log for durability.
pub struct Wal {
    _path: PathBuf,
    writer: BufWriter<File>,
    offset: u64,
}

impl Wal {
    /// Opens or creates a WAL file at the given path.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path.as_ref())?;

        let offset = file.metadata()?.len();

        Ok(Self {
            _path: path.as_ref().to_path_buf(),
            writer: BufWriter::new(file),
            offset,
        })
    }

    /// Appends an entry to the WAL.
    ///
    /// Writes to the OS buffer (flush) but does NOT fsync for performance.
    /// Call `sync()` if you need durability guarantees beyond OS buffering.
    /// Returns the byte offset where the entry was written.
    pub fn append(&mut self, entry: &Entry) -> Result<u64> {
        let payload = Self::serialize_entry(entry);
        let crc = crc32fast::hash(&payload);
        let len = payload.len() as u32;

        let offset = self.offset;

        // Write: [length][crc32][payload]
        self.writer.write_all(&len.to_le_bytes())?;
        self.writer.write_all(&crc.to_le_bytes())?;
        self.writer.write_all(&payload)?;

        // Flush BufWriter to OS file cache (not fsync, just ensures data
        // leaves the process buffer). Survives process crash on most OSes.
        self.writer.flush()?;

        self.offset += 4 + 4 + payload.len() as u64;

        Ok(offset)
    }

    /// Flushes buffered writes to disk (fsync).
    pub fn sync(&mut self) -> Result<()> {
        self.writer.flush()?;
        self.writer.get_ref().sync_all()?;
        Ok(())
    }

    /// Serializes an entry to bytes for WAL storage.
    fn serialize_entry(entry: &Entry) -> Vec<u8> {
        let mut buf = Vec::new();

        // seq_no (8 bytes)
        buf.extend_from_slice(&entry.seq_no.to_le_bytes());

        // kind (1 byte): 0 = Put, 1 = Delete
        buf.push(match entry.kind {
            EntryKind::Put => 0,
            EntryKind::Delete => 1,
        });

        // key length (4 bytes) + key
        buf.extend_from_slice(&(entry.key.len() as u32).to_le_bytes());
        buf.extend_from_slice(&entry.key);

        // value length (4 bytes) + value
        buf.extend_from_slice(&(entry.value.len() as u32).to_le_bytes());
        buf.extend_from_slice(&entry.value);

        buf
    }

    /// Deserializes an entry from WAL bytes.
    fn deserialize_entry(data: &[u8]) -> Result<Entry> {
        if data.len() < 17 {
            // Minimum: 8 (seq) + 1 (kind) + 4 (key_len) + 0 (key) + 4 (val_len)
            return Err(CoreError::corruption("WAL entry too short"));
        }

        let seq_no = u64::from_le_bytes(data[0..8].try_into().unwrap());
        let kind = match data[8] {
            0 => EntryKind::Put,
            1 => EntryKind::Delete,
            _ => return Err(CoreError::corruption("invalid entry kind")),
        };

        let key_len = u32::from_le_bytes(data[9..13].try_into().unwrap()) as usize;
        if data.len() < 17 + key_len {
            return Err(CoreError::corruption("WAL entry key truncated"));
        }
        let key = data[13..13 + key_len].to_vec();

        let val_offset = 13 + key_len;
        if data.len() < val_offset + 4 {
            return Err(CoreError::corruption("WAL entry value length truncated"));
        }
        let val_len =
            u32::from_le_bytes(data[val_offset..val_offset + 4].try_into().unwrap()) as usize;
        if data.len() < val_offset + 4 + val_len {
            return Err(CoreError::corruption("WAL entry value truncated"));
        }
        let value = data[val_offset + 4..val_offset + 4 + val_len].to_vec();

        Ok(Entry {
            key,
            value,
            seq_no,
            kind,
        })
    }
}

/// Replays a WAL file, returning all entries in order.
/// Skips entries with CRC mismatches (partial writes at end of file).
pub fn replay_wal(path: impl AsRef<Path>) -> Result<Vec<Entry>> {
    let mut file = File::open(path.as_ref())?;
    let mut entries = Vec::new();
    let mut buf = Vec::new();

    file.read_to_end(&mut buf)?;

    let mut pos = 0;
    while pos + 8 <= buf.len() {
        // Read length
        let len = u32::from_le_bytes(buf[pos..pos + 4].try_into().unwrap()) as usize;
        pos += 4;

        // Read CRC
        if pos + 4 > buf.len() {
            break; // Truncated CRC at end of file
        }
        let expected_crc = u32::from_le_bytes(buf[pos..pos + 4].try_into().unwrap());
        pos += 4;

        // Read payload
        if pos + len > buf.len() {
            break; // Truncated payload at end of file (partial write)
        }
        let payload = &buf[pos..pos + len];
        pos += len;

        // Verify CRC
        let actual_crc = crc32fast::hash(payload);
        if expected_crc != actual_crc {
            // Skip corrupted entry but continue (could be partial write)
            continue;
        }

        match Wal::deserialize_entry(payload) {
            Ok(entry) => entries.push(entry),
            Err(_) => continue, // Skip malformed entries
        }
    }

    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_wal_append_and_replay() {
        let dir = tempdir().unwrap();
        let wal_path = dir.path().join("test.wal");

        let mut wal = Wal::open(&wal_path).unwrap();

        let e1 = Entry::put(b"key1".to_vec(), b"value1".to_vec(), 1);
        let e2 = Entry::put(b"key2".to_vec(), b"value2".to_vec(), 2);
        let e3 = Entry::delete(b"key1".to_vec(), 3);

        wal.append(&e1).unwrap();
        wal.append(&e2).unwrap();
        wal.append(&e3).unwrap();
        wal.sync().unwrap();

        // Replay
        let entries = replay_wal(&wal_path).unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0], e1);
        assert_eq!(entries[1], e2);
        assert_eq!(entries[2], e3);
    }

    #[test]
    fn test_wal_serde_roundtrip() {
        let entry = Entry::put(b"hello".to_vec(), b"world".to_vec(), 42);
        let payload = Wal::serialize_entry(&entry);
        let recovered = Wal::deserialize_entry(&payload).unwrap();
        assert_eq!(entry, recovered);
    }
}
