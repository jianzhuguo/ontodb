//! Index manager: manages secondary indexes and integrates with the LSM engine.
//!
//! Supports both in-memory B+Trees (for small indexes) and disk-based B+Trees
//! (for indexes larger than available RAM). Disk-based indexes are stored as
//! dedicated `.idx` files with 4KB pages and a buffer pool for caching.
//!
//! Index entries are also persisted in the LSM-Tree with key format:
//!   __idx__{class}__{column}::{encoded_value}::{primary_key}
//!
//! On startup, the manager can rebuild in-memory trees from LSM entries or
//! open existing disk-based index files.

use crate::index::btree::BPlusTree;
use crate::index::disk::BTreeIndex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Prefix for index keys in the LSM-Tree.
const INDEX_PREFIX: &[u8] = b"__idx__";

/// Configuration for index storage mode.
#[derive(Debug, Clone)]
pub enum IndexStorageMode {
    /// All indexes stored in memory (default, fast but limited by RAM).
    InMemory,
    /// All indexes stored on disk (slower but handles large datasets).
    DiskBased,
    /// Hybrid: small indexes in memory, large ones on disk based on threshold.
    Hybrid { threshold_entries: usize },
}

impl Default for IndexStorageMode {
    fn default() -> Self {
        IndexStorageMode::InMemory
    }
}

