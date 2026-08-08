//! MemTable: In-memory sorted key-value store.
//!
//! The MemTable is the write buffer. All writes go here first.
//! When it reaches the size limit, it's flushed to an SSTable on disk.
//!
//! Backed by `std::collections::BTreeMap` with composite keys for sorted access.
//! The `scan_prefix()` method uses BTreeMap range queries for efficient prefix scanning.

use onto_core::{EntryKind, Key, SeqNo, Value};

/// An entry in the MemTable, sorted by (key, seq_no DESC).
#[derive(Debug, Clone)]
pub struct MemTableEntry {
    pub key: Vec<u8>,
    pub value: Vec<u8>,
    pub seq_no: SeqNo,
    pub kind: EntryKind,
}

impl MemTableEntry {
    /// Composite key for ordering: (user_key, seq_no DESC).
    /// This ensures the latest version of a key comes first in iteration.
    fn composite_key(&self) -> Vec<u8> {
        let mut composite = Vec::with_capacity(self.key.len() + 8);
        composite.extend_from_slice(&self.key);
        // Invert seq_no so that higher seq_no sorts first
        composite.extend_from_slice(&(!self.seq_no).to_be_bytes());
        composite
    }

    pub fn is_tombstone(&self) -> bool {
        self.kind == EntryKind::Delete
    }
}

/// In-memory sorted store backed by a BTreeMap.
///
/// Uses composite key (user_key, !seq_no) for ordering, so that
/// the latest version of each key appears first during iteration.
pub struct MemTable {
    /// BTreeMap with composite keys for sorted access.
    data: std::collections::BTreeMap<Vec<u8>, MemTableEntry>,

    /// Approximate size in bytes.
    size: usize,

    /// Monotonically increasing sequence number generator.
    next_seq_no: SeqNo,
}

impl MemTable {
    /// Creates a new empty MemTable.
    pub fn new() -> Self {
        Self {
            data: std::collections::BTreeMap::new(),
            size: 0,
            next_seq_no: 0,
        }
    }

    /// Creates a new MemTable starting from a given sequence number.
    pub fn with_seq_no(start_seq: SeqNo) -> Self {
        Self {
            data: std::collections::BTreeMap::new(),
            size: 0,
            next_seq_no: start_seq,
        }
    }

    /// Returns the next sequence number and increments it.
    pub fn next_seq_no(&mut self) -> SeqNo {
        let seq = self.next_seq_no;
        self.next_seq_no += 1;
        seq
    }

    /// Puts a key-value pair into the MemTable with an externally provided sequence number.
    pub fn put(&mut self, key: Key, value: Value) -> SeqNo {
        let seq_no = self.next_seq_no();
        let entry = MemTableEntry {
            key: key.clone(),
            value: value.clone(),
            seq_no,
            kind: EntryKind::Put,
        };

        let composite = entry.composite_key();
        // Subtract old entry size if overwriting
        if let Some(old) = self.data.get(&composite) {
            self.size = self.size.saturating_sub(old.key.len() + old.value.len() + 16);
        }
        let size_delta = key.len() + value.len() + 16; // key + value + overhead
        self.size += size_delta;

        self.data.insert(composite, entry);

        seq_no
    }

    /// Puts a key-value pair with a specific sequence number (from the engine).
    pub fn put_with_seq(&mut self, key: Key, value: Value, seq_no: SeqNo) {
        // Build composite key directly (key + inverted seq_no) to avoid extra allocation
        let mut composite = Vec::with_capacity(key.len() + 8);
        composite.extend_from_slice(&key);
        composite.extend_from_slice(&(!seq_no).to_be_bytes());

        // Subtract old entry size if overwriting
        if let Some(old) = self.data.get(&composite) {
            self.size = self.size.saturating_sub(old.key.len() + old.value.len() + 16);
        }
        let size_delta = key.len() + value.len() + 16;
        self.size += size_delta;

        let entry = MemTableEntry {
            key,
            value,
            seq_no,
            kind: EntryKind::Put,
        };

        self.data.insert(composite, entry);

        // Keep next_seq_no in sync
        if seq_no >= self.next_seq_no {
            self.next_seq_no = seq_no + 1;
        }
    }

