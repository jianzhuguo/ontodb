//! SSTable (Sorted String Table): On-disk sorted key-value storage.
//!
//! Format:
//! [data blocks][index block][bloom filter][footer]
//!
//! Each data block contains sorted key-value pairs with a restart point array
//! for efficient binary search within the block.
//!
//! The index block maps the last key of each data block to its offset.
//! The bloom filter enables efficient "key not found" checks.
//! The footer contains offsets of the index block and bloom filter.

use super::block_cache::BlockCache;
use super::bloom_filter::BloomFilter;
use onto_core::{CoreError, Entry, EntryKind, Result, SeqNo};
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use parking_lot::Mutex;

/// Default block cache capacity (number of blocks per SSTable).
const DEFAULT_BLOCK_CACHE_CAPACITY: usize = 64;

/// Footer size: index_offset (8) + bloom_offset (8) + magic (8) + flags (1)
const FOOTER_SIZE: u64 = 25;
const MAGIC: u64 = 0x4F4E544F_44425353; // "ONTO_DBSS"

/// Flag bit indicating data blocks are zstd-compressed.
const FLAG_COMPRESSED: u8 = 0x01;

/// Block format: [entries...][restart_points...][num_restarts: u32]
const RESTART_INTERVAL: usize = 16;

/// Interior of SsTable requiring interior mutability (file I/O + block cache).
struct SsTableInner {
    file: File,
    block_cache: BlockCache,
}

/// An SSTable file on disk.
pub struct SsTable {
    _path: PathBuf,
    /// File handle and block cache, behind a Mutex for `&self` read access.
    inner: Mutex<SsTableInner>,
    /// Size of the data section.
    _data_size: u64,
    /// Index: last_key -> (offset, size) of each data block.
    index: Vec<BlockIndexEntry>,
    /// Bloom filter for point lookups.
    bloom: Option<BloomFilter>,
    /// Whether data blocks are zstd-compressed.
    compressed: bool,
    /// Cached first key (minimum key) in this SSTable.
    /// Loaded once at open() to avoid repeated disk I/O.
    cached_first_key: Vec<u8>,
}

#[derive(Debug, Clone)]
struct BlockIndexEntry {
    last_key: Vec<u8>,
    offset: u64,
    size: u64,
}

/// Builder for constructing an SSTable from sorted entries.
pub struct SsTableBuilder {
    data: Vec<u8>,
    current_block: Vec<u8>,
    current_block_entries: usize,
    block_entries: Vec<BlockIndexEntry>,
    current_block_offset: u64,
    current_block_first_key: Vec<u8>,
    current_block_last_key: Vec<u8>,
    restart_points: Vec<u32>,
    entry_count_in_block: usize,
    /// Keys collected during add(), used to build bloom filter at build time.
    /// This is more memory-efficient than pre-allocating a large bloom filter
    /// for small SSTables.
    keys_for_bloom: Vec<Vec<u8>>,
    /// zstd compression level (0 = disabled, 1-21 = enabled).
    compression_level: i32,
}

impl SsTableBuilder {
    pub fn new() -> Self {
        Self {
            data: Vec::new(),
            current_block: Vec::new(),
            current_block_entries: 0,
            block_entries: Vec::new(),
            current_block_offset: 0,
            current_block_first_key: Vec::new(),
            current_block_last_key: Vec::new(),
            restart_points: Vec::new(),
            entry_count_in_block: 0,
            keys_for_bloom: Vec::new(),
            compression_level: 0,
        }
    }

    /// Enables zstd compression on data blocks. Level 1-21 (higher = better ratio, slower).
    pub fn set_compression_level(&mut self, level: i32) {
        self.compression_level = level.clamp(0, 21);
    }

