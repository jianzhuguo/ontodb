// Copyright (c) 2024-2026 OntoDB Team
// Licensed under the Business Source License 1.1 (BUSL-1.1).
// See LICENSE for details. Change Date: 2031-09-15.
// On the Change Date, this file will be licensed under Apache License 2.0.
//! Index manager: manages secondary indexes and integrates with the LSM engine.
//!
//! Supports both in-memory B+Trees (for small indexes) and disk-based B+Trees
//! (for indexes larger than available RAM). Disk-based indexes are stored as
//! dedicated `.idx` files with 4KB pages and a buffer pool for caching.
//!
//! Index entries are also persisted in the LSM-Tree with key format:
//!   __idx2__{class_len:u16}{class}{col_len:u16}{col}{val_len:u16}{value}{pk_len:u16}{pk}
//!
//! On startup, the manager can rebuild in-memory trees from LSM entries or
//! open existing disk-based index files.

use crate::index::btree::{BPlusTree, ValuePkCursor};
use crate::index::disk::{BTreeIndex, DiskRangeCursor};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Prefix for index keys in the LSM-Tree (v2, length-prefixed).
const INDEX_PREFIX: &[u8] = b"__idx2__";

/// Legacy prefix (v1, `::`-delimited).  Recognised only for migration.
const INDEX_PREFIX_V1: &[u8] = b"__idx__";

/// Collect pks from a borrowed mem-tree cursor. Only pks are copied; values
/// stay in the tree. Stops at `limit`.
fn collect_pks(mut cur: ValuePkCursor<'_>, limit: Option<usize>) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    while let Some((_, pk)) = cur.current() {
        out.push(pk.to_vec());
        if limit.is_some_and(|l| out.len() >= l) {
            break;
        }
        cur.advance();
    }
    out
}

/// Collect pks from a disk cursor (one-entry buffer). Stops at `limit`.
fn collect_pks_disk(mut cur: DiskRangeCursor<'_>, limit: Option<usize>) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    while let Some((_, pk)) = cur.current() {
        out.push(pk.to_vec());
        if limit.is_some_and(|l| out.len() >= l) {
            break;
        }
        if cur.advance().is_err() {
            break;
        }
    }
    out
}

/// Collect pks from a disk cursor, dropping entries whose value == `exclude`
/// (disk high bound is inclusive; LT needs strict `<`). Stops at `limit`.
fn collect_pks_disk_lt(
    mut cur: DiskRangeCursor<'_>,
    limit: Option<usize>,
    exclude: &[u8],
) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    while let Some((val, pk)) = cur.current() {
        if val != exclude {
            out.push(pk.to_vec());
            if limit.is_some_and(|l| out.len() >= l) {
                break;
            }
        }
        if cur.advance().is_err() {
            break;
        }
    }
    out
}

/// Streaming merge of mem + disk by the composite comparator (value, pk).
///
/// Both cursors yield in (value, pk) order. Values are compared as borrowed
/// slices — never cloned. Only the winning pk is copied for the output.
/// Equal (value, pk) pairs are emitted once (mem+disk overlap dedup).
/// Stops at `limit`.
fn merge_cursors(
    mut mem: ValuePkCursor<'_>,
    mut disk: DiskRangeCursor<'_>,
    limit: Option<usize>,
) -> Vec<Vec<u8>> {
    #[derive(Clone, Copy)]
    enum Side {
        Mem,
        Disk,
        Both,
    }
    let mut out = Vec::new();
    loop {
        if limit.is_some_and(|l| out.len() >= l) {
            break;
        }
        // Compare while both peeks are alive (distinct borrows), then copy
        // only the pk and advance — no value materialization.
        let (side, pk): (Side, Vec<u8>) = match (mem.current(), disk.current()) {
            (None, None) => break,
            (Some((_, mp)), None) => (Side::Mem, mp.to_vec()),
            (None, Some((_, dp))) => (Side::Disk, dp.to_vec()),
            (Some((mv, mp)), Some((dv, dp))) => match mv.cmp(dv).then_with(|| mp.cmp(dp)) {
                std::cmp::Ordering::Less => (Side::Mem, mp.to_vec()),
                std::cmp::Ordering::Greater => (Side::Disk, dp.to_vec()),
                std::cmp::Ordering::Equal => (Side::Both, mp.to_vec()),
            },
        };
        out.push(pk);
        match side {
            Side::Mem => mem.advance(),
            Side::Disk => {
                if disk.advance().is_err() {
                    // Drain the remaining mem side.
                    while let Some((_, mp)) = mem.current() {
                        if limit.is_some_and(|l| out.len() >= l) {
                            break;
                        }
                        out.push(mp.to_vec());
                        mem.advance();
                    }
                    break;
                }
            }
            Side::Both => {
                mem.advance();
                if disk.advance().is_err() {
                    while let Some((_, mp)) = mem.current() {
                        if limit.is_some_and(|l| out.len() >= l) {
                            break;
                        }
                        out.push(mp.to_vec());
                        mem.advance();
                    }
                    break;
                }
            }
        }
    }
    out
}

/// Like [`merge_cursors`], but drops disk entries whose value == `bound`
/// (used for LT where disk's high bound is inclusive).
fn merge_cursors_lt(
    mut mem: ValuePkCursor<'_>,
    mut disk: DiskRangeCursor<'_>,
    limit: Option<usize>,
    bound: &[u8],
) -> Vec<Vec<u8>> {
    #[derive(Clone, Copy)]
    enum Side {
        Mem,
        Disk,
        Both,
    }
    let mut out = Vec::new();
    loop {
        if limit.is_some_and(|l| out.len() >= l) {
            break;
        }
        // Skip disk entries at the inclusive boundary (value == bound).
        while let Some((dv, _)) = disk.current() {
            if dv == bound {
                if disk.advance().is_err() {
                    break;
                }
            } else {
                break;
            }
        }
        let (side, pk): (Side, Vec<u8>) = match (mem.current(), disk.current()) {
            (None, None) => break,
            (Some((_, mp)), None) => (Side::Mem, mp.to_vec()),
            (None, Some((_, dp))) => (Side::Disk, dp.to_vec()),
            (Some((mv, mp)), Some((dv, dp))) => match mv.cmp(dv).then_with(|| mp.cmp(dp)) {
                std::cmp::Ordering::Less => (Side::Mem, mp.to_vec()),
                std::cmp::Ordering::Greater => (Side::Disk, dp.to_vec()),
                std::cmp::Ordering::Equal => (Side::Both, mp.to_vec()),
            },
        };
        out.push(pk);
        match side {
            Side::Mem => mem.advance(),
            Side::Disk => {
                if disk.advance().is_err() {
                    while let Some((_, mp)) = mem.current() {
                        if limit.is_some_and(|l| out.len() >= l) {
                            break;
                        }
                        out.push(mp.to_vec());
                        mem.advance();
                    }
                    break;
                }
            }
            Side::Both => {
                mem.advance();
                if disk.advance().is_err() {
                    while let Some((_, mp)) = mem.current() {
                        if limit.is_some_and(|l| out.len() >= l) {
                            break;
                        }
                        out.push(mp.to_vec());
                        mem.advance();
                    }
                    break;
                }
            }
        }
    }
    out
}

/// Configuration for index storage mode.
#[derive(Debug, Clone, Default)]
pub enum IndexStorageMode {
    /// All indexes stored in memory (default, fast but limited by RAM).
    #[default]
    InMemory,
    /// All indexes stored on disk (slower but handles large datasets).
    DiskBased,
    /// Hybrid: small indexes in memory, large ones on disk based on threshold.
    Hybrid { threshold_entries: usize },
}

/// Manages all secondary indexes.
pub struct IndexManager {
    /// In-memory B+Tree indexes, keyed by (class, column).
    indexes: HashMap<(String, String), BPlusTree>,
    /// Disk-based B+Tree indexes, keyed by (class, column).
    /// Uses Mutex for interior mutability to allow read-path lookups.
    disk_indexes: HashMap<(String, String), Mutex<BTreeIndex>>,
    /// Keys whose in-memory tree is known to hold the complete entry set
    /// (bulk_load / rebuild). Lookups skip the disk merge for these — merging
    /// 500k pks from disk on every query costs ~250ms for nothing when both
    /// sides are in sync. Cleared whenever the memory tree is recreated empty.
    mem_complete: std::collections::HashSet<(String, String)>,
    /// Base directory for disk-based index files.
    data_dir: Option<PathBuf>,
    /// Storage mode configuration.
    _storage_mode: IndexStorageMode,
}

impl IndexManager {
    /// Creates a new empty index manager (in-memory mode).
    pub fn new() -> Self {
        Self {
            indexes: HashMap::new(),
            disk_indexes: HashMap::new(),
            mem_complete: std::collections::HashSet::new(),
            data_dir: None,
            _storage_mode: IndexStorageMode::default(),
        }
    }

    /// Creates a new index manager with disk-based storage support.
    pub fn with_disk_storage(data_dir: &Path, mode: IndexStorageMode) -> Self {
        Self {
            indexes: HashMap::new(),
            disk_indexes: HashMap::new(),
            mem_complete: std::collections::HashSet::new(),
            data_dir: Some(data_dir.to_path_buf()),
            _storage_mode: mode,
        }
    }

