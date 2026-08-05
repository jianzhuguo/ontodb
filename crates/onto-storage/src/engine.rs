//! LSM Engine: The main storage engine that orchestrates WAL, MemTable, and SSTables.
//!
//! Write path:  WAL → MemTable → (when full) flush to SSTable
//! Read path:   MemTable → SSTables (newest to oldest)
//! Delete:      Write tombstone entry

use crate::lsm::memtable::MemTable;
use crate::lsm::sstable::{SsTable, SsTableBuilder};
use crate::lsm::wal::{self, Wal};
use crate::options::StorageOptions;
use onto_core::{Entry, EntryKind, Key, Result, SeqNo, Value};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

/// The main LSM-Tree storage engine.
pub struct LsmEngine {
    /// Active MemTable for writes.
    memtable: MemTable,

    /// Read-only MemTable waiting to be flushed.
    immutable_memtable: Option<MemTable>,

    /// SSTables organized by level. Level 0 is newest.
    levels: Vec<Vec<SsTableInfo>>,

    /// Write-Ahead Log for durability.
    wal: Wal,

    /// Engine configuration.
    options: StorageOptions,

    /// Sequence number generator.
    seq_counter: AtomicU64,

    /// SSTable file ID counter.
    sst_counter: AtomicU64,
}

/// Metadata about an SSTable file, kept in memory.
#[derive(Clone)]
struct SsTableInfo {
    /// File path.
    path: PathBuf,
    /// Approximate size in bytes.
    size: u64,
    /// Min key in this SSTable.
    min_key: Vec<u8>,
    /// Max key in this SSTable.
    max_key: Vec<u8>,
}

impl LsmEngine {
    /// Opens or creates an LSM engine at the given directory.
    pub fn open(options: StorageOptions) -> Result<Self> {
        fs::create_dir_all(&options.data_dir)?;

        let wal_path = options.data_dir.join("wal.log");
        let wal = Wal::open(&wal_path)?;

        let mut engine = Self {
            memtable: MemTable::new(),
            immutable_memtable: None,
            levels: vec![Vec::new(); options.num_levels],
            wal,
            options,
            seq_counter: AtomicU64::new(0),
            sst_counter: AtomicU64::new(0),
        };

        // Recover from WAL
        engine.recover()?;

        // Load existing SSTables
        engine.load_sstables()?;

        Ok(engine)
    }

    /// Puts a key-value pair.
    pub fn put(&mut self, key: Key, value: Value) -> Result<()> {
        let entry = Entry::put(key, value, self.next_seq());

        // Write to WAL first (durability)
        self.wal.append(&entry)?;

        // Write to MemTable
        self.memtable.put(entry.key.clone(), entry.value.clone());

        // Check if we need to flush
        if self.memtable.size() >= self.options.memtable_size_limit {
            self.flush_memtable()?;
        }

        Ok(())
    }

    /// Gets a value by key.
    pub fn get(&mut self, key: &[u8]) -> Result<Option<Value>> {
        // 1. Check active MemTable
        if let Some((val, _)) = self.memtable.get(key) {
            return Ok(Some(val.to_vec()));
        }

        // 2. Check immutable MemTable
        if let Some(ref imm) = self.immutable_memtable {
            if let Some((val, _)) = imm.get(key) {
                return Ok(Some(val.to_vec()));
            }
        }

        // 3. Check SSTables (newest to oldest)
        for level in &self.levels {
            for sst_info in level.iter().rev() {
                // Quick range check
                if key < sst_info.min_key.as_slice() || key > sst_info.max_key.as_slice() {
                    continue;
                }

                let mut sst = SsTable::open(&sst_info.path)?;
                if let Some((value, _)) = sst.get(key)? {
                    return Ok(Some(value));
                }
            }
        }

        Ok(None)
    }

    /// Deletes a key (writes a tombstone).
    pub fn delete(&mut self, key: Key) -> Result<()> {
        let entry = Entry::delete(key, self.next_seq());

        self.wal.append(&entry)?;
        self.memtable.delete(entry.key);

        if self.memtable.size() >= self.options.memtable_size_limit {
            self.flush_memtable()?;
        }

        Ok(())
    }