    /// Marks a key as deleted (tombstone) with a specific sequence number.
    pub fn delete_with_seq(&mut self, key: Key, seq_no: SeqNo) {
        let size_delta = key.len() + 16;
        self.size += size_delta;

        // Build composite key directly (key + inverted seq_no) to avoid extra allocation
        let mut composite = Vec::with_capacity(key.len() + 8);
        composite.extend_from_slice(&key);
        composite.extend_from_slice(&(!seq_no).to_be_bytes());

        let entry = MemTableEntry {
            key,
            value: Vec::new(),
            seq_no,
            kind: EntryKind::Delete,
        };

        self.data.insert(composite, entry);

        if seq_no >= self.next_seq_no {
            self.next_seq_no = seq_no + 1;
        }
    }

    /// Marks a key as deleted (tombstone).
    pub fn delete(&mut self, key: Key) -> SeqNo {
        let seq_no = self.next_seq_no();
        let entry = MemTableEntry {
            key: key.clone(),
            value: Vec::new(),
            seq_no,
            kind: EntryKind::Delete,
        };

        let size_delta = key.len() + 16;
        self.size += size_delta;

        let composite = entry.composite_key();
        self.data.insert(composite, entry);

        seq_no
    }

    /// Gets the latest value for a key.
    /// Uses BTreeMap range query for O(log n) lookup instead of linear scan.
    pub fn get(&self, key: &[u8]) -> Option<(&[u8], SeqNo)> {
        // Composite key format: user_key ++ (!seq_no).to_be_bytes()
        // Since !seq_no inverts bits, higher seq_no → smaller composite.
        // All versions of a key K are in range [K++0x00*8, K++0xFF*8].
        let mut lower = Vec::with_capacity(key.len() + 8);
        lower.extend_from_slice(key);
        lower.extend_from_slice(&0u64.to_be_bytes()); // smallest seq part

        let mut upper = Vec::with_capacity(key.len() + 8);
        upper.extend_from_slice(key);
        upper.extend_from_slice(&u64::MAX.to_be_bytes()); // largest seq part

        // Iterate versions of this key, newest first (highest seq_no = smallest composite).
        for (_composite, entry) in self.data.range(lower..=upper) {
            if entry.key != key {
                continue; // Skip entries with different user keys (e.g., "key\x00")
            }
            if entry.is_tombstone() {
                return None;
            }
            return Some((&entry.value, entry.seq_no));
        }
        None
    }

    /// Returns an iterator over all versions of a given key, newest first.
    pub fn get_versions<'a>(&'a self, key: &'a [u8]) -> impl Iterator<Item = &'a MemTableEntry> + 'a {
        let mut lower = Vec::with_capacity(key.len() + 8);
        lower.extend_from_slice(key);
        lower.extend_from_slice(&0u64.to_be_bytes());

        let mut upper = Vec::with_capacity(key.len() + 8);
        upper.extend_from_slice(key);
        upper.extend_from_slice(&u64::MAX.to_be_bytes());

        self.data.range(lower..=upper)
            .filter(move |(_, e)| e.key.as_slice() == key)
            .map(|(_, e)| e)
    }

    /// Returns an iterator over all entries in sorted order.
    pub fn entries(&self) -> impl Iterator<Item = &MemTableEntry> {
        self.data.values()
    }

    /// Returns an iterator over entries whose key starts with `prefix`.
    /// Uses BTreeMap range query to skip non-matching entries — O(log n + matches)
    /// instead of O(total_entries) for full iteration + filter.
    ///
    /// Note: returns ALL versions (including tombstones and older versions).
    /// The caller is responsible for deduplication and tombstone filtering.
    pub fn scan_prefix(&self, prefix: &[u8]) -> impl Iterator<Item = &MemTableEntry> {
        // Composite key lower bound: prefix ++ 0x00*8 (smallest seq part)
        let mut lower = Vec::with_capacity(prefix.len() + 8);
        lower.extend_from_slice(prefix);
        lower.extend_from_slice(&0u64.to_be_bytes());

        // Composite key upper bound: prefix_upper ++ 0x00*8
        // prefix_upper is prefix with last byte incremented by 1.
        // This captures all keys starting with prefix, regardless of seq_no.
        let upper = Self::prefix_upper_bound(prefix);

        self.data.range(lower..upper)
            .map(|(_, e)| e)
    }

    /// Compute the exclusive upper bound for a prefix range scan.
    /// Returns prefix with its last byte incremented by 1.
    /// E.g., "Product::" → "Product:;"  (':' = 0x3A, ';' = 0x3B)
    fn prefix_upper_bound(prefix: &[u8]) -> Vec<u8> {
        let mut upper = prefix.to_vec();
        // Find the rightmost byte that can be incremented
        for i in (0..upper.len()).rev() {
            if upper[i] < 0xFF {
                upper[i] += 1;
                upper.truncate(i + 1);
                return upper;
            }
        }
        // All bytes were 0xFF — prefix is the maximum possible key
        // Return an empty upper bound that compares greater than everything
        upper.push(0x00);
        upper
    }

    /// Returns the approximate size in bytes.
    pub fn size(&self) -> usize {
        self.size
    }

    /// Returns true if the MemTable is empty.
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Returns the number of entries.
    pub fn len(&self) -> usize {
        self.data.len()
    }
}

