//! Index manager: manages secondary indexes and integrates with the LSM engine.
//!
//! Index entries are persisted in the LSM-Tree with key format:
//!   __idx__{class}__{column}::{encoded_value}::{primary_key}
//!
//! On startup, the manager rebuilds in-memory B+Trees by scanning index entries.
//! On write operations, the manager updates both the in-memory tree and the LSM engine.

use crate::index::btree::BPlusTree;
use std::collections::HashMap;

/// Prefix for index keys in the LSM-Tree.
const INDEX_PREFIX: &[u8] = b"__idx__";

/// Manages all secondary indexes.
pub struct IndexManager {
    /// In-memory B+Tree indexes, keyed by (class, column).
    indexes: HashMap<(String, String), BPlusTree>,
}

impl IndexManager {
    /// Creates a new empty index manager.
    pub fn new() -> Self {
        Self {
            indexes: HashMap::new(),
        }
    }

    /// Creates a new index on a class.column.
    /// Returns Ok(()) if created, or if it already exists.
    pub fn create_index(&mut self, class: &str, column: &str) {
        let key = (class.to_string(), column.to_string());
        self.indexes
            .entry(key)
            .or_insert_with(|| BPlusTree::new(class, column));
    }

    /// Drops an index on a class.column.
    pub fn drop_index(&mut self, class: &str, column: &str) -> bool {
        let key = (class.to_string(), column.to_string());
        self.indexes.remove(&key).is_some()
    }

    /// Returns true if an index exists on the given class.column.
    pub fn has_index(&self, class: &str, column: &str) -> bool {
        self.indexes.contains_key(&(class.to_string(), column.to_string()))
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

        for ((idx_class, idx_col), tree) in &mut self.indexes {
            if idx_class != class {
                continue;
            }
            if let Some(val) = doc.get(idx_col.as_str()) {
                let encoded = Self::encode_value(val);
                tree.insert(encoded.clone(), primary_key.to_vec());
                index_entries.push((
                    Self::make_index_key(class, idx_col, &encoded, primary_key),
                    Vec::new(), // value is empty; key contains all info
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

        for ((idx_class, idx_col), tree) in &mut self.indexes {
            if idx_class != class {
                continue;
            }
            if let Some(val) = doc.get(idx_col.as_str()) {
                let encoded = Self::encode_value(val);
                tree.remove(&encoded, primary_key);
                removed_keys.push(Self::make_index_key(class, idx_col, &encoded, primary_key));
            }
        }

        removed_keys
    }

    /// Returns all indexes for a given class.
    pub fn indexes_for_class(&self, class: &str) -> Vec<&str> {
        self.indexes
            .keys()
            .filter(|(c, _)| c == class)
            .map(|(_, col)| col.as_str())
            .collect()
    }

    /// Returns the number of indexes.
    pub fn index_count(&self) -> usize {
        self.indexes.len()
    }

    /// Looks up primary keys for a given class, column, and value using the index.
    /// Returns None if no index exists on that column.
    pub fn lookup_eq(&self, class: &str, column: &str, value: &serde_json::Value) -> Option<Vec<Vec<u8>>> {
        let tree = self.indexes.get(&(class.to_string(), column.to_string()))?;
        let encoded = Self::encode_value(value);
        Some(tree.lookup(&encoded))
    }

    /// Range scan on an indexed column.
    /// Returns None if no index exists.
    pub fn lookup_range(
        &self,
        class: &str,
        column: &str,
        low: Option<&serde_json::Value>,
        high: Option<&serde_json::Value>,
    ) -> Option<Vec<Vec<u8>>> {
        let tree = self.indexes.get(&(class.to_string(), column.to_string()))?;
        let low_bytes = low.map(|v| Self::encode_value(v));
        let high_bytes = high.map(|v| Self::encode_value(v));
        Some(tree.range_scan(
            low_bytes.as_deref(),
            high_bytes.as_deref(),
        ))
    }

    /// Looks up primary keys for a GT condition.
    pub fn lookup_gt(&self, class: &str, column: &str, value: &serde_json::Value) -> Option<Vec<Vec<u8>>> {
        let tree = self.indexes.get(&(class.to_string(), column.to_string()))?;
        let encoded = Self::encode_value(value);
        Some(tree.gt_scan(&encoded))
    }

    /// Looks up primary keys for a LT condition.
    pub fn lookup_lt(&self, class: &str, column: &str, value: &serde_json::Value) -> Option<Vec<Vec<u8>>> {
        let tree = self.indexes.get(&(class.to_string(), column.to_string()))?;
        let encoded = Self::encode_value(value);
        Some(tree.lt_scan(&encoded))
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
                    // Encode as zero-padded 20-digit string for correct sort order
                    format!("{:020}", i).into_bytes()
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
    pub fn rebuild_from_entries(&mut self, entries: &[(Vec<u8>, Vec<u8>)]) {
        for (key, _value) in entries {
            if let Some((class, column, encoded_val, pk)) = Self::parse_index_key(key) {
                let tree = self
                    .indexes
                    .entry((class.clone(), column.clone()))
                    .or_insert_with(|| BPlusTree::new(&class, &column));
                tree.insert(encoded_val, pk);
            }
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
}