    /// Flushes the current MemTable to an SSTable on disk.
    fn flush_memtable(&mut self) -> Result<()> {
        // Swap current MemTable to immutable
        let old_mem = std::mem::replace(&mut self.memtable, MemTable::new());
        self.immutable_memtable = Some(old_mem);

        // Build SSTable from immutable MemTable
        if let Some(ref imm) = self.immutable_memtable {
            let sst_id = self.sst_counter.fetch_add(1, Ordering::Relaxed);
            let sst_path = self.options.data_dir.join(format!("L0_{}.sst", sst_id));

            let mut builder = SsTableBuilder::new();
            for entry in imm.entries() {
                builder.add(&Entry {
                    key: entry.key.clone(),
                    value: entry.value.clone(),
                    seq_no: entry.seq_no,
                    kind: entry.kind,
                });
            }

            builder.build(&sst_path)?;

            // Record SSTable metadata
            let min_key = imm
                .entries()
                .next()
                .map(|e| e.key.clone())
                .unwrap_or_default();
            let max_key = imm
                .entries()
                .last()
                .map(|e| e.key.clone())
                .unwrap_or_default();

            let metadata = fs::metadata(&sst_path)?;

            self.levels[0].push(SsTableInfo {
                path: sst_path,
                size: metadata.len(),
                min_key,
                max_key,
            });
        }

        // Clear immutable MemTable
        self.immutable_memtable = None;

        // Reset WAL (we've persisted everything to SSTable)
        self.reset_wal()?;

        // Trigger compaction if needed
        self.maybe_compact(0)?;

        Ok(())
    }

    /// Recovers MemTable state from WAL on startup.
    fn recover(&mut self) -> Result<()> {
        let wal_path = self.options.data_dir.join("wal.log");
        if !wal_path.exists() {
            return Ok(());
        }

        let entries = wal::replay_wal(&wal_path)?;
        let mut max_seq = 0u64;

        for entry in entries {
            match entry.kind {
                EntryKind::Put => {
                    self.memtable.put(entry.key, entry.value);
                }
                EntryKind::Delete => {
                    self.memtable.delete(entry.key);
                }
            }
            max_seq = max_seq.max(entry.seq_no);
        }

        self.seq_counter = AtomicU64::new(max_seq + 1);

        Ok(())
    }

    /// Loads existing SSTable files from the data directory.
    fn load_sstables(&mut self) -> Result<()> {
        let entries = fs::read_dir(&self.options.data_dir)?;

        let mut sst_files: Vec<PathBuf> = Vec::new();
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if path.extension().map_or(false, |ext| ext == "sst") {
                sst_files.push(path);
            }
        }

        // Sort by filename (which includes level and ID)
        sst_files.sort();

        for path in sst_files {
            let fname = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("");

            // Parse level from filename like "L0_123.sst"
            let level = if fname.starts_with('L') {
                fname[1..2].parse::<usize>().unwrap_or(0)
            } else {
                0
            };

            if level >= self.levels.len() {
                continue;
            }

            let _sst = SsTable::open(&path)?;
            // TODO: extract min/max key from SSTable index
            let metadata = fs::metadata(&path)?;

            self.levels[level].push(SsTableInfo {
                path,
                size: metadata.len(),
                min_key: Vec::new(), // TODO: populate from SST
                max_key: Vec::new(), // TODO: populate from SST
            });
        }

        Ok(())
    }

    /// Simple leveled compaction: if level N is too big, merge into level N+1.
    fn maybe_compact(&mut self, level: usize) -> Result<()> {
        if level >= self.levels.len() - 1 {
            return Ok(());
        }

        let max_ssts = self.options.size_ratio;
        if self.levels[level].len() <= max_ssts {
            return Ok(());
        }

        // TODO: Implement actual compaction logic
        // For now, just log that compaction is needed
        tracing::info!(
            "Level {} has {} SSTables (max {}), compaction needed",
            level,
            self.levels[level].len(),
            max_ssts
        );

        Ok(())
    }

    /// Resets the WAL file after a successful flush.
    fn reset_wal(&mut self) -> Result<()> {
        let wal_path = self.options.data_dir.join("wal.log");
        fs::remove_file(&wal_path)?;
        self.wal = Wal::open(&wal_path)?;
        Ok(())
    }

    fn next_seq(&self) -> SeqNo {
        self.seq_counter.fetch_add(1, Ordering::Relaxed)
    }

    /// Returns engine statistics.
    pub fn stats(&self) -> EngineStats {
        let total_sstables: usize = self.levels.iter().map(|l| l.len()).sum();
        let total_sst_size: u64 = self
            .levels
            .iter()
            .flat_map(|l| l.iter())
            .map(|s| s.size)
            .sum();

        EngineStats {
            memtable_size: self.memtable.size(),
            memtable_entries: self.memtable.len(),
            num_levels: self.levels.len(),
            total_sstables,
            total_sst_size,
        }
    }
}

