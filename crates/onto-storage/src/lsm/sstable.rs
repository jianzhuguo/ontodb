// Copyright (c) 2024-2026 OntoDB Team
// Licensed under the Business Source License 1.1 (BUSL-1.1).
// See LICENSE for details. Change Date: 2031-09-15.
// On the Change Date, this file will be licensed under Apache License 2.0.
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
use parking_lot::Mutex;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Default block cache capacity (number of blocks per SSTable).
const DEFAULT_BLOCK_CACHE_CAPACITY: usize = 64;

/// Footer size: index_offset (8) + bloom_offset (8) + magic (8) + flags (1)
const FOOTER_SIZE: u64 = 25;
const MAGIC: u64 = 0x4F4E544F_44425353; // "ONTO_DBSS"

/// Flag bit indicating data blocks are zstd-compressed (table-level hint).
/// With `FLAG_BLOCK_CRC`, actual compression is decided per block.
const FLAG_COMPRESSED: u8 = 0x01;
/// Flag bit indicating footer has max_seq_no field (8 bytes after flags).
const FLAG_HAS_MAX_SEQ: u8 = 0x02;
/// Flag bit indicating each data block is stored as
/// `[u8 block_flags][payload][u32 crc32]` (crc covers flags+payload).
const FLAG_BLOCK_CRC: u8 = 0x04;

/// Per-block flag: payload is zstd-compressed.
const BLOCK_FLAG_COMPRESSED: u8 = 0x01;

/// Hard cap on a single block's decompressed size. Blocks flush at ~4 KiB of
/// entries, but a single huge value can make one larger. Anything above this
/// is treated as corruption rather than being handed to the decompressor.
const MAX_DECOMPRESSED_BLOCK_SIZE: usize = 64 * 1024 * 1024;

/// Extra output slack so `ZSTD_wildcopy` cannot store past the allocation
/// even if a corrupt sequence slips past the length checks.
const ZSTD_WILDCOPY_MARGIN: usize = 64;

/// Guard bytes after the wildcopy slack. Intact after a successful
/// decompress proves the decompressor stayed inside the allocation;
/// a smashed canary means a corrupt frame bypassed zstd's length checks.
const ZSTD_GUARD_CANARY: [u8; 16] = [
    0xC5, 0x9A, 0x3B, 0x71, 0xE2, 0x4D, 0x88, 0x16, 0x5F, 0xA3, 0x0C, 0xD7, 0x64, 0x29, 0xB1, 0x7E,
];

/// Block format: [entries...][restart_points...][num_restarts: u32]
const RESTART_INTERVAL: usize = 16;

/// Interior of SsTable requiring interior mutability (file I/O + block cache).
///
/// `file` and `block_cache` are separate locks so a cache hit never waits on
/// disk I/O, and decompression runs outside both.
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
    /// Whether data blocks are zstd-compressed (legacy table-level flag).
    /// When `block_crc` is set, per-block flags take precedence.
    compressed: bool,
    /// Whether each data block carries a per-block header + CRC32 suffix.
    block_crc: bool,
    /// Cached first key (minimum key) in this SSTable.
    /// Loaded once at open() to avoid repeated disk I/O.
    cached_first_key: Vec<u8>,
    /// Maximum sequence number in this SSTable (stored in footer for fast startup).
    max_seq_no: SeqNo,
}

#[derive(Debug, Clone)]
struct BlockIndexEntry {
    last_key: Vec<u8>,
    offset: u64,
    size: u64,
}

/// `ZSTD_error_dstSize_tooSmall` is -70. zstd-safe 7 reports errors as a raw
/// wrapping `usize` (e.g. 18446744073709551546), and its `Display` prints that
/// number — so string matching on the message never matches.
fn is_dst_size_too_small(err: isize) -> bool {
    err == -70
}

/// Thread-local reused zstd DCtx. Same rationale as the thread-local
/// `Compressor` on the write side: one-shot `zstd_safe::decompress` builds a
/// fresh DCtx per call, and a failed workspace reservation in zstd 1.5.7
/// becomes a NULL write (heap corruption / 0xc0000374) under memory pressure.
pub(crate) fn zstd_decompress_to(dst: &mut [u8], src: &[u8]) -> std::result::Result<usize, isize> {
    thread_local! {
        static DECOMPRESSOR: std::cell::RefCell<Option<zstd::bulk::Decompressor<'static>>> =
            std::cell::RefCell::new(None);
    }
    DECOMPRESSOR.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.is_none() {
            match zstd::bulk::Decompressor::new() {
                Ok(d) => *slot = Some(d),
                Err(_) => {
                    // Fall back to one-shot; still bounded by dst slice.
                    return zstd::zstd_safe::decompress(dst, src).map_err(|e| e as isize);
                }
            }
        }
        let dec = slot.as_mut().expect("just initialized");
        match dec.decompress_to_buffer(src, dst) {
            Ok(n) => Ok(n),
            Err(e) => {
                // zstd-safe maps ZSTD codes onto io::Error. Recover the code
                // so the dstSize_tooSmall retry loop keeps working.
                if let Some(code) = e.raw_os_error() {
                    if code != 0 {
                        return Err(code as isize);
                    }
                }
                let s = e.to_string();
                // Display is often the raw wrapping usize / negative code.
                if let Ok(n) = s.trim().parse::<i64>() {
                    return Err(n as isize);
                }
                // ErrorCode Display variants.
                if s.contains("Destination buffer is too small") || s.contains("dstSize") {
                    return Err(-70);
                }
                if s.contains("prefix") || s.contains("Generic") {
                    return Err(-1);
                }
                Err(-1)
            }
        }
    })
}

/// Decompress a zstd frame, refusing frames that would blow past `max_out`.
///
/// Uses a reused one-shot DCtx (not the streaming decoder).
///
/// Critical sizing: allocate `out_cap + ZSTD_WILDCOPY_MARGIN`, but hand zstd
/// only `&mut buf[..out_cap]`. `ZSTD_wildcopy` may store up to 32 bytes past
/// the logical output end — if `dstCapacity` includes the slack, a frame that
/// fills the buffer lets wildcopy write past the allocation and trash the
/// heap. Slice `WriteBuf` also avoids `Vec::set_len` (which can mint
/// `ptr=dangling, len>0`).
fn decompress_limited(src: &[u8], max_out: usize) -> Result<Vec<u8>> {
    if src.is_empty() {
        return Err(CoreError::corruption("empty zstd block"));
    }

    // Size the output from the frame header when present. A declared content
    // size above the limit (or a header that cannot be parsed) is already
    // proof of corruption — never hand it to the decompressor.
    let declared = match zstd::zstd_safe::get_frame_content_size(src) {
        Ok(Some(0)) => {
            return Err(CoreError::corruption("zstd frame declares empty content"));
        }
        Ok(Some(n)) if n > max_out as u64 => {
            return Err(CoreError::corruption(format!(
                "zstd frame declares {} bytes, exceeds limit {}",
                n, max_out
            )));
        }
        Ok(Some(n)) => Some(n as usize),
        Ok(None) => None,
        Err(_) => {
            return Err(CoreError::corruption("invalid zstd frame header"));
        }
    };

    let margin = ZSTD_WILDCOPY_MARGIN;

    // Decompress into a buffer sized `out_cap + margin`, with zstd's
    // destination capacity limited to `out_cap` so wildcopy stays inside
    // the allocation.
    //
    // The DCtx is thread-local and reused, mirroring the compressor side:
    // `zstd_safe::decompress` builds a fresh DCtx (multi-MB workspace) per
    // call, and under heavy compaction the workspace allocation can fail —
    // zstd 1.5.7 does not always check `cwksp_reserve_failed`, so a failed
    // reservation becomes a NULL write and later surfaces as 0xc0000374.
    let try_once = |out_cap: usize| -> std::result::Result<(Vec<u8>, usize), isize> {
        if out_cap == 0 {
            return Err(-64);
        }
        // Layout: [out_cap dst][wildcopy slack][guard canary].
        // zstd only sees out_cap; slack absorbs legal wildcopy overshoot;
        // an intact canary proves nothing wrote past the allocation.
        let canary_start = out_cap.saturating_add(margin);
        let alloc = canary_start.saturating_add(ZSTD_GUARD_CANARY.len());
        let mut buf = vec![0u8; alloc];
        buf[canary_start..].copy_from_slice(&ZSTD_GUARD_CANARY);
        let written = zstd_decompress_to(&mut buf[..out_cap], src)?;
        if buf[canary_start..] != ZSTD_GUARD_CANARY[..] {
            // Corrupt frame wrote past dstCapacity + slack. Refuse the data;
            // the heap may already be damaged, but do not propagate it.
            return Err(-80);
        }
        if written > out_cap {
            return Err(-70);
        }
        buf.truncate(written);
        Ok((buf, written))
    };

    // Known content size (one-shot frames): allocate exactly what we need.
    if let Some(n) = declared {
        let (buf, written) = try_once(n).map_err(|e| {
            if e == -80 {
                CoreError::corruption(
                    "zstd wrote past output buffer (guard canary smashed — corrupt frame)",
                )
            } else {
                CoreError::corruption(format!("zstd decompression failed: {}", e))
            }
        })?;
        if written > max_out {
            return Err(CoreError::corruption(format!(
                "decompressed block size {} exceeds limit {}",
                written, max_out
            )));
        }
        return Ok(buf);
    }

    // Unknown content size (streaming-compressed frames): grow geometrically
    // from a modest start. Never reserve the hard cap up front — a 64 MiB
    // allocation per block OOMs the process on this dataset.
    let mut cap = src.len().saturating_mul(4).clamp(64 * 1024, 4 * 1024 * 1024);
    if cap > max_out {
        cap = max_out;
    }
    loop {
        match try_once(cap) {
            Ok((buf, written)) => {
                if written > max_out {
                    return Err(CoreError::corruption(format!(
                        "decompressed block size {} exceeds limit {}",
                        written, max_out
                    )));
                }
                return Ok(buf);
            }
            Err(e) => {
                if e == -80 {
                    return Err(CoreError::corruption(
                        "zstd wrote past output buffer (guard canary smashed — corrupt frame)",
                    ));
                }
                if !is_dst_size_too_small(e) || cap >= max_out {
                    return Err(CoreError::corruption(format!(
                        "zstd decompression failed: {}",
                        e
                    )));
                }
                cap = cap.saturating_mul(2).min(max_out);
            }
        }
    }
}