    /// Returns the path for a disk-based index file.
    fn index_path(&self, class: &str, column: &str) -> Option<PathBuf> {
        self.data_dir
            .as_ref()
            .map(|dir| dir.join(format!("{}_{}.idx", class, column)))
    }

    /// Returns the data directory for disk-based indexes, if configured.
    pub fn data_dir_ref(&self) -> Option<&Path> {
        self.data_dir.as_deref()
    }

    /// Creates a new index on a class.column.
    /// Returns Ok(()) if created, or if it already exists.
    pub fn create_index(&mut self, class: &str, column: &str) {
        let key = (class.to_string(), column.to_string());

        // Always create in-memory index
        self.indexes
            .entry(key.clone())
            .or_insert_with(|| BPlusTree::new(class, column));
        self.mem_complete.remove(&key);

        // Create disk-based index if configured
        if let Some(path) = self.index_path(class, column) {
            if let std::collections::hash_map::Entry::Vacant(e) = self.disk_indexes.entry(key) {
                match BTreeIndex::create(&path, class, column) {
                    Ok(idx) => {
                        e.insert(Mutex::new(idx));
                    }
                    Err(e) => {
                        eprintln!(
                            "Warning: failed to create disk index for {}.{}: {}",
                            class, column, e
                        );
                    }
                }
            }
        }
    }

    /// Bulk-loads (value, pk) pairs into an index in one shot.
    ///
    /// Used by CREATE INDEX backfill: replaces per-row `insert` (O(n log n)
    /// with page splits) with sort + bottom-up page construction (O(n)).
    /// Entries need not be sorted; duplicates are dropped.
    ///
    /// If the in-memory tree already holds entries (INSERTs that ran while the
    /// backfill scan was in flight), they are merged in so the rebuild does not
    /// wipe them.
    pub fn bulk_load_index(
        &mut self,
        class: &str,
        column: &str,
        entries: &[(Vec<u8>, Vec<u8>)],
    ) -> Result<(), onto_core::CoreError> {
        let key = (class.to_string(), column.to_string());

        // In-memory tree: merge with any entries that landed after the scan
        // started, then one sort + bottom-up rebuild.
        if let Some(tree) = self.indexes.get_mut(&key) {
            if tree.is_empty() {
                tree.bulk_load(entries.to_vec());
            } else {
                let mut all = tree.all_entries();
                all.extend(entries.iter().cloned());
                tree.bulk_load(all);
            }
        }

        // Disk tree: empty tree takes the O(n) bulk_load fast path; a non-empty
        // tree means concurrent inserts already landed — per-row insert so the
        // rebuild does not drop them.
        if let Some(disk) = self.disk_indexes.get(&key) {
            let mut idx = disk.lock().unwrap_or_else(|e| e.into_inner());
            if idx.is_empty() {
                idx.bulk_load(entries)?;
            } else {
                for (val, pk) in entries {
                    idx.insert(val, pk.clone())?;
                }
            }
            idx.flush()?;
        }

        // Memory tree now holds the complete entry set.
        self.mem_complete.insert(key);

        Ok(())
    }

    /// Opens existing disk-based indexes from the data directory.
    /// Corrupted `.idx` files are automatically deleted so they can be rebuilt.
    pub fn open_disk_indexes(&mut self) -> Result<(), String> {
        let data_dir = match &self.data_dir {
            Some(d) => d.clone(),
            None => return Ok(()),
        };

        if !data_dir.exists() {
            std::fs::create_dir_all(&data_dir).map_err(|e| e.to_string())?;
        }

        // Scan for .idx files
        let entries = std::fs::read_dir(&data_dir).map_err(|e| e.to_string())?;
        for entry in entries {
            let entry = entry.map_err(|e| e.to_string())?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("idx") {
                // Validate before opening — if corrupted, delete and skip
                if !BTreeIndex::validate(&path) {
                    eprintln!(
                        "Warning: corrupted disk index {}, deleting for rebuild",
                        path.display()
                    );
                    let _ = std::fs::remove_file(&path);
                    continue;
                }
                match BTreeIndex::open(&path) {
                    Ok(idx) => {
                        // Use class/column from the file header, not the filename —
                        // filenames like `a.b_with_c.idx` are ambiguous when the
                        // class name itself contains underscores.
                        let meta = idx.meta();
                        let key = (meta.class.clone(), meta.column.clone());
                        self.disk_indexes.insert(key.clone(), Mutex::new(idx));
                        // Also create in-memory index (empty — disk holds the
                        // history until rebuilt; mark incomplete so lookups merge).
                        self.indexes
                            .entry(key.clone())
                            .or_insert_with(|| BPlusTree::new(&key.0, &key.1));
                        self.mem_complete.remove(&key);
                    }
                    Err(e) => {
                        eprintln!(
                            "Warning: failed to open disk index {}, deleting: {}",
                            path.display(),
                            e
                        );
                        let _ = std::fs::remove_file(&path);
                    }
                }
            }
        }

        Ok(())
    }

    /// Drops an index on a class.column.
    pub fn drop_index(&mut self, class: &str, column: &str) -> bool {
        let key = (class.to_string(), column.to_string());
        self.mem_complete.remove(&key);
        let in_memory_removed = self.indexes.remove(&key).is_some();

        // Also remove disk-based index
        let disk_removed = if let Some(idx) = self.disk_indexes.remove(&key) {
            let _ = idx.lock().unwrap().flush();
            // Optionally delete the file
            if let Some(path) = self.index_path(class, column) {
                let _ = std::fs::remove_file(path);
            }
            true
        } else {
            false
        };

        in_memory_removed || disk_removed
    }

    /// Returns true if an index exists on the given class.column.
    pub fn has_index(&self, class: &str, column: &str) -> bool {
        let key = (class.to_string(), column.to_string());
        self.indexes.contains_key(&key) || self.disk_indexes.contains_key(&key)
    }

    /// Gets a reference to the B+Tree for a given class.column.
    pub fn get_index(&self, class: &str, column: &str) -> Option<&BPlusTree> {
        self.indexes.get(&(class.to_string(), column.to_string()))
    }

    /// Indexes a document: extracts indexed column values and inserts into B+Trees.
    /// Called during INSERT and UPDATE operations.
    pub fn index_document(
        &mut self,
        class: &str,
        primary_key: &[u8],
        doc: &serde_json::Map<String, serde_json::Value>,
    ) -> Vec<(Vec<u8>, Vec<u8>)> {
        let mut index_entries = Vec::new();

        // Collect keys to avoid borrow issues
        let index_keys: Vec<(String, String)> = self
            .indexes
            .keys()
            .filter(|(c, _)| c == class)
            .cloned()
            .collect();

        for (idx_class, idx_col) in &index_keys {
            if let Some(val) = doc.get(idx_col.as_str()) {
                let encoded = Self::encode_value(val);

                // Insert into in-memory index
                if let Some(tree) = self.indexes.get_mut(&(idx_class.clone(), idx_col.clone())) {
                    tree.insert(encoded.clone(), primary_key.to_vec());
                }

                // Insert into disk-based index
                if let Some(disk_idx) = self
                    .disk_indexes
                    .get(&(idx_class.clone(), idx_col.clone()))
                {
                    if let Err(e) = disk_idx.lock().unwrap().insert(&encoded, primary_key.to_vec()) {
                        eprintln!(
                            "Warning: disk index insert failed for {}.{}: {}",
                            idx_class, idx_col, e
                        );
                    }
                }

                index_entries.push((
                    Self::make_index_key(class, idx_col, &encoded, primary_key),
                    Vec::new(), // value is empty; key contains all info
                ));
            }
        }

        index_entries
    }

    /// Computes LSM index key entries for a document WITHOUT modifying the index.
    /// Used by commit_txn Phase 1 to pre-compute WAL entries.
    pub fn index_document_read_only(
        &self,
        class: &str,
        primary_key: &[u8],
        doc: &serde_json::Map<String, serde_json::Value>,
    ) -> Vec<(Vec<u8>, Vec<u8>)> {
        let mut index_entries = Vec::new();

        // Check both in-memory and disk-based indexes
        let mut seen = std::collections::HashSet::new();
        for (idx_class, idx_col) in self.indexes.keys().chain(self.disk_indexes.keys()) {
            if idx_class != class || !seen.insert(idx_col.clone()) {
                continue;
            }
            if let Some(val) = doc.get(idx_col.as_str()) {
                let encoded = Self::encode_value(val);
                index_entries.push((
                    Self::make_index_key(class, idx_col, &encoded, primary_key),
                    Vec::new(),
                ));
            }
        }

        index_entries
    }