    /// Adds an entry to the SSTable. Entries MUST be added in sorted key order.
    pub fn add(&mut self, entry: &Entry) {
        self.keys_for_bloom.push(entry.key.clone());

        // Record restart point
        if self.entry_count_in_block % RESTART_INTERVAL == 0 {
            self.restart_points
                .push(self.current_block.len() as u32);
        }

        if self.current_block_first_key.is_empty() {
            self.current_block_first_key = entry.key.clone();
        }
        self.current_block_last_key = entry.key.clone();

        // Encode entry into current block
        Self::encode_entry_to(&mut self.current_block, entry);
        self.current_block_entries += 1;
        self.entry_count_in_block += 1;

        // Flush block if it's large enough
        if self.current_block.len() >= 4096 {
            self.flush_block();
        }
    }

    /// Adds an entry by taking ownership, avoiding clones for callers that
    /// no longer need the Entry after this call.
    pub fn add_owned(&mut self, entry: Entry) {
        // Record restart point
        if self.entry_count_in_block % RESTART_INTERVAL == 0 {
            self.restart_points
                .push(self.current_block.len() as u32);
        }

        if self.current_block_first_key.is_empty() {
            self.current_block_first_key = entry.key.clone();
        }
        self.current_block_last_key = entry.key.clone();

        // Encode entry into current block (borrows entry)
        Self::encode_entry_to(&mut self.current_block, &entry);
        self.current_block_entries += 1;
        self.entry_count_in_block += 1;

        // Push key into bloom filter (takes ownership, no clone)
        self.keys_for_bloom.push(entry.key);

        // Flush block if it's large enough
        if self.current_block.len() >= 4096 {
            self.flush_block();
        }
    }

    /// Flushes the current block to the main data buffer.
    /// Compresses the block with zstd if compression_level > 0.
    fn flush_block(&mut self) {
        if self.current_block.is_empty() {
            return;
        }

        // Write restart points
        let num_restarts = self.restart_points.len() as u32;
        for rp in &self.restart_points {
            self.current_block.extend_from_slice(&rp.to_le_bytes());
        }
        self.current_block
            .extend_from_slice(&num_restarts.to_le_bytes());

        // Compress block if enabled
        let block_data = if self.compression_level > 0 {
            match zstd::encode_all(self.current_block.as_slice(), self.compression_level) {
                Ok(compressed) => compressed,
                Err(_) => self.current_block.clone(), // Fallback to uncompressed
            }
        } else {
            self.current_block.clone()
        };

        let block_size = block_data.len() as u64;

        // Record block index
        self.block_entries.push(BlockIndexEntry {
            last_key: self.current_block_last_key.clone(),
            offset: self.current_block_offset,
            size: block_size,
        });

        // Append to main data
        self.data.extend_from_slice(&block_data);
        self.current_block_offset += block_size;

        // Reset
        self.current_block.clear();
        self.current_block_entries = 0;
        self.current_block_first_key.clear();
        self.current_block_last_key.clear();
        self.restart_points.clear();
        self.entry_count_in_block = 0;
    }

    /// Builds the SSTable and writes it to disk.
    pub fn build(mut self, path: impl AsRef<Path>) -> Result<SsTable> {
        // Flush remaining block
        self.flush_block();

        let mut file = BufWriter::new(
            OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(path.as_ref())?,
        );

        // Write data blocks
        file.write_all(&self.data)?;
        let data_size = self.data.len() as u64;

        // Build and write index block
        let index_offset = data_size;
        let index_data = Self::encode_index(&self.block_entries);
        file.write_all(&index_data)?;

        // Build and write bloom filter
        let bloom_offset = index_offset + index_data.len() as u64;
        let mut bloom = BloomFilter::new(self.keys_for_bloom.len().max(1), 0.01);
        for key in &self.keys_for_bloom {
            bloom.insert(key);
        }
        let bloom_data = bloom.to_bytes();
        file.write_all(&bloom_data)?;

        // Write footer: index_offset (8) + bloom_offset (8) + magic (8) + flags (1)
        let flags: u8 = if self.compression_level > 0 { FLAG_COMPRESSED } else { 0 };
        file.write_all(&index_offset.to_le_bytes())?;
        file.write_all(&bloom_offset.to_le_bytes())?;
        file.write_all(&MAGIC.to_le_bytes())?;
        file.write_all(&[flags])?;

        file.flush()?;
        file.get_ref().sync_all()?;

        // Reopen for reading
        let read_file = File::open(path.as_ref())?;
        let compressed = self.compression_level > 0;

        // Cache the first key (minimum key) for fast access
        let cached_first_key = self.keys_for_bloom.first().cloned().unwrap_or_default();

        Ok(SsTable {
            _path: path.as_ref().to_path_buf(),
            inner: Mutex::new(SsTableInner {
                file: read_file,
                block_cache: BlockCache::new(DEFAULT_BLOCK_CACHE_CAPACITY),
            }),
            _data_size: data_size,
            index: self.block_entries,
            bloom: Some(bloom),
            compressed,
            cached_first_key,
        })
    }