/// Builder for constructing an SSTable from sorted entries.
pub struct SsTableBuilder {
    data: Vec<u8>,
    current_block: Vec<u8>,
    current_block_entries: usize,
    block_entries: Vec<BlockIndexEntry>,
    current_block_offset: u64,
    restart_points: Vec<u32>,
    entry_count_in_block: usize,
    /// Bloom filter filled incrementally as entries are added. Replaces the
    /// old `keys_for_bloom: Vec<Vec<u8>>` which held every key for the whole
    /// SST in millions of tiny allocations and OOM'd large merges.
    bloom: BloomFilter,
    /// First key of the SST (for `SsTable::cached_first_key`). Captured once.
    first_key: Vec<u8>,
    /// Total entries added. Replaces `keys_for_bloom.is_empty()` checks.
    entry_count: u64,
    /// Byte range of the last entry's key inside `current_block`, so
    /// `flush_block` can recover the index key without cloning per entry.
    last_key_start: usize,
    last_key_len: usize,
    /// zstd compression level (0 = disabled, 1-21 = enabled).
    compression_level: i32,
    /// Maximum sequence number seen during build.
    max_seq_no: SeqNo,
}

/// Default bloom capacity for builders with no entry hint. Sized for a
/// typical memtable flush (tens of thousands of entries, ~80 KB bit array).
/// Callers that build large SSTs should pass a better estimate via
/// `with_expected_entries`; under-estimating only raises the false-positive
/// rate — never false negatives.
const DEFAULT_BLOOM_EXPECTED_ENTRIES: usize = 65536;

impl Default for SsTableBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl SsTableBuilder {
    pub fn new() -> Self {
        Self::with_expected_entries(DEFAULT_BLOOM_EXPECTED_ENTRIES)
    }

    /// Creates a builder whose bloom is sized for `expected_entries`.
    /// Under-estimating only raises the false-positive rate; correctness is
    /// preserved either way.
    pub fn with_expected_entries(expected_entries: usize) -> Self {
        Self {
            data: Vec::new(),
            current_block: Vec::new(),
            current_block_entries: 0,
            block_entries: Vec::new(),
            current_block_offset: 0,
            restart_points: Vec::new(),
            entry_count_in_block: 0,
            bloom: BloomFilter::new(expected_entries.max(1), 0.01),
            first_key: Vec::new(),
            entry_count: 0,
            last_key_start: 0,
            last_key_len: 0,
            compression_level: 0,
            max_seq_no: 0,
        }
    }

    /// Enables zstd compression on data blocks. Level 1-21 (higher = better ratio, slower).
    pub fn set_compression_level(&mut self, level: i32) {
        self.compression_level = level.clamp(0, 21);
    }

    /// Adds an entry to the SSTable. Entries MUST be added in sorted key order.
    pub fn add(&mut self, entry: &Entry) {
        self.max_seq_no = self.max_seq_no.max(entry.seq_no);
        self.entry_count += 1;
        // Dangling empty-allocation Vecs (len > 0, cap == 0) must not be
        // read — that memcpy takes the process down mid-compaction.
        let key: &[u8] = if entry.key.capacity() == 0 {
            &[]
        } else {
            entry.key.as_slice()
        };
        self.bloom.insert(key);
        if self.first_key.is_empty() && !key.is_empty() {
            self.first_key.extend_from_slice(key);
        }

        // Record restart point
        if self.entry_count_in_block.is_multiple_of(RESTART_INTERVAL) {
            self.restart_points.push(self.current_block.len() as u32);
        }

        // Encode entry into current block; remember where the key landed so
        // flush_block can slice the index key out without a per-entry clone.
        let key_start = self.current_block.len() + 4;
        Self::encode_entry_to(&mut self.current_block, entry);
        self.last_key_start = key_start;
        self.last_key_len = key.len();
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
        self.max_seq_no = self.max_seq_no.max(entry.seq_no);
        self.entry_count += 1;
        let key: &[u8] = if entry.key.capacity() == 0 {
            &[]
        } else {
            entry.key.as_slice()
        };
        self.bloom.insert(key);
        if self.first_key.is_empty() && !key.is_empty() {
            self.first_key.extend_from_slice(key);
        }

        // Record restart point
        if self.entry_count_in_block.is_multiple_of(RESTART_INTERVAL) {
            self.restart_points.push(self.current_block.len() as u32);
        }

        let key_start = self.current_block.len() + 4;
        Self::encode_entry_to(&mut self.current_block, &entry);
        self.last_key_start = key_start;
        self.last_key_len = key.len();
        self.current_block_entries += 1;
        self.entry_count_in_block += 1;

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

        // Compress block if enabled. One-shot compression writes the content
        // size into the frame header so the reader can allocate exactly.
        // On failure fall back to raw payload — the per-block flag records
        // what was actually stored so the reader never feeds uncompressed
        // bytes to zstd.
        //
        // Compressor is thread-local and reused: `zstd::bulk::compress` builds
        // a fresh CCtx (multi-MB workspace) per call, and under heavy compaction
        // the workspace allocation can fail — zstd 1.5.7 does not check
        // `cwksp_reserve_failed` after reserving `seqStore.llCode`, so a failed
        // reservation becomes a NULL write in `ZSTD_seqToCodes`. Reusing the
        // CCtx keeps the workspace warm and avoids the churn.
        let (block_flags, payload) = if self.compression_level > 0 {
            thread_local! {
                static COMPRESSOR: std::cell::RefCell<Option<zstd::bulk::Compressor<'static>>> =
                    std::cell::RefCell::new(None);
            }
            let result = COMPRESSOR.with(|cell| {
                let mut slot = cell.borrow_mut();
                if slot.is_none() {
                    match zstd::bulk::Compressor::new(self.compression_level) {
                        Ok(c) => *slot = Some(c),
                        Err(_) => return None,
                    }
                }
                let compressor = slot.as_mut()?;
                // Slice WriteBuf: never `Vec::set_len`, same contract as decompress.
                let bound = zstd::zstd_safe::compress_bound(self.current_block.len());
                let mut buf = vec![0u8; bound];
                match compressor.compress_to_buffer(self.current_block.as_slice(), &mut buf[..]) {
                    Ok(n) => Some(buf[..n].to_vec()),
                    Err(_) => None,
                }
            });
            match result {
                Some(compressed) => (BLOCK_FLAG_COMPRESSED, compressed),
                None => (0u8, self.current_block.clone()),
            }
        } else {
            (0u8, self.current_block.clone())
        };

        // New block on-disk format: [flags:1][payload][crc32:4]
        let mut stored = Vec::with_capacity(1 + payload.len() + 4);
        stored.push(block_flags);
        stored.extend_from_slice(&payload);
        let crc = crc32fast::hash(&stored);
        stored.extend_from_slice(&crc.to_le_bytes());

        let block_size = stored.len() as u64;

        // Recover the index key from the block bytes — restart points were
        // appended after the entries, so the recorded key range is still valid.
        let last_key = if self.last_key_len > 0
            && self.last_key_start + self.last_key_len <= self.current_block.len()
        {
            self.current_block[self.last_key_start..self.last_key_start + self.last_key_len].to_vec()
        } else {
            Vec::new()
        };

        // Record block index
        self.block_entries.push(BlockIndexEntry {
            last_key,
            offset: self.current_block_offset,
            size: block_size,
        });

        // Append to main data
        self.data.extend_from_slice(&stored);
        self.current_block_offset += block_size;

        // Reset
        self.current_block.clear();
        self.current_block_entries = 0;
        self.restart_points.clear();
        self.entry_count_in_block = 0;
        self.last_key_start = 0;
        self.last_key_len = 0;
    }