    /// Removes index entries for a document.
    /// Called during DELETE and UPDATE (before re-indexing).
    pub fn deindex_document(
        &mut self,
        class: &str,
        primary_key: &[u8],
        doc: &serde_json::Map<String, serde_json::Value>,
    ) -> Vec<Vec<u8>> {
        let mut removed_keys = Vec::new();

        // Collect keys to avoid borrow issues
        let index_keys: Vec<(String, String)> = self
            .indexes
            .keys()
            .filter(|(c, _)| c == class)
            .cloned()
            .collect();

        for (idx_class, idx_col) in &index_keys {
            if let Some(val) = doc.get(idx_col.as_str()) {
                let encoded = Self::encode_value(val);

                // Remove from in-memory index
                if let Some(tree) = self.indexes.get_mut(&(idx_class.clone(), idx_col.clone())) {
                    tree.remove(&encoded, primary_key);
                }

                // Remove from disk-based index
                if let Some(disk_idx) = self
                    .disk_indexes
                    .get(&(idx_class.clone(), idx_col.clone()))
                {
                    if let Err(e) = disk_idx.lock().unwrap().remove(&encoded, primary_key) {
                        eprintln!(
                            "Warning: disk index remove failed for {}.{}: {}",
                            idx_class, idx_col, e
                        );
                    }
                }

                removed_keys.push(Self::make_index_key(class, idx_col, &encoded, primary_key));
            }
        }

        removed_keys
    }

    /// Returns all indexes for a given class (both in-memory and disk-based).
    pub fn indexes_for_class(&self, class: &str) -> Vec<&str> {
        let mut cols: Vec<&str> = self
            .indexes
            .keys()
            .filter(|(c, _)| c == class)
            .map(|(_, col)| col.as_str())
            .collect();
        // Add disk-only indexes not in memory
        for (c, col) in self.disk_indexes.keys() {
            if c == class && !cols.contains(&col.as_str()) {
                cols.push(col.as_str());
            }
        }
        cols
    }

    /// Flushes all disk-based indexes to disk (with fsync).
    pub fn flush_disk_indexes(&mut self) {
        for disk_idx in self.disk_indexes.values() {
            let _ = disk_idx.lock().unwrap().flush();
        }
    }

    /// Returns the number of unique indexes (counting each class.column once).
    pub fn index_count(&self) -> usize {
        let mut seen: std::collections::HashSet<(&str, &str)> = std::collections::HashSet::new();
        for (c, col) in self.indexes.keys() {
            seen.insert((c.as_str(), col.as_str()));
        }
        for (c, col) in self.disk_indexes.keys() {
            seen.insert((c.as_str(), col.as_str()));
        }
        seen.len()
    }

    /// Returns all registered index keys as (class, column) pairs.
    pub fn all_index_keys(&self) -> Vec<(String, String)> {
        let mut seen: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
        for key in self.indexes.keys() {
            seen.insert(key.clone());
        }
        for key in self.disk_indexes.keys() {
            seen.insert(key.clone());
        }
        seen.into_iter().collect()
    }

    /// Looks up primary keys for a given class, column, and value using the index.
    /// Returns None if no index exists on that column.
    /// Checks both in-memory and disk-based indexes.
    pub fn lookup_eq(
        &mut self,
        class: &str,
        column: &str,
        value: &serde_json::Value,
    ) -> Option<Vec<Vec<u8>>> {
        // Same merged semantics as lookup_eq_read (in-memory + disk).
        self.lookup_eq_read(class, column, value)
    }

    /// Range scan on an indexed column.
    /// Returns None if no index exists.
    /// Checks both in-memory and disk-based indexes.
    pub fn lookup_range(
        &mut self,
        class: &str,
        column: &str,
        low: Option<&serde_json::Value>,
        high: Option<&serde_json::Value>,
    ) -> Option<Vec<Vec<u8>>> {
        self.lookup_range_read(class, column, low, high)
    }

    /// Looks up primary keys for a GT condition.
    /// Checks both in-memory and disk-based indexes (merged).
    pub fn lookup_gt(
        &mut self,
        class: &str,
        column: &str,
        value: &serde_json::Value,
    ) -> Option<Vec<Vec<u8>>> {
        self.lookup_gt_read(class, column, value)
    }

    /// Looks up primary keys for a LT condition.
    /// Checks both in-memory and disk-based indexes (merged).
    pub fn lookup_lt(
        &mut self,
        class: &str,
        column: &str,
        value: &serde_json::Value,
    ) -> Option<Vec<Vec<u8>>> {
        self.lookup_lt_read(class, column, value)
    }

    // ── Read-only lookup methods (for concurrent read path, &self) ──

    /// Read-only equality lookup using in-memory index first, then disk-based index.
    /// Results are merged only when the in-memory tree is not known-complete
    /// (e.g. right after restart). When bulk_load/rebuild has populated memory
    /// fully, the disk merge is skipped — merging 500k pks per query costs
    /// ~250ms for nothing.
    pub fn lookup_eq_read(
        &self,
        class: &str,
        column: &str,
        value: &serde_json::Value,
    ) -> Option<Vec<Vec<u8>>> {
        let key = (class.to_string(), column.to_string());
        let encoded = Self::encode_value(value);

        let mem_has = self.indexes.contains_key(&key);
        let disk_has = self.disk_indexes.contains_key(&key);
        if !mem_has && !disk_has {
            return None;
        }

        if mem_has && self.mem_complete.contains(&key) {
            return self.indexes.get(&key).map(|tree| tree.lookup(&encoded));
        }

        // Merge only when both sides may hold rows; a single source is already
        // unique+sorted (disk walks escaped_value||0x00x00||pk in order), so skip the
        // 500k-element sort/dedup on the restart path.
        let mem_pks = self.indexes.get(&key).map(|tree| tree.lookup(&encoded));
        let disk_pks = match self.disk_indexes.get(&key) {
            Some(disk_idx) => disk_idx.lock().unwrap().lookup(&encoded).ok(),
            None => None,
        };
        match (mem_pks, disk_pks) {
            (Some(mut m), Some(d)) => {
                if m.is_empty() {
                    Some(d)
                } else if d.is_empty() {
                    Some(m)
                } else {
                    m.extend(d);
                    m.sort();
                    m.dedup();
                    Some(m)
                }
            }
            (Some(m), None) => Some(m),
            (None, Some(d)) => Some(d),
            (None, None) => None,
        }
    }

    /// Read-only range scan using in-memory and disk indexes (merged).
    pub fn lookup_range_read(
        &self,
        class: &str,
        column: &str,
        low: Option<&serde_json::Value>,
        high: Option<&serde_json::Value>,
    ) -> Option<Vec<Vec<u8>>> {
        self.lookup_range_read_limit(class, column, low, high, None)
    }

    /// Like [`Self::lookup_range_read`], stops after `limit` pks.
    pub fn lookup_range_read_limit(
        &self,
        class: &str,
        column: &str,
        low: Option<&serde_json::Value>,
        high: Option<&serde_json::Value>,
        limit: Option<usize>,
    ) -> Option<Vec<Vec<u8>>> {
        let key = (class.to_string(), column.to_string());
        let low_bytes = low.map(Self::encode_value);
        let high_bytes = high.map(Self::encode_value);

        let mem_has = self.indexes.contains_key(&key);
        let disk_has = self.disk_indexes.contains_key(&key);
        if !mem_has && !disk_has {
            return None;
        }

        // Fast path: memory tree is authoritative, already in value order.
        if mem_has && self.mem_complete.contains(&key) {
            return self.indexes.get(&key).map(|tree| {
                collect_pks(tree.range_cursor(low_bytes.as_deref(), high_bytes.as_deref()), limit)
            });
        }

        // Single source — already value-ordered, no merge needed.
        if mem_has && !disk_has {
            return self.indexes.get(&key).map(|tree| {
                collect_pks(tree.range_cursor(low_bytes.as_deref(), high_bytes.as_deref()), limit)
            });
        }
        if !mem_has && disk_has {
            return self
                .disk_indexes
                .get(&key)
                .map(|d| {
                    let mut idx = d.lock().unwrap_or_else(|e| e.into_inner());
                    let cur = idx
                        .range_cursor(low_bytes.as_deref(), high_bytes.as_deref())
                        .ok()?;
                    Some(collect_pks_disk(cur, limit))
                })
                .flatten();
        }

        // Both sources — streaming merge by (value, pk), no value clones.
        let tree = self.indexes.get(&key)?;
        let disk = self.disk_indexes.get(&key)?;
        let mut disk_idx = disk.lock().unwrap_or_else(|e| e.into_inner());
        let disk_cur = disk_idx
            .range_cursor(low_bytes.as_deref(), high_bytes.as_deref())
            .ok()?;
        let mem_cur = tree.range_cursor(low_bytes.as_deref(), high_bytes.as_deref());
        Some(merge_cursors(mem_cur, disk_cur, limit))
    }

    /// Read-only GT lookup using in-memory and disk indexes (merged).
    pub fn lookup_gt_read(
        &self,
        class: &str,
        column: &str,
        value: &serde_json::Value,
    ) -> Option<Vec<Vec<u8>>> {
        self.lookup_gt_read_limit(class, column, value, None)
    }