    fn encode_entry_to(buf: &mut Vec<u8>, entry: &Entry) {
        // key_len (4) + key + value_len (4) + value + seq_no (8) + kind (1)
        buf.extend_from_slice(&(entry.key.len() as u32).to_le_bytes());
        buf.extend_from_slice(&entry.key);
        buf.extend_from_slice(&(entry.value.len() as u32).to_le_bytes());
        buf.extend_from_slice(&entry.value);
        buf.extend_from_slice(&entry.seq_no.to_le_bytes());
        buf.push(match entry.kind {
            EntryKind::Put => 0,
            EntryKind::Delete => 1,
        });
    }

    fn encode_index(entries: &[BlockIndexEntry]) -> Vec<u8> {
        let mut buf = Vec::new();
        let count = entries.len() as u32;
        buf.extend_from_slice(&count.to_le_bytes());

        for entry in entries {
            buf.extend_from_slice(&(entry.last_key.len() as u32).to_le_bytes());
            buf.extend_from_slice(&entry.last_key);
            buf.extend_from_slice(&entry.offset.to_le_bytes());
            buf.extend_from_slice(&entry.size.to_le_bytes());
        }

        buf
    }

}

impl SsTable {
    /// Opens an existing SSTable file.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let mut file = File::open(path.as_ref())?;

        // Read footer: index_offset (8) + bloom_offset (8) + magic (8) + flags (1)
        file.seek(SeekFrom::End(-(FOOTER_SIZE as i64)))?;
        let mut footer = [0u8; 25];
        file.read_exact(&mut footer)?;

        let index_offset = u64::from_le_bytes(footer[0..8].try_into().unwrap());
        let bloom_offset = u64::from_le_bytes(footer[8..16].try_into().unwrap());
        let magic = u64::from_le_bytes(footer[16..24].try_into().unwrap());
        let flags = footer[24];
        let compressed = flags & FLAG_COMPRESSED != 0;

        if magic != MAGIC {
            return Err(CoreError::corruption("invalid SSTable magic number"));
        }

        // Read bloom filter
        file.seek(SeekFrom::Start(bloom_offset))?;
        let file_len = file.metadata()?.len();
        let bloom_data_len = (file_len - FOOTER_SIZE - bloom_offset) as usize;
        let mut bloom_data = vec![0u8; bloom_data_len];
        file.read_exact(&mut bloom_data)?;
        let bloom = BloomFilter::from_bytes(&bloom_data);

        // Read index
        file.seek(SeekFrom::Start(index_offset))?;
        let index_data_len = (bloom_offset - index_offset) as usize;
        let mut index_data = vec![0u8; index_data_len];
        file.read_exact(&mut index_data)?;
        let index = Self::decode_index(&index_data);

        // Cache the first key to avoid repeated disk I/O
        let cached_first_key = if index.is_empty() {
            Vec::new()
        } else {
            // Read first block to get the first key
            let offset = index[0].offset;
            let size = index[0].size;
            file.seek(SeekFrom::Start(offset))?;
            let mut block_data = vec![0u8; size as usize];
            file.read_exact(&mut block_data)?;
            
            // Decompress if needed
            let block = if compressed {
                zstd::decode_all(&block_data[..]).unwrap_or(block_data)
            } else {
                block_data
            };
            
            // Decode first key
            if block.len() < 4 {
                Vec::new()
            } else {
                let key_len = u32::from_le_bytes(block[0..4].try_into().unwrap()) as usize;
                if block.len() < 4 + key_len {
                    Vec::new()
                } else {
                    block[4..4 + key_len].to_vec()
                }
            }
        };