    /// Builds the SSTable and writes it to disk.
    ///
    /// Uses write-to-temp-then-rename pattern for crash safety:
    /// 1. Write all data to `path.sst.tmp`
    /// 2. fsync the temp file
    /// 3. Atomic rename to `path.sst`
    ///
    /// If the process crashes during step 1-2, the `.tmp` file is orphaned but
    /// the engine won't load it (wrong extension). Only completed files are loaded.
    pub fn build(mut self, path: impl AsRef<Path>) -> Result<SsTable> {
        // Flush remaining block
        self.flush_block();

        // If we actually wrote entries but every seq_no was 0 (e.g. output of
        // a merge over legacy files whose seqs were zeroed), the footer would
        // land max_seq_no=0 and the engine would classify the file as a
        // permanent "old SST" zombie. Stamp a sentinel so it is not.
        if self.max_seq_no == 0 && self.entry_count > 0 {
            self.max_seq_no = 1;
        }

        let final_path = path.as_ref().to_path_buf();
        let tmp_path = final_path.with_extension("sst.tmp");

        let mut file = BufWriter::new(
            OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&tmp_path)?,
        );

        // Write data blocks
        file.write_all(&self.data)?;
        let data_size = self.data.len() as u64;

        // Build and write index block
        let index_offset = data_size;
        let index_data = Self::encode_index(&self.block_entries);
        file.write_all(&index_data)?;

        // Write bloom filter (built incrementally during add)
        let bloom_offset = index_offset + index_data.len() as u64;
        let bloom = self.bloom;
        let bloom_data = bloom.to_bytes();
        file.write_all(&bloom_data)?;

        // Write footer: index_offset (8) + bloom_offset (8) + magic (8) + flags (1) + max_seq_no (8)
        // FLAG_BLOCK_CRC is always set for files written by this builder so the
        // reader can require per-block CRCs and per-block compression bits.
        let mut flags: u8 = FLAG_HAS_MAX_SEQ | FLAG_BLOCK_CRC;
        if self.compression_level > 0 {
            flags |= FLAG_COMPRESSED;
        }
        file.write_all(&index_offset.to_le_bytes())?;
        file.write_all(&bloom_offset.to_le_bytes())?;
        file.write_all(&MAGIC.to_le_bytes())?;
        file.write_all(&[flags])?;
        file.write_all(&self.max_seq_no.to_le_bytes())?;

        file.flush()?;
        file.get_ref().sync_all()?;
        drop(file); // Close the temp file before renaming

        // Atomic rename: only after this point does the engine see the file.
        // If the process crashes before this line, the .tmp file is orphaned
        // and will be cleaned up on next startup.
        std::fs::rename(&tmp_path, &final_path)?;

        // Reopen for reading
        let read_file = File::open(&final_path)?;
        let compressed = self.compression_level > 0;

        // Cache the first key (minimum key) for fast access
        let cached_first_key = self.first_key;

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
            block_crc: true,
            cached_first_key,
            max_seq_no: self.max_seq_no,
        })
    }

    fn encode_entry_to(buf: &mut Vec<u8>, entry: &Entry) {
        // An impossible Vec (len > 0 but no allocation) is always a dangling
        // pointer (typically 0x1). extend_from_slice would memcpy from it and
        // take the process down during compaction. Treat it as empty instead.
        let key: &[u8] = if entry.key.capacity() == 0 {
            &[]
        } else {
            entry.key.as_slice()
        };
        let value: &[u8] = if entry.value.capacity() == 0 {
            &[]
        } else {
            entry.value.as_slice()
        };
        // key_len (4) + key + value_len (4) + value + seq_no (8) + kind (1)
        buf.extend_from_slice(&(key.len() as u32).to_le_bytes());
        buf.extend_from_slice(key);
        buf.extend_from_slice(&(value.len() as u32).to_le_bytes());
        buf.extend_from_slice(value);
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

        // Add CRC32 checksum for integrity verification
        let crc = crc32fast::hash(&buf);
        buf.extend_from_slice(&crc.to_le_bytes());

        buf
    }
}

impl SsTable {
    /// Opens an existing SSTable file.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let mut file = File::open(path.as_ref())?;
        let file_len = file.metadata()?.len();

        // Try new footer format first (33 bytes: +max_seq_no), fall back to old (25 bytes)
        let mut max_seq_no: SeqNo = 0;
        let has_max_seq = if file_len >= 33 {
            file.seek(SeekFrom::End(-33))?;
            let mut buf33 = [0u8; 33];
            file.read_exact(&mut buf33)?;
            let magic33 = u64::from_le_bytes(buf33[16..24].try_into().unwrap());
            let flags33 = buf33[24];
            if magic33 == MAGIC && flags33 & FLAG_HAS_MAX_SEQ != 0 {
                max_seq_no = u64::from_le_bytes(buf33[25..33].try_into().unwrap());
                true
            } else {
                false
            }
        } else {
            false
        };

        // Read footer: index_offset (8) + bloom_offset (8) + magic (8) + flags (1)
        let footer_size: i64 = if has_max_seq { 33 } else { 25 };
        file.seek(SeekFrom::End(-footer_size))?;
        let mut footer = [0u8; 25];
        file.read_exact(&mut footer)?;

        let index_offset = u64::from_le_bytes(footer[0..8].try_into().expect("should be valid"));
        let bloom_offset = u64::from_le_bytes(footer[8..16].try_into().expect("should be valid"));
        let magic = u64::from_le_bytes(footer[16..24].try_into().expect("should be valid"));
        let flags = footer[24];
        let compressed = flags & FLAG_COMPRESSED != 0;
        let block_crc = flags & FLAG_BLOCK_CRC != 0;

        if magic != MAGIC {
            return Err(CoreError::corruption("invalid SSTable magic number"));
        }

        let file_len = file.metadata()?.len();
        let footer_bytes = footer_size as u64;

        // Validate footer offsets before any length arithmetic. A truncated or
        // half-written file can report offsets past the payload region; unchecked
        // u64 subtraction would underflow into a multi-GB allocation.
        if file_len < footer_bytes {
            return Err(CoreError::corruption(format!(
                "SSTable too short: {} bytes (footer needs {})",
                file_len, footer_bytes
            )));
        }
        let payload_end = file_len - footer_bytes;
        if index_offset > payload_end || bloom_offset > payload_end {
            return Err(CoreError::corruption(format!(
                "SSTable footer offsets out of range: index_offset={}, bloom_offset={}, payload_end={}",
                index_offset, bloom_offset, payload_end
            )));
        }
        if index_offset > bloom_offset {
            return Err(CoreError::corruption(format!(
                "SSTable footer offset order invalid: index_offset={} > bloom_offset={}",
                index_offset, bloom_offset
            )));
        }

        // Read bloom filter
        file.seek(SeekFrom::Start(bloom_offset))?;
        let bloom_data_len = (payload_end - bloom_offset) as usize;
        let mut bloom_data = vec![0u8; bloom_data_len];
        file.read_exact(&mut bloom_data)?;
        let bloom = BloomFilter::from_bytes(&bloom_data);

        // Read index
        file.seek(SeekFrom::Start(index_offset))?;
        let index_data_len = (bloom_offset - index_offset) as usize;
        let mut index_data = vec![0u8; index_data_len];
        file.read_exact(&mut index_data)?;
        let index = Self::decode_index(&index_data, index_offset, block_crc)?;

        // Cache the first key to avoid repeated disk I/O
        let cached_first_key = if index.is_empty() {
            Vec::new()
        } else {
            // Read first block to get the first key
            let offset = index[0].offset;
            let size = index[0].size;
            if size as usize > MAX_DECOMPRESSED_BLOCK_SIZE + ZSTD_WILDCOPY_MARGIN * 2 {
                Vec::new()
            } else {
                file.seek(SeekFrom::Start(offset))?;
                let mut block_data = vec![0u8; size as usize];
                file.read_exact(&mut block_data)?;

                let block = Self::decode_block_payload(&block_data, compressed, block_crc)?;

                // Decode first key
                if block.len() < 4 {
                    Vec::new()
                } else {
                    let key_len = u32::from_le_bytes(
                        block[0..4].try_into().expect("should be valid"),
                    ) as usize;
                    if block.len() < 4 + key_len {
                        Vec::new()
                    } else {
                        block[4..4 + key_len].to_vec()
                    }
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
            block_crc,
            cached_first_key,
            max_seq_no,
        })
    }

    /// Returns the maximum sequence number in this SSTable.
    /// Returns 0 for SSTables written before this feature was added.
    pub fn max_seq_no(&self) -> SeqNo {
        self.max_seq_no
    }