    /// Like [`Self::lookup_gt_read`], stops after `limit` pks.
    pub fn lookup_gt_read_limit(
        &self,
        class: &str,
        column: &str,
        value: &serde_json::Value,
        limit: Option<usize>,
    ) -> Option<Vec<Vec<u8>>> {
        let key = (class.to_string(), column.to_string());
        let encoded = Self::encode_value(value);

        let mem_has = self.indexes.contains_key(&key);
        let disk_has = self.disk_indexes.contains_key(&key);
        if !mem_has && !disk_has {
            return None;
        }

        if mem_has && self.mem_complete.contains(&key) {
            return self.indexes.get(&key).map(|tree| {
                collect_pks(tree.gt_cursor(&encoded), limit)
            });
        }

        if mem_has && !disk_has {
            return self
                .indexes
                .get(&key)
                .map(|tree| collect_pks(tree.gt_cursor(&encoded), limit));
        }
        if !mem_has && disk_has {
            // Disk: GT means value > encoded.  Append a 0x00 byte so the lower
            // bound's value-part (`escaped_val||0x00`) sorts strictly after
            // `escaped_val`, excluding exact-value entries.
            let mut lo = encoded.clone();
            lo.push(0u8);
            return self
                .disk_indexes
                .get(&key)
                .map(|d| {
                    let mut idx = d.lock().unwrap_or_else(|e| e.into_inner());
                    let cur = idx.range_cursor(Some(&lo), None).ok()?;
                    Some(collect_pks_disk(cur, limit))
                })
                .flatten();
        }

        let tree = self.indexes.get(&key)?;
        let disk = self.disk_indexes.get(&key)?;
        let mut disk_idx = disk.lock().unwrap_or_else(|e| e.into_inner());
        let mut lo = encoded.clone();
        lo.push(0u8);
        let disk_cur = disk_idx.range_cursor(Some(&lo), None).ok()?;
        let mem_cur = tree.gt_cursor(&encoded);
        Some(merge_cursors(mem_cur, disk_cur, limit))
    }

    /// Read-only LT lookup using in-memory and disk indexes (merged).
    pub fn lookup_lt_read(
        &self,
        class: &str,
        column: &str,
        value: &serde_json::Value,
    ) -> Option<Vec<Vec<u8>>> {
        self.lookup_lt_read_limit(class, column, value, None)
    }

    /// Like [`Self::lookup_lt_read`], stops after `limit` pks.
    pub fn lookup_lt_read_limit(
        &self,
        class: &str,
        column: &str,
        value: &serde_json::Value,
        limit: Option<usize>,
    ) -> Option<Vec<Vec<u8>>> {
        let key = (class.to_string(), column.to_string());
        let encoded = Self::encode_value(value);

        let mem_has = self.indexes.contains_key(&key);
        let disk_has = self.disk_indexes.contains_key(&key);
        if !mem_has && !disk_has {
            return None;
        }

        if mem_has && self.mem_complete.contains(&key) {
            return self.indexes.get(&key).map(|tree| {
                collect_pks(tree.lt_cursor(&encoded), limit)
            });
        }

        if mem_has && !disk_has {
            return self
                .indexes
                .get(&key)
                .map(|tree| collect_pks(tree.lt_cursor(&encoded), limit));
        }
        if !mem_has && disk_has {
            return self
                .disk_indexes
                .get(&key)
                .map(|d| {
                    let mut idx = d.lock().unwrap_or_else(|e| e.into_inner());
                    let cur = idx.range_cursor(None, Some(&encoded)).ok()?;
                    // disk high bound is inclusive (value <= encoded); LT needs
                    // strict <, so drop boundary equality hits.
                    Some(collect_pks_disk_lt(cur, limit, &encoded))
                })
                .flatten();
        }

        let tree = self.indexes.get(&key)?;
        let disk = self.disk_indexes.get(&key)?;
        let mut disk_idx = disk.lock().unwrap_or_else(|e| e.into_inner());
        // Disk high bound is inclusive; mem lt_cursor is exclusive. Feed disk
        // an inclusive bound and drop the equality rows during merge via the
        // composite comparator (mem side excludes them, so they lose).
        let disk_cur = disk_idx.range_cursor(None, Some(&encoded)).ok()?;
        let mem_cur = tree.lt_cursor(&encoded);
        Some(merge_cursors_lt(mem_cur, disk_cur, limit, &encoded))
    }

    /// Like [`Self::lookup_eq_read`], but returns at most `limit` pks.
    /// Used by SELECT ... WHERE indexed = x LIMIT n — avoids materializing
    /// 500k pks just to keep the first 100.
    pub fn lookup_eq_read_limit(
        &self,
        class: &str,
        column: &str,
        value: &serde_json::Value,
        limit: usize,
    ) -> Option<Vec<Vec<u8>>> {
        let key = (class.to_string(), column.to_string());
        let encoded = Self::encode_value(value);

        let mem_has = self.indexes.contains_key(&key);
        let disk_has = self.disk_indexes.contains_key(&key);
        if !mem_has && !disk_has {
            return None;
        }

        // Fast path: complete memory tree, take only what we need.
        if mem_has && self.mem_complete.contains(&key) {
            return self
                .indexes
                .get(&key)
                .map(|tree| tree.lookup_limited(&encoded, limit));
        }

        // Merge mem+disk. The same pk can live on both sides after restart
        // (open_disk_indexes leaves mem incomplete while disk holds history),
        // so concatenation without dedup returns the row twice — COUNT goes
        // through lookup_eq_read (which sorts+dedups) and disagrees with
        // SELECT LIMIT. Pull up to `limit` from EACH side (an unordered eq
        // result only needs `limit` unique pks; the union of two `limit`-sized
        // samples always contains enough), then sort + dedup.
        let mut out: Vec<Vec<u8>> = Vec::new();
        if let Some(tree) = self.indexes.get(&key) {
            out.extend(tree.lookup_limited(&encoded, limit));
        }
        if let Some(disk_idx) = self.disk_indexes.get(&key) {
            if let Ok(pks) = disk_idx.lock().unwrap().lookup_limited(&encoded, limit) {
                out.extend(pks);
            }
        }
        if out.is_empty() {
            // Distinguish "index missing" from "no matches".
            return if mem_has || disk_has {
                Some(out)
            } else {
                None
            };
        }
        out.sort();
        out.dedup();
        out.truncate(limit);
        Some(out)
    }

    /// Count of primary keys matching an equality predicate, without
    /// materializing the pk list. Returns None when no index exists.
    pub fn count_eq_read(
        &self,
        class: &str,
        column: &str,
        value: &serde_json::Value,
    ) -> Option<u64> {
        let key = (class.to_string(), column.to_string());
        let encoded = Self::encode_value(value);

        let mem_has = self.indexes.contains_key(&key);
        let disk_has = self.disk_indexes.contains_key(&key);
        if !mem_has && !disk_has {
            return None;
        }

        if mem_has && self.mem_complete.contains(&key) {
            return self.indexes.get(&key).map(|tree| tree.count(&encoded));
        }

        // Disk count is exact (one entry per pk). If the memory tree also has
        // rows (partially rebuilt), the union is unknown without materializing;
        // fall back to the merged lookup in that rare case.
        let mem_cnt = self
            .indexes
            .get(&key)
            .map(|tree| tree.count(&encoded))
            .unwrap_or(0);
        if mem_cnt > 0 {
            return self.lookup_eq_read(class, column, value).map(|p| p.len() as u64);
        }
        self.disk_indexes.get(&key).and_then(|disk_idx| {
            disk_idx.lock().unwrap().count_value(&encoded).ok()
        })
    }

    /// Count of primary keys in an inclusive [low, high] value range
    /// (None bound = unbounded). Returns None when no index exists.
    pub fn count_range_read(
        &self,
        class: &str,
        column: &str,
        low: Option<&serde_json::Value>,
        high: Option<&serde_json::Value>,
    ) -> Option<u64> {
        let key = (class.to_string(), column.to_string());
        let low_bytes = low.map(Self::encode_value);
        let high_bytes = high.map(Self::encode_value);

        let mem_has = self.indexes.contains_key(&key);
        let disk_has = self.disk_indexes.contains_key(&key);
        if !mem_has && !disk_has {
            return None;
        }

        if mem_has && self.mem_complete.contains(&key) {
            return self.indexes.get(&key).map(|tree| {
                tree.count_range(low_bytes.as_deref(), high_bytes.as_deref())
            });
        }

        let mem_cnt = self
            .indexes
            .get(&key)
            .map(|tree| tree.count_range(low_bytes.as_deref(), high_bytes.as_deref()))
            .unwrap_or(0);
        if mem_cnt > 0 {
            return self
                .lookup_range_read(class, column, low, high)
                .map(|p| p.len() as u64);
        }
        self.disk_indexes.get(&key).and_then(|disk_idx| {
            disk_idx
                .lock()
                .unwrap()
                .count_range(low_bytes.as_deref(), high_bytes.as_deref())
                .ok()
        })
    }