        Ok(SsTable {
            _path: path.as_ref().to_path_buf(),
            inner: Mutex::new(SsTableInner {
                file,
                block_cache: BlockCache::new(DEFAULT_BLOCK_CACHE_CAPACITY),
            }),
            _data_size: index_offset,
            index,
            bloom,
            compressed,
            cached_first_key,
        })
    }

    /// Gets a value by key. Returns:
    /// - `Ok(Some((value, seq_no)))` if found
    /// - `Ok(None)` if not found in this SSTable
    /// - `Err` on corruption
    ///
    /// Note: tombstones (deleted entries) are returned as `Ok(None)` to signal
    /// "key was deleted here, stop searching older SSTables". The engine should
    /// use `get_full` if it needs to distinguish "not found" from "deleted".
    pub fn get(&self, key: &[u8]) -> Result<Option<(Vec<u8>, SeqNo)>> {
        // Check bloom filter first
        if let Some(ref bloom) = self.bloom {
            if !bloom.might_contain(key) {
                return Ok(None);
            }
        }

        // Find the block that might contain this key.
        // KeyNotFound means key > all keys in this SST (bloom false positive).
        let block_idx = match self.find_block(key) {
            Ok(idx) => idx,
            Err(CoreError::KeyNotFound { .. }) => return Ok(None),
            Err(e) => return Err(e),
        };

        // Copy offset/size to avoid holding borrow on self.index
        let block_offset = self.index[block_idx].offset;
        let block_size = self.index[block_idx].size;

        // Read the block
        let block_data = self.read_block_at(block_offset, block_size)?;

        // Search within the block
        self.search_block(&block_data, key)
    }

    /// Gets a value by key with full tombstone awareness.
    /// Returns `Ok(Some((value, seq_no, kind)))` for both Put and Delete entries.
    /// Returns `Ok(None)` only if the key truly doesn't exist in this SSTable.
    pub fn get_full(&self, key: &[u8]) -> Result<Option<(Vec<u8>, SeqNo, EntryKind)>> {
        if let Some(ref bloom) = self.bloom {
            if !bloom.might_contain(key) {
                return Ok(None);
            }
        }

        let block_idx = match self.find_block(key) {
            Ok(idx) => idx,
            Err(CoreError::KeyNotFound { .. }) => return Ok(None),
            Err(e) => return Err(e),
        };
        let block_offset = self.index[block_idx].offset;
        let block_size = self.index[block_idx].size;
        let block_data = self.read_block_at(block_offset, block_size)?;
        self.search_block_full(&block_data, key)
    }

    /// Returns the maximum (last) key in the SSTable, from the index.
    pub fn max_key(&self) -> &[u8] {
        self.index
            .last()
            .map(|e| e.last_key.as_slice())
            .unwrap_or(&[])
    }

    /// Returns the minimum (first) key in the SSTable.
    /// Uses cached value from initialization to avoid disk I/O.
    pub fn first_key(&self) -> Result<Vec<u8>> {
        Ok(self.cached_first_key.clone())
    }

    /// Returns the number of data blocks.
    pub fn num_blocks(&self) -> usize {
        self.index.len()
    }

    /// Returns an iterator over all entries in the SSTable.
    pub fn iter(&self) -> Result<SsTableIterator<'_>> {
        SsTableIterator::new(self)
    }

    fn find_block(&self, key: &[u8]) -> Result<usize> {
        // Binary search in the index
        let mut lo = 0;
        let mut hi = self.index.len();

        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if key <= self.index[mid].last_key.as_slice() {
                hi = mid;
            } else {
                lo = mid + 1;
            }
        }

        if lo >= self.index.len() {
            return Err(CoreError::KeyNotFound { key: key.to_vec() });
        }

        Ok(lo)
    }

    fn _read_block(&self, entry: &BlockIndexEntry) -> Result<Vec<u8>> {
        self.read_block_at(entry.offset, entry.size)
    }

    fn read_block_at(&self, offset: u64, size: u64) -> Result<Vec<u8>> {
        let mut inner = self.inner.lock();

        // Check block cache first (returns decompressed data)
        if let Some(cached) = inner.block_cache.get(offset) {
            return Ok(cached.to_vec());
        }

        // Cache miss: read from disk
        let mut buf = vec![0u8; size as usize];
        inner.file.seek(SeekFrom::Start(offset))?;
        inner.file.read_exact(&mut buf)?;

        // Decompress if the SSTable uses compression
        let block_data = if self.compressed {
            zstd::decode_all(buf.as_slice())
                .map_err(|e| CoreError::corruption(&format!("zstd decompression failed: {}", e)))?
        } else {
            buf
        };

        // Cache the decompressed block
        inner.block_cache.put(offset, block_data.clone());

        Ok(block_data)
    }

    fn search_block(&self, block: &[u8], key: &[u8]) -> Result<Option<(Vec<u8>, SeqNo)>> {
        match self.search_block_full(block, key)? {
            Some((v, seq, EntryKind::Put)) => Ok(Some((v, seq))),
            Some((_, _, EntryKind::Delete)) => Ok(None), // Tombstone
            None => Ok(None),
        }
    }

    fn search_block_full(&self, block: &[u8], key: &[u8]) -> Result<Option<(Vec<u8>, SeqNo, EntryKind)>> {
        // Read restart points
        if block.len() < 4 {
            return Ok(None);
        }

        let num_restarts =
            u32::from_le_bytes(block[block.len() - 4..block.len()].try_into().unwrap()) as usize;

        if num_restarts == 0 {
            return Ok(None);
        }

        let restart_start = block.len() - 4 - num_restarts * 4;
        let mut restarts = Vec::with_capacity(num_restarts);
        for i in 0..num_restarts {
            let off = restart_start + i * 4;
            restarts.push(u32::from_le_bytes(block[off..off + 4].try_into().unwrap()) as usize);
        }

        // Binary search restart points
        let mut lo = 0;
        let mut hi = num_restarts;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let entry = self.decode_entry_at(block, restarts[mid]);
            match entry {
                Some((k, _, _, _)) if k.as_slice() <= key => lo = mid + 1,
                _ => hi = mid,
            }
        }

        // Linear scan from the restart point
        let start = if lo > 0 { restarts[lo - 1] } else { 0 };

        let mut pos = start;
        while pos < restart_start {
            if let Some((k, v, seq, kind)) = self.decode_entry_at(block, pos) {
                if k.as_slice() == key {
                    return Ok(Some((v, seq, kind)));
                }
                if k.as_slice() > key {
                    break;
                }
                pos += 4 + k.len() + 4 + v.len() + 8 + 1;
            } else {
                break;
            }
        }

        Ok(None)
    }

    fn decode_entry_at(&self, data: &[u8], offset: usize) -> Option<(Vec<u8>, Vec<u8>, SeqNo, EntryKind)> {
        let mut pos = offset;

        // key_len
        if pos + 4 > data.len() {
            return None;
        }
        let key_len = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
        pos += 4;

        // key
        if pos + key_len > data.len() {
            return None;
        }
        let key = data[pos..pos + key_len].to_vec();
        pos += key_len;

        // value_len
        if pos + 4 > data.len() {
            return None;
        }
        let val_len = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
        pos += 4;

        // value
        if pos + val_len > data.len() {
            return None;
        }
        let value = data[pos..pos + val_len].to_vec();
        pos += val_len;

        // seq_no
        if pos + 8 > data.len() {
            return None;
        }
        let seq_no = u64::from_le_bytes(data[pos..pos + 8].try_into().unwrap());
        pos += 8;

        // kind
        if pos >= data.len() {
            return None;
        }
        let kind = match data[pos] {
            0 => EntryKind::Put,
            _ => EntryKind::Delete,
        };

        Some((key, value, seq_no, kind))
    }

    fn decode_index(data: &[u8]) -> Vec<BlockIndexEntry> {
        if data.len() < 4 {
            return Vec::new();
        }

        let count = u32::from_le_bytes(data[0..4].try_into().unwrap()) as usize;
        let mut entries = Vec::with_capacity(count);
        let mut pos = 4;

        for _ in 0..count {
            if pos + 4 > data.len() {
                break;
            }
            let key_len = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
            pos += 4;

            if pos + key_len > data.len() {
                break;
            }
            let last_key = data[pos..pos + key_len].to_vec();
            pos += key_len;

            if pos + 16 > data.len() {
                break;
            }
            let offset = u64::from_le_bytes(data[pos..pos + 8].try_into().unwrap());
            let size = u64::from_le_bytes(data[pos + 8..pos + 16].try_into().unwrap());
            pos += 16;

            entries.push(BlockIndexEntry {
                last_key,
                offset,
                size,
            });
        }

        entries
    }
}

