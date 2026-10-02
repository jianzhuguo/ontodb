// Copyright (c) 2024-2026 OntoDB Team
// Licensed under the Business Source License 1.1 (BUSL-1.1).
// See LICENSE for details. Change Date: 2031-09-15.
// On the Change Date, this file will be licensed under Apache License 2.0.
//! Streaming merge iterator for compaction.
//!
//! Reads from multiple SSTables in sorted order without loading all entries into memory.

use crate::lsm::sstable::{SsTable, SsTableIterator};
use onto_core::{EntryKind, SeqNo};
use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::path::Path;

/// Entry from an SSTable iterator with comparison support for min-heap.
#[derive(Debug)]
pub struct HeapEntry {
    pub key: Vec<u8>,
    pub value: Vec<u8>,
    pub seq_no: SeqNo,
    pub kind: EntryKind,
    /// Index of the source SSTable (for stable sort)
    pub source_idx: usize,
}

impl PartialEq for HeapEntry {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key && self.seq_no == other.seq_no
    }
}

impl Eq for HeapEntry {}

impl PartialOrd for HeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for HeapEntry {
    /// BinaryHeap is a max-heap. We want: key ascending, seq_no descending (newest first).
    /// To get min-key-first from max-heap, reverse the key comparison.
    /// For same key, we want LARGER seq_no to come out FIRST from max-heap.
    fn cmp(&self, other: &Self) -> Ordering {
        // Key: smaller key = higher priority -> reverse for max-heap
        other.key.cmp(&self.key)
            // Seq_no: larger seq_no = higher priority -> keep ascending for max-heap
            // (max-heap pops the largest, so larger seq_no comes first)
            .then(self.seq_no.cmp(&other.seq_no))
            // Tie-break by source index (smaller index first)
            .then(self.source_idx.cmp(&other.source_idx))
    }
}

/// Streaming merge iterator that reads from multiple SSTables in sorted order.
///
/// This avoids loading all entries into memory by using a min-heap to merge
/// entries from multiple iterators on-the-fly.
pub struct StreamingMergeIterator {
    /// Min-heap for merging entries from multiple SSTables.
    heap: BinaryHeap<HeapEntry>,
    /// Source iterators (one per SSTable).
    sources: Vec<Box<dyn Iterator<Item = (Vec<u8>, Vec<u8>, SeqNo, EntryKind)>>>,
    /// Current entry (peeked).
    current: Option<HeapEntry>,
}

impl StreamingMergeIterator {
    /// Creates a new streaming merge iterator from multiple SSTable paths.
    pub fn new(sst_paths: &[impl AsRef<Path>]) -> Result<Self, Box<dyn std::error::Error>> {
        let mut heap = BinaryHeap::new();
        let mut sources: Vec<Box<dyn Iterator<Item = (Vec<u8>, Vec<u8>, SeqNo, EntryKind)>>> = Vec::new();

        for (idx, path) in sst_paths.iter().enumerate() {
            let sst = SsTable::open(path)?;
            let iter = SstEntryIterator::new(sst)?;
            
            // Collect first entry from each iterator
            let mut iter_box: Box<dyn Iterator<Item = (Vec<u8>, Vec<u8>, SeqNo, EntryKind)>> = Box::new(iter);
            if let Some(entry) = iter_box.next() {
                heap.push(HeapEntry {
                    key: entry.0,
                    value: entry.1,
                    seq_no: entry.2,
                    kind: entry.3,
                    source_idx: idx,
                });
            }
            sources.push(iter_box);
        }

        let current = heap.pop();
        
        Ok(Self {
            heap,
            sources,
            current,
        })
    }

    /// Returns true if there are more entries.
    pub fn has_next(&self) -> bool {
        self.current.is_some()
    }

    /// Returns the current entry without advancing.
    pub fn peek(&self) -> Option<&HeapEntry> {
        self.current.as_ref()
    }

    /// Advances to the next entry.
    pub fn next(&mut self) -> Option<HeapEntry> {
        let result = self.current.take();
        
        // Refill from the source that provided the current entry
        if let Some(ref entry) = result {
            let source_idx = entry.source_idx;
            if let Some(new_entry) = self.sources[source_idx].next() {
                self.heap.push(HeapEntry {
                    key: new_entry.0,
                    value: new_entry.1,
                    seq_no: new_entry.2,
                    kind: new_entry.3,
                    source_idx,
                });
            }
        }
        
        // Get next from heap
        self.current = self.heap.pop();
        
        result
    }
}

/// Iterator wrapper for SsTable entries.
///
/// Uses unsafe to create a self-referential struct that owns the SSTable.
/// Drop order is critical: `iter` must be dropped BEFORE `_sst`.
/// Rust drops fields in declaration order, so `iter` is declared first.
struct SstEntryIterator {
    /// The iterator borrowing from the SSTable. Dropped FIRST (declared before _sst).
    iter: SsTableIterator<'static>,
    /// Owns the SSTable data. Dropped SECOND (after iter is gone).
    _sst: Box<SsTable>,
}