/// Manages all secondary indexes.
pub struct IndexManager {
    /// In-memory B+Tree indexes, keyed by (class, column).
    indexes: HashMap<(String, String), BPlusTree>,
    /// Disk-based B+Tree indexes, keyed by (class, column).
    disk_indexes: HashMap<(String, String), BTreeIndex>,
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
            data_dir: None,
            _storage_mode: IndexStorageMode::default(),
        }
    }

    /// Creates a new index manager with disk-based storage support.
    pub fn with_disk_storage(data_dir: &Path, mode: IndexStorageMode) -> Self {
        Self {
            indexes: HashMap::new(),
            disk_indexes: HashMap::new(),
            data_dir: Some(data_dir.to_path_buf()),
            _storage_mode: mode,
        }
    }

    /// Returns the path for a disk-based index file.
    fn index_path(&self, class: &str, column: &str) -> Option<PathBuf> {
        self.data_dir.as_ref().map(|dir| {
            dir.join(format!("{}_{}.idx", class, column))
        })
    }

    /// Creates a new index on a class.column.
    /// Returns Ok(()) if created, or if it already exists.
    pub fn create_index(&mut self, class: &str, column: &str) {
        let key = (class.to_string(), column.to_string());

        // Always create in-memory index
        self.indexes
            .entry(key.clone())
            .or_insert_with(|| BPlusTree::new(class, column));

        // Create disk-based index if configured
        if let Some(path) = self.index_path(class, column) {
            if !self.disk_indexes.contains_key(&key) {
                match BTreeIndex::create(&path, class, column) {
                    Ok(idx) => {
                        self.disk_indexes.insert(key, idx);
                    }
                    Err(e) => {
                        eprintln!("Warning: failed to create disk index for {}.{}: {}", class, column, e);
                    }
                }
            }
        }
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
                let filename = path.file_stem().and_then(|f| f.to_str()).unwrap_or("");
                if let Some((class, column)) = filename.split_once('_') {
                    // Validate before opening — if corrupted, delete and skip
                    if !BTreeIndex::validate(&path) {
                        eprintln!("Warning: corrupted disk index {}, deleting for rebuild", path.display());
                        let _ = std::fs::remove_file(&path);
                        continue;
                    }
                    match BTreeIndex::open(&path) {
                        Ok(idx) => {
                            let key = (class.to_string(), column.to_string());
                            self.disk_indexes.insert(key, idx);
                            // Also create in-memory index
                            self.indexes
                                .entry((class.to_string(), column.to_string()))
                                .or_insert_with(|| BPlusTree::new(class, column));
                        }
                        Err(e) => {
                            eprintln!("Warning: failed to open disk index {}, deleting: {}", path.display(), e);
                            let _ = std::fs::remove_file(&path);
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Drops an index on a class.column.
    pub fn drop_index(&mut self, class: &str, column: &str) -> bool {
        let key = (class.to_string(), column.to_string());
        let in_memory_removed = self.indexes.remove(&key).is_some();

        // Also remove disk-based index
        let disk_removed = if let Some(mut idx) = self.disk_indexes.remove(&key) {
            let _ = idx.flush();
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
        let index_keys: Vec<(String, String)> = self.indexes.keys()
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
                if let Some(disk_idx) = self.disk_indexes.get_mut(&(idx_class.clone(), idx_col.clone())) {
                    if let Err(e) = disk_idx.insert(&encoded, primary_key.to_vec()) {
                        eprintln!("Warning: disk index insert failed for {}.{}: {}", idx_class, idx_col, e);
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
        let index_keys: Vec<(String, String)> = self.indexes.keys()
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
                if let Some(disk_idx) = self.disk_indexes.get_mut(&(idx_class.clone(), idx_col.clone())) {
                    if let Err(e) = disk_idx.remove(&encoded, primary_key) {
                        eprintln!("Warning: disk index remove failed for {}.{}: {}", idx_class, idx_col, e);
                    }
                }

                removed_keys.push(Self::make_index_key(class, idx_col, &encoded, primary_key));
            }
        }

        removed_keys
    }

    /// Returns all indexes for a given class (both in-memory and disk-based).
    pub fn indexes_for_class(&self, class: &str) -> Vec<&str> {
        let mut cols: Vec<&str> = self.indexes
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
        for (_, disk_idx) in &mut self.disk_indexes {
            let _ = disk_idx.flush();
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

    /// Looks up primary keys for a given class, column, and value using the index.
    /// Returns None if no index exists on that column.
    /// Checks both in-memory and disk-based indexes.
    pub fn lookup_eq(&mut self, class: &str, column: &str, value: &serde_json::Value) -> Option<Vec<Vec<u8>>> {
        let key = (class.to_string(), column.to_string());
        let encoded = Self::encode_value(value);

        // Try in-memory index first
        if let Some(tree) = self.indexes.get(&key) {
            return Some(tree.lookup(&encoded));
        }

        // Fall back to disk-based index
        if let Some(disk_idx) = self.disk_indexes.get_mut(&key) {
            return disk_idx.lookup(&encoded).ok();
        }

        None
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
        let key = (class.to_string(), column.to_string());
        let low_bytes = low.map(|v| Self::encode_value(v));
        let high_bytes = high.map(|v| Self::encode_value(v));

        // Try in-memory index first
        if let Some(tree) = self.indexes.get(&key) {
            return Some(tree.range_scan(low_bytes.as_deref(), high_bytes.as_deref()));
        }

        // Fall back to disk-based index
        if let Some(disk_idx) = self.disk_indexes.get_mut(&key) {
            return disk_idx.range_scan(low_bytes.as_deref(), high_bytes.as_deref()).ok();
        }

        None
    }

    /// Looks up primary keys for a GT condition.
    /// Checks both in-memory and disk-based indexes.
    pub fn lookup_gt(&mut self, class: &str, column: &str, value: &serde_json::Value) -> Option<Vec<Vec<u8>>> {
        let key = (class.to_string(), column.to_string());
        let encoded = Self::encode_value(value);

        // Try in-memory index first
        if let Some(tree) = self.indexes.get(&key) {
            return Some(tree.gt_scan(&encoded));
        }

        // Fall back to disk-based index (use range_scan with lo=encoded, hi=None)
        if let Some(disk_idx) = self.disk_indexes.get_mut(&key) {
            // GT means strictly greater than, so we need to find the first key > encoded
            // and scan from there. Use range_scan with lo just past encoded.
            // For simplicity, use range_scan with lo=encoded+1 byte
            let mut lo = encoded.clone();
            lo.push(0u8); // This makes it strictly greater
            return disk_idx.range_scan(Some(&lo), None).ok();
        }

        None
    }

    /// Looks up primary keys for a LT condition.
    /// Checks both in-memory and disk-based indexes.
    pub fn lookup_lt(&mut self, class: &str, column: &str, value: &serde_json::Value) -> Option<Vec<Vec<u8>>> {
        let key = (class.to_string(), column.to_string());
        let encoded = Self::encode_value(value);

        // Try in-memory index first
        if let Some(tree) = self.indexes.get(&key) {
            return Some(tree.lt_scan(&encoded));
        }

        // Fall back to disk-based index (use range_scan with lo=None, hi=encoded-1)
        if let Some(disk_idx) = self.disk_indexes.get_mut(&key) {
            return disk_idx.range_scan(None, Some(&encoded)).ok();
        }

        None
    }

    // ── Read-only lookup methods (for concurrent read path, &self) ──

    /// Read-only equality lookup using in-memory index only.
    pub fn lookup_eq_read(&self, class: &str, column: &str, value: &serde_json::Value) -> Option<Vec<Vec<u8>>> {
        let key = (class.to_string(), column.to_string());
        let encoded = Self::encode_value(value);
        self.indexes.get(&key).map(|tree| tree.lookup(&encoded))
    }

    /// Read-only range scan using in-memory index only.
    pub fn lookup_range_read(
        &self,
        class: &str,
        column: &str,
        low: Option<&serde_json::Value>,
        high: Option<&serde_json::Value>,
    ) -> Option<Vec<Vec<u8>>> {
        let key = (class.to_string(), column.to_string());
        let low_bytes = low.map(|v| Self::encode_value(v));
        let high_bytes = high.map(|v| Self::encode_value(v));
        self.indexes.get(&key).map(|tree| tree.range_scan(low_bytes.as_deref(), high_bytes.as_deref()))
    }

    /// Read-only GT lookup using in-memory index only.
    pub fn lookup_gt_read(&self, class: &str, column: &str, value: &serde_json::Value) -> Option<Vec<Vec<u8>>> {
        let key = (class.to_string(), column.to_string());
        let encoded = Self::encode_value(value);
        self.indexes.get(&key).map(|tree| tree.gt_scan(&encoded))
    }

    /// Read-only LT lookup using in-memory index only.
    pub fn lookup_lt_read(&self, class: &str, column: &str, value: &serde_json::Value) -> Option<Vec<Vec<u8>>> {
        let key = (class.to_string(), column.to_string());
        let encoded = Self::encode_value(value);
        self.indexes.get(&key).map(|tree| tree.lt_scan(&encoded))
    }

    /// Builds the index key format:
    /// __idx__{class}__{column}::{encoded_value}::{primary_key}
    pub fn make_index_key(class: &str, column: &str, value: &[u8], primary_key: &[u8]) -> Vec<u8> {
        let mut key = Vec::new();
        key.extend_from_slice(INDEX_PREFIX);
        key.extend_from_slice(class.as_bytes());
        key.extend_from_slice(b"__");
        key.extend_from_slice(column.as_bytes());
        key.extend_from_slice(b"::");
        key.extend_from_slice(value);
        key.extend_from_slice(b"::");
        key.extend_from_slice(primary_key);
        key
    }

    /// Encodes a JSON value to bytes for index key comparison.
    /// Numbers are encoded with zero-padding for correct lexicographic ordering.
    pub fn encode_value(val: &serde_json::Value) -> Vec<u8> {
        match val {
            serde_json::Value::String(s) => s.as_bytes().to_vec(),
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    // Encode with offset to ensure correct sort order for negative values
                    // i64 range: -9223372036854775808..=9223372036854775807
                    // Add offset to shift all values to non-negative range
                    let offset_i = (i as i128 + 9223372036854775808i128) as u128;
                    format!("{:020}", offset_i).into_bytes()
                } else if let Some(f) = n.as_f64() {
                    format!("{:020.10}", f).into_bytes()
                } else {
                    n.to_string().into_bytes()
                }
            }
            serde_json::Value::Bool(b) => {
                if *b { b"1" } else { b"0" }.to_vec()
            }
            _ => val.to_string().into_bytes(),
        }
    }

    /// Rebuilds all indexes from index entries stored in the LSM-Tree.
    /// Call this on startup after loading SSTables.
    ///
    /// Index entries have key format: `__idx__{class}__{column}::{encoded_value}::{primary_key}`
    ///
    /// If a disk-based index is missing (e.g., corrupted `.idx` file was deleted),
    /// it is automatically recreated from the LSM entries.
    pub fn rebuild_from_entries(&mut self, entries: &[(Vec<u8>, Vec<u8>)]) {
        for (key, _value) in entries {
            if let Some((class, column, encoded_val, pk)) = Self::parse_index_key(key) {
                // Rebuild in-memory index
                let tree = self
                    .indexes
                    .entry((class.clone(), column.clone()))
                    .or_insert_with(|| BPlusTree::new(&class, &column));
                tree.insert(encoded_val.clone(), pk.clone());

                // Rebuild disk-based index — create if missing
                let disk_key = (class.clone(), column.clone());
                if !self.disk_indexes.contains_key(&disk_key) {
                    // Disk index missing (corrupted file was deleted or never existed) — recreate
                    if let Some(path) = self.index_path(&class, &column) {
                        match BTreeIndex::create(&path, &class, &column) {
                            Ok(idx) => {
                                self.disk_indexes.insert(disk_key.clone(), idx);
                            }
                            Err(e) => {
                                eprintln!("Warning: failed to create disk index {}.{}: {}", class, column, e);
                                continue;
                            }
                        }
                    }
                }
                if let Some(disk_idx) = self.disk_indexes.get_mut(&disk_key) {
                    if let Err(e) = disk_idx.insert(&encoded_val, pk) {
                        eprintln!("Warning: disk index rebuild failed for {}.{}: {}", class, column, e);
                    }
                }
            }
        }

        // Flush all disk indexes after rebuild (with fsync)
        for (_, disk_idx) in &mut self.disk_indexes {
            let _ = disk_idx.flush();
        }
    }

    /// Parses an index key into its components.
    /// Format: `__idx__{class}__{column}::{encoded_value}::{primary_key}`
    fn parse_index_key(key: &[u8]) -> Option<(String, String, Vec<u8>, Vec<u8>)> {
        let key_str = std::str::from_utf8(key).ok()?;

        // Strip prefix
        let rest = key_str.strip_prefix("__idx__")?;

        // Find the `__` separator between class and column
        let sep1 = rest.find("__")?;
        let class = rest[..sep1].to_string();
        let after_class = &rest[sep1 + 2..];

        // Find the `::` separator between column and encoded value
        let sep2 = after_class.find("::")?;
        let column = after_class[..sep2].to_string();
        let after_column = &after_class[sep2 + 2..];

        // Find the `::` separator between encoded value and primary key
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

        // Now rebuild from LSM entries
        let entries = vec![
            (b"__idx__Product__price::00000000000000000100::pk1".to_vec(), Vec::new()),
            (b"__idx__Product__price::00000000000000000200::pk2".to_vec(), Vec::new()),
        ];
        mgr.rebuild_from_entries(&entries);

        // Disk index should be recreated
        assert!(idx_path.exists(), ".idx file should be recreated after rebuild");
        assert!(mgr.has_index("Product", "price"));

        // Verify data is accessible
        let pks = mgr.lookup_eq("Product", "price", &json!(100)).unwrap();
        assert_eq!(pks.len(), 1);
        assert_eq!(pks[0], b"pk1");
    }
}