/// Iterator over SSTable entries.
///
/// Zero-copy design: stores byte offsets into the current block instead of
/// cloning key/value into separate Vecs. `key()` and `value()` return
/// slices borrowed from `block_data`, avoiding per-entry allocations.
pub struct SsTableIterator<'a> {
    table: &'a SsTable,
    block_idx: usize,
    block_data: Vec<u8>,
    restart_start: usize,
    restarts: Vec<usize>,
    pos: usize,
    // Byte offsets into block_data for the current entry (zero-copy).
    current_key_start: usize,
    current_key_end: usize,
    current_value_start: usize,
    current_value_end: usize,
    current_seq: SeqNo,
    current_kind: EntryKind,
    valid: bool,
}

impl<'a> SsTableIterator<'a> {
    fn new(table: &'a SsTable) -> Result<Self> {
        let mut iter = Self {
            table,
            block_idx: 0,
            block_data: Vec::new(),
            restart_start: 0,
            restarts: Vec::new(),
            pos: 0,
            current_key_start: 0,
            current_key_end: 0,
            current_value_start: 0,
            current_value_end: 0,
            current_seq: 0,
            current_kind: EntryKind::Put,
            valid: false,
        };

        if !iter.table.index.is_empty() {
            iter.load_block(0)?;
            iter.advance_to_next_entry();
        }

        Ok(iter)
    }

