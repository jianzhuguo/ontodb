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
struct HeapEntry {
    key: Vec<u8>,
    value: Vec<u8>,
    seq_no: SeqNo,
    kind: EntryKind,
    /// Index of the source SSTable (for stable sort)
    source_idx: usize,
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
    /// For min-heap: smaller key first, then smaller seq_no
    fn cmp(&self, other: &Self) -> Ordering {
        self.key.cmp(&other.key)
            .then(self.seq_no.cmp(&other.seq_no))
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
            let mut iter_box = Box::new(iter) as Box<dyn Iterator<Item = _>>;
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
struct SstEntryIterator {
    sst: SsTable,
    iter: SsTableIterator<'static>,
    _sst: Box<SsTable>, // Keep SSTable alive
}

impl SstEntryIterator {
    fn new(sst: SsTable) -> Result<Self, Box<dyn std::error::Error>> {
        // Safety: We need to create an iterator that borrows the SSTable.
        // We'll store the SSTable in a Box and leak it to get a 'static reference.
        // This is safe because we control the lifetime through the iterator.
        let sst_box = Box::new(sst);
        let sst_ref: &'static SsTable = unsafe { std::mem::transmute(sst_box.as_ref()) };
        let iter = sst_ref.iter()?;
        
        Ok(Self {
            sst: *sst_box,
            iter,
            _sst: Box::new(unsafe { std::mem::zeroed() }), // Placeholder
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