impl Default for MemTable {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_memtable_put_and_get() {
        let mut mt = MemTable::new();
        mt.put(b"name".to_vec(), b"alice".to_vec());
        mt.put(b"age".to_vec(), b"30".to_vec());

        let (val, _) = mt.get(b"name").unwrap();
        assert_eq!(val, b"alice");

        let (val, _) = mt.get(b"age").unwrap();
        assert_eq!(val, b"30");

        assert!(mt.get(b"missing").is_none());
    }

    #[test]
    fn test_memtable_overwrite() {
        let mut mt = MemTable::new();
        mt.put(b"key".to_vec(), b"v1".to_vec());
        mt.put(b"key".to_vec(), b"v2".to_vec());

        let (val, seq) = mt.get(b"key").unwrap();
        assert_eq!(val, b"v2");
        assert_eq!(seq, 1); // Latest seq_no
    }

    #[test]
    fn test_memtable_delete() {
        let mut mt = MemTable::new();
        mt.put(b"key".to_vec(), b"value".to_vec());
        assert!(mt.get(b"key").is_some());

        mt.delete(b"key".to_vec());
        assert!(mt.get(b"key").is_none());
    }

    #[test]
    fn test_memtable_iteration_order() {
        let mut mt = MemTable::new();
        mt.put(b"b".to_vec(), b"2".to_vec());
        mt.put(b"a".to_vec(), b"1".to_vec());
        mt.put(b"c".to_vec(), b"3".to_vec());

        let keys: Vec<&[u8]> = mt.entries().map(|e| e.key.as_slice()).collect();
        assert_eq!(keys, vec![b"a".as_slice(), b"b".as_slice(), b"c".as_slice()]);
    }

    #[test]
    fn test_get_versions() {
        let mut mt = MemTable::new();
        mt.put(b"key".to_vec(), b"v1".to_vec());
        mt.put(b"key".to_vec(), b"v2".to_vec());
        mt.put(b"key".to_vec(), b"v3".to_vec());
        mt.put(b"other".to_vec(), b"x".to_vec());

        let versions: Vec<&[u8]> = mt.get_versions(b"key").map(|e| e.value.as_slice()).collect();
        assert_eq!(versions, vec![b"v3".as_slice(), b"v2".as_slice(), b"v1".as_slice()]);
    }

    #[test]
    fn test_get_prefix_key_no_false_match() {
        // Ensure "key" doesn't accidentally match "key1" or "key\x00"
        let mut mt = MemTable::new();
        mt.put(b"key".to_vec(), b"exact".to_vec());
        mt.put(b"key1".to_vec(), b"longer".to_vec());
        mt.put(b"key\x00".to_vec(), b"nullbyte".to_vec());

        let (val, _) = mt.get(b"key").unwrap();
        assert_eq!(val, b"exact");

        let (val, _) = mt.get(b"key1").unwrap();
        assert_eq!(val, b"longer");

        let (val, _) = mt.get(b"key\x00").unwrap();
        assert_eq!(val, b"nullbyte");
    }

    #[test]
    fn test_get_after_delete_and_reinsert() {
        let mut mt = MemTable::new();
        mt.put(b"k".to_vec(), b"v1".to_vec());
        mt.delete(b"k".to_vec());
        mt.put(b"k".to_vec(), b"v2".to_vec());

        let (val, _) = mt.get(b"k").unwrap();
        assert_eq!(val, b"v2");
    }
}