    fn load_block(&mut self, idx: usize) -> Result<()> {
        // Copy offset/size to avoid holding borrow on table.index
        let block_offset = self.table.index[idx].offset;
        let block_size = self.table.index[idx].size;
        self.block_data = self.table.read_block_at(block_offset, block_size)?;
        self.block_idx = idx;

        // Parse restart points
        let data = &self.block_data;
        if data.len() < 4 {
            self.restarts = Vec::new();
            self.restart_start = 0;
            return Ok(());
        }

        let num_restarts =
            u32::from_le_bytes(data[data.len() - 4..data.len()].try_into().unwrap()) as usize;
        let restart_data_size = num_restarts * 4;
        if restart_data_size + 4 > data.len() {
            return Err(CoreError::corruption("invalid restart count in block"));
        }
        self.restart_start = data.len() - 4 - restart_data_size;
        self.restarts = Vec::with_capacity(num_restarts);

        for i in 0..num_restarts {
            let off = self.restart_start + i * 4;
            self.restarts
                .push(u32::from_le_bytes(data[off..off + 4].try_into().unwrap()) as usize);
        }

        self.pos = 0;
        Ok(())
    }

    fn advance_to_next_entry(&mut self) {
        if self.pos >= self.restart_start {
            // Try next block
            if self.block_idx + 1 < self.table.index.len() {
                let next_idx = self.block_idx + 1;
                if self.load_block(next_idx).is_err() {
                    self.valid = false;
                    return;
                }
            } else {
                self.valid = false;
                return;
            }
        }

        // Decode entry offsets without copying data (zero-copy).
        if let Some((key_start, key_end, val_start, val_end, seq, kind)) =
            self.decode_entry_offsets(&self.block_data, self.pos)
        {
            self.current_kind = kind;
            self.current_key_start = key_start;
            self.current_key_end = key_end;
            self.current_value_start = val_start;
            self.current_value_end = val_end;
            self.current_seq = seq;
            // Skip past value + seq_no (8) + kind (1)
            self.pos = val_end + 8 + 1;
            self.valid = true;
        } else {
            self.valid = false;
        }
    }