    /// Upgrades an old-format SST footer to include max_seq_no.
    /// Scans all entries once, then appends max_seq_no to the footer.
    /// Returns the max_seq_no written, or 0 if already upgraded / nothing to write.
    ///
    /// Also repairs "zombie" footers where `FLAG_HAS_MAX_SEQ` is set but the
    /// stored `max_seq_no` is 0 — those are permanently classified as "old" by
    /// `load_sstables` and were never fixed because this function used to
    /// treat the flag alone as "already upgraded".
    pub fn upgrade_footer(path: impl AsRef<Path>) -> Result<SeqNo> {
        let path_ref = path.as_ref();
        let file_len = std::fs::metadata(path_ref)?.len();
        if file_len < 25 {
            return Ok(0);
        }

        // Check if already has new footer with a real max_seq_no
        let mut already_33 = false;
        if file_len >= 33 {
            let mut f = std::fs::File::open(path_ref)?;
            // Read last 33 bytes to verify magic + flags
            f.seek(SeekFrom::End(-33))?;
            let mut buf33 = [0u8; 33];
            f.read_exact(&mut buf33)?;
            let magic = u64::from_le_bytes(buf33[16..24].try_into().unwrap());
            let flags = buf33[24];
            let stored_max_seq = u64::from_le_bytes(buf33[25..33].try_into().unwrap());
            if magic == MAGIC && flags & FLAG_HAS_MAX_SEQ != 0 {
                if stored_max_seq > 0 {
                    return Ok(0); // already upgraded with a real value
                }
                // Zombie: flag set but max_seq_no == 0 → fall through and repair.
                already_33 = true;
            }
        }

        // Scan all entries for max_seq_no
        let sst = match SsTable::open(path_ref) {
            Ok(s) => Arc::new(s),
            Err(e) => {
                eprintln!("[upgrade_footer] open failed for {:?}: {}", path_ref, e);
                return Ok(0);
            }
        };
        let mut max_seq: SeqNo = 0;
        let mut entry_count: u64 = 0;
        if let Ok(iter) = sst.iter() {
            let mut it = iter;
            while it.is_valid() {
                max_seq = max_seq.max(it.seq_no());
                entry_count += 1;
                it.next();
            }
        }

        if max_seq == 0 {
            if entry_count == 0 {
                eprintln!(
                    "[upgrade_footer] max_seq=0 for {:?} (entries=0)",
                    path_ref
                );
                return Ok(0);
            }
            // Entries exist but every seq_no is 0 (legacy zeroed-seq files).
            // Stamp a sentinel so load_sstables stops classifying the file as
            // "old" forever. Reads use level-order first-seen-wins, so a
            // synthetic seq is safe.
            max_seq = 1;
        }

        let mut f = std::fs::OpenOptions::new().read(true).write(true).open(path_ref)?;
        if already_33 {
            // 33-byte footer already in place — overwrite the trailing max_seq_no.
            f.seek(SeekFrom::End(-8))?;
            f.write_all(&max_seq.to_le_bytes())?;
        } else {
            // Read old footer (last 25 bytes)
            f.seek(SeekFrom::End(-25))?;
            let mut old_footer = [0u8; 25];
            f.read_exact(&mut old_footer)?;

            // Update flags to include FLAG_HAS_MAX_SEQ
            old_footer[24] |= FLAG_HAS_MAX_SEQ;

            // Write back: old footer (25 bytes) + max_seq_no (8 bytes)
            f.seek(SeekFrom::End(-25))?;
            f.write_all(&old_footer)?;
            f.write_all(&max_seq.to_le_bytes())?;
        }
        f.sync_all()?;

        Ok(max_seq)
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

    /// True if this SSTable uses the per-block CRC format
    /// (`[flags][payload][crc32]` per block). Legacy files return false.
    pub fn has_block_crc(&self) -> bool {
        self.block_crc
    }

    /// Returns an iterator over all entries in the SSTable.
    pub fn iter(self: &Arc<Self>) -> Result<SsTableIterator> {
        SsTableIterator::new(Arc::clone(self))
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
        // Fast path: cache hit — copy out and release the lock immediately.
        {
            let mut inner = self.inner.lock();
            if let Some(cached) = inner.block_cache.get(offset) {
                return Ok(cached.to_vec());
            }
        }

        // Cache miss: bound the on-disk size (garbage index must not become
        // a multi-GB allocation), read bytes under the lock, then drop it
        // before the expensive decompress.
        if size as usize > MAX_DECOMPRESSED_BLOCK_SIZE + ZSTD_WILDCOPY_MARGIN * 2 {
            return Err(CoreError::corruption(format!(
                "block size {} at offset {} exceeds limit",
                size, offset
            )));
        }
        let buf = {
            let mut inner = self.inner.lock();
            // Double-check: another thread may have filled the cache.
            if let Some(cached) = inner.block_cache.get(offset) {
                return Ok(cached.to_vec());
            }
            let mut buf = vec![0u8; size as usize];
            inner.file.seek(SeekFrom::Start(offset))?;
            inner.file.read_exact(&mut buf)?;
            buf
        };

        // Decompress OUTSIDE the lock — this is the expensive part (CRC + zstd).
        let block_data = Self::decode_block_payload(&buf, self.compressed, self.block_crc)?;

        {
            let mut inner = self.inner.lock();
            inner.block_cache.put(offset, block_data.clone());
        }

        Ok(block_data)
    }

    /// Decode one on-disk block into its raw entry payload.
    ///
    /// New format (`block_crc`): `[u8 flags][payload][u32 crc32]`, where the
    /// CRC covers `flags || payload` and bit0 of `flags` selects zstd.
    /// Legacy format: payload only, compressed iff the table-level flag is set.
    fn decode_block_payload(raw: &[u8], compressed: bool, block_crc: bool) -> Result<Vec<u8>> {
        if block_crc {
            if raw.len() < 5 {
                return Err(CoreError::corruption(format!(
                    "data block too short for CRC format: {} bytes",
                    raw.len()
                )));
            }
            let (body, crc_bytes) = raw.split_at(raw.len() - 4);
            let expected = u32::from_le_bytes(crc_bytes.try_into().expect("should be valid"));
            let actual = crc32fast::hash(body);
            if actual != expected {
                return Err(CoreError::corruption(format!(
                    "data block CRC mismatch: expected {:#010x}, got {:#010x}",
                    expected, actual
                )));
            }
            let block_flags = body[0];
            let payload = &body[1..];
            if block_flags & BLOCK_FLAG_COMPRESSED != 0 {
                decompress_limited(payload, MAX_DECOMPRESSED_BLOCK_SIZE)
            } else {
                Ok(payload.to_vec())
            }
        } else if compressed {
            decompress_limited(raw, MAX_DECOMPRESSED_BLOCK_SIZE)
        } else {
            Ok(raw.to_vec())
        }
    }

    /// Prefetch the next `n` data blocks into the block cache (sequential scan).
    /// Called after loading block `idx` so the following blocks are ready.
    fn prefetch_after(&self, idx: usize) {
        let n = 2usize;
        let start = idx + 1;
        let end = (idx + 1 + n).min(self.index.len());
        if start >= end {
            return;
        }
        // Phase 1: read raw bytes under the file lock (short).
        let mut raws: Vec<(u64, Vec<u8>)> = Vec::new();
        {
            let mut inner = self.inner.lock();
            for i in start..end {
                let offset = self.index[i].offset;
                let size = self.index[i].size;
                if inner.block_cache.contains(offset) {
                    continue;
                }
                if size as usize > MAX_DECOMPRESSED_BLOCK_SIZE + ZSTD_WILDCOPY_MARGIN * 2 {
                    continue;
                }
                let mut buf = vec![0u8; size as usize];
                if inner.file.seek(SeekFrom::Start(offset)).is_err() {
                    continue;
                }
                if inner.file.read_exact(&mut buf).is_err() {
                    continue;
                }
                raws.push((offset, buf));
            }
        }
        // Phase 2: decompress outside the lock, then insert.
        for (offset, buf) in raws {
            if let Ok(data) = Self::decode_block_payload(&buf, self.compressed, self.block_crc) {
                let mut inner = self.inner.lock();
                inner.block_cache.put(offset, data);
            }
        }
    }

    fn search_block(&self, block: &[u8], key: &[u8]) -> Result<Option<(Vec<u8>, SeqNo)>> {
        match self.search_block_full(block, key)? {
            Some((v, seq, EntryKind::Put)) => Ok(Some((v, seq))),
            Some((_, _, EntryKind::Delete)) => Ok(None), // Tombstone
            None => Ok(None),
        }
    }

    fn search_block_full(
        &self,
        block: &[u8],
        key: &[u8],
    ) -> Result<Option<(Vec<u8>, SeqNo, EntryKind)>> {
        // Read restart points
        if block.len() < 4 {
            return Ok(None);
        }

        let num_restarts = u32::from_le_bytes(
            block[block.len() - 4..block.len()]
                .try_into()
                .expect("should be valid"),
        ) as usize;

        if num_restarts == 0 {
            return Ok(None);
        }

        let restart_start = block.len() - 4 - num_restarts * 4;
        let mut restarts = Vec::with_capacity(num_restarts);
        for i in 0..num_restarts {
            let off = restart_start + i * 4;
            restarts.push(u32::from_le_bytes(
                block[off..off + 4].try_into().expect("should be valid"),
            ) as usize);
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

    fn decode_entry_at(
        &self,
        data: &[u8],
        offset: usize,
    ) -> Option<(Vec<u8>, Vec<u8>, SeqNo, EntryKind)> {
        let mut pos = offset;

        // key_len
        if pos + 4 > data.len() {
            return None;
        }
        let key_len =
            u32::from_le_bytes(data[pos..pos + 4].try_into().expect("should be valid")) as usize;
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
        let val_len =
            u32::from_le_bytes(data[pos..pos + 4].try_into().expect("should be valid")) as usize;
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
        let seq_no = u64::from_le_bytes(data[pos..pos + 8].try_into().expect("should be valid"));
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

    /// Decode the block index. `data_end` is the end of the data section
    /// (start of the index) and is used to reject offsets that would read
    /// past the payload into the index/footer.
    ///
    /// Files written by this builder always append a CRC32 to the index.
    /// A CRC mismatch on a `block_crc` file is hard corruption. On legacy
    /// files the CRC is optional; a bad CRC falls back to no-CRC parsing
    /// only after the resulting entries pass offset validation.
    fn decode_index(data: &[u8], data_end: u64, block_crc: bool) -> Result<Vec<BlockIndexEntry>> {
        if data.len() < 4 {
            return Ok(Vec::new());
        }

        // Check if data has CRC32 suffix (new format)
        // New format: [index_data][crc32: u32]
        // Old format: [index_data] only
        let (index_data, has_valid_crc) = if data.len() >= 8 {
            let potential_crc =
                u32::from_le_bytes(data[data.len() - 4..].try_into().expect("should be valid"));
            let data_without_crc = &data[..data.len() - 4];
            let computed_crc = crc32fast::hash(data_without_crc);

            if computed_crc == potential_crc {
                (data_without_crc, true)
            } else {
                (data, false)
            }
        } else {
            (data, false)
        };

        if block_crc && !has_valid_crc {
            return Err(CoreError::corruption(
                "block index CRC mismatch on CRC-format SSTable",
            ));
        }

        let count =
            u32::from_le_bytes(index_data[0..4].try_into().expect("should be valid")) as usize;
        // A single SST cannot legitimately have more blocks than bytes/16;
        // a huge count means we are parsing garbage.
        if count > index_data.len() {
            return Err(CoreError::corruption(format!(
                "block index count {} exceeds index size {}",
                count,
                index_data.len()
            )));
        }
        let mut entries = Vec::with_capacity(count);
        let mut pos = 4;

        for _ in 0..count {
            if pos + 4 > index_data.len() {
                return Err(CoreError::corruption("block index truncated at key_len"));
            }
            let key_len = u32::from_le_bytes(
                index_data[pos..pos + 4]
                    .try_into()
                    .expect("should be valid"),
            ) as usize;
            pos += 4;

            if pos + key_len > index_data.len() {
                return Err(CoreError::corruption("block index truncated at last_key"));
            }
            let last_key = index_data[pos..pos + key_len].to_vec();
            pos += key_len;

            if pos + 16 > index_data.len() {
                return Err(CoreError::corruption("block index truncated at offset/size"));
            }
            let offset = u64::from_le_bytes(
                index_data[pos..pos + 8]
                    .try_into()
                    .expect("should be valid"),
            );
            let size = u64::from_le_bytes(
                index_data[pos + 8..pos + 16]
                    .try_into()
                    .expect("should be valid"),
            );
            pos += 16;

            // Offset/size must stay inside the data section. A misparsed
            // index (e.g. CRC fallback on garbage) would otherwise feed
            // index/footer bytes to the zstd decoder.
            if size == 0 {
                return Err(CoreError::corruption("block index entry with zero size"));
            }
            if offset > data_end || size > data_end || offset.saturating_add(size) > data_end {
                return Err(CoreError::corruption(format!(
                    "block index entry out of range: offset={}, size={}, data_end={}",
                    offset, size, data_end
                )));
            }

            entries.push(BlockIndexEntry {
                last_key,
                offset,
                size,
            });
        }

        Ok(entries)
    }
}

/// Iterator over SSTable entries.
///
/// Zero-copy design: stores byte offsets into the current block instead of
/// cloning key/value into separate Vecs. `key()` and `value()` return
/// slices borrowed from `block_data`, avoiding per-entry allocations.
///
/// Owns the table via `Arc`, so no self-referential `'static` borrow is
/// needed and the iterator can be moved freely.
pub struct SsTableIterator {
    table: Arc<SsTable>,
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

impl SsTableIterator {
    fn new(table: Arc<SsTable>) -> Result<Self> {
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
        // Warm the next blocks so sequential scan doesn't wait on disk each hop.
        self.table.prefetch_after(idx);

        // Parse restart points
        let data = &self.block_data;
        if data.len() < 4 {
            self.restarts = Vec::new();
            self.restart_start = 0;
            return Ok(());
        }

        let num_restarts = u32::from_le_bytes(
            data[data.len() - 4..data.len()]
                .try_into()
                .expect("should be valid"),
        ) as usize;
        let restart_data_size = num_restarts * 4;
        if restart_data_size + 4 > data.len() {
            return Err(CoreError::corruption("invalid restart count in block"));
        }
        self.restart_start = data.len() - 4 - restart_data_size;
        self.restarts = Vec::with_capacity(num_restarts);

        for i in 0..num_restarts {
            let off = self.restart_start + i * 4;
            self.restarts.push(u32::from_le_bytes(
                data[off..off + 4].try_into().expect("should be valid"),
            ) as usize);
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
        let key_len =
            u32::from_le_bytes(data[pos..pos + 4].try_into().expect("should be valid")) as usize;
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
        let val_len =
            u32::from_le_bytes(data[pos..pos + 4].try_into().expect("should be valid")) as usize;
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
        let seq_no = u64::from_le_bytes(data[pos..pos + 8].try_into().expect("should be valid"));
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
    /// Returns empty slice if offsets are out of bounds (corrupted entry).
    pub fn key(&self) -> &[u8] {
        self.block_slice(self.current_key_start, self.current_key_end)
    }

    /// Returns the current value as a zero-copy slice into the block data.
    /// Returns empty slice if offsets are out of bounds (corrupted entry).
    pub fn value(&self) -> &[u8] {
        self.block_slice(self.current_value_start, self.current_value_end)
    }

    /// Slice `block_data[start..end]`, refusing to build a slice over an
    /// impossible Vec (len > 0 but no allocation — dangling ptr like 0x1).
    fn block_slice(&self, start: usize, end: usize) -> &[u8] {
        if self.block_data.capacity() == 0 && !self.block_data.is_empty() {
            return &[];
        }
        if start <= end && end <= self.block_data.len() {
            &self.block_data[start..end]
        } else {
            &[]
        }
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

    /// Decode all remaining entries in the current block into `out` (batch).
    ///
    /// Avoids repeated per-entry bounds/offset work and lets the caller process
    /// a whole 4KB block at once. Stops at the block boundary; call `next()`
    /// (or this again) after the block is exhausted to move on.
    /// Returns the number of entries appended.
    pub fn drain_current_block(
        &mut self,
        out: &mut Vec<(Vec<u8>, Vec<u8>, SeqNo, EntryKind)>,
    ) -> usize {
        let start_block = self.block_idx;
        let mut n = 0;
        while self.valid && self.block_idx == start_block {
            out.push((
                self.key().to_vec(),
                self.value().to_vec(),
                self.seq_no(),
                self.kind(),
            ));
            n += 1;
            self.advance_to_next_entry();
        }
        n
    }

    /// Like [`Self::drain_current_block`], but copies only `(key, seq, kind)`.
    ///
    /// Values are skipped entirely (still walked for framing, never allocated).
    /// Used by COUNT / winner-collection paths that must stay O(keys).
    /// Returns the number of entries appended.
    pub fn drain_current_block_keys(
        &mut self,
        out: &mut Vec<(Vec<u8>, SeqNo, EntryKind)>,
    ) -> usize {
        let start_block = self.block_idx;
        let mut n = 0;
        while self.valid && self.block_idx == start_block {
            out.push((self.key().to_vec(), self.seq_no(), self.kind()));
            n += 1;
            self.advance_to_next_entry();
        }
        n
    }

    /// Seek to the first key >= `target`. Uses binary search on block index
    /// to jump directly to the right block, then scans within the block.
    /// This avoids linear scanning from the beginning for prefix scans.
    pub fn seek(&mut self, target: &[u8]) {
        // Find the block that might contain `target`
        match self.table.find_block(target) {
            Ok(idx) => {
                if let Err(_) = self.load_block(idx) {
                    self.valid = false;
                    return;
                }
                // load_block replaces block_data and resets pos, but leaves
                // current_* pointing at the previous block's entry. Decode the
                // first entry of the new block before any key()/value() call.
                self.advance_to_next_entry();
                // Scan within the block to find first key >= target
                loop {
                    if !self.valid {
                        break;
                    }
                    if self.key() >= target {
                        break;
                    }
                    self.advance_to_next_entry();
                }
            }
            Err(_) => {
                self.valid = false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_sstable_write_and_read() {
        let dir = tempdir().expect("should be valid");
        let path = dir.path().join("test.sst");

        let mut builder = SsTableBuilder::new();
        for i in 0..100u32 {
            let key = format!("key_{:04}", i);
            let value = format!("value_{}", i);
            builder.add(&Entry::put(key.into_bytes(), value.into_bytes(), i as u64));
        }
        builder.build(&path).expect("should be valid");

        // Open and read
        let sst = Arc::new(SsTable::open(&path).expect("should be valid"));

        // Point lookups
        let (val, _) = sst
            .get(b"key_0050")
            .expect("should be valid")
            .expect("should be valid");
        assert_eq!(val, b"value_50");

        let (val, _) = sst
            .get(b"key_0001")
            .expect("should be valid")
            .expect("should be valid");
        assert_eq!(val, b"value_1");

        // Missing key
        assert!(sst.get(b"missing_key").expect("should be valid").is_none());
    }

    #[test]
    fn test_sstable_iteration() {
        let dir = tempdir().expect("should be valid");
        let path = dir.path().join("iter_test.sst");

        let mut builder = SsTableBuilder::new();
        for i in 0..50u32 {
            let key = format!("k{:03}", i);
            builder.add(&Entry::put(key.into_bytes(), vec![i as u8], i as u64));
        }
        builder.build(&path).expect("should be valid");

        let sst = Arc::new(SsTable::open(&path).expect("should be valid"));
        let mut iter = sst.iter().expect("should be valid");

        let mut count = 0;
        while iter.is_valid() {
            count += 1;
            iter.next();
        }
        assert_eq!(count, 50);
    }

    /// Regression: seek() must re-decode the first entry after load_block().
    /// Previously it reused the previous block's key/value byte offsets against
    /// the new block buffer, panicking (slice OOB) when the new block was shorter.
    #[test]
    fn test_seek_into_smaller_block_after_long_key_block() {
        let dir = tempdir().expect("should be valid");
        let path = dir.path().join("seek_stale_idx.sst");

        // Block 0: first entry has a very long key so current_key_end after
        // iter()/new() is ~3KB. Fill past the 4096-byte flush threshold.
        let mut builder = SsTableBuilder::new();
        let long_key = {
            let mut k = b"a".to_vec();
            k.extend(std::iter::repeat(b'x').take(3000));
            k
        };
        builder.add(&Entry::put(long_key.clone(), b"v0".to_vec(), 1));
        for i in 0..4u32 {
            let mut key = b"a".to_vec();
            key.extend(std::iter::repeat(b'y').take(2000));
            key.push(b'0' + i as u8);
            builder.add(&Entry::put(key, vec![b'p'; 100], 2 + i as u64));
        }

        // Final block: a single small entry (short key + short value) so its
        // decompressed size is far below block 0's first-entry key_end.
        builder.add(&Entry::put(b"z".to_vec(), b"vz".to_vec(), 100));

        builder.build(&path).expect("should be valid");
        let sst = Arc::new(SsTable::open(&path).expect("should be valid"));
        assert!(
            sst.num_blocks() >= 2,
            "need at least 2 blocks, got {}",
            sst.num_blocks()
        );

        // new() positions at block 0's first entry (long key). seek() into the
        // small final block must not use the stale long-key offsets.
        let mut iter = sst.iter().expect("should be valid");
        assert!(iter.is_valid());
        assert_eq!(iter.key(), long_key.as_slice());

        iter.seek(b"z");
        assert!(iter.is_valid(), "seek to last-block key must land on an entry");
        assert_eq!(iter.key(), b"z");
        assert_eq!(iter.value(), b"vz");
        assert_eq!(iter.seq_no(), 100);
    }

    /// seek() across multiple blocks lands on the first key >= target and
    /// subsequent iteration stays consistent with the new block.
    #[test]
    fn test_seek_positions_correctly_across_blocks() {
        let dir = tempdir().expect("should be valid");
        let path = dir.path().join("seek_multi_block.sst");

        let mut builder = SsTableBuilder::new();
        for i in 0..300u32 {
            let key = format!("key_{:06}", i);
            let value = format!("val_{:06}_{}", i, "p".repeat(80));
            builder.add(&Entry::put(key.into_bytes(), value.into_bytes(), i as u64));
        }
        builder.build(&path).expect("should be valid");

        let sst = Arc::new(SsTable::open(&path).expect("should be valid"));
        assert!(
            sst.num_blocks() >= 2,
            "need at least 2 blocks, got {}",
            sst.num_blocks()
        );

        let mut iter = sst.iter().expect("should be valid");
        iter.seek(b"key_000150");
        assert!(iter.is_valid());
        assert_eq!(iter.key(), b"key_000150");
        assert_eq!(iter.seq_no(), 150);

        iter.seek(b"key_000000");
        assert!(iter.is_valid());
        assert_eq!(iter.key(), b"key_000000");

        // Past-the-end target invalidates the iterator instead of panicking.
        iter.seek(b"zzz");
        assert!(!iter.is_valid());
    }

    /// seek() after mid-iteration must refresh offsets from the target block,
    /// not the entry the iterator was previously sitting on.
    #[test]
    fn test_seek_after_iteration_refreshes_offsets() {
        let dir = tempdir().expect("should be valid");
        let path = dir.path().join("seek_after_iter.sst");

        let mut builder = SsTableBuilder::new();
        // Long-key block first, then enough small entries to form more blocks.
        let long_key = {
            let mut k = b"a".to_vec();
            k.extend(std::iter::repeat(b'x').take(3000));
            k
        };
        builder.add(&Entry::put(long_key, vec![b'L'; 500], 1));
        for i in 0..200u32 {
            let key = format!("m{:05}", i);
            let value = format!("v{:05}_{}", i, "q".repeat(50));
            builder.add(&Entry::put(key.into_bytes(), value.into_bytes(), 10 + i as u64));
        }
        builder.build(&path).expect("should be valid");

        let sst = Arc::new(SsTable::open(&path).expect("should be valid"));
        assert!(sst.num_blocks() >= 2);

        let mut iter = sst.iter().expect("should be valid");
        // Walk into the middle of the table so current_* reference a later block.
        for _ in 0..50 {
            iter.next();
        }
        assert!(iter.is_valid());

        // Jump forward again: offsets must belong to the destination block.
        iter.seek(b"m00100");
        assert!(iter.is_valid());
        assert_eq!(iter.key(), b"m00100");
        assert_eq!(iter.seq_no(), 110);

        // And backwards to the first block's long key entry.
        iter.seek(b"a");
        assert!(iter.is_valid());
        assert_eq!(iter.seq_no(), 1);
    }

    /// drain_current_block_keys must visit every entry in the block with
    /// (key, seq, kind) only — same coverage as drain_current_block, without
    /// copying values. COUNT paths rely on this staying O(keys).
    #[test]
    fn test_drain_current_block_keys_matches_full_drain() {
        let dir = tempdir().expect("should be valid");
        let path = dir.path().join("drain_keys.sst");

        let mut builder = SsTableBuilder::new();
        for i in 0..80u32 {
            let key = format!("k{:04}", i);
            let value = format!("v{:04}_{}", i, "x".repeat(200));
            let entry = if i % 7 == 0 {
                Entry::delete(key.into_bytes(), 10 + i as u64)
            } else {
                Entry::put(key.into_bytes(), value.into_bytes(), 10 + i as u64)
            };
            builder.add(&entry);
        }
        builder.build(&path).expect("should be valid");
        let sst = Arc::new(SsTable::open(&path).expect("should be valid"));

        // Full drain reference.
        let mut full = Vec::new();
        {
            let mut iter = sst.iter().expect("should be valid");
            while iter.is_valid() {
                iter.drain_current_block(&mut full);
            }
        }
        assert!(!full.is_empty());

        // Key-only drain must match on (key, seq, kind).
        let mut keys = Vec::new();
        {
            let mut iter = sst.iter().expect("should be valid");
            while iter.is_valid() {
                iter.drain_current_block_keys(&mut keys);
            }
        }
        assert_eq!(keys.len(), full.len());
        for (i, ((k, seq, kind), (fk, _fv, fseq, fkind))) in
            keys.iter().zip(full.iter()).enumerate()
        {
            assert_eq!(k, fk, "key mismatch at {i}");
            assert_eq!(seq, fseq, "seq mismatch at {i}");
            assert_eq!(kind, fkind, "kind mismatch at {i}");
        }
    }

    #[test]
    fn test_sstable_compressed_write_and_read() {
        let dir = tempdir().expect("should be valid");
        let path = dir.path().join("compressed.sst");

        let mut builder = SsTableBuilder::new();
        builder.set_compression_level(3);
        for i in 0..200u32 {
            let key = format!("key_{:06}", i);
            let value = format!("value_{:06}_padding_data_to_make_compression_worthwhile", i);
            builder.add(&Entry::put(key.into_bytes(), value.into_bytes(), i as u64));
        }
        builder.build(&path).expect("should be valid");

        // Open and read
        let sst = Arc::new(SsTable::open(&path).expect("should be valid"));
        assert!(sst.compressed, "SSTable should be marked as compressed");

        // Point lookups
        let (val, _) = sst
            .get(b"key_000050")
            .expect("should be valid")
            .expect("should be valid");
        assert_eq!(
            val,
            b"value_000050_padding_data_to_make_compression_worthwhile"
        );

        let (val, _) = sst
            .get(b"key_000001")
            .expect("should be valid")
            .expect("should be valid");
        assert_eq!(
            val,
            b"value_000001_padding_data_to_make_compression_worthwhile"
        );

        // Missing key
        assert!(sst.get(b"missing_key").expect("should be valid").is_none());
    }

    #[test]
    fn test_sstable_compressed_iteration() {
        let dir = tempdir().expect("should be valid");
        let path = dir.path().join("compressed_iter.sst");

        let mut builder = SsTableBuilder::new();
        builder.set_compression_level(3);
        for i in 0..100u32 {
            let key = format!("k{:04}", i);
            let value = format!("v{:04}_some_repetitive_value_for_compression", i);
            builder.add(&Entry::put(key.into_bytes(), value.into_bytes(), i as u64));
        }
        builder.build(&path).expect("should be valid");

        let sst = Arc::new(SsTable::open(&path).expect("should be valid"));
        let mut iter = sst.iter().expect("should be valid");

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
        let dir = tempdir().expect("should be valid");
        let path_uncompressed = dir.path().join("uncompressed.sst");
        let path_compressed = dir.path().join("compressed.sst");

        // Build uncompressed
        let mut builder = SsTableBuilder::new();
        for i in 0..500u32 {
            let key = format!("key_{:06}", i);
            let value = format!(
                "value_with_repetitive_padding_data_for_compression_test_{:06}",
                i
            );
            builder.add(&Entry::put(key.into_bytes(), value.into_bytes(), i as u64));
        }
        builder.build(&path_uncompressed).expect("should be valid");

        // Build compressed
        let mut builder = SsTableBuilder::new();
        builder.set_compression_level(3);
        for i in 0..500u32 {
            let key = format!("key_{:06}", i);
            let value = format!(
                "value_with_repetitive_padding_data_for_compression_test_{:06}",
                i
            );
            builder.add(&Entry::put(key.into_bytes(), value.into_bytes(), i as u64));
        }
        builder.build(&path_compressed).expect("should be valid");

        let size_uncompressed = std::fs::metadata(&path_uncompressed)
            .expect("should be valid")
            .len();
        let size_compressed = std::fs::metadata(&path_compressed)
            .expect("should be valid")
            .len();

        println!(
            "Uncompressed: {} bytes, Compressed: {} bytes, Ratio: {:.1}%",
            size_uncompressed,
            size_compressed,
            (size_compressed as f64 / size_uncompressed as f64) * 100.0
        );

        assert!(
            size_compressed < size_uncompressed,
            "Compressed SSTable ({}) should be smaller than uncompressed ({})",
            size_compressed,
            size_uncompressed
        );
    }

    #[test]
    fn test_sstable_compressed_with_tombstones() {
        let dir = tempdir().expect("should be valid");
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
        builder.build(&path).expect("should be valid");

        let sst = Arc::new(SsTable::open(&path).expect("should be valid"));

        // Check put entries
        let result = sst
            .get_full(b"key_0001")
            .expect("should be valid")
            .expect("should be valid");
        assert_eq!(result.2, EntryKind::Put);
        assert_eq!(result.0, b"value_0001");

        // Check tombstone entries
        let result = sst
            .get_full(b"key_0000")
            .expect("should be valid")
            .expect("should be valid");
        assert_eq!(result.2, EntryKind::Delete);

        // get() should return None for tombstones
        assert!(sst.get(b"key_0000").expect("should be valid").is_none());
    }

    /// Build a small valid SST used as the base for footer-corruption tests.
    fn build_valid_sst(path: &std::path::Path) {
        let mut builder = SsTableBuilder::new();
        for i in 0..10u32 {
            let key = format!("k{:04}", i);
            builder.add(&Entry::put(key.into_bytes(), format!("v{i}").into_bytes(), i as u64));
        }
        builder.build(path).expect("should be valid");
    }

    #[test]
    fn test_open_rejects_index_offset_past_payload() {
        let dir = tempdir().expect("should be valid");
        let path = dir.path().join("bad_index_off.sst");
        build_valid_sst(&path);

        // Overwrite index_offset in the 33-byte footer with a huge value.
        {
            use std::io::{Read, Seek, SeekFrom, Write};
            let mut f = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .expect("should be valid");
            let len = f.metadata().expect("should be valid").len();
            f.seek(SeekFrom::End(-33)).expect("should be valid");
            let mut footer = [0u8; 33];
            f.read_exact(&mut footer).expect("should be valid");
            footer[0..8].copy_from_slice(&u64::MAX.to_le_bytes());
            f.seek(SeekFrom::End(-33)).expect("should be valid");
            f.write_all(&footer).expect("should be valid");
            let _ = len;
        }

        let err = match SsTable::open(&path) {
            Ok(_) => panic!("must reject oversized index_offset"),
            Err(e) => e,
        };
        let msg = format!("{err}");
        assert!(msg.contains("offsets out of range"), "unexpected error: {msg}");
    }

    #[test]
    fn test_open_rejects_bloom_offset_before_index() {
        let dir = tempdir().expect("should be valid");
        let path = dir.path().join("bad_bloom_off.sst");
        build_valid_sst(&path);

        // Set bloom_offset < index_offset so the index length would underflow.
        {
            use std::io::{Read, Seek, SeekFrom, Write};
            let mut f = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .expect("should be valid");
            f.seek(SeekFrom::End(-33)).expect("should be valid");
            let mut footer = [0u8; 33];
            f.read_exact(&mut footer).expect("should be valid");
            let index_offset = u64::from_le_bytes(footer[0..8].try_into().unwrap());
            footer[8..16].copy_from_slice(&index_offset.saturating_sub(1).to_le_bytes());
            // Keep magic/flags intact; only the bloom offset is wrong.
            f.seek(SeekFrom::End(-33)).expect("should be valid");
            f.write_all(&footer).expect("should be valid");
        }

        let err = match SsTable::open(&path) {
            Ok(_) => panic!("must reject bloom_offset < index_offset"),
            Err(e) => e,
        };
        let msg = format!("{err}");
        assert!(
            msg.contains("offset order invalid") || msg.contains("offsets out of range"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn test_open_rejects_truncated_file() {
        let dir = tempdir().expect("should be valid");
        let path = dir.path().join("truncated.sst");
        build_valid_sst(&path);

        // Keep only 20 bytes — shorter than either footer format.
        {
            let f = std::fs::File::create(&path).expect("should be valid");
            f.set_len(20).expect("should be valid");
        }

        assert!(
            SsTable::open(&path).is_err(),
            "truncated file must not open"
        );
    }

    /// Flip one byte inside the first data block. The per-block CRC must
    /// catch it on read instead of letting zstd chew on garbage.
    #[test]
    fn test_block_crc_mismatch_is_rejected() {
        let dir = tempdir().expect("should be valid");
        let path = dir.path().join("crc_block.sst");
        let mut builder = SsTableBuilder::new();
        builder.set_compression_level(3);
        for i in 0..50u32 {
            builder.add(&Entry::put(
                format!("key_{i:04}").into_bytes(),
                format!("value_{i:04}_{}", "x".repeat(80)).into_bytes(),
                i as u64,
            ));
        }
        builder.build(&path).expect("should be valid");

        // Corrupt one byte in the first block (after the flags byte).
        {
            use std::io::{Seek, SeekFrom, Write};
            let mut f = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .expect("should be valid");
            f.seek(SeekFrom::Start(2)).expect("should be valid");
            f.write_all(&[0xFF]).expect("should be valid");
        }

        // open() reads the first block for cached_first_key, so corruption
        // there is caught immediately; later blocks surface on get().
        match SsTable::open(&path) {
            Err(e) => {
                let msg = format!("{e}");
                assert!(msg.contains("CRC"), "unexpected error: {msg}");
            }
            Ok(sst) => {
                let err = match sst.get(b"key_0000") {
                    Err(e) => e,
                    Ok(v) => panic!("corrupt block must be rejected, got {v:?}"),
                };
                let msg = format!("{err}");
                assert!(msg.contains("CRC"), "unexpected error: {msg}");
            }
        }
    }

    /// A flip inside a zstd-compressed block must produce a clean error,
    /// never an unbounded wildcopy write.
    #[test]
    fn test_corrupt_compressed_payload_errors_not_crashes() {
        let dir = tempdir().expect("should be valid");
        let path = dir.path().join("corrupt_zstd.sst");
        let mut builder = SsTableBuilder::new();
        builder.set_compression_level(3);
        for i in 0..30u32 {
            builder.add(&Entry::put(
                format!("k{i:03}").into_bytes(),
                vec![b'v'; 200],
                i as u64,
            ));
        }
        builder.build(&path).expect("should be valid");

        // Corrupt bytes in the middle of the first block payload (skip flags+crc).
        {
            use std::io::{Seek, SeekFrom, Write};
            let mut f = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .expect("should be valid");
            f.seek(SeekFrom::Start(8)).expect("should be valid");
            f.write_all(&[0x00, 0xFF, 0x00, 0xFF]).expect("should be valid");
        }

        let sst = match SsTable::open(&path) {
            Ok(s) => s,
            Err(e) => {
                let msg = format!("{e}");
                assert!(msg.contains("CRC") || msg.contains("zstd"), "unexpected: {msg}");
                return;
            }
        };
        match sst.get(b"k000") {
            Ok(None) => {}
            Ok(Some(_)) => {}
            Err(e) => {
                let msg = format!("{e}");
                assert!(
                    msg.contains("CRC") || msg.contains("zstd") || msg.contains("corrupt"),
                    "unexpected error: {msg}"
                );
            }
        }
    }

    /// Index CRC is mandatory on FLAG_BLOCK_CRC files.
    #[test]
    fn test_index_crc_mismatch_is_rejected_on_new_format() {
        let dir = tempdir().expect("should be valid");
        let path = dir.path().join("bad_index_crc.sst");
        build_valid_sst(&path);

        // Flip the last byte of the index (the CRC32 suffix).
        {
            use std::io::{Read, Seek, SeekFrom, Write};
            let mut f = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .expect("should be valid");
            let len = f.metadata().expect("should be valid").len();
            // footer is 33 bytes; index ends just before bloom. Easiest: flip
            // a byte just before the bloom section by targeting the byte at
            // index_offset + index_len - 1, which is the last CRC byte.
            // Use footer index/bloom offsets.
            f.seek(SeekFrom::End(-33)).expect("should be valid");
            let mut footer = [0u8; 33];
            f.read_exact(&mut footer).expect("should be valid");
            let index_offset = u64::from_le_bytes(footer[0..8].try_into().unwrap());
            let bloom_offset = u64::from_le_bytes(footer[8..16].try_into().unwrap());
            // Last byte of index region = bloom_offset - 1 = last CRC byte.
            let crc_last = bloom_offset - 1;
            assert!(crc_last >= index_offset);
            let mut b = [0u8; 1];
            f.seek(SeekFrom::Start(crc_last)).expect("should be valid");
            f.read_exact(&mut b).expect("should be valid");
            b[0] ^= 0xFF;
            f.seek(SeekFrom::Start(crc_last)).expect("should be valid");
            f.write_all(&b).expect("should be valid");
            let _ = len;
        }

        let err = match SsTable::open(&path) {
            Ok(_) => panic!("must reject bad index CRC on new-format SST"),
            Err(e) => e,
        };
        let msg = format!("{err}");
        assert!(msg.contains("CRC"), "unexpected error: {msg}");
    }

    /// decompress_limited must reject empty and non-zstd input cleanly.
    #[test]
    fn test_decompress_limited_rejects_bad_input() {
        assert!(super::decompress_limited(&[], 1024).is_err());
        assert!(super::decompress_limited(b"not a zstd frame at all....", 1024).is_err());
        // Valid tiny frame must succeed within the limit.
        let frame = zstd::encode_all(&b"hello world"[..], 1).expect("should be valid");
        let out = super::decompress_limited(&frame, 1024).expect("should be valid");
        assert_eq!(out, b"hello world");
        // A limit below the declared content size must fail.
        assert!(super::decompress_limited(&frame, 4).is_err());
    }

    /// Systematic corruption injection: every class must fail cleanly (Err),
    /// never abort, never smash the heap / canary.
    #[test]
    fn test_corruption_injection_matrix() {
        let raw = vec![0x5Au8; 8192];
        let frame = zstd::bulk::compress(&raw, 3).expect("should be valid");

        // 1. Truncated frame
        for cut in [1, 4, 8, 16, frame.len() / 2, frame.len().saturating_sub(1)] {
            if cut == 0 || cut >= frame.len() {
                continue;
            }
            let r = super::decompress_limited(&frame[..cut], 64 * 1024);
            assert!(r.is_err(), "truncated frame at {cut} must fail");
        }

        // 2. Flipped bytes in the compressed body
        let mut flipped = frame.clone();
        let n = flipped.len();
        for pos in [n / 4, n / 2, (3 * n) / 4, n.saturating_sub(3)] {
            if pos < n {
                flipped[pos] ^= 0xFF;
                let r = super::decompress_limited(&flipped, 64 * 1024);
                // May error (good) or — extremely rarely — still decode.
                // It must never panic / abort.
                if let Ok(out) = r {
                    assert!(out.len() <= 64 * 1024);
                }
                flipped[pos] ^= 0xFF;
            }
        }

        // 3. Random garbage with a zstd magic-looking header
        let mut junk = vec![0u8; 256];
        junk[0] = 0x28;
        junk[1] = 0xB5;
        junk[2] = 0x2F;
        junk[3] = 0xFD;
        assert!(super::decompress_limited(&junk, 4096).is_err());

        // 4. Declared content size = huge (must refuse before allocating)
        //    Craft: take a valid frame and lie is hard; just check the limit path
        //    with a small max_out on a large real frame.
        assert!(super::decompress_limited(&frame, 16).is_err());

        // 5. Exact-fit must succeed and preserve data (canary intact)
        let out = super::decompress_limited(&frame, raw.len()).expect("should be valid");
        assert_eq!(out, raw);

        // 6. Unknown-size (streaming) frame with garbage suffix
        let mut stream = zstd::encode_all(&raw[..], 1).expect("should be valid");
        stream.push(0x00);
        stream.push(0xFF);
        // Trailing garbage after a complete frame: one-shot should still
        // return the frame contents (extra bytes ignored) OR error — never crash.
        match super::decompress_limited(&stream, 64 * 1024) {
            Ok(out) => assert_eq!(out, raw),
            Err(_) => {}
        }
    }

    /// A corrupt frame that tries to force writes past dstCapacity must be
    /// caught by the guard canary (or by zstd's own checks) — never UB.
    #[test]
    fn test_decompress_guard_canary_intact_on_valid() {
        let raw = vec![0x11u8; 4096];
        let frame = zstd::bulk::compress(&raw, 1).expect("should be valid");
        let out = super::decompress_limited(&frame, 4096).expect("should be valid");
        assert_eq!(out, raw);
        // Second call after many iterations — no cross-call canary leakage.
        for _ in 0..100 {
            let out = super::decompress_limited(&frame, 4096).expect("should be valid");
            assert_eq!(out, raw);
        }
    }

    /// Streaming-compressed frames (no content size in the header) must
    /// decompress without reserving the hard cap up front.
    #[test]
    fn test_decompress_limited_unknown_content_size() {
        // encode_all uses the streaming encoder → ZSTD_CONTENTSIZE_UNKNOWN.
        let raw = vec![0xABu8; 300_000];
        let frame = zstd::encode_all(&raw[..], 1).expect("should be valid");
        assert_eq!(
            zstd::zstd_safe::get_frame_content_size(&frame).ok().flatten(),
            None,
            "test precondition: streaming frame has no declared size"
        );
        let out = super::decompress_limited(&frame, 4 * 1024 * 1024).expect("should be valid");
        assert_eq!(out, raw);

        // Must still refuse to exceed the cap.
        assert!(super::decompress_limited(&frame, 1024).is_err());
    }

    /// A frame whose decompressed size exactly fills the destination must
    /// not let ZSTD_wildcopy write past the allocation. We allocate
    /// `out + margin` but pass only `out` as dstCapacity.
    #[test]
    fn test_decompress_limited_exact_fit_does_not_overflow() {
        // bulk::compress writes the content size into the frame header.
        let raw = vec![0x5Au8; 4096];
        let frame = zstd::bulk::compress(&raw, 1).expect("should be valid");
        assert_eq!(
            zstd::zstd_safe::get_frame_content_size(&frame).ok().flatten(),
            Some(raw.len() as u64),
            "test precondition: declared size is exact"
        );
        let out = super::decompress_limited(&frame, raw.len()).expect("should be valid");
        assert_eq!(out, raw);

        // Exact-cap unknown-size path (streaming frame) with a tight max_out.
        let frame2 = zstd::encode_all(&raw[..], 1).expect("should be valid");
        let out2 = super::decompress_limited(&frame2, raw.len()).expect("should be valid");
        assert_eq!(out2, raw);
    }

    /// Uncompressed blocks (compression_level = 0) still round-trip through
    /// the CRC block format.
    #[test]
    fn test_uncompressed_crc_blocks_roundtrip() {
        let dir = tempdir().expect("should be valid");
        let path = dir.path().join("raw_crc.sst");
        let mut builder = SsTableBuilder::new();
        for i in 0..40u32 {
            builder.add(&Entry::put(
                format!("k{i:03}").into_bytes(),
                format!("v{i:03}").into_bytes(),
                i as u64,
            ));
        }
        let sst = builder.build(&path).expect("should be valid");
        assert!(sst.block_crc);
        assert!(!sst.compressed);

        let reopened = SsTable::open(&path).expect("should be valid");
        let (val, seq) = reopened.get(b"k010").expect("should be valid").expect("exists");
        assert_eq!(val, b"v010");
        assert_eq!(seq, 10);
    }

    /// Pre-FLAG_BLOCK_CRC SSTables must keep decoding through the legacy
    /// path (payload only, table-level compression bit).
    #[test]
    fn test_legacy_block_format_still_decodes() {
        let raw = b"legacy payload bytes";
        let out = SsTable::decode_block_payload(raw, false, false).expect("should be valid");
        assert_eq!(out, raw);

        let frame = zstd::encode_all(&raw[..], 1).expect("should be valid");
        let out = SsTable::decode_block_payload(&frame, true, false).expect("should be valid");
        assert_eq!(out, raw);

        // Corrupt legacy compressed data must error, not crash.
        let mut bad = frame.clone();
        if bad.len() > 10 {
            bad[8] ^= 0xFF;
        }
        let res = SsTable::decode_block_payload(&bad, true, false);
        assert!(res.is_err(), "corrupt legacy zstd must error");
    }
}
