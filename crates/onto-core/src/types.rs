//! Core types used throughout OntoDB.

/// Byte vector type used for keys and values.
pub type Bytes = Vec<u8>;

/// A key in the storage engine.
pub type Key = Vec<u8>;

/// A value in the storage engine.
pub type Value = Vec<u8>;

/// Sequence number for MVCC / WAL ordering.
pub type SeqNo = u64;

/// Microsecond timestamp.
pub type Timestamp = u64;

/// Internal entry type used by the storage engine.
/// A key-value pair with an associated sequence number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub key: Key,
    pub value: Value,
    pub seq_no: SeqNo,
    pub kind: EntryKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Put,
    Delete,
}

impl Entry {
    pub fn put(key: Key, value: Value, seq_no: SeqNo) -> Self {
        Self {
            key,
            value,
            seq_no,
            kind: EntryKind::Put,
        }
    }

    pub fn delete(key: Key, seq_no: SeqNo) -> Self {
        Self {
            key,
            value: Vec::new(),
            seq_no,
            kind: EntryKind::Delete,
        }
    }

    pub fn is_tombstone(&self) -> bool {
        self.kind == EntryKind::Delete
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_entry_put() {
        let entry = Entry::put(b"key".to_vec(), b"value".to_vec(), 42);
        assert_eq!(entry.key, b"key");
        assert_eq!(entry.value, b"value");
        assert_eq!(entry.seq_no, 42);
        assert_eq!(entry.kind, EntryKind::Put);
        assert!(!entry.is_tombstone());
    }

    #[test]
    fn test_entry_delete() {
        let entry = Entry::delete(b"key".to_vec(), 100);
        assert_eq!(entry.key, b"key");
        assert!(entry.value.is_empty());
        assert_eq!(entry.seq_no, 100);
        assert_eq!(entry.kind, EntryKind::Delete);
        assert!(entry.is_tombstone());
    }

    #[test]
    fn test_entry_clone() {
        let entry = Entry::put(b"key".to_vec(), b"value".to_vec(), 1);
        let cloned = entry.clone();
        assert_eq!(entry, cloned);
    }

    #[test]
    fn test_entry_kind_eq() {
        assert_eq!(EntryKind::Put, EntryKind::Put);
        assert_eq!(EntryKind::Delete, EntryKind::Delete);
        assert_ne!(EntryKind::Put, EntryKind::Delete);
    }
}