    /// Decodes entry offsets without allocating (zero-copy).
    fn decode_entry_offsets(
        &self,
        data: &[u8],
        offset: usize,
    ) -> Option<(usize, usize, usize, usize, SeqNo, EntryKind)> {
        let mut pos = offset;

        // key_len
        if pos + 4 > data.len() {
            return None;
        }
        let key_len = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
        pos += 4;

        // key
        if pos + key_len > data.len() {
            return None;
        }
        let key_start = pos;
        let key_end = pos + key_len;
        pos = key_end;

        // value_len
        if pos + 4 > data.len() {
            return None;
        }
        let val_len = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
        pos += 4;

        // value
        if pos + val_len > data.len() {
            return None;
        }
        let val_start = pos;
        let val_end = pos + val_len;
        pos = val_end;

        // seq_no (8 bytes)
        if pos + 8 > data.len() {
            return None;
        }
        let seq_no = u64::from_le_bytes(data[pos..pos + 8].try_into().unwrap());
        pos += 8;

        // kind (1 byte)
        if pos >= data.len() {
            return None;
        }
        let kind = match data[pos] {
            0 => EntryKind::Put,
            _ => EntryKind::Delete,
        };

        Some((key_start, key_end, val_start, val_end, seq_no, kind))
    }

    pub fn is_valid(&self) -> bool {
        self.valid
    }

    /// Returns the current key as a zero-copy slice into the block data.
    pub fn key(&self) -> &[u8] {
        &self.block_data[self.current_key_start..self.current_key_end]
    }

    /// Returns the current value as a zero-copy slice into the block data.
    pub fn value(&self) -> &[u8] {
        &self.block_data[self.current_value_start..self.current_value_end]
    }

    pub fn seq_no(&self) -> SeqNo {
        self.current_seq
    }

    pub fn kind(&self) -> EntryKind {
        self.current_kind
    }

    pub fn next(&mut self) {
        self.advance_to_next_entry();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_sstable_write_and_read() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.sst");

        let mut builder = SsTableBuilder::new();
        for i in 0..100u32 {
            let key = format!("key_{:04}", i);
            let value = format!("value_{}", i);
            builder.add(&Entry::put(
                key.into_bytes(),
                value.into_bytes(),
                i as u64,
            ));
        }
        builder.build(&path).unwrap();

        // Open and read
        let mut sst = SsTable::open(&path).unwrap();

        // Point lookups
        let (val, _) = sst.get(b"key_0050").unwrap().unwrap();
        assert_eq!(val, b"value_50");

        let (val, _) = sst.get(b"key_0001").unwrap().unwrap();
        assert_eq!(val, b"value_1");

        // Missing key
        assert!(sst.get(b"missing_key").unwrap().is_none());
    }

    #[test]
    fn test_sstable_iteration() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("iter_test.sst");

        let mut builder = SsTableBuilder::new();
        for i in 0..50u32 {
            let key = format!("k{:03}", i);
            builder.add(&Entry::put(key.into_bytes(), vec![i as u8], i as u64));
        }
        builder.build(&path).unwrap();

        let mut sst = SsTable::open(&path).unwrap();
        let mut iter = sst.iter().unwrap();