/// Engine statistics.
#[derive(Debug)]
pub struct EngineStats {
    pub memtable_size: usize,
    pub memtable_entries: usize,
    pub num_levels: usize,
    pub total_sstables: usize,
    pub total_sst_size: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_engine_basic_put_get() {
        let dir = tempdir().unwrap();
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            memtable_size_limit: 1024 * 1024, // 1MB
            ..Default::default()
        };

        let mut engine = LsmEngine::open(options).unwrap();

        engine
            .put(b"name".to_vec(), b"alice".to_vec())
            .unwrap();
        engine.put(b"age".to_vec(), b"30".to_vec()).unwrap();

        let val = engine.get(b"name").unwrap();
        assert_eq!(val, Some(b"alice".to_vec()));

        let val = engine.get(b"age").unwrap();
        assert_eq!(val, Some(b"30".to_vec()));

        let val = engine.get(b"missing").unwrap();
        assert_eq!(val, None);
    }

    #[test]
    fn test_engine_overwrite() {
        let dir = tempdir().unwrap();
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            ..Default::default()
        };

        let mut engine = LsmEngine::open(options).unwrap();

        engine.put(b"key".to_vec(), b"v1".to_vec()).unwrap();
        engine.put(b"key".to_vec(), b"v2".to_vec()).unwrap();

        let val = engine.get(b"key").unwrap();
        assert_eq!(val, Some(b"v2".to_vec()));
    }

    #[test]
    fn test_engine_delete() {
        let dir = tempdir().unwrap();
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            ..Default::default()
        };

        let mut engine = LsmEngine::open(options).unwrap();

        engine.put(b"key".to_vec(), b"value".to_vec()).unwrap();
        assert!(engine.get(b"key").unwrap().is_some());

        engine.delete(b"key".to_vec()).unwrap();
        assert!(engine.get(b"key").unwrap().is_none());
    }

    #[test]
    fn test_engine_flush_to_sstable() {
        let dir = tempdir().unwrap();
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            memtable_size_limit: 128, // Very small to trigger flush
            ..Default::default()
        };

        let mut engine = LsmEngine::open(options).unwrap();

        // Write enough data to trigger a flush
        for i in 0..20u32 {
            let key = format!("key_{:04}", i);
            let value = format!("value_{}", i);
            engine.put(key.into_bytes(), value.into_bytes()).unwrap();
        }

        // All data should still be readable
        let val = engine.get(b"key_0005").unwrap();
        assert_eq!(val, Some(b"value_5".to_vec()));

        let stats = engine.stats();
        assert!(stats.total_sstables > 0, "should have flushed at least one SSTable");
    }

    #[test]
    fn test_engine_recovery() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path().to_path_buf();

        // Write data
        {
            let options = StorageOptions {
                data_dir: data_dir.clone(),
                ..Default::default()
            };
            let mut engine = LsmEngine::open(options).unwrap();
            engine
                .put(b"key1".to_vec(), b"value1".to_vec())
                .unwrap();
            engine
                .put(b"key2".to_vec(), b"value2".to_vec())
                .unwrap();
            // Don't drop cleanly - simulate crash (WAL should persist)
        }

        // Recover
        {
            let options = StorageOptions {
                data_dir,
                ..Default::default()
            };
            let mut engine = LsmEngine::open(options).unwrap();

            let val = engine.get(b"key1").unwrap();
            assert_eq!(val, Some(b"value1".to_vec()));

            let val = engine.get(b"key2").unwrap();
            assert_eq!(val, Some(b"value2".to_vec()));
        }
    }
}