impl SstEntryIterator {
    fn new(sst: SsTable) -> Result<Self, Box<dyn std::error::Error>> {
        let sst_box = Box::new(sst);
        let sst_ptr: *const SsTable = &*sst_box;
        // SAFETY: sst_ptr is derived from sst_box which is stored in the same struct.
        // Rust drops fields in declaration order: `iter` (first) then `_sst` (second).
        // So the iterator is always dropped before the SSTable it borrows from.
        let sst_ref: &'static SsTable = unsafe { &*sst_ptr };
        let iter = sst_ref.iter()?;

        Ok(Self {
            iter,
            _sst: sst_box,
        })
    }
}

impl Iterator for SstEntryIterator {
    type Item = (Vec<u8>, Vec<u8>, SeqNo, EntryKind);
    
    fn next(&mut self) -> Option<Self::Item> {
        if self.iter.is_valid() {
            let entry = (
                self.iter.key().to_vec(),
                self.iter.value().to_vec(),
                self.iter.seq_no(),
                self.iter.kind(),
            );
            self.iter.next();
            Some(entry)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering;

    #[test]
    fn test_heap_entry_ordering() {
        // Test: key ascending, seq_no descending (newest first for same key)
        let entry_a = HeapEntry {
            key: b"key1".to_vec(),
            value: b"val1".to_vec(),
            seq_no: 10,
            kind: EntryKind::Put,
            source_idx: 0,
        };
        let entry_b = HeapEntry {
            key: b"key1".to_vec(),
            value: b"val2".to_vec(),
            seq_no: 20,
            kind: EntryKind::Put,
            source_idx: 1,
        };
        let entry_c = HeapEntry {
            key: b"key2".to_vec(),
            value: b"val3".to_vec(),
            seq_no: 5,
            kind: EntryKind::Put,
            source_idx: 0,
        };

        // For max-heap: larger value has higher priority
        // entry_b (seq_no=20) should have higher priority than entry_a (seq_no=10) for same key
        assert_eq!(entry_b.cmp(&entry_a), Ordering::Greater);
        
        // entry_c (key2) should have higher priority than entry_a (key1) because key2 > key1
        // but we want smaller key first, so entry_a should have higher priority
        // In max-heap, Greater means higher priority
        // entry_a.key < entry_c.key, so entry_a should have higher priority
        // entry_a.cmp(&entry_c) should return Greater
        assert_eq!(entry_a.cmp(&entry_c), Ordering::Greater);
    }

    #[test]
    fn test_binary_heap_ordering() {
        // Test BinaryHeap behavior with our Ord implementation
        let mut heap = BinaryHeap::new();
        
        // Push entries with same key, different seq_no
        heap.push(HeapEntry {
            key: b"key1".to_vec(),
            value: b"val1".to_vec(),
            seq_no: 10,
            kind: EntryKind::Put,
            source_idx: 0,
        });
        heap.push(HeapEntry {
            key: b"key1".to_vec(),
            value: b"val2".to_vec(),
            seq_no: 20,
            kind: EntryKind::Put,
            source_idx: 1,
        });
        heap.push(HeapEntry {
            key: b"key2".to_vec(),
            value: b"val3".to_vec(),
            seq_no: 5,
            kind: EntryKind::Put,
            source_idx: 0,
        });
        
        // Pop should return: key1/seq20 (newest first for same key), then key1/seq10, then key2/seq5
        let first = heap.pop().unwrap();
        assert_eq!(first.key, b"key1");
        assert_eq!(first.seq_no, 20); // Newest first
        
        let second = heap.pop().unwrap();
        assert_eq!(second.key, b"key1");
        assert_eq!(second.seq_no, 10);
        
        let third = heap.pop().unwrap();
        assert_eq!(third.key, b"key2");
        assert_eq!(third.seq_no, 5);
    }

    #[test]
    fn test_sst_entry_iterator() {
        use crate::lsm::sstable::SsTableBuilder;
        use onto_core::Entry;
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let path = dir.path().join("test.sst");

        // Create a test SSTable
        let mut builder = SsTableBuilder::new();
        builder.add(&Entry::put(b"key1".to_vec(), b"val1".to_vec(), 10));
        builder.add(&Entry::put(b"key2".to_vec(), b"val2".to_vec(), 20));
        builder.add(&Entry::put(b"key3".to_vec(), b"val3".to_vec(), 30));
        builder.build(&path).unwrap();

        // Open and iterate
        let sst = SsTable::open(&path).unwrap();
        let mut iter = SstEntryIterator::new(sst).unwrap();

        // Should return entries in order
        let entry1 = iter.next().unwrap();
        assert_eq!(entry1.0, b"key1");
        assert_eq!(entry1.2, 10);

        let entry2 = iter.next().unwrap();
        assert_eq!(entry2.0, b"key2");
        assert_eq!(entry2.2, 20);

        let entry3 = iter.next().unwrap();
        assert_eq!(entry3.0, b"key3");
        assert_eq!(entry3.2, 30);

        assert!(iter.next().is_none());
    }

    #[test]
    fn test_streaming_merge_with_tombstones() {
        use crate::lsm::sstable::SsTableBuilder;
        use onto_core::Entry;
        use tempfile::tempdir;

        let dir = tempdir().unwrap();

        // SSTable 1: key1=v1 (seq=10), key2=v2 (seq=10)
        let path1 = dir.path().join("sst1.sst");
        let mut builder = SsTableBuilder::new();
        builder.add(&Entry::put(b"key1".to_vec(), b"v1".to_vec(), 10));
        builder.add(&Entry::put(b"key2".to_vec(), b"v2".to_vec(), 10));
        builder.build(&path1).unwrap();

        // SSTable 2: key1=DELETE (seq=20), key3=v3 (seq=10)
        let path2 = dir.path().join("sst2.sst");
        let mut builder = SsTableBuilder::new();
        builder.add(&Entry::delete(b"key1".to_vec(), 20));
        builder.add(&Entry::put(b"key3".to_vec(), b"v3".to_vec(), 10));
        builder.build(&path2).unwrap();

        // Create streaming merge iterator
        let mut iter = StreamingMergeIterator::new(&[path1, path2]).unwrap();

        // Should return: key1/DELETE (seq=20), key1/v1 (seq=10), key2/v2 (seq=10), key3/v3 (seq=10)
        // Note: same key appears twice (newest first)

        let entry1 = iter.next().unwrap();
        assert_eq!(entry1.key, b"key1");
        assert_eq!(entry1.kind, EntryKind::Delete);
        assert_eq!(entry1.seq_no, 20);

        let entry2 = iter.next().unwrap();
        assert_eq!(entry2.key, b"key1");
        assert_eq!(entry2.kind, EntryKind::Put);
        assert_eq!(entry2.seq_no, 10);

        let entry3 = iter.next().unwrap();
        assert_eq!(entry3.key, b"key2");
        assert_eq!(entry3.kind, EntryKind::Put);
        assert_eq!(entry3.seq_no, 10);

        let entry4 = iter.next().unwrap();
        assert_eq!(entry4.key, b"key3");
        assert_eq!(entry4.kind, EntryKind::Put);
        assert_eq!(entry4.seq_no, 10);

        assert!(iter.next().is_none());
    }

    #[test]
    fn test_streaming_merge_with_overwrites() {
        use crate::lsm::sstable::SsTableBuilder;
        use onto_core::Entry;
        use tempfile::tempdir;

        let dir = tempdir().unwrap();

        // SSTable 1: key1=v1 (seq=10), key2=v2 (seq=10)
        let path1 = dir.path().join("sst1.sst");
        let mut builder = SsTableBuilder::new();
        builder.add(&Entry::put(b"key1".to_vec(), b"v1".to_vec(), 10));
        builder.add(&Entry::put(b"key2".to_vec(), b"v2".to_vec(), 10));
        builder.build(&path1).unwrap();

        // SSTable 2: key1=v1_new (seq=20), key3=v3 (seq=10)
        let path2 = dir.path().join("sst2.sst");
        let mut builder = SsTableBuilder::new();
        builder.add(&Entry::put(b"key1".to_vec(), b"v1_new".to_vec(), 20));
        builder.add(&Entry::put(b"key3".to_vec(), b"v3".to_vec(), 10));
        builder.build(&path2).unwrap();

        // Create streaming merge iterator
        let mut iter = StreamingMergeIterator::new(&[path1, path2]).unwrap();

        // Should return: key1/v1_new (seq=20), key1/v1 (seq=10), key2/v2 (seq=10), key3/v3 (seq=10)
        // Note: same key appears twice (newest first)

        let entry1 = iter.next().unwrap();
        assert_eq!(entry1.key, b"key1");
        assert_eq!(entry1.value, b"v1_new");
        assert_eq!(entry1.seq_no, 20);

        let entry2 = iter.next().unwrap();
        assert_eq!(entry2.key, b"key1");
        assert_eq!(entry2.value, b"v1");
        assert_eq!(entry2.seq_no, 10);

        let entry3 = iter.next().unwrap();
        assert_eq!(entry3.key, b"key2");
        assert_eq!(entry3.value, b"v2");
        assert_eq!(entry3.seq_no, 10);

        let entry4 = iter.next().unwrap();
        assert_eq!(entry4.key, b"key3");
        assert_eq!(entry4.value, b"v3");
        assert_eq!(entry4.seq_no, 10);

        assert!(iter.next().is_none());
    }
}