        let mut count = 0;
        while iter.is_valid() {
            count += 1;
            iter.next();
        }
        assert_eq!(count, 50);
    }

    #[test]
    fn test_sstable_compressed_write_and_read() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("compressed.sst");

        let mut builder = SsTableBuilder::new();
        builder.set_compression_level(3);
        for i in 0..200u32 {
            let key = format!("key_{:06}", i);
            let value = format!("value_{:06}_padding_data_to_make_compression_worthwhile", i);
            builder.add(&Entry::put(
                key.into_bytes(),
                value.into_bytes(),
                i as u64,
            ));
        }
        builder.build(&path).unwrap();

        // Open and read
        let mut sst = SsTable::open(&path).unwrap();
        assert!(sst.compressed, "SSTable should be marked as compressed");

        // Point lookups
        let (val, _) = sst.get(b"key_000050").unwrap().unwrap();
        assert_eq!(val, b"value_000050_padding_data_to_make_compression_worthwhile");

        let (val, _) = sst.get(b"key_000001").unwrap().unwrap();
        assert_eq!(val, b"value_000001_padding_data_to_make_compression_worthwhile");

        // Missing key
        assert!(sst.get(b"missing_key").unwrap().is_none());
    }

    #[test]
    fn test_sstable_compressed_iteration() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("compressed_iter.sst");

        let mut builder = SsTableBuilder::new();
        builder.set_compression_level(3);
        for i in 0..100u32 {
            let key = format!("k{:04}", i);
            let value = format!("v{:04}_some_repetitive_value_for_compression", i);
            builder.add(&Entry::put(key.into_bytes(), value.into_bytes(), i as u64));
        }
        builder.build(&path).unwrap();

        let mut sst = SsTable::open(&path).unwrap();
        let mut iter = sst.iter().unwrap();

        let mut count = 0;
        while iter.is_valid() {
            let key = iter.key();
            let expected_key = format!("k{:04}", count);
            assert_eq!(key, expected_key.as_bytes());
            count += 1;
            iter.next();
        }
        assert_eq!(count, 100);
    }

    #[test]
    fn test_sstable_compressed_smaller_size() {
        let dir = tempdir().unwrap();
        let path_uncompressed = dir.path().join("uncompressed.sst");
        let path_compressed = dir.path().join("compressed.sst");

        // Build uncompressed
        let mut builder = SsTableBuilder::new();
        for i in 0..500u32 {
            let key = format!("key_{:06}", i);
            let value = format!("value_with_repetitive_padding_data_for_compression_test_{:06}", i);
            builder.add(&Entry::put(key.into_bytes(), value.into_bytes(), i as u64));
        }
        builder.build(&path_uncompressed).unwrap();

        // Build compressed
        let mut builder = SsTableBuilder::new();
        builder.set_compression_level(3);
        for i in 0..500u32 {
            let key = format!("key_{:06}", i);
            let value = format!("value_with_repetitive_padding_data_for_compression_test_{:06}", i);
            builder.add(&Entry::put(key.into_bytes(), value.into_bytes(), i as u64));
        }
        builder.build(&path_compressed).unwrap();

        let size_uncompressed = std::fs::metadata(&path_uncompressed).unwrap().len();
        let size_compressed = std::fs::metadata(&path_compressed).unwrap().len();

        println!("Uncompressed: {} bytes, Compressed: {} bytes, Ratio: {:.1}%",
            size_uncompressed, size_compressed,
            (size_compressed as f64 / size_uncompressed as f64) * 100.0);

        assert!(
            size_compressed < size_uncompressed,
            "Compressed SSTable ({}) should be smaller than uncompressed ({})",
            size_compressed, size_uncompressed
        );
    }

    #[test]
    fn test_sstable_compressed_with_tombstones() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("compressed_tomb.sst");

        let mut builder = SsTableBuilder::new();
        builder.set_compression_level(3);
        // Mix Put and Delete entries
        for i in 0..100u32 {
            let key = format!("key_{:04}", i);
            if i % 3 == 0 {
                builder.add(&Entry::delete(key.into_bytes(), i as u64));
            } else {
                let value = format!("value_{:04}", i);
                builder.add(&Entry::put(key.into_bytes(), value.into_bytes(), i as u64));
            }
        }
        builder.build(&path).unwrap();

        let mut sst = SsTable::open(&path).unwrap();

        // Check put entries
        let result = sst.get_full(b"key_0001").unwrap().unwrap();
        assert_eq!(result.2, EntryKind::Put);
        assert_eq!(result.0, b"value_0001");

        // Check tombstone entries
        let result = sst.get_full(b"key_0000").unwrap().unwrap();
        assert_eq!(result.2, EntryKind::Delete);

        // get() should return None for tombstones
        assert!(sst.get(b"key_0000").unwrap().is_none());
    }
}
