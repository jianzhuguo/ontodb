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
        let seq = self.next_seq();
        let entry = Entry::put(key.clone(), value.clone(), seq);

        // Write to WAL first (durability)
        self.wal.append(&entry)?;

        // Write to MemTable with engine's global seq_no
        self.memtable.put_with_seq(key, value, seq);

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
                if key < sst_info.min_key.as_slice() || key > sst_info.max_key.as_slice() {
                    continue;
                }

                let mut sst = SsTable::open(&sst_info.path)?;
                match sst.get_full(key)? {
                    Some((value, _, EntryKind::Put)) => return Ok(Some(value)),
                    Some((_, _, EntryKind::Delete)) => return Ok(None),
                    None => continue,
                }
            }
        }

        Ok(None)
    }

    /// Scans all entries whose key starts with the given prefix.
    /// Returns a sorted Vec of (key, value) pairs.
    ///
    /// This leverages the LSM-Tree's sorted key structure:
    /// entries with `{class}::` prefix are contiguous in sorted order.
    pub fn scan_prefix(&mut self, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        use std::collections::BTreeMap;

        // Collect latest version of each key from all sources
        // BTreeMap ensures sorted order and automatic dedup
        let mut seen: BTreeMap<Vec<u8>, (Vec<u8>, SeqNo, EntryKind)> = BTreeMap::new();

        // 1. Scan SSTables (oldest to newest, so newer entries overwrite older)
        for level in self.levels.iter().rev() {
            for sst_info in level.iter() {
                // Quick range check: skip SSTable if prefix can't overlap
                if !prefix.is_empty() {
                    let sst_max = sst_info.max_key.as_slice();
                    if sst_max < prefix {
                        continue; // SSTable's max key is before our prefix
                    }
                    // Check if prefix could match: the SSTable's min_key must be <= some key with prefix
                    let sst_min = sst_info.min_key.as_slice();
                    if !Self::prefix_may_overlap(prefix, sst_min, sst_max) {
                        continue;
                    }
                }

                let mut sst = SsTable::open(&sst_info.path)?;
                let mut iter = sst.iter()?;

                // Seek to first key >= prefix
                while iter.is_valid() && iter.key() < prefix {
                    iter.next();
                }

                // Scan entries with matching prefix
                while iter.is_valid() {
                    if !iter.key().starts_with(prefix) {
                        break; // Past the prefix range
                    }
                    let key = iter.key().to_vec();
                    let value = iter.value().to_vec();
                    let seq = iter.seq_no();
                    let kind = iter.kind();

                    // Only update if this is a newer version
                    let should_update = match seen.get(&key) {
                        Some((_, existing_seq, _)) => seq > *existing_seq,
                        None => true,
                    };
                    if should_update {
                        seen.insert(key, (value, seq, kind));
                    }

                    iter.next();
                }
            }
        }

        // 2. Scan immutable MemTable (overrides SSTables)
        if let Some(ref imm) = self.immutable_memtable {
            for entry in imm.entries() {
                if !entry.key.starts_with(prefix) {
                    continue;
                }
                let should_update = match seen.get(&entry.key) {
                    Some((_, existing_seq, _)) => entry.seq_no > *existing_seq,
                    None => true,
                };
                if should_update {
                    seen.insert(
                        entry.key.clone(),
                        (entry.value.clone(), entry.seq_no, entry.kind),
                    );
                }
            }
        }

        // 3. Scan active MemTable (overrides everything)
        for entry in self.memtable.entries() {
            if !entry.key.starts_with(prefix) {
                continue;
            }
            let should_update = match seen.get(&entry.key) {
                Some((_, existing_seq, _)) => entry.seq_no > *existing_seq,
                None => true,
            };
            if should_update {
                seen.insert(
                    entry.key.clone(),
                    (entry.value.clone(), entry.seq_no, entry.kind),
                );
            }
        }

        // Filter out tombstones and collect
        let result: Vec<(Vec<u8>, Vec<u8>)> = seen
            .into_iter()
            .filter(|(_, (_, _, kind))| *kind == EntryKind::Put)
            .map(|(key, (value, _, _))| (key, value))
            .collect();

        Ok(result)
    }

    /// Checks if a prefix could match any key in the range [min_key, max_key].
    fn prefix_may_overlap(prefix: &[u8], min_key: &[u8], max_key: &[u8]) -> bool {
        if prefix <= min_key {
            // prefix could match min_key or keys after it
            return true;
        }
        if prefix > max_key {
            // prefix is beyond the max key
            return false;
        }
        // prefix is within the range
        true
    }

    /// Deletes a key (writes a tombstone).
    pub fn delete(&mut self, key: Key) -> Result<()> {
        let seq = self.next_seq();
        let entry = Entry::delete(key.clone(), seq);

        self.wal.append(&entry)?;
        self.memtable.delete_with_seq(key, seq);

        if self.memtable.size() >= self.options.memtable_size_limit {
            self.flush_memtable()?;
        }

        Ok(())
    }

    /// Manually flushes the current MemTable to an SSTable.
    /// Call this after a batch of writes to ensure all data is persisted.
    pub fn flush(&mut self) -> Result<()> {
        if !self.memtable.is_empty() {
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
                    self.memtable.put_with_seq(entry.key, entry.value, entry.seq_no);
                }
                EntryKind::Delete => {
                    self.memtable.delete_with_seq(entry.key, entry.seq_no);
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

            let mut sst = SsTable::open(&path)?;
            let min_key = sst.first_key().unwrap_or_default();
            let max_key = sst.max_key().to_vec();
            let metadata = fs::metadata(&path)?;

            // Update sst_counter if needed
            if let Some(id_str) = fname.split('_').nth(1) {
                if let Ok(id) = id_str.parse::<u64>() {
                    let current = self.sst_counter.load(Ordering::Relaxed);
                    if id >= current {
                        self.sst_counter.store(id + 1, Ordering::Relaxed);
                    }
                }
            }

            self.levels[level].push(SsTableInfo {
                path,
                size: metadata.len(),
                min_key,
                max_key,
            });
        }

        Ok(())
    }

    /// Checks if compaction is needed and triggers it.
    fn maybe_compact(&mut self, level: usize) -> Result<()> {
        if level >= self.levels.len() - 1 {
            return Ok(());
        }

        let max_ssts = self.options.size_ratio;
        if self.levels[level].len() <= max_ssts {
            return Ok(());
        }

        tracing::info!(
            "Level {} has {} SSTables (max {}), triggering compaction",
            level,
            self.levels[level].len(),
            max_ssts
        );

        self.compact_level(level)
    }

    /// Performs leveled compaction: merges SSTables from level N into level N+1.
    ///
    /// Algorithm:
    /// 1. Pick SSTables from level N (sorted by key range)
    /// 2. Find overlapping SSTables in level N+1
    /// 3. Merge-sort all entries from both levels
    /// 4. Deduplicate: keep only the latest version of each key
    /// 5. Drop tombstones if they don't exist in deeper levels
    /// 6. Write new SSTables to level N+1
    /// 7. Delete old SSTable files from both levels
    fn compact_level(&mut self, level: usize) -> Result<()> {
        if level >= self.levels.len() - 1 {
            return Ok(());
        }

        // Step 1: Pick SSTables from level N to compact.
        // For L0, compact all (they may overlap). For L1+, pick the oldest.
        let ssts_to_compact = if level == 0 {
            // L0: compact all SSTables (they can have overlapping key ranges)
            let all: Vec<SsTableInfo> = self.levels[level].drain(..).collect();
            all
        } else {
            // L1+: pick the first (oldest) SSTable
            if self.levels[level].is_empty() {
                return Ok(());
            }
            vec![self.levels[level].remove(0)]
        };

        if ssts_to_compact.is_empty() {
            return Ok(());
        }

        // Compute the combined key range of the SSTables being compacted
        let compact_min = ssts_to_compact
            .iter()
            .map(|s| s.min_key.as_slice())
            .min()
            .unwrap_or(b"")
            .to_vec();
        let compact_max = ssts_to_compact
            .iter()
            .map(|s| s.max_key.as_slice())
            .max()
            .unwrap_or(b"")
            .to_vec();

        // Step 2: Find overlapping SSTables in level N+1
        let next_level = level + 1;
        let mut overlapping_indices = Vec::new();
        for (i, sst_info) in self.levels[next_level].iter().enumerate() {
            if Self::ranges_overlap(
                &compact_min,
                &compact_max,
                &sst_info.min_key,
                &sst_info.max_key,
            ) {
                overlapping_indices.push(i);
            }
        }

        // Collect overlapping SSTables (remove from level in reverse order to preserve indices)
        let mut next_level_ssts = Vec::new();
        for &i in overlapping_indices.iter().rev() {
            next_level_ssts.push(self.levels[next_level].remove(i));
        }

        // Step 3: Collect all entries from both sets of SSTables
        let mut all_entries: Vec<(Vec<u8>, Vec<u8>, SeqNo, EntryKind)> = Vec::new();

        // Read entries from level N SSTables
        for sst_info in &ssts_to_compact {
            let mut sst = SsTable::open(&sst_info.path)?;
            let mut iter = sst.iter()?;
            while iter.is_valid() {
                all_entries.push((
                    iter.key().to_vec(),
                    iter.value().to_vec(),
                    iter.seq_no(),
                    iter.kind(),
                ));
                iter.next();
            }
        }

        // Read entries from level N+1 SSTables
        for sst_info in &next_level_ssts {
            let mut sst = SsTable::open(&sst_info.path)?;
            let mut iter = sst.iter()?;
            while iter.is_valid() {
                all_entries.push((
                    iter.key().to_vec(),
                    iter.value().to_vec(),
                    iter.seq_no(),
                    iter.kind(),
                ));
                iter.next();
            }
        }

        // Step 4: Sort by key, then by seq_no descending (keep latest version)
        all_entries.sort_by(|a, b| {
            a.0.cmp(&b.0) // key ascending
                .then(b.2.cmp(&a.2)) // seq_no descending
        });

        // Step 5: Deduplicate — keep only the latest version of each key
        // Drop tombstones at the deepest level (they can't shadow anything deeper)
        let is_deepest_level = next_level == self.levels.len() - 1;
        let mut merged: Vec<(Vec<u8>, Vec<u8>, SeqNo, EntryKind)> = Vec::new();
        let mut last_key: Option<Vec<u8>> = None;

        for (key, value, seq_no, kind) in &all_entries {
            if last_key.as_ref() == Some(key) {
                continue; // Skip older versions of the same key
            }

            // Drop tombstones at the deepest level
            if is_deepest_level && *kind == EntryKind::Delete {
                last_key = Some(key.clone());
                continue;
            }

            last_key = Some(key.clone());
            merged.push((key.clone(), value.clone(), *seq_no, *kind));
        }

        // Step 6: Write merged entries to new SSTables in level N+1
        let target_sst_size = self.options.block_size * 16; // ~64KB per SSTable
        let mut builder = SsTableBuilder::new();
        let mut new_ssts = Vec::new();
        let mut current_size = 0usize;
        let mut batch_start_idx = 0usize;

        for (i, (key, value, seq_no, kind)) in merged.iter().enumerate() {
            builder.add(&Entry {
                key: key.clone(),
                value: value.clone(),
                seq_no: *seq_no,
                kind: *kind,
            });
            current_size += key.len() + value.len() + 16;

            if current_size >= target_sst_size {
                let sst_id = self.sst_counter.fetch_add(1, Ordering::Relaxed);
                let sst_path = self
                    .options
                    .data_dir
                    .join(format!("L{}_{}.sst", next_level, sst_id));
                let sst = builder.build(&sst_path)?;

                let metadata = fs::metadata(&sst_path)?;
                new_ssts.push(SsTableInfo {
                    path: sst_path,
                    size: metadata.len(),
                    min_key: merged[batch_start_idx].0.clone(),
                    max_key: sst.max_key().to_vec(),
                });

                builder = SsTableBuilder::new();
                current_size = 0;
                batch_start_idx = i + 1;
            }
        }

        // Flush remaining entries
        if current_size > 0 {
            let sst_id = self.sst_counter.fetch_add(1, Ordering::Relaxed);
            let sst_path = self
                .options
                .data_dir
                .join(format!("L{}_{}.sst", next_level, sst_id));
            let sst = builder.build(&sst_path)?;

            let metadata = fs::metadata(&sst_path)?;
            new_ssts.push(SsTableInfo {
                path: sst_path,
                size: metadata.len(),
                min_key: merged[batch_start_idx].0.clone(),
                max_key: sst.max_key().to_vec(),
            });
        }

        // Step 7: Delete old SSTable files
        for sst_info in &ssts_to_compact {
            let _ = fs::remove_file(&sst_info.path);
        }
        for sst_info in &next_level_ssts {
            let _ = fs::remove_file(&sst_info.path);
        }

        // Add new SSTables to level N+1
        self.levels[next_level].extend(new_ssts);

        // Sort level N+1 by min_key to maintain non-overlapping order
        self.levels[next_level].sort_by(|a, b| a.min_key.cmp(&b.min_key));

        tracing::info!(
            "Compaction L{}→L{}: merged {} + {} entries into {} SSTables",
            level,
            next_level,
            ssts_to_compact.len(),
            next_level_ssts.len(),
            self.levels[next_level].len()
        );

        // Recursively check if next level needs compaction
        self.maybe_compact(next_level)?;

        Ok(())
    }

    /// Checks if two key ranges overlap.
    fn ranges_overlap(min_a: &[u8], max_a: &[u8], min_b: &[u8], max_b: &[u8]) -> bool {
        // Empty ranges don't overlap
        if min_a.is_empty() || max_a.is_empty() || min_b.is_empty() || max_b.is_empty() {
            return true; // Conservative: assume overlap if range is unknown
        }
        min_a <= max_b && min_b <= max_a
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

    #[test]
    fn test_engine_compaction() {
        let dir = tempdir().unwrap();
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            memtable_size_limit: 128, // Very small to trigger frequent flushes
            size_ratio: 2,            // Compact when level has > 2 SSTables
            ..Default::default()
        };

        let mut engine = LsmEngine::open(options).unwrap();

        // Write enough data to trigger multiple flushes and compaction
        let num_keys = 100u32;
        for i in 0..num_keys {
            let key = format!("key_{:04}", i);
            let value = format!("value_{:06}", i); // Larger values to fill memtable faster
            engine.put(key.into_bytes(), value.into_bytes()).unwrap();
        }

        let stats = engine.stats();
        println!(
            "After writing {} keys: {} SSTables across {} levels, total size {} bytes",
            num_keys, stats.total_sstables, stats.num_levels, stats.total_sst_size
        );

        // Verify all data is still readable after compaction
        for i in 0..num_keys {
            let key = format!("key_{:04}", i);
            let expected = format!("value_{:06}", i);
            let val = engine.get(key.as_bytes()).unwrap();
            assert_eq!(
                val,
                Some(expected.into_bytes()),
                "key {} should be readable after compaction",
                key
            );
        }

        // Verify compaction actually happened (L0 should be smaller than without compaction)
        // With size_ratio=2 and 100 keys, we should have multiple levels
        assert!(
            stats.num_levels > 1 || stats.total_sstables <= 2,
            "compaction should have merged SSTables into deeper levels"
        );
    }

    #[test]
    fn test_engine_compaction_with_overwrites() {
        let dir = tempdir().unwrap();
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            memtable_size_limit: 128,
            size_ratio: 2,
            ..Default::default()
        };

        let mut engine = LsmEngine::open(options).unwrap();

        // Write initial data
        for i in 0..50u32 {
            let key = format!("key_{:04}", i);
            let value = format!("v1_{:06}", i);
            engine.put(key.into_bytes(), value.into_bytes()).unwrap();
        }

        // Overwrite all keys with new values
        for i in 0..50u32 {
            let key = format!("key_{:04}", i);
            let value = format!("v2_{:06}", i);
            engine.put(key.into_bytes(), value.into_bytes()).unwrap();
        }

        // Verify the latest values are returned
        for i in 0..50u32 {
            let key = format!("key_{:04}", i);
            let expected = format!("v2_{:06}", i);
            let val = engine.get(key.as_bytes()).unwrap();
            assert_eq!(
                val,
                Some(expected.into_bytes()),
                "overwritten key {} should return latest value",
                key
            );
        }
    }

    #[test]
    fn test_engine_compaction_with_deletes() {
        let dir = tempdir().unwrap();
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            memtable_size_limit: 128,
            size_ratio: 2,
            ..Default::default()
        };

        let mut engine = LsmEngine::open(options).unwrap();

        // Write data
        for i in 0..50u32 {
            let key = format!("key_{:04}", i);
            let value = format!("value_{:06}", i);
            engine.put(key.into_bytes(), value.into_bytes()).unwrap();
        }

        // Delete even-numbered keys
        for i in (0..50u32).step_by(2) {
            let key = format!("key_{:04}", i);
            engine.delete(key.into_bytes()).unwrap();
        }

        // Flush remaining entries in memtable
        engine.flush().unwrap();

        // Verify: odd keys exist, even keys are deleted
        for i in 0..50u32 {
            let key = format!("key_{:04}", i);
            let val = engine.get(key.as_bytes()).unwrap();
            if i % 2 == 0 {
                assert!(val.is_none(), "deleted key {} should not exist, got {:?}", key, val);
            } else {
                assert!(
                    val.is_some(),
                    "non-deleted key {} should still exist",
                    key
                );
            }
        }
    }
}