    /// Distinct index values with their pk counts, in index-key order.
    /// Powers index-driven `GROUP BY col` / `SELECT DISTINCT col` without
    /// scanning the base table. Returns None when no index exists.
    ///
    /// When the memory tree is `mem_complete` it is authoritative and the disk
    /// merge is skipped. Otherwise memory and disk counts are merged (a pk can
    /// appear on both sides after restart, so the count is a union estimate
    /// only when one side is empty — same rule as `lookup_eq_read`).
    pub fn distinct_counts_read(
        &self,
        class: &str,
        column: &str,
    ) -> Option<Vec<(Vec<u8>, u64)>> {
        let key = (class.to_string(), column.to_string());
        let mem_has = self.indexes.contains_key(&key);
        let disk_has = self.disk_indexes.contains_key(&key);
        if !mem_has && !disk_has {
            return None;
        }

        if mem_has && self.mem_complete.contains(&key) {
            let mut out = Vec::new();
            if let Some(tree) = self.indexes.get(&key) {
                tree.for_each_key_count(|k, n| {
                    out.push((k.to_vec(), n));
                    true
                });
            }
            return Some(out);
        }

        // Merge path: prefer whichever side has data. When both have data the
        // union count is unknown without pk materialization — use max() which
        // is exact when one side is empty and a safe lower bound otherwise.
        let mut merged: std::collections::BTreeMap<Vec<u8>, u64> = std::collections::BTreeMap::new();
        if let Some(tree) = self.indexes.get(&key) {
            tree.for_each_key_count(|k, n| {
                *merged.entry(k.to_vec()).or_insert(0) += n;
                true
            });
        }
        if let Some(disk_idx) = self.disk_indexes.get(&key) {
            let mut disk = disk_idx.lock().unwrap();
            let _ = disk.for_each_value_count(|v, n| {
                *merged.entry(v.to_vec()).or_insert(0) += n;
                true
            });
        }
        if merged.is_empty() {
            return None;
        }
        Some(merged.into_iter().collect())
    }

    /// Builds the index key format (v2, length-prefixed — no delimiter collision):
    /// `__idx2__` || class_len:u16 || class || col_len:u16 || col || val_len:u16 || value || pk_len:u16 || pk
    pub fn make_index_key(class: &str, column: &str, value: &[u8], primary_key: &[u8]) -> Vec<u8> {
        let cb = class.as_bytes();
        let kb = column.as_bytes();
        let mut key = Vec::with_capacity(
            INDEX_PREFIX.len() + 2 + cb.len() + 2 + kb.len() + 2 + value.len() + 2 + primary_key.len(),
        );
        key.extend_from_slice(INDEX_PREFIX);
        key.extend_from_slice(&(cb.len() as u16).to_le_bytes());
        key.extend_from_slice(cb);
        key.extend_from_slice(&(kb.len() as u16).to_le_bytes());
        key.extend_from_slice(kb);
        key.extend_from_slice(&(value.len() as u16).to_le_bytes());
        key.extend_from_slice(value);
        key.extend_from_slice(&(primary_key.len() as u16).to_le_bytes());
        key.extend_from_slice(primary_key);
        key
    }

    /// Type tags for the memcomparable value encoding (v2).
    /// All tags are non-zero so the escaped payload never collides with them.
    pub const TAG_NULL: u8 = 0x01;
    pub const TAG_BOOL: u8 = 0x02;
    pub const TAG_INT: u8 = 0x03;
    pub const TAG_FLOAT: u8 = 0x04;
    pub const TAG_STRING: u8 = 0x05;

    /// Encodes a JSON value to order-preserving, 0x00-escaped bytes.
    ///
    /// Format: `[type_tag:1][escaped_payload]`
    /// - Null   → `[0x01]`
    /// - Bool   → `[0x02, 0x02|0x03]`  (0x02=true, 0x03=false — avoids 0x00/0x01)
    /// - Int    → `[0x03, escape(be_bytes(i64 ^ 1<<63))]`
    /// - Float  → `[0x04, escape(be_bytes(ordered_f64))]`
    /// - String → `[0x05, escape(utf8_bytes)]`
    ///
    /// The payload is escaped with `escape_bytes` (0x00 → 0x00 0xFF) so the
    /// full output is safe for `compose_key`'s `0x00 0x00` separator.
    /// Byte-lexicographic order of the output matches the logical order of
    /// the values within each type; type tag orders types as
    /// Null < Bool < Int < Float < String.
    pub fn encode_value(val: &serde_json::Value) -> Vec<u8> {
        match val {
            serde_json::Value::Null => vec![Self::TAG_NULL],
            serde_json::Value::Bool(b) => vec![Self::TAG_BOOL, if *b { 0x02 } else { 0x03 }],
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    // Order-preserving: flip sign bit so byte order == numeric order.
                    let ordered = (i as u64) ^ (1u64 << 63);
                    let mut out = Vec::with_capacity(1 + 8);
                    out.push(Self::TAG_INT);
                    out.extend_from_slice(&crate::index::disk::escape_bytes(&ordered.to_be_bytes()));
                    out
                } else if let Some(f) = n.as_f64() {
                    // Order-preserving f64: positive → flip sign bit; negative → flip all bits.
                    let bits = f.to_bits();
                    let ordered = if bits & (1u64 << 63) != 0 {
                        !bits
                    } else {
                        bits ^ (1u64 << 63)
                    };
                    let mut out = Vec::with_capacity(1 + 8);
                    out.push(Self::TAG_FLOAT);
                    out.extend_from_slice(&crate::index::disk::escape_bytes(&ordered.to_be_bytes()));
                    out
                } else {
                    // Fallback: treat as string
                    let mut out = Vec::with_capacity(1 + n.to_string().len());
                    out.push(Self::TAG_STRING);
                    out.extend_from_slice(&crate::index::disk::escape_bytes(n.to_string().as_bytes()));
                    out
                }
            }
            serde_json::Value::String(s) => {
                let mut out = Vec::with_capacity(1 + s.len());
                out.push(Self::TAG_STRING);
                out.extend_from_slice(&crate::index::disk::escape_bytes(s.as_bytes()));
                out
            }
            // Array / Object: JSON text as string (lossy but functional)
            other => {
                let s = other.to_string();
                let mut out = Vec::with_capacity(1 + s.len());
                out.push(Self::TAG_STRING);
                out.extend_from_slice(&crate::index::disk::escape_bytes(s.as_bytes()));
                out
            }
        }
    }

    /// Inverse of [`Self::encode_value`].  Handles the type tag and undoes
    /// the 0x00 escaping.  Unknown tags fall back to raw string.
    pub fn decode_value(encoded: &[u8]) -> serde_json::Value {
        if encoded.is_empty() {
            return serde_json::Value::Null;
        }
        let tag = encoded[0];
        let payload = crate::index::disk::unescape_bytes(&encoded[1..]);
        match tag {
            Self::TAG_NULL => serde_json::Value::Null,
            Self::TAG_BOOL => {
                serde_json::Value::Bool(payload.first() == Some(&0x02))
            }
            Self::TAG_INT => {
                if payload.len() == 8 {
                    let mut buf = [0u8; 8];
                    buf.copy_from_slice(&payload);
                    let ordered = u64::from_be_bytes(buf);
                    let i = (ordered ^ (1u64 << 63)) as i64;
                    serde_json::Value::from(i)
                } else {
                    serde_json::Value::Null
                }
            }
            Self::TAG_FLOAT => {
                if payload.len() == 8 {
                    let mut buf = [0u8; 8];
                    buf.copy_from_slice(&payload);
                    let ordered = u64::from_be_bytes(buf);
                    let bits = if ordered & (1u64 << 63) != 0 {
                        ordered ^ (1u64 << 63)
                    } else {
                        !ordered
                    };
                    let f = f64::from_bits(bits);
                    serde_json::Number::from_f64(f)
                        .map(serde_json::Value::Number)
                        .unwrap_or(serde_json::Value::Null)
                } else {
                    serde_json::Value::Null
                }
            }
            Self::TAG_STRING => match std::str::from_utf8(&payload) {
                Ok(s) => serde_json::Value::String(s.to_string()),
                Err(_) => {
                    serde_json::Value::String(String::from_utf8_lossy(&payload).into_owned())
                }
            },
            _ => match std::str::from_utf8(&payload) {
                Ok(s) => serde_json::Value::String(s.to_string()),
                Err(_) => {
                    serde_json::Value::String(String::from_utf8_lossy(&payload).into_owned())
                }
            },
        }
    }

    /// Rebuilds all indexes from index entries stored in the LSM-Tree.
    /// Call this on startup after loading SSTables.
    ///
    /// Index entries have key format: `__idx2__` || class_len:u16 || class || col_len:u16 || col || val_len:u16 || value || pk_len:u16 || pk
    ///
    /// If a disk-based index is missing (e.g., corrupted `.idx` file was deleted),
    /// it is automatically recreated from the LSM entries.
    pub fn rebuild_from_entries(&mut self, entries: &[(Vec<u8>, Vec<u8>)]) {
        // Group entries by (class, column) for O(n) bulk loading of in-memory indexes
        let mut per_index: HashMap<(String, String), Vec<(Vec<u8>, Vec<u8>)>> = HashMap::new();
        for (key, _value) in entries {
            if let Some((class, column, encoded_val, pk)) = Self::parse_index_key(key) {
                per_index
                    .entry((class, column))
                    .or_default()
                    .push((encoded_val, pk));
            }
        }

        for ((class, column), index_entries) in per_index {
            // Rebuild disk-based index from the LSM (source of truth).
            //
            // Always `bulk_load` (replace) rather than per-row `insert` into a
            // tree that `open_disk_indexes` just loaded: insert-into-existing
            // accumulates pages across restarts (the .idx file only grows) and
            // is O(n log n). bulk_load is O(n), dedups, and truncates the file.
            let disk_key = (class.clone(), column.clone());
            let path = self.index_path(&class, &column);
            let needs_disk = path.is_some();
            if needs_disk && !self.disk_indexes.contains_key(&disk_key) {
                if let Some(ref path) = path {
                    match BTreeIndex::create(path, &class, &column) {
                        Ok(idx) => {
                            self.disk_indexes.insert(disk_key.clone(), Mutex::new(idx));
                        }
                        Err(e) => {
                            eprintln!(
                                "Warning: failed to create disk index {}.{}: {}",
                                class, column, e
                            );
                            // Still bulk-load the in-memory index even if disk fails
                            let tree = self
                                .indexes
                                .entry((class.clone(), column.clone()))
                                .or_insert_with(|| BPlusTree::new(&class, &column));
                            tree.bulk_load(index_entries);
                            continue;
                        }
                    }
                }
            }
            if self.disk_indexes.contains_key(&disk_key) {
                let bulk_result = {
                    let disk_idx = self.disk_indexes.get(&disk_key).expect("just checked");
                    let mut idx = disk_idx.lock().unwrap_or_else(|e| e.into_inner());
                    idx.bulk_load(&index_entries)
                };
                match bulk_result {
                    Ok(()) => {
                        if let Some(disk_idx) = self.disk_indexes.get(&disk_key) {
                            let mut idx = disk_idx.lock().unwrap_or_else(|e| e.into_inner());
                            if let Err(e) = idx.flush() {
                                eprintln!(
                                    "Warning: disk index {}.{} flush failed: {}",
                                    class, column, e
                                );
                            }
                        }
                    }
                    Err(e) => {
                        // bulk_load is all-or-nothing on a fresh tree; if it
                        // fails the file is unreliable — drop it and fall back
                        // to memory-only for this index.
                        eprintln!(
                            "Warning: disk index {}.{} bulk_load failed ({}), deleting file",
                            class, column, e
                        );
                        if let Some(ref path) = path {
                            let _ = std::fs::remove_file(path);
                        }
                        self.disk_indexes.remove(&disk_key);
                    }
                }
            }

            // Bulk load in-memory index — O(n) instead of O(n²) with individual inserts
            tracing::info!(
                entries = index_entries.len(),
                "Using O(n) bulk_load for index construction"
            );
            let tree = self
                .indexes
                .entry((class.clone(), column.clone()))
                .or_insert_with(|| BPlusTree::new(&class, &column));
            tree.bulk_load(index_entries);
        }
    }

    /// Parses a v2 length-prefixed index key into its components.
    /// Format: `__idx2__` || class_len:u16 || class || col_len:u16 || col || val_len:u16 || value || pk_len:u16 || pk
    pub fn parse_index_key(key: &[u8]) -> Option<(String, String, Vec<u8>, Vec<u8>)> {
        let rest = key.strip_prefix(INDEX_PREFIX)?;
        let mut off = 0usize;

        let class_len = u16::from_le_bytes([*rest.get(off)?, *rest.get(off + 1)?]) as usize;
        off += 2;
        let class = std::str::from_utf8(rest.get(off..off + class_len)?).ok()?.to_string();
        off += class_len;

        let col_len = u16::from_le_bytes([*rest.get(off)?, *rest.get(off + 1)?]) as usize;
        off += 2;
        let column = std::str::from_utf8(rest.get(off..off + col_len)?).ok()?.to_string();
        off += col_len;

        let val_len = u16::from_le_bytes([*rest.get(off)?, *rest.get(off + 1)?]) as usize;
        off += 2;
        let encoded_val = rest.get(off..off + val_len)?.to_vec();
        off += val_len;

        let pk_len = u16::from_le_bytes([*rest.get(off)?, *rest.get(off + 1)?]) as usize;
        off += 2;
        let pk = rest.get(off..off + pk_len)?.to_vec();

        Some((class, column, encoded_val, pk))
    }

    /// Parses a legacy v1 `::`-delimited index key.  Used only for migration.
    /// Format: `__idx__{class}__{column}::{encoded_value}::{primary_key}`
    pub fn parse_index_key_v1(key: &[u8]) -> Option<(String, String, Vec<u8>, Vec<u8>)> {
        let key_str = std::str::from_utf8(key).ok()?;
        let rest = key_str.strip_prefix("__idx__")?;
        let sep1 = rest.find("__")?;
        let class = rest[..sep1].to_string();
        let after_class = &rest[sep1 + 2..];
        let sep2 = after_class.find("::")?;
        let column = after_class[..sep2].to_string();
        let after_column = &after_class[sep2 + 2..];
        let sep3 = after_column.find("::")?;
        let encoded_val = after_column[..sep3].as_bytes().to_vec();
        let pk = after_column[sep3 + 2..].as_bytes().to_vec();
        Some((class, column, encoded_val, pk))
    }
}

