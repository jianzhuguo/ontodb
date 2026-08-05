//! MemTable: In-memory sorted key-value store using a skip list.
//!
//! The MemTable is the write buffer. All writes go here first.
//! When it reaches the size limit, it's flushed to an SSTable on disk.
//!
//! We use `crossbeam-skiplist` for a concurrent-friendly skip list.

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

    /// Puts a key-value pair into the MemTable.
    pub fn put(&mut self, key: Key, value: Value) -> SeqNo {
        let seq_no = self.next_seq_no();
        let entry = MemTableEntry {
            key: key.clone(),
            value: value.clone(),
            seq_no,
            kind: EntryKind::Put,
        };

        let size_delta = key.len() + value.len() + 16; // key + value + overhead
        self.size += size_delta;

        let composite = entry.composite_key();
        self.data.insert(composite, entry);

        seq_no
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
    pub fn get(&self, key: &[u8]) -> Option<(&[u8], SeqNo)> {
        // Iterate all entries and find the latest version of this key
        for (_composite, entry) in &self.data {
            if entry.key == key {
                if entry.is_tombstone() {
                    return None;
                }
                return Some((&entry.value, entry.seq_no));
            }
        }
        None
    }

    /// Returns an iterator over all entries in sorted order.
    pub fn entries(&self) -> impl Iterator<Item = &MemTableEntry> {
        self.data.values()
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
}
