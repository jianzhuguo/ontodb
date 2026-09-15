// Copyright (c) 2024-2026 OntoDB Team
// Licensed under the Business Source License 1.1 (BUSL-1.1).
// See LICENSE for details. Change Date: 2031-09-15.
// On the Change Date, this file will be licensed under Apache License 2.0.
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
    /// Reusable buffer for entry serialization to avoid per-write allocation.
    serialize_buf: Vec<u8>,
    /// Track if there are unsynced writes.
    dirty: bool,
    /// Maximum WAL file size before rotation (default: 256 MB).
    max_size: u64,
    /// Counter for rotated WAL files.
    rotation_count: u32,
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
            serialize_buf: Vec::with_capacity(256),
            dirty: false,
            max_size: 256 * 1024 * 1024, // 256 MB default
            rotation_count: 0,
        })
    }

    /// Sets the maximum WAL file size before rotation.
    pub fn with_max_size(mut self, max_size: u64) -> Self {
        self.max_size = max_size;
        self
    }

    /// Appends an entry to the WAL.
    ///
    /// Writes to the BufWriter but does NOT flush. Call `flush_buf()` or
    /// `sync()` after a batch of appends to push data to the OS file cache.
    /// Returns the byte offset where the entry was written.
    pub fn append(&mut self, entry: &Entry) -> Result<u64> {
        // Reuse serialize buffer to avoid per-write allocation
        self.serialize_buf.clear();
        Self::serialize_entry_into(entry, &mut self.serialize_buf);
        let crc = crc32fast::hash(&self.serialize_buf);
        let len = self.serialize_buf.len() as u32;

        let offset = self.offset;

        // Write: [length][crc32][payload]
        self.writer.write_all(&len.to_le_bytes())?;
        self.writer.write_all(&crc.to_le_bytes())?;
        self.writer.write_all(&self.serialize_buf)?;

        self.offset += 4 + 4 + self.serialize_buf.len() as u64;
        self.dirty = true;

        Ok(offset)
    }

    /// Appends multiple entries to the WAL with a single flush.
    /// More efficient than calling `append()` in a loop (one flush instead of N).
    pub fn append_batch(&mut self, entries: &[Entry]) -> Result<()> {
        for entry in entries {
            let payload = Self::serialize_entry(entry);
            let crc = crc32fast::hash(&payload);
            let len = payload.len() as u32;

            self.writer.write_all(&len.to_le_bytes())?;
            self.writer.write_all(&crc.to_le_bytes())?;
            self.writer.write_all(&payload)?;

            self.offset += 4 + 4 + payload.len() as u64;
        }
        // Single flush for the entire batch
        self.writer.flush()?;
        Ok(())
    }

    /// Flushes the BufWriter to the OS file cache (not fsync).
    /// Call after a batch of `append()` calls to push buffered data out.
    pub fn flush_buf(&mut self) -> Result<()> {
        self.writer.flush()?;
        Ok(())
    }

    /// Flushes and checks if rotation is needed.
    /// Returns true if the WAL file should be rotated.
    pub fn flush_and_check_rotation(&mut self) -> Result<bool> {
        self.flush_buf()?;
        Ok(self.needs_rotation())
    }

    /// Flushes buffered writes to disk (fsync).
    pub fn sync(&mut self) -> Result<()> {
        self.writer.flush()?;
        self.writer.get_ref().sync_all()?;
        self.dirty = false;
        Ok(())
    }

    /// Sync only if there are dirty writes. Avoids redundant fsync calls.
    pub fn sync_if_dirty(&mut self) -> Result<()> {
        if self.dirty {
            self.sync()?;
        }
        Ok(())
    }

    /// Returns true if the WAL file exceeds the maximum size.
    pub fn needs_rotation(&self) -> bool {
        self.offset >= self.max_size
    }

    /// Rotates the WAL file: syncs current file, renames it, creates a new one.
    /// Returns the path to the archived WAL file.
    pub fn rotate(&mut self) -> Result<PathBuf> {
        // Sync current file
        self.sync()?;

        // Generate archive filename
        self.rotation_count += 1;
        let archive_path = self
            ._path
            .with_extension(format!("{}.wal", self.rotation_count));

        // Rename current file to archive
        std::fs::rename(&self._path, &archive_path)?;

        // Create new WAL file
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self._path)?;

        self.writer = BufWriter::new(file);
        self.offset = 0;
        self.dirty = false;

        Ok(archive_path)
    }

    /// Returns the current WAL file size in bytes.
    pub fn size(&self) -> u64 {
        self.offset
    }

    /// Serializes an entry to bytes for WAL storage.
    fn serialize_entry(entry: &Entry) -> Vec<u8> {
        let mut buf = Vec::new();
        Self::serialize_entry_into(entry, &mut buf);
        buf
    }

    /// Appends a put entry directly from key/value references (zero-clone path).
    /// Avoids constructing an Entry and cloning key/value.
    pub fn append_raw_put(&mut self, key: &[u8], value: &[u8], seq_no: u64) -> Result<u64> {
        self.serialize_buf.clear();
        // seq_no (8 bytes)
        self.serialize_buf.extend_from_slice(&seq_no.to_le_bytes());
        // kind = Put (1 byte)
        self.serialize_buf.push(0);
        // key length + key
        self.serialize_buf
            .extend_from_slice(&(key.len() as u32).to_le_bytes());
        self.serialize_buf.extend_from_slice(key);
        // value length + value
        self.serialize_buf
            .extend_from_slice(&(value.len() as u32).to_le_bytes());
        self.serialize_buf.extend_from_slice(value);

        let crc = crc32fast::hash(&self.serialize_buf);
        let len = self.serialize_buf.len() as u32;
        let offset = self.offset;

        self.writer.write_all(&len.to_le_bytes())?;
        self.writer.write_all(&crc.to_le_bytes())?;
        self.writer.write_all(&self.serialize_buf)?;

        self.offset += 4 + 4 + self.serialize_buf.len() as u64;
        self.dirty = true;

        Ok(offset)
    }

    /// Serializes an entry into an existing buffer (zero-allocation path).
    fn serialize_entry_into(entry: &Entry, buf: &mut Vec<u8>) {
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
    }

    /// Deserializes an entry from WAL bytes.
    fn deserialize_entry(data: &[u8]) -> Result<Entry> {
        if data.len() < 17 {
            // Minimum: 8 (seq) + 1 (kind) + 4 (key_len) + 0 (key) + 4 (val_len)
            return Err(CoreError::corruption("WAL entry too short"));
        }

        let seq_no = u64::from_le_bytes(data[0..8].try_into().expect("should be valid"));
        let kind = match data[8] {
            0 => EntryKind::Put,
            1 => EntryKind::Delete,
            _ => return Err(CoreError::corruption("invalid entry kind")),
        };

        let key_len = u32::from_le_bytes(data[9..13].try_into().expect("should be valid")) as usize;
        if data.len() < 17 + key_len {
            return Err(CoreError::corruption("WAL entry key truncated"));
        }
        let key = data[13..13 + key_len].to_vec();

        let val_offset = 13 + key_len;
        if data.len() < val_offset + 4 {
            return Err(CoreError::corruption("WAL entry value length truncated"));
        }
        let val_len = u32::from_le_bytes(
            data[val_offset..val_offset + 4]
                .try_into()
                .expect("should be valid"),
        ) as usize;
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
///
/// Stops parsing at the first CRC mismatch or malformed entry.
/// This is critical because if a length field is corrupted, all subsequent
/// entries would be misinterpreted — continuing would produce garbage data.
/// Partial writes at the end of the file (from a crash during append) are
/// detected by CRC mismatch and safely ignored.
pub fn replay_wal(path: impl AsRef<Path>) -> Result<Vec<Entry>> {
    let mut file = File::open(path.as_ref())?;
    let mut entries = Vec::new();
    let mut buf = Vec::new();

    file.read_to_end(&mut buf)?;

    let mut pos = 0;
    while pos + 8 <= buf.len() {
        // Read length
        let len =
            u32::from_le_bytes(buf[pos..pos + 4].try_into().expect("should be valid")) as usize;
        pos += 4;

        // Sanity check: length shouldn't be unreasonably large (max 100MB per entry)
        if len > 100 * 1024 * 1024 {
            tracing::warn!(
                "WAL: unreasonable entry length {} at offset {}, stopping",
                len,
                pos - 4
            );
            break;
        }

        // Read CRC
        if pos + 4 > buf.len() {
            break; // Truncated CRC at end of file
        }
        let expected_crc =
            u32::from_le_bytes(buf[pos..pos + 4].try_into().expect("should be valid"));
        pos += 4;

        // Read payload
        if pos + len > buf.len() {
            tracing::warn!(
                "WAL: truncated payload at offset {} (need {} bytes, have {}), stopping",
                pos,
                len,
                buf.len() - pos
            );
            break; // Truncated payload at end of file (partial write)
        }
        let payload = &buf[pos..pos + len];
        pos += len;

        // Verify CRC — stop on mismatch (corrupted length would cascade errors)
        let actual_crc = crc32fast::hash(payload);
        if expected_crc != actual_crc {
            tracing::warn!(
                "WAL: CRC mismatch at offset {}, stopping replay",
                pos - len - 8
            );
            break;
        }

        match Wal::deserialize_entry(payload) {
            Ok(entry) => entries.push(entry),
            Err(e) => {
                tracing::warn!(
                    "WAL: malformed entry at offset {}: {:?}, stopping",
                    pos - len,
                    e
                );
                break;
            }
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
        let dir = tempdir().expect("should be valid");
        let wal_path = dir.path().join("test.wal");

        let mut wal = Wal::open(&wal_path).expect("should be valid");

        let e1 = Entry::put(b"key1".to_vec(), b"value1".to_vec(), 1);
        let e2 = Entry::put(b"key2".to_vec(), b"value2".to_vec(), 2);
        let e3 = Entry::delete(b"key1".to_vec(), 3);

        wal.append(&e1).expect("should be valid");
        wal.append(&e2).expect("should be valid");
        wal.append(&e3).expect("should be valid");
        wal.sync().expect("should be valid");

        // Replay
        let entries = replay_wal(&wal_path).expect("should be valid");
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0], e1);
        assert_eq!(entries[1], e2);
        assert_eq!(entries[2], e3);
    }

    #[test]
    fn test_wal_serde_roundtrip() {
        let entry = Entry::put(b"hello".to_vec(), b"world".to_vec(), 42);
        let payload = Wal::serialize_entry(&entry);
        let recovered = Wal::deserialize_entry(&payload).expect("should be valid");
        assert_eq!(entry, recovered);
    }

    #[test]
    fn test_wal_corruption_stops_replay() {
        // Write 3 valid entries, then corrupt the 4th
        let dir = tempdir().expect("should be valid");
        let wal_path = dir.path().join("corrupt.wal");

        let mut wal = Wal::open(&wal_path).expect("should be valid");

        let e1 = Entry::put(b"key1".to_vec(), b"v1".to_vec(), 1);
        let e2 = Entry::put(b"key2".to_vec(), b"v2".to_vec(), 2);
        let e3 = Entry::put(b"key3".to_vec(), b"v3".to_vec(), 3);

        wal.append(&e1).expect("should be valid");
        wal.append(&e2).expect("should be valid");
        wal.append(&e3).expect("should be valid");
        wal.sync().expect("should be valid");

        // Append a corrupted entry (bad CRC)
        use std::io::Write;
        let bad_payload = b"garbage_data";
        let bad_len = bad_payload.len() as u32;
        let bad_crc = 0xDEADBEEFu32; // Wrong CRC
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&wal_path)
            .expect("should be valid");
        file.write_all(&bad_len.to_le_bytes())
            .expect("should be valid");
        file.write_all(&bad_crc.to_le_bytes())
            .expect("should be valid");
        file.write_all(bad_payload).expect("should be valid");
        file.sync_all().expect("should be valid");

        // Replay should return only the 3 valid entries, stopping at corruption
        let entries = replay_wal(&wal_path).expect("should be valid");
        assert_eq!(entries.len(), 3, "should stop at corrupted entry");
        assert_eq!(entries[0], e1);
        assert_eq!(entries[1], e2);
        assert_eq!(entries[2], e3);
    }

    #[test]
    fn test_wal_truncated_payload_stops_replay() {
        let dir = tempdir().expect("should be valid");
        let wal_path = dir.path().join("truncated.wal");

        let mut wal = Wal::open(&wal_path).expect("should be valid");

        let e1 = Entry::put(b"key1".to_vec(), b"v1".to_vec(), 1);
        let e2 = Entry::put(b"key2".to_vec(), b"v2".to_vec(), 2);
        wal.append(&e1).expect("should be valid");
        wal.append(&e2).expect("should be valid");
        wal.sync().expect("should be valid");

        // Truncate the file mid-way through the second entry's payload
        // Entry format: [len:4][crc:4][payload:N]. We want to keep all of
        // entry 1, plus the len+crc of entry 2, but cut into entry 2's payload.
        let payload2 = Wal::serialize_entry(&e2);
        let entry2_total = 4 + 4 + payload2.len(); // len + crc + payload
        let metadata = std::fs::metadata(&wal_path).expect("should be valid");
        // Keep entry 1 fully + 10 bytes of entry 2 (enough for len+crc but not full payload)
        let truncate_to = metadata.len() - (entry2_total as u64) + 10;
        std::fs::OpenOptions::new()
            .write(true)
            .open(&wal_path)
            .expect("should be valid")
            .set_len(truncate_to)
            .expect("should be valid");

        // Should recover entry 1, stop at truncated entry 2
        let entries = replay_wal(&wal_path).expect("should be valid");
        assert_eq!(entries.len(), 1, "should stop at truncated entry");
        assert_eq!(entries[0], e1);
    }
}