impl Default for IndexManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_create_and_lookup() {
        let mut mgr = IndexManager::new();
        mgr.create_index("Product", "price");
        assert!(mgr.has_index("Product", "price"));
        assert!(!mgr.has_index("Product", "name"));

        let tree = mgr.get_index("Product", "price").unwrap();
        assert!(tree.is_empty());
    }

    #[test]
    fn test_index_document() {
        let mut mgr = IndexManager::new();
        mgr.create_index("Product", "price");

        let mut doc = serde_json::Map::new();
        doc.insert("name".to_string(), json!("iPhone"));
        doc.insert("price".to_string(), json!(999));

        let entries = mgr.index_document("Product", b"pk1", &doc);
        assert_eq!(entries.len(), 1); // one index entry for "price"

        let tree = mgr.get_index("Product", "price").unwrap();
        let encoded = IndexManager::encode_value(&json!(999));
        let keys = tree.lookup(&encoded);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0], b"pk1");
    }

    #[test]
    fn test_deindex_document() {
        let mut mgr = IndexManager::new();
        mgr.create_index("Product", "price");

        let mut doc = serde_json::Map::new();
        doc.insert("price".to_string(), json!(999));

        mgr.index_document("Product", b"pk1", &doc);
        assert_eq!(mgr.get_index("Product", "price").unwrap().len(), 1);

        let removed = mgr.deindex_document("Product", b"pk1", &doc);
        assert_eq!(removed.len(), 1);
        assert!(mgr.get_index("Product", "price").unwrap().is_empty());
    }

    #[test]
    fn test_drop_index() {
        let mut mgr = IndexManager::new();
        mgr.create_index("Product", "price");
        assert!(mgr.has_index("Product", "price"));

        mgr.drop_index("Product", "price");
        assert!(!mgr.has_index("Product", "price"));
    }

    #[test]
    fn test_encode_value_ordering() {
        let a = IndexManager::encode_value(&json!(100));
        let b = IndexManager::encode_value(&json!(200));
        let c = IndexManager::encode_value(&json!(1000));
        assert!(a < b);
        assert!(b < c);
    }

    // ── Dictionary-trap regression tests (2026-10-16) ──────────────────

    /// #1 `::` in string values must round-trip through make/parse_index_key.
    #[test]
    fn test_trap1_delimiter_collision_in_value() {
        let val = IndexManager::encode_value(&json!("a::b"));
        let pk = b"pk1";
        let key = IndexManager::make_index_key("Cls", "col", &val, pk);
        let (class, column, decoded_val, decoded_pk) =
            IndexManager::parse_index_key(&key).unwrap();
        assert_eq!(class, "Cls");
        assert_eq!(column, "col");
        assert_eq!(decoded_val, val);
        assert_eq!(decoded_pk, pk);
        // The value must survive decode_value too
        let json_val = IndexManager::decode_value(&decoded_val);
        assert_eq!(json_val, json!("a::b"));
    }

    /// #2 `0x00` in string values must not break compose_key / key_value_part.
    #[test]
    fn test_trap2_null_byte_in_value() {
        use crate::index::disk::{compose_key, key_pk_part, key_value_part};
        let val = IndexManager::encode_value(&json!("hello\u{0}world"));
        let pk = b"pk1";
        let full = compose_key(&val, pk);
        let extracted_val = key_value_part(&full);
        let extracted_pk = key_pk_part(&full);
        assert_eq!(extracted_val, val.as_slice());
        assert_eq!(extracted_pk, pk);
        let json_val = IndexManager::decode_value(extracted_val);
        assert_eq!(json_val, json!("hello\u{0}world"));
    }

    /// #3 Class name containing `__` must round-trip through make/parse_index_key.
    #[test]
    fn test_trap3_double_underscore_in_class_name() {
        let val = IndexManager::encode_value(&json!("x"));
        let pk = b"pk1";
        let key = IndexManager::make_index_key("my__table", "col", &val, pk);
        let (class, column, decoded_val, decoded_pk) =
            IndexManager::parse_index_key(&key).unwrap();
        assert_eq!(class, "my__table");
        assert_eq!(column, "col");
        assert_eq!(decoded_val, val);
        assert_eq!(decoded_pk, pk);
    }

    /// #4 Non-UTF-8 values must round-trip (no silent drop).
    #[test]
    fn test_trap4_non_utf8_value() {
        // String containing invalid UTF-8 is not expressible in serde_json::Value,
        // but binary pk can be non-UTF-8.  Verify pk round-trips.
        let val = IndexManager::encode_value(&json!("safe"));
        let pk: Vec<u8> = vec![0xFF, 0xFE, 0x00, 0x01];
        let key = IndexManager::make_index_key("Cls", "col", &val, &pk);
        let (_, _, _, decoded_pk) = IndexManager::parse_index_key(&key).unwrap();
        assert_eq!(decoded_pk, pk);
    }

    /// #5 Bool must be distinguishable from string "0"/"1".
    #[test]
    fn test_trap5_bool_vs_string_collision() {
        let b_true = IndexManager::encode_value(&json!(true));
        let b_false = IndexManager::encode_value(&json!(false));
        let s_0 = IndexManager::encode_value(&json!("0"));
        let s_1 = IndexManager::encode_value(&json!("1"));
        assert_ne!(b_true, s_1);
        assert_ne!(b_false, s_0);
        assert_ne!(b_true, b_false);
        // Round-trip
        assert_eq!(IndexManager::decode_value(&b_true), json!(true));
        assert_eq!(IndexManager::decode_value(&b_false), json!(false));
        assert_eq!(IndexManager::decode_value(&s_0), json!("0"));
        assert_eq!(IndexManager::decode_value(&s_1), json!("1"));
    }

    /// #6 Negative floats must sort before positives; int/float must not collide.
    #[test]
    fn test_trap6_numeric_ordering() {
        // Float ordering
        let f_neg2 = IndexManager::encode_value(&json!(-2.0));
        let f_neg1 = IndexManager::encode_value(&json!(-1.0));
        let f_pos1 = IndexManager::encode_value(&json!(1.0));
        let f_pos2 = IndexManager::encode_value(&json!(2.0));
        assert!(f_neg2 < f_neg1);
        assert!(f_neg1 < f_pos1);
        assert!(f_pos1 < f_pos2);

        // Integer ordering (including negatives)
        let i_min = IndexManager::encode_value(&json!(i64::MIN));
        let i_neg = IndexManager::encode_value(&json!(-1));
        let i_zero = IndexManager::encode_value(&json!(0));
        let i_one = IndexManager::encode_value(&json!(1));
        let i_max = IndexManager::encode_value(&json!(i64::MAX));
        assert!(i_min < i_neg);
        assert!(i_neg < i_zero);
        assert!(i_zero < i_one);
        assert!(i_one < i_max);

        // Int and Float must not collide
        let i_1 = IndexManager::encode_value(&json!(1));
        let f_1 = IndexManager::encode_value(&json!(1.0));
        assert_ne!(i_1, f_1);

        // Round-trip
        assert_eq!(IndexManager::decode_value(&i_neg), json!(-1));
        assert_eq!(IndexManager::decode_value(&i_max), json!(i64::MAX));
    }

    /// Type tag ordering: Null < Bool < Int < Float < String
    #[test]
    fn test_type_tag_ordering() {
        let null_v = IndexManager::encode_value(&json!(null));
        let bool_v = IndexManager::encode_value(&json!(false));
        let int_v = IndexManager::encode_value(&json!(0));
        let float_v = IndexManager::encode_value(&json!(0.0));
        let str_v = IndexManager::encode_value(&json!("a"));
        assert!(null_v < bool_v);
        assert!(bool_v < int_v);
        assert!(int_v < float_v);
        assert!(float_v < str_v);
    }

    /// Escape round-trip for edge cases.
    #[test]
    fn test_escape_roundtrip() {
        use crate::index::disk::{escape_bytes, unescape_bytes};
        let cases: Vec<Vec<u8>> = vec![
            vec![],
            vec![0x00],
            vec![0x01],
            vec![0x00, 0x00],
            vec![0x00, 0xFF],
            vec![0xFF, 0x00, 0xFF],
            b"hello".to_vec(),
            {
                let mut v = b"hello".to_vec();
                v.push(0);
                v.extend_from_slice(b"world");
                v
            },
        ];
        for case in cases {
            let escaped = escape_bytes(&case);
            // Escaped output must never contain 0x00 0x00
            let has_sep = escaped.windows(2).any(|w| w == [0x00, 0x00]);
            assert!(!has_sep, "escaped output must not contain 0x00 0x00");
            let unescaped = unescape_bytes(&escaped);
            assert_eq!(unescaped, case, "roundtrip failed for {:?}", case);
        }
    }

    #[test]
    fn test_corrupted_idx_file_deleted_and_rebuilt() {
        use crate::index::disk::BTreeIndex;
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().join("data");
        std::fs::create_dir_all(&data_dir).unwrap();

        // Create a valid disk index
        let idx_path = data_dir.join("Product_price.idx");
        {
            let mut idx = BTreeIndex::create(&idx_path, "Product", "price").unwrap();
            idx.insert(b"100", b"pk1".to_vec()).unwrap();
            idx.flush().unwrap();
        }
        assert!(idx_path.exists());

        // Corrupt the file (overwrite with garbage)
        std::fs::write(&idx_path, b"garbage data that is not a valid index").unwrap();

        // Open manager — should detect corruption and delete the file
        let mut mgr = IndexManager::with_disk_storage(&data_dir, IndexStorageMode::DiskBased);
        mgr.open_disk_indexes().unwrap();

        // Corrupted file should be deleted
        assert!(!idx_path.exists(), "corrupted .idx file should be deleted");

        // Now rebuild from LSM entries (use current encode_value format)
        let encoded_100 = IndexManager::encode_value(&json!(100));
        let encoded_200 = IndexManager::encode_value(&json!(200));
        let key1 = IndexManager::make_index_key("Product", "price", &encoded_100, b"pk1");
        let key2 = IndexManager::make_index_key("Product", "price", &encoded_200, b"pk2");
        let entries = vec![
            (key1, Vec::new()),
            (key2, Vec::new()),
        ];
        mgr.rebuild_from_entries(&entries);

        // Disk index should be recreated
        assert!(
            idx_path.exists(),
            ".idx file should be recreated after rebuild"
        );
        assert!(mgr.has_index("Product", "price"));

        // Verify data is accessible
        let pks = mgr.lookup_eq("Product", "price", &json!(100)).unwrap();
        assert_eq!(pks.len(), 1);
        assert_eq!(pks[0], b"pk1");
    }

    // ── Hybrid index mode tests ──────────────────────────────────────

    #[test]
    fn test_hybrid_bulk_load_and_lookup() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().join("indexes");
        std::fs::create_dir_all(&data_dir).unwrap();

        let mut mgr = IndexManager::with_disk_storage(
            &data_dir,
            IndexStorageMode::Hybrid {
                threshold_entries: 100,
            },
        );
        mgr.create_index("BioAssay", "assay_type");

        // Bulk-load 263 entries (above threshold → disk-backed)
        let entries: Vec<(Vec<u8>, Vec<u8>)> = (0..263)
            .map(|i| {
                let val = if i % 2 == 0 {
                    IndexManager::encode_value(&json!("B"))
                } else {
                    IndexManager::encode_value(&json!("A"))
                };
                (val, format!("pk_{}", i).into_bytes())
            })
            .collect();
        mgr.bulk_load_index("BioAssay", "assay_type", &entries).unwrap();

        // lookup_eq should return all matching pks
        let pks_b = mgr
            .lookup_eq("BioAssay", "assay_type", &json!("B"))
            .unwrap();
        assert_eq!(pks_b.len(), 132); // 0..263 even = 132

        let pks_a = mgr
            .lookup_eq("BioAssay", "assay_type", &json!("A"))
            .unwrap();
        assert_eq!(pks_a.len(), 131); // 0..263 odd = 131

        // After bulk_load, mem_complete should be set (skips disk merge on lookup)
        let key = ("BioAssay".to_string(), "assay_type".to_string());
        assert!(
            mgr.mem_complete.contains(&key),
            "mem_complete should be set after bulk_load"
        );
    }

    #[test]
    fn test_hybrid_open_disk_indexes_merge() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().join("indexes");
        std::fs::create_dir_all(&data_dir).unwrap();

        // Phase 1: Create index with data, then drop manager (simulate shutdown)
        {
            let mut mgr = IndexManager::with_disk_storage(
                &data_dir,
                IndexStorageMode::Hybrid {
                    threshold_entries: 50,
                },
            );
            mgr.create_index("Drug", "drug_name");

            let entries: Vec<(Vec<u8>, Vec<u8>)> = (0..100)
                .map(|i| {
                    let name = if i < 50 { "aspirin" } else { "ibuprofen" };
                    (IndexManager::encode_value(&json!(name)), format!("drug_{}", i).into_bytes())
                })
                .collect();
            mgr.bulk_load_index("Drug", "drug_name", &entries).unwrap();
            mgr.flush_disk_indexes();
        }

        // Phase 2: Reopen — should load disk indexes and merge with memory
        {
            let mut mgr = IndexManager::with_disk_storage(
                &data_dir,
                IndexStorageMode::Hybrid {
                    threshold_entries: 50,
                },
            );
            mgr.open_disk_indexes().unwrap();

            // Index should be found
            assert!(mgr.has_index("Drug", "drug_name"));

            // mem_complete should NOT be set (disk has history, memory is empty)
            let key = ("Drug".to_string(), "drug_name".to_string());
            assert!(
                !mgr.mem_complete.contains(&key),
                "mem_complete should be cleared after open_disk_indexes"
            );

            // Lookup should merge memory (empty) + disk (100 entries)
            let pks = mgr
                .lookup_eq("Drug", "drug_name", &json!("aspirin"))
                .unwrap();
            assert_eq!(pks.len(), 50);

            let pks = mgr
                .lookup_eq("Drug", "drug_name", &json!("ibuprofen"))
                .unwrap();
            assert_eq!(pks.len(), 50);
        }
    }

    /// Regression: mem+disk overlap must not double-return the same pk on the
    /// LIMIT path. `lookup_eq_read` deduped; `lookup_eq_read_limit` used to
    /// concatenate and emit the row twice (COUNT=1 vs SELECT=2).
    #[test]
    fn test_lookup_eq_limit_dedups_mem_disk_overlap() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().join("indexes");
        std::fs::create_dir_all(&data_dir).unwrap();

        let key = ("Compound".to_string(), "id".to_string());
        let mut mgr = IndexManager::with_disk_storage(
            &data_dir,
            IndexStorageMode::Hybrid {
                threshold_entries: 10,
            },
        );
        mgr.create_index("Compound", "id");

        // Disk holds the history; memory will be re-seeded with the same pks
        // (the post-restart / rebuild state).
        let entries: Vec<(Vec<u8>, Vec<u8>)> = vec![
            (
                IndexManager::encode_value(&json!("CHEMBL25")),
                b"pk_a".to_vec(),
            ),
            (
                IndexManager::encode_value(&json!("CHEMBL25")),
                b"pk_b".to_vec(),
            ),
            (
                IndexManager::encode_value(&json!("CHEMBL99")),
                b"pk_c".to_vec(),
            ),
        ];
        mgr.bulk_load_index("Compound", "id", &entries).unwrap();
        mgr.flush_disk_indexes();

        // Reopen: disk has entries, mem starts empty, mem_complete cleared.
        let mut mgr = IndexManager::with_disk_storage(
            &data_dir,
            IndexStorageMode::Hybrid {
                threshold_entries: 10,
            },
        );
        mgr.open_disk_indexes().unwrap();
        assert!(!mgr.mem_complete.contains(&key));

        // Seed memory with the SAME pks that disk already holds (INSERT path).
        for (val, pk) in &entries {
            let mut doc = serde_json::Map::new();
            let v = IndexManager::decode_value(val);
            doc.insert("id".to_string(), v);
            mgr.index_document("Compound", pk, &doc);
        }

        let limited = mgr
            .lookup_eq_read_limit("Compound", "id", &json!("CHEMBL25"), 10)
            .unwrap();
        let unlimited = mgr
            .lookup_eq_read("Compound", "id", &json!("CHEMBL25"))
            .unwrap();
        assert_eq!(
            limited.len(),
            unlimited.len(),
            "LIMIT path must not double-count mem+disk overlap: limited={:?} unlimited={:?}",
            limited,
            unlimited
        );
        assert_eq!(limited.len(), 2, "two distinct pks for CHEMBL25");
        let mut sorted = limited.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), limited.len(), "no duplicate pks in result");
    }

    #[test]
    fn test_hybrid_mem_complete_marker_lifecycle() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().join("indexes");
        std::fs::create_dir_all(&data_dir).unwrap();

        let mut mgr = IndexManager::with_disk_storage(
            &data_dir,
            IndexStorageMode::Hybrid {
                threshold_entries: 10,
            },
        );
        let key = ("Product".to_string(), "color".to_string());

        // After create_index: mem_complete is NOT set
        mgr.create_index("Product", "color");
        assert!(!mgr.mem_complete.contains(&key));

        // After bulk_load: mem_complete IS set
        let entries: Vec<(Vec<u8>, Vec<u8>)> = vec![
            (b"red".to_vec(), b"p1".to_vec()),
            (b"blue".to_vec(), b"p2".to_vec()),
        ];
        mgr.bulk_load_index("Product", "color", &entries).unwrap();
        assert!(mgr.mem_complete.contains(&key));

        // After drop_index: mem_complete is cleared
        mgr.drop_index("Product", "color");
        assert!(!mgr.mem_complete.contains(&key));
    }

    #[test]
    fn test_hybrid_range_scan_after_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().join("indexes");
        std::fs::create_dir_all(&data_dir).unwrap();

        // Phase 1: Build index with numeric values
        {
            let mut mgr = IndexManager::with_disk_storage(
                &data_dir,
                IndexStorageMode::Hybrid {
                    threshold_entries: 50,
                },
            );
            mgr.create_index("Item", "score");
            let entries: Vec<(Vec<u8>, Vec<u8>)> = (0..200)
                .map(|i| {
                    let val = IndexManager::encode_value(&json!(i));
                    (val, format!("item_{}", i).into_bytes())
                })
                .collect();
            mgr.bulk_load_index("Item", "score", &entries).unwrap();
            mgr.flush_disk_indexes();
        }

        // Phase 2: Reopen and do range scan
        {
            let mut mgr = IndexManager::with_disk_storage(
                &data_dir,
                IndexStorageMode::Hybrid {
                    threshold_entries: 50,
                },
            );
            mgr.open_disk_indexes().unwrap();

            // Range [50, 99] should return 50 items
            let pks = mgr
                .lookup_range("Item", "score", Some(&json!(50)), Some(&json!(99)))
                .unwrap();
            assert_eq!(pks.len(), 50);

            // GT 150 should return 49 items (151..199)
            let pks = mgr
                .lookup_gt("Item", "score", &json!(150))
                .unwrap();
            assert_eq!(pks.len(), 49);
        }
    }

    /// merge_pks must NOT destroy index-value order.
    ///
    /// The secondary index is keyed by column value; pks are opaque row ids
    /// (`class::{ts<<64|seq}`) that sort in *insertion* order. Range lookups
    /// must return pks in **value order** so callers can emit sorted rows.
    /// When both mem and disk contribute, merging by pk sort scrambles that.
    #[test]
    fn test_range_lookup_preserves_value_order_on_merge() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().join("indexes");
        std::fs::create_dir_all(&data_dir).unwrap();

        let mut mgr = IndexManager::with_disk_storage(
            &data_dir,
            IndexStorageMode::Hybrid {
                threshold_entries: 10,
            },
        );
        mgr.create_index("Compound", "id");

        // Insert in an order that does NOT match value order. Pks are opaque
        // row ids — here deliberately reverse-sorted vs the values.
        let pairs = [
            ("CHEMBL300", "pk_zzz"),
            ("CHEMBL100", "pk_yyy"),
            ("CHEMBL200", "pk_xxx"),
        ];
        for (val, pk) in pairs {
            let doc = serde_json::Map::from_iter(vec![(
                "id".to_string(),
                serde_json::Value::String(val.to_string()),
            )]);
            mgr.index_document("Compound", pk.as_bytes(), &doc);
        }
        // Drop mem_complete so lookups take the mem+disk merge path
        // (the live situation after restart / open_disk_indexes / new writes).
        mgr.mem_complete.remove(&("Compound".to_string(), "id".to_string()));

        // Gte-style range: value order must be CHEMBL100, CHEMBL200, CHEMBL300
        let pks = mgr
            .lookup_range_read(
                "Compound",
                "id",
                Some(&serde_json::Value::String("CHEMBL100".into())),
                None,
            )
            .expect("range should hit");
        let got: Vec<String> = pks
            .iter()
            .map(|p| String::from_utf8_lossy(p).into_owned())
            .collect();
        assert_eq!(
            got,
            vec!["pk_yyy", "pk_xxx", "pk_zzz"],
            "Gte merge path must keep value order, got {got:?}"
        );

        // Gt-style lookup must agree on order too.
        let pks = mgr
            .lookup_gt_read("Compound", "id", &serde_json::Value::String("CHEMBL100".into()))
            .expect("gt should hit");
        let got: Vec<String> = pks
            .iter()
            .map(|p| String::from_utf8_lossy(p).into_owned())
            .collect();
        assert_eq!(
            got,
            vec!["pk_xxx", "pk_zzz"],
            "Gt merge path must keep value order, got {got:?}"
        );
    }
}
