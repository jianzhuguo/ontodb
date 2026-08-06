//! Disk-based page structures and buffer pool for B+Tree indexes.
//!
//! Each B+Tree index is stored in a dedicated `.idx` file with 4KB pages.
//! A shared buffer pool caches hot pages in memory with LRU eviction.

use onto_core::Result;
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

// ═══════════════════════════════════════════════════════════════════
//  Constants
// ═══════════════════════════════════════════════════════════════════

/// Page size in bytes (4KB, matches OS page size).
pub const PAGE_SIZE: usize = 4096;

/// Page header size in bytes.
pub const HEADER_SIZE: usize = 24;

/// Size of each slot entry in the slot array.
pub const SLOT_SIZE: usize = 4;

/// Magic number identifying an OntoDB index file.
pub const INDEX_MAGIC: &[u8; 4] = b"OIDX";

/// Current index file format version.
pub const INDEX_VERSION: u32 = 1;

/// Page ID for the file header (page 0).
pub const HEADER_PAGE_ID: u32 = 0;

/// Invalid/empty page ID.
pub const NULL_PAGE: u32 = 0;

// ═══════════════════════════════════════════════════════════════════
//  Page types
// ═══════════════════════════════════════════════════════════════════

/// Type of a B+Tree node page.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageType {
    /// File header / metadata page (page 0).
    Header = 0,
    /// Internal node: keys + child page pointers.
    Internal = 1,
    /// Leaf node: keys + primary key lists + sibling links.
    Leaf = 2,
}

impl PageType {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Header),
            1 => Some(Self::Internal),
            2 => Some(Self::Leaf),
            _ => None,
        }
    }
}

// ═══════════════════════════════════════════════════════════════════
//  Page header (24 bytes)
// ═══════════════════════════════════════════════════════════════════

/// On-disk page header. Fixed 24 bytes at the start of every page.
///
/// Layout:
/// ```text
///  Offset  Size  Field
///  0       1     page_type
///  1       1     flags
///  2       2     num_entries
///  4       2     data_start (start of data region, grows backward)
///  6       2     free_size
///  8       4     parent page id
///  12      4     left sibling page id (leaf only)
///  16      4     right sibling page id (leaf only)
///  20      4     reserved
/// ```
#[derive(Debug, Clone)]
pub struct PageHeader {
    pub page_type: PageType,
    pub flags: u8,
    pub num_entries: u16,
    pub data_start: u16,
    pub free_size: u16,
    pub parent: u32,
    pub left_leaf: u32,
    pub right_leaf: u32,
}

impl PageHeader {
    /// Serializes the header into the first 24 bytes of `buf`.
    pub fn encode(&self, buf: &mut [u8]) {
        buf[0] = self.page_type as u8;
        buf[1] = self.flags;
        buf[2..4].copy_from_slice(&self.num_entries.to_le_bytes());
        buf[4..6].copy_from_slice(&self.data_start.to_le_bytes());
        buf[6..8].copy_from_slice(&self.free_size.to_le_bytes());
        buf[8..12].copy_from_slice(&self.parent.to_le_bytes());
        buf[12..16].copy_from_slice(&self.left_leaf.to_le_bytes());
        buf[16..20].copy_from_slice(&self.right_leaf.to_le_bytes());
        buf[20..24].fill(0); // reserved
    }

    /// Deserializes the header from the first 24 bytes of `buf`.
    pub fn decode(buf: &[u8]) -> Option<Self> {
        let page_type = PageType::from_u8(buf[0])?;
        Some(Self {
            page_type,
            flags: buf[1],
            num_entries: u16::from_le_bytes([buf[2], buf[3]]),
            data_start: u16::from_le_bytes([buf[4], buf[5]]),
            free_size: u16::from_le_bytes([buf[6], buf[7]]),
            parent: u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]),
            left_leaf: u32::from_le_bytes([buf[12], buf[13], buf[14], buf[15]]),
            right_leaf: u32::from_le_bytes([buf[16], buf[17], buf[18], buf[19]]),
        })
    }
}

// ═══════════════════════════════════════════════════════════════════
//  On-disk entry formats
// ═══════════════════════════════════════════════════════════════════

/// Encodes an internal node entry: `[key_len:2][key][child_right:4]`
/// The leftmost child pointer is stored separately in the page header.
pub fn encode_internal_entry(key: &[u8], child_right: u32) -> Vec<u8> {
    let mut buf = Vec::with_capacity(2 + key.len() + 4);
    buf.extend_from_slice(&(key.len() as u16).to_le_bytes());
    buf.extend_from_slice(key);
    buf.extend_from_slice(&child_right.to_le_bytes());
    buf
}

/// Decodes an internal node entry. Returns (key, child_right).
pub fn decode_internal_entry(data: &[u8]) -> Option<(&[u8], u32)> {
    if data.len() < 6 {
        return None;
    }
    let key_len = u16::from_le_bytes([data[0], data[1]]) as usize;
    if data.len() < 2 + key_len + 4 {
        return None;
    }
    let key = &data[2..2 + key_len];
    let child_right = u32::from_le_bytes([
        data[2 + key_len],
        data[3 + key_len],
        data[4 + key_len],
        data[5 + key_len],
    ]);
    Some((key, child_right))
}

/// Encodes a leaf node entry: `[key_len:2][key][num_pks:2][pk_len:2][pk]...`
pub fn encode_leaf_entry(key: &[u8], pks: &[Vec<u8>]) -> Vec<u8> {
    let pk_data_len: usize = pks.iter().map(|pk| 2 + pk.len()).sum();
    let mut buf = Vec::with_capacity(2 + key.len() + 2 + pk_data_len);
    buf.extend_from_slice(&(key.len() as u16).to_le_bytes());
    buf.extend_from_slice(key);
    buf.extend_from_slice(&(pks.len() as u16).to_le_bytes());
    for pk in pks {
        buf.extend_from_slice(&(pk.len() as u16).to_le_bytes());
        buf.extend_from_slice(pk);
    }
    buf
}

/// Decodes a leaf node entry. Returns (key, primary_keys).
pub fn decode_leaf_entry(data: &[u8]) -> Option<(&[u8], Vec<&[u8]>)> {
    if data.len() < 4 {
        return None;
    }
    let key_len = u16::from_le_bytes([data[0], data[1]]) as usize;
    if data.len() < 2 + key_len + 2 {
        return None;
    }
    let key = &data[2..2 + key_len];
    let num_pks = u16::from_le_bytes([data[2 + key_len], data[3 + key_len]]) as usize;
    let mut offset = 4 + key_len;
    let mut pks = Vec::with_capacity(num_pks);
    for _ in 0..num_pks {
        if offset + 2 > data.len() {
            return None;
        }
        let pk_len = u16::from_le_bytes([data[offset], data[offset + 1]]) as usize;
        offset += 2;
        if offset + pk_len > data.len() {
            return None;
        }
        pks.push(&data[offset..offset + pk_len]);
        offset += pk_len;
    }
    Some((key, pks))
}

/// Entry size for capacity estimation.
pub fn internal_entry_size(key_len: usize) -> usize {
    2 + key_len + 4 // key_len + key + child_right
}

pub fn leaf_entry_size(key_len: usize, pks: &[Vec<u8>]) -> usize {
    2 + key_len + 2 + pks.iter().map(|pk| 2 + pk.len()).sum::<usize>()
}

// ═══════════════════════════════════════════════════════════════════
//  Disk page
// ═══════════════════════════════════════════════════════════════════

/// An in-memory representation of a 4KB disk page.
///
/// Slotted page layout:
/// ```text
///  ┌──────────────────────────────────────┐
///  │ Header (24B)                         │
///  │ Slot[0] Slot[1] ... Slot[n-1]       │  ← grows forward
///  │          ... free space ...          │
///  │ [entry_n-1] ... [entry_1] [entry_0] │  ← grows backward
///  └──────────────────────────────────────┘
/// ```
pub struct DiskPage {
    pub page_id: u32,
    pub data: [u8; PAGE_SIZE],
}

impl DiskPage {
    /// Creates a blank page with the given type and ID.
    pub fn new(page_id: u32, page_type: PageType) -> Self {
        let mut page = Self {
            page_id,
            data: [0u8; PAGE_SIZE],
        };
        let header = PageHeader {
            page_type,
            flags: 0,
            num_entries: 0,
            data_start: PAGE_SIZE as u16,
            free_size: (PAGE_SIZE - HEADER_SIZE) as u16,
            parent: NULL_PAGE,
            left_leaf: NULL_PAGE,
            right_leaf: NULL_PAGE,
        };
        header.encode(&mut page.data);
        page
    }

    /// Wraps a raw 4KB buffer as a DiskPage (for reading from disk).
    pub fn from_raw(page_id: u32, data: [u8; PAGE_SIZE]) -> Self {
        Self { page_id, data }
    }

    /// Returns the decoded page header.
    pub fn header(&self) -> Option<PageHeader> {
        PageHeader::decode(&self.data)
    }

    /// Returns the page type.
    pub fn page_type(&self) -> PageType {
        self.header()
            .map(|h| h.page_type)
            .unwrap_or(PageType::Header)
    }

    /// Returns the number of entries in this page.
    pub fn num_entries(&self) -> u16 {
        self.header().map(|h| h.num_entries).unwrap_or(0)
    }

    /// Updates the header in the page data.
    fn set_header(&mut self, header: &PageHeader) {
        header.encode(&mut self.data);
    }

    // ── Slot access ──────────────────────────────────────────────

    /// Returns the byte offset of the i-th slot in the slot array.
    #[inline]
    fn slot_offset(i: u16) -> usize {
        HEADER_SIZE + (i as usize) * SLOT_SIZE
    }

    /// Reads the i-th slot (offset, key_len).
    fn read_slot(&self, i: u16) -> Option<(u16, u16)> {
        let off = Self::slot_offset(i);
        if off + SLOT_SIZE > PAGE_SIZE {
            return None;
        }
        let entry_offset = u16::from_le_bytes([self.data[off], self.data[off + 1]]);
        let key_len = u16::from_le_bytes([self.data[off + 2], self.data[off + 3]]);
        Some((entry_offset, key_len))
    }

    /// Writes the i-th slot.
    fn write_slot(&mut self, i: u16, entry_offset: u16, key_len: u16) {
        let off = Self::slot_offset(i);
        self.data[off..off + 2].copy_from_slice(&entry_offset.to_le_bytes());
        self.data[off + 2..off + 4].copy_from_slice(&key_len.to_le_bytes());
    }

    /// Returns the key bytes for the i-th slot (for binary search).
    pub fn slot_key(&self, i: u16) -> Option<&[u8]> {
        let (entry_offset, key_len) = self.read_slot(i)?;
        let start = entry_offset as usize;
        let end = start + key_len as usize;
        if end > PAGE_SIZE {
            return None;
        }
        // Key starts at entry_offset (format: [key_len:2][key]...)
        // But we already know key_len from the slot, so key is at entry_offset+2
        let key_start = start + 2;
        let key_end = key_start + key_len as usize;
        if key_end > PAGE_SIZE {
            return None;
        }
        Some(&self.data[key_start..key_end])
    }

    // ── Entry insertion ──────────────────────────────────────────

    /// Inserts a raw entry into the page at the given slot position.
    /// Shifts existing slots to make room. Returns Ok(()) or Err if full.
    pub fn insert_entry(&mut self, slot_pos: u16, entry_data: &[u8], key_len: u16) -> Result<()> {
        let mut header = self.header().ok_or_else(|| {
            onto_core::CoreError::Corruption("invalid page header".into())
        })?;

        let entry_size = entry_data.len();
        let new_slot_end = Self::slot_offset(header.num_entries + 1);
        let required = entry_size + SLOT_SIZE;

        if required > header.free_size as usize {
            return Err(onto_core::CoreError::Custom("page full".into()));
        }
        if new_slot_end + entry_size > PAGE_SIZE {
            return Err(onto_core::CoreError::Custom("page overflow".into()));
        }

        // Allocate space from the data area (grows backward)
        header.data_start -= entry_size as u16;
        let entry_offset = header.data_start;

        // Write entry data
        self.data[entry_offset as usize..entry_offset as usize + entry_size]
            .copy_from_slice(entry_data);

        // Shift slots from slot_pos..num_entries right by 1
        let num = header.num_entries;
        for i in (slot_pos..num).rev() {
            let (off, kl) = self.read_slot(i).unwrap();
            self.write_slot(i + 1, off, kl);
        }

        // Write new slot
        self.write_slot(slot_pos, entry_offset, key_len);

        header.num_entries += 1;
        header.free_size -= (entry_size + SLOT_SIZE) as u16;
        self.set_header(&header);

        Ok(())
    }

    /// Appends a raw entry at the end of the slot array (for bulk loading).
    pub fn append_entry(&mut self, entry_data: &[u8], key_len: u16) -> Result<()> {
        self.insert_entry(self.num_entries(), entry_data, key_len)
    }

    // ── Entry removal ────────────────────────────────────────────

    /// Removes the entry at slot position `slot_pos`.
    /// Does NOT reclaim space (defrag needed later). Shifts slots left.
    pub fn remove_entry(&mut self, slot_pos: u16) -> Result<()> {
        let mut header = self.header().ok_or_else(|| {
            onto_core::CoreError::Corruption("invalid page header".into())
        })?;

        if slot_pos >= header.num_entries {
            return Err(onto_core::CoreError::InvalidArgument("slot out of bounds".into()));
        }

        // Read the entry from the slot
        let (_entry_offset, _key_len) = self.read_slot(slot_pos).unwrap();

        // Shift slots left
        let num = header.num_entries;
        for i in slot_pos..num - 1 {
            let (off, kl) = self.read_slot(i + 1).unwrap();
            self.write_slot(i, off, kl);
        }

        header.num_entries -= 1;
        // Note: we don't reclaim data space immediately (would need compaction)
        self.set_header(&header);

        Ok(())
    }

    // ── Read entry data ──────────────────────────────────────────

    /// Returns the raw entry bytes for the i-th slot.
    ///
    /// Computes the exact entry size by parsing the entry format:
    /// - Internal: [key_len:2][key][child:4]  → size = 2 + key_len + 4
    /// - Leaf:     [key_len:2][key][num_pks:2][pk_len:2][pk]...  → variable
    pub fn entry_data(&self, i: u16) -> Option<&[u8]> {
        let (entry_offset, _) = self.read_slot(i)?;
        let start = entry_offset as usize;
        if start + 2 > PAGE_SIZE {
            return None;
        }
        let key_len = u16::from_le_bytes([self.data[start], self.data[start + 1]]) as usize;

        let entry_size = match self.page_type() {
            PageType::Internal => 2 + key_len + 4, // key_len + key + child_right
            PageType::Leaf => {
                // Parse num_pks, then sum up pk sizes
                let pks_offset = start + 2 + key_len;
                if pks_offset + 2 > PAGE_SIZE {
                    return None;
                }
                let num_pks = u16::from_le_bytes([self.data[pks_offset], self.data[pks_offset + 1]]) as usize;
                let mut offset = pks_offset + 2;
                for _ in 0..num_pks {
                    if offset + 2 > PAGE_SIZE {
                        return None;
                    }
                    let pk_len = u16::from_le_bytes([self.data[offset], self.data[offset + 1]]) as usize;
                    offset += 2 + pk_len;
                }
                offset - start
            }
            PageType::Header => return None,
        };

        let end = start + entry_size;
        if end > PAGE_SIZE {
            return None;
        }
        Some(&self.data[start..end])
    }

    // ── High-level insert (internal) ─────────────────────────────

    /// Inserts an internal entry (key, child_right) at the given slot position.
    /// The leftmost child pointer is stored via `set_first_child`.
    pub fn insert_internal(
        &mut self,
        slot_pos: u16,
        key: &[u8],
        child_right: u32,
    ) -> Result<()> {
        let entry = encode_internal_entry(key, child_right);
        self.insert_entry(slot_pos, &entry, key.len() as u16)
    }

    /// Appends an internal entry at the end.
    pub fn append_internal(&mut self, key: &[u8], child_right: u32) -> Result<()> {
        self.insert_internal(self.num_entries(), key, child_right)
    }

    /// Reads an internal entry at slot `i`. Returns (key, child_right).
    pub fn get_internal(&self, i: u16) -> Option<(Vec<u8>, u32)> {
        let data = self.entry_data(i)?;
        let (key, child_right) = decode_internal_entry(data)?;
        Some((key.to_vec(), child_right))
    }

    // ── High-level insert (leaf) ─────────────────────────────────

    /// Inserts a leaf entry (key + primary keys) at the given slot position.
    pub fn insert_leaf(&mut self, slot_pos: u16, key: &[u8], pks: &[Vec<u8>]) -> Result<()> {
        let entry = encode_leaf_entry(key, pks);
        self.insert_entry(slot_pos, &entry, key.len() as u16)
    }

    /// Appends a leaf entry at the end.
    pub fn append_leaf(&mut self, key: &[u8], pks: &[Vec<u8>]) -> Result<()> {
        self.insert_leaf(self.num_entries(), key, pks)
    }

    /// Reads a leaf entry at slot `i`. Returns (key, primary_keys).
    pub fn get_leaf(&self, i: u16) -> Option<(Vec<u8>, Vec<Vec<u8>>)> {
        let data = self.entry_data(i)?;
        let (key, pks) = decode_leaf_entry(data)?;
        Some((key.to_vec(), pks.into_iter().map(|pk| pk.to_vec()).collect()))
    }

    // ── Binary search ────────────────────────────────────────────

    /// Binary search for a key in this page's slot array.
    /// Returns Ok(i) if found, Err(i) for insertion point.
    pub fn binary_search(&self, target: &[u8]) -> std::result::Result<u16, u16> {
        let num = self.num_entries();
        if num == 0 {
            return Err(0);
        }

        let mut lo = 0u16;
        let mut hi = num;

        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            match self.slot_key(mid) {
                Some(key) => match key.cmp(target) {
                    std::cmp::Ordering::Less => lo = mid + 1,
                    std::cmp::Ordering::Greater => hi = mid,
                    std::cmp::Ordering::Equal => return Ok(mid),
                },
                None => return Err(mid),
            }
        }

        Err(lo)
    }

    // ── Page splitting ───────────────────────────────────────────

    /// Splits this leaf page, moving the upper half to `new_page`.
    /// Returns the split key (first key of the new page).
    pub fn split_leaf(&mut self, new_page: &mut DiskPage) -> Result<Vec<u8>> {
        let header = self.header().ok_or_else(|| {
            onto_core::CoreError::Corruption("invalid page header".into())
        })?;

        let num = header.num_entries;
        let mid = num / 2;

        // Copy upper half entries to new page
        for i in mid..num {
            let (key, pks) = self.get_leaf(i).ok_or_else(|| {
                onto_core::CoreError::Corruption("failed to read leaf entry".into())
            })?;
            new_page.append_leaf(&key, &pks)?;
        }

        // Update sibling pointers
        let new_right = header.right_leaf;
        new_page.set_right_leaf(new_right);
        new_page.set_left_leaf(self.page_id);

        // Truncate self: update num_entries
        let mut h = header;
        h.num_entries = mid;
        h.right_leaf = new_page.page_id;
        self.set_header(&h);

        // The split key is the first key in the new page
        let split_key = new_page.get_leaf(0)
            .map(|(k, _)| k)
            .unwrap_or_default();

        Ok(split_key)
    }

    /// Splits this internal page, moving the upper half to `new_page`.
    /// Returns the promoted key that goes up to the parent.
    ///
    /// Layout: first_child | entry[0]=(key0,child1) | entry[1]=(key1,child2) | ...
    /// After split:
    /// - Left keeps entries [0..mid), first_child unchanged
    /// - entry[mid].key is promoted up
    /// - Right gets entries [mid+1..), first_child = entry[mid].child_right
    pub fn split_internal(&mut self, new_page: &mut DiskPage) -> Result<Vec<u8>> {
        let header = self.header().ok_or_else(|| {
            onto_core::CoreError::Corruption("invalid page header".into())
        })?;

        let num = header.num_entries;
        let mid = num / 2;

        // The promoted key is at position mid
        let (promoted_key, promoted_child_right) = self.get_internal(mid).ok_or_else(|| {
            onto_core::CoreError::Corruption("failed to read mid entry".into())
        })?;

        // Right page: first_child = promoted_child_right, entries = mid+1..num
        new_page.set_first_child(promoted_child_right);
        for i in (mid + 1)..num {
            let (key, child_right) = self.get_internal(i).ok_or_else(|| {
                onto_core::CoreError::Corruption("failed to read internal entry".into())
            })?;
            new_page.append_internal(&key, child_right)?;
        }

        // Left page: keep entries [0..mid), first_child stays the same
        let mut h = header;
        h.num_entries = mid;
        self.set_header(&h);

        Ok(promoted_key)
    }

    /// Sets the left_leaf pointer.
    pub fn set_left_leaf(&mut self, page_id: u32) {
        self.data[12..16].copy_from_slice(&page_id.to_le_bytes());
    }

    /// Sets the right_leaf pointer.
    pub fn set_right_leaf(&mut self, page_id: u32) {
        self.data[16..20].copy_from_slice(&page_id.to_le_bytes());
    }

    /// Sets the parent pointer.
    pub fn set_parent(&mut self, page_id: u32) {
        self.data[8..12].copy_from_slice(&page_id.to_le_bytes());
    }

    /// Gets the first child pointer (leftmost child, for internal nodes only).
    /// Reuses the left_leaf field since internal nodes don't need sibling links.
    pub fn first_child(&self) -> u32 {
        u32::from_le_bytes([self.data[12], self.data[13], self.data[14], self.data[15]])
    }

    /// Sets the first child pointer (leftmost child, for internal nodes only).
    pub fn set_first_child(&mut self, page_id: u32) {
        self.data[12..16].copy_from_slice(&page_id.to_le_bytes());
    }
}

// ═══════════════════════════════════════════════════════════════════
//  Index metadata (stored on page 0)
// ═══════════════════════════════════════════════════════════════════

/// Metadata about an index, stored on the file's header page (page 0).
pub struct IndexMeta {
    pub root_page: u32,
    pub num_pages: u32,
    pub free_list_head: u32,
    pub class: String,
    pub column: String,
}

impl IndexMeta {
    /// Encodes metadata into a 4KB page buffer.
    pub fn encode(&self, buf: &mut [u8; PAGE_SIZE]) {
        buf[0..4].copy_from_slice(INDEX_MAGIC);
        buf[4..8].copy_from_slice(&INDEX_VERSION.to_le_bytes());
        buf[8..12].copy_from_slice(&self.root_page.to_le_bytes());
        buf[12..16].copy_from_slice(&self.num_pages.to_le_bytes());
        buf[16..20].copy_from_slice(&self.free_list_head.to_le_bytes());

        let class_bytes = self.class.as_bytes();
        let column_bytes = self.column.as_bytes();
        buf[20] = class_bytes.len() as u8;
        buf[21..21 + class_bytes.len()].copy_from_slice(class_bytes);
        let col_offset = 21 + class_bytes.len();
        buf[col_offset] = column_bytes.len() as u8;
        buf[col_offset + 1..col_offset + 1 + column_bytes.len()].copy_from_slice(column_bytes);

        // Zero the rest
        let end = col_offset + 1 + column_bytes.len();
        if end < PAGE_SIZE {
            buf[end..].fill(0);
        }
    }

    /// Decodes metadata from a 4KB page buffer.
    pub fn decode(buf: &[u8; PAGE_SIZE]) -> Option<Self> {
        if &buf[0..4] != INDEX_MAGIC {
            return None;
        }
        let _version = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
        let root_page = u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]);
        let num_pages = u32::from_le_bytes([buf[12], buf[13], buf[14], buf[15]]);
        let free_list_head = u32::from_le_bytes([buf[16], buf[17], buf[18], buf[19]]);

        let class_len = buf[20] as usize;
        if 21 + class_len > PAGE_SIZE {
            return None;
        }
        let class = std::str::from_utf8(&buf[21..21 + class_len]).ok()?.to_string();

        let col_offset = 21 + class_len;
        if col_offset >= PAGE_SIZE {
            return None;
        }
        let col_len = buf[col_offset] as usize;
        if col_offset + 1 + col_len > PAGE_SIZE {
            return None;
        }
        let column = std::str::from_utf8(&buf[col_offset + 1..col_offset + 1 + col_len])
            .ok()?
            .to_string();

        Some(Self {
            root_page,
            num_pages,
            free_list_head,
            class,
            column,
        })
    }
}

// ═══════════════════════════════════════════════════════════════════
//  Buffer pool
// ═══════════════════════════════════════════════════════════════════

/// A cached page in the buffer pool.
pub struct CachedPage {
    data: [u8; PAGE_SIZE],
    dirty: bool,
}

/// LRU buffer pool for B+Tree pages.
///
/// Caches frequently accessed pages in memory.
/// Pages are evicted in LRU order when the pool is full.
///
/// Uses a monotonic counter for O(1) access tracking instead of
/// maintaining an ordered Vec (which was O(n) per touch).
pub struct BufferPool {
    cache: HashMap<u32, CachedPage>,
    /// Monotonic access counter per page. Higher value = more recently used.
    access_order: HashMap<u32, u64>,
    /// Monotonically increasing counter for access ordering.
    counter: u64,
    capacity: usize,
}

impl BufferPool {
    /// Creates a new buffer pool with the given capacity (in pages).
    pub fn new(capacity: usize) -> Self {
        Self {
            cache: HashMap::with_capacity(capacity),
            access_order: HashMap::with_capacity(capacity),
            counter: 0,
            capacity,
        }
    }

    /// Returns the number of cached pages.
    pub fn len(&self) -> usize {
        self.cache.len()
    }

    /// Returns true if the pool is empty.
    pub fn is_empty(&self) -> bool {
        self.cache.is_empty()
    }

    /// Fetches a page, reading from file if not cached.
    /// If the page is beyond the file end, returns a blank page.
    pub fn fetch(&mut self, page_id: u32, file: &mut File) -> Result<&CachedPage> {
        if !self.cache.contains_key(&page_id) {
            let mut data = [0u8; PAGE_SIZE];
            let offset = (page_id as u64) * (PAGE_SIZE as u64);
            // Try to read; if beyond file end, use blank page
            if file.seek(SeekFrom::Start(offset)).is_ok() {
                let _ = file.read_exact(&mut data); // ignore EOF for sparse files
            }

            if self.cache.len() >= self.capacity {
                self.evict(file)?;
            }

            self.cache.insert(page_id, CachedPage { data, dirty: false });
        }

        self.touch(page_id);
        Ok(self.cache.get(&page_id).unwrap())
    }

    /// Fetches a page for writing (marks it as dirty).
    /// If the page is beyond the file end, starts with a blank page.
    pub fn fetch_mut(&mut self, page_id: u32, file: &mut File) -> Result<&mut CachedPage> {
        if !self.cache.contains_key(&page_id) {
            let mut data = [0u8; PAGE_SIZE];
            let offset = (page_id as u64) * (PAGE_SIZE as u64);
            if file.seek(SeekFrom::Start(offset)).is_ok() {
                let _ = file.read_exact(&mut data);
            }

            if self.cache.len() >= self.capacity {
                self.evict(file)?;
            }

            self.cache.insert(page_id, CachedPage { data, dirty: false });
        }

        self.touch(page_id);
        let page = self.cache.get_mut(&page_id).unwrap();
        page.dirty = true;
        Ok(page)
    }

    /// Flushes all dirty pages to disk.
    pub fn flush(&mut self, file: &mut File) -> Result<()> {
        for (page_id, page) in &self.cache {
            if page.dirty {
                let offset = (*page_id as u64) * (PAGE_SIZE as u64);
                file.seek(SeekFrom::Start(offset))?;
                file.write_all(&page.data)?;
            }
        }
        // Clear dirty flags
        for page in self.cache.values_mut() {
            page.dirty = false;
        }
        Ok(())
    }

    /// Evicts the least recently used page. Flushes it if dirty.
    fn evict(&mut self, file: &mut File) -> Result<()> {
        // Find the page with the lowest access counter
        let victim_id = self.access_order
            .iter()
            .min_by_key(|(_, &counter)| counter)
            .map(|(&id, _)| id);

        if let Some(victim_id) = victim_id {
            self.access_order.remove(&victim_id);
            if let Some(page) = self.cache.remove(&victim_id) {
                if page.dirty {
                    let offset = (victim_id as u64) * (PAGE_SIZE as u64);
                    file.seek(SeekFrom::Start(offset))?;
                    file.write_all(&page.data)?;
                }
            }
        }
        Ok(())
    }

    /// Records a page access. O(1) operation.
    fn touch(&mut self, page_id: u32) {
        self.counter += 1;
        self.access_order.insert(page_id, self.counter);
    }
}

// ═══════════════════════════════════════════════════════════════════
//  BTreeIndex — disk-based B+Tree
// ═══════════════════════════════════════════════════════════════════

/// A disk-based B+Tree index for a single (class, column) pair.
///
/// Each index is stored in a dedicated `.idx` file. Pages are cached
/// in a buffer pool with LRU eviction.
pub struct BTreeIndex {
    file: File,
    pool: BufferPool,
    meta: IndexMeta,
    _path: PathBuf,
}

impl BTreeIndex {
    /// Creates a new B+Tree index file at the given path.
    pub fn create(path: &Path, class: &str, column: &str) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)?;

        let meta = IndexMeta {
            root_page: 1,
            num_pages: 2, // page 0 = header, page 1 = initial root leaf
            free_list_head: NULL_PAGE,
            class: class.to_string(),
            column: column.to_string(),
        };

        let mut pool = BufferPool::new(256);

        // Write header page (page 0)
        {
            let cached = pool.fetch_mut(HEADER_PAGE_ID, &mut { file.try_clone()? })?;
            meta.encode(&mut cached.data);
        }

        // Create initial root leaf page (page 1)
        {
            let root = DiskPage::new(1, PageType::Leaf);
            let cached = pool.fetch_mut(1, &mut { file.try_clone()? })?;
            cached.data.copy_from_slice(&root.data);
        }

        // Flush to disk
        pool.flush(&mut { file.try_clone()? })?;

        Ok(Self {
            file,
            pool,
            meta,
            _path: path.to_path_buf(),
        })
    }

    /// Opens an existing B+Tree index file.
    pub fn open(path: &Path) -> Result<Self> {
        let mut file = OpenOptions::new().read(true).write(true).open(path)?;
        let mut pool = BufferPool::new(256);

        // Read header page
        let cached = pool.fetch(HEADER_PAGE_ID, &mut file)?;
        let meta = IndexMeta::decode(&cached.data)
            .ok_or_else(|| onto_core::CoreError::Corruption("invalid index file header".into()))?;

        Ok(Self {
            file,
            pool,
            meta,
            _path: path.to_path_buf(),
        })
    }

    /// Returns the metadata for this index.
    pub fn meta(&self) -> &IndexMeta {
        &self.meta
    }

    /// Flushes all dirty pages to disk.
    pub fn flush(&mut self) -> Result<()> {
        self.pool.flush(&mut self.file)
    }

    // ── Page I/O helpers ─────────────────────────────────────────

    /// Reads a page from the buffer pool (fetches from disk if needed).
    fn read_page(&mut self, page_id: u32) -> Result<DiskPage> {
        let cached = self.pool.fetch(page_id, &mut self.file)?;
        Ok(DiskPage::from_raw(page_id, cached.data))
    }

    /// Reads a page for writing (marks dirty).
    fn _write_page(&mut self, page_id: u32) -> Result<()> {
        self.pool.fetch_mut(page_id, &mut self.file)?;
        Ok(())
    }

    /// Allocates a new page. Returns the new page ID.
    fn alloc_page(&mut self) -> Result<u32> {
        let page_id = self.meta.num_pages;
        self.meta.num_pages += 1;

        // Update header
        let cached = self.pool.fetch_mut(HEADER_PAGE_ID, &mut self.file)?;
        self.meta.encode(&mut cached.data);

        Ok(page_id)
    }

    /// Gets a DiskPage snapshot from the pool.
    fn get_page(&mut self, page_id: u32) -> Result<DiskPage> {
        self.read_page(page_id)
    }

    /// Writes a DiskPage back to the pool.
    fn put_page(&mut self, page: &DiskPage) -> Result<()> {
        let cached = self.pool.fetch_mut(page.page_id, &mut self.file)?;
        cached.data.copy_from_slice(&page.data);
        Ok(())
    }

    // ── Lookup ───────────────────────────────────────────────────

    /// Point lookup: returns all primary keys with the given value.
    pub fn lookup(&mut self, key: &[u8]) -> Result<Vec<Vec<u8>>> {
        let leaf = self.find_leaf(key)?;

        // Binary search in the leaf
        match leaf.binary_search(key) {
            Ok(i) => {
                let (_, pks) = leaf.get_leaf(i).unwrap_or_default();
                Ok(pks)
            }
            Err(_) => Ok(Vec::new()),
        }
    }

    /// Range scan: returns all primary keys with values in [lo, hi].
    /// If lo is None, scans from the beginning. If hi is None, scans to the end.
    pub fn range_scan(&mut self, lo: Option<&[u8]>, hi: Option<&[u8]>) -> Result<Vec<Vec<u8>>> {
        let mut result = Vec::new();

        // Find starting leaf
        let mut leaf = match lo {
            Some(k) => self.find_leaf(k)?,
            None => self.find_leftmost_leaf()?,
        };

        // Find starting position within the leaf
        let mut start_idx = match lo {
            Some(k) => match leaf.binary_search(k) {
                Ok(i) => i,
                Err(i) => i,
            },
            None => 0,
        };

        // Scan through leaves
        loop {
            let num = leaf.num_entries();
            for i in start_idx..num {
                let (key, pks) = leaf.get_leaf(i).unwrap_or_default();
                // Check upper bound
                if let Some(hi_key) = hi {
                    if key.as_slice() > hi_key {
                        return Ok(result);
                    }
                }
                result.extend(pks);
            }

            // Move to right sibling
            let right = leaf.header().map(|h| h.right_leaf).unwrap_or(NULL_PAGE);
            if right == NULL_PAGE {
                break;
            }
            leaf = self.get_page(right)?;
            start_idx = 0;
        }

        Ok(result)
    }

    // ── Remove ───────────────────────────────────────────────────

    /// Removes a primary key from the entry for the given value.
    /// If the entry becomes empty, the key is removed from the leaf.
    /// Handles underflow by redistributing or merging with siblings.
    pub fn remove(&mut self, key: &[u8], pk: &[u8]) -> Result<bool> {
        let mut leaf = self.find_leaf(key)?;
        let leaf_id = leaf.page_id;

        // Find the key in the leaf
        let slot = match leaf.binary_search(key) {
            Ok(i) => i,
            Err(_) => return Ok(false), // key not found
        };

        // Get existing PKs and remove the target
        let (entry_key, mut pks) = leaf.get_leaf(slot).ok_or_else(|| {
            onto_core::CoreError::Corruption("failed to read leaf entry".into())
        })?;
        let pk_pos = pks.iter().position(|p| p.as_slice() == pk);
        let pk_pos = match pk_pos {
            Some(i) => i,
            None => return Ok(false), // pk not found
        };
        pks.remove(pk_pos);

        if pks.is_empty() {
            // Last PK removed — delete the entire entry
            leaf.remove_entry(slot)?;
            self.put_page(&leaf)?;

            // Handle underflow if this isn't the root
            if leaf_id != self.meta.root_page {
                let min = Self::min_leaf_entries();
                if leaf.num_entries() < min {
                    self.handle_leaf_underflow(leaf_id)?;
                }
            }

            // Update parent separator if the first key changed
            self.update_parent_separator(leaf_id)?;

            // Root collapse: if root is internal with 0 entries, make child the new root
            self.try_collapse_root()?;
        } else {
            // PKs remain — update the entry in place
            leaf.remove_entry(slot)?;
            leaf.insert_leaf(slot, &entry_key, &pks)?;
            self.put_page(&leaf)?;
        }

        Ok(true)
    }

    /// Minimum entries in a non-root leaf before underflow.
    fn min_leaf_entries() -> u16 {
        50 // half of max_leaf_entries (100)
    }

    /// Minimum entries in a non-root internal node before underflow.
    fn min_internal_entries() -> u16 {
        64 // half of max_internal_entries (128)
    }

    /// Handles underflow in a leaf by borrowing from siblings or merging.
    fn handle_leaf_underflow(&mut self, leaf_id: u32) -> Result<()> {
        let leaf = self.get_page(leaf_id)?;
        let header = leaf.header().ok_or_else(|| {
            onto_core::CoreError::Corruption("invalid page header".into())
        })?;
        let parent_id = header.parent;
        if parent_id == NULL_PAGE {
            return Ok(());
        }

        let parent = self.get_page(parent_id)?;
        let child_idx = self.find_child_index(&parent, leaf_id)?;

        let num_children = parent.num_entries() + 1; // entries + 1 = children

        // Try borrow from left sibling
        if child_idx > 0 {
            let left_id = self.get_child(&parent, child_idx - 1)?;
            let left = self.get_page(left_id)?;
            if left.num_entries() > Self::min_leaf_entries() {
                self.redistribute_leaf_left(parent_id, child_idx)?;
                return Ok(());
            }
        }

        // Try borrow from right sibling
        if child_idx < num_children - 1 {
            let right_id = self.get_child(&parent, child_idx + 1)?;
            let right = self.get_page(right_id)?;
            if right.num_entries() > Self::min_leaf_entries() {
                self.redistribute_leaf_right(parent_id, child_idx)?;
                return Ok(());
            }
        }

        // Merge with a sibling
        if child_idx > 0 {
            self.merge_leaves(parent_id, child_idx - 1, child_idx)?;
        } else if num_children > 1 {
            self.merge_leaves(parent_id, child_idx, child_idx + 1)?;
        }

        Ok(())
    }

    /// Handles underflow in an internal node by borrowing or merging.
    fn handle_internal_underflow(&mut self, node_id: u32) -> Result<()> {
        let node = self.get_page(node_id)?;
        let header = node.header().ok_or_else(|| {
            onto_core::CoreError::Corruption("invalid page header".into())
        })?;
        let parent_id = header.parent;
        if parent_id == NULL_PAGE {
            return Ok(());
        }

        let parent = self.get_page(parent_id)?;
        let child_idx = self.find_child_index(&parent, node_id)?;
        let num_children = parent.num_entries() + 1;

        // Try borrow from left
        if child_idx > 0 {
            let left_id = self.get_child(&parent, child_idx - 1)?;
            let left = self.get_page(left_id)?;
            if left.num_entries() > Self::min_internal_entries() {
                self.redistribute_internal_left(parent_id, child_idx)?;
                return Ok(());
            }
        }

        // Try borrow from right
        if child_idx < num_children - 1 {
            let right_id = self.get_child(&parent, child_idx + 1)?;
            let right = self.get_page(right_id)?;
            if right.num_entries() > Self::min_internal_entries() {
                self.redistribute_internal_right(parent_id, child_idx)?;
                return Ok(());
            }
        }

        // Merge
        if child_idx > 0 {
            self.merge_internals(parent_id, child_idx - 1, child_idx)?;
        } else if num_children > 1 {
            self.merge_internals(parent_id, child_idx, child_idx + 1)?;
        }

        Ok(())
    }

    /// Redistributes a key from left leaf sibling to the underflowing leaf.
    fn redistribute_leaf_left(&mut self, parent_id: u32, child_idx: u16) -> Result<()> {
        let parent = self.get_page(parent_id)?;
        let left_id = self.get_child(&parent, child_idx - 1)?;
        let leaf_id = self.get_child(&parent, child_idx)?;

        let mut left = self.get_page(left_id)?;
        let mut leaf = self.get_page(leaf_id)?;

        // Move last entry from left to front of leaf
        let last_idx = left.num_entries() - 1;
        let (key, pks) = left.get_leaf(last_idx).ok_or_else(|| {
            onto_core::CoreError::Corruption("failed to read left sibling entry".into())
        })?;
        left.remove_entry(last_idx)?;
        leaf.insert_leaf(0, &key, &pks)?;

        self.put_page(&left)?;
        self.put_page(&leaf)?;

        // Update parent separator to new first key of leaf
        let new_first = leaf.get_leaf(0).map(|(k, _)| k).unwrap_or_default();
        let mut parent = self.get_page(parent_id)?;
        parent.remove_entry(child_idx - 1)?;
        parent.insert_internal(child_idx - 1, &new_first, leaf_id)?;
        self.put_page(&parent)?;

        Ok(())
    }

    /// Redistributes a key from right leaf sibling to the underflowing leaf.
    fn redistribute_leaf_right(&mut self, parent_id: u32, child_idx: u16) -> Result<()> {
        let parent = self.get_page(parent_id)?;
        let leaf_id = self.get_child(&parent, child_idx)?;
        let right_id = self.get_child(&parent, child_idx + 1)?;

        let mut leaf = self.get_page(leaf_id)?;
        let mut right = self.get_page(right_id)?;

        // Move first entry from right to end of leaf
        let (key, pks) = right.get_leaf(0).ok_or_else(|| {
            onto_core::CoreError::Corruption("failed to read right sibling entry".into())
        })?;
        right.remove_entry(0)?;
        let insert_pos = leaf.num_entries();
        leaf.insert_leaf(insert_pos, &key, &pks)?;

        self.put_page(&leaf)?;
        self.put_page(&right)?;

        // Update parent separator to new first key of right
        let new_first = right.get_leaf(0).map(|(k, _)| k).unwrap_or_default();
        let mut parent = self.get_page(parent_id)?;
        parent.remove_entry(child_idx)?;
        parent.insert_internal(child_idx, &new_first, right_id)?;
        self.put_page(&parent)?;

        Ok(())
    }

    /// Merges two adjacent leaf nodes. left absorbs right.
    fn merge_leaves(&mut self, parent_id: u32, left_idx: u16, right_idx: u16) -> Result<()> {
        let parent = self.get_page(parent_id)?;
        let left_id = self.get_child(&parent, left_idx)?;
        let right_id = self.get_child(&parent, right_idx)?;

        let left_orig = self.get_page(left_id)?;
        let right = self.get_page(right_id)?;

        // Collect all entries from both pages
        let mut all_entries: Vec<(Vec<u8>, Vec<Vec<u8>>)> = Vec::new();
        let num_left = left_orig.num_entries();
        for i in 0..num_left {
            let (key, pks) = left_orig.get_leaf(i).ok_or_else(|| {
                onto_core::CoreError::Corruption("failed to read left leaf entry".into())
            })?;
            all_entries.push((key, pks));
        }
        let num_right = right.num_entries();
        for i in 0..num_right {
            let (key, pks) = right.get_leaf(i).ok_or_else(|| {
                onto_core::CoreError::Corruption("failed to read right leaf entry".into())
            })?;
            all_entries.push((key, pks));
        }

        // Rebuild left page from scratch to avoid fragmentation issues
        let left_header = left_orig.header().unwrap();
        let right_header = right.header().unwrap();
        let mut left = DiskPage::new(left_id, PageType::Leaf);
        left.set_parent(left_header.parent);
        left.set_right_leaf(right_header.right_leaf);

        for (key, pks) in &all_entries {
            let pos = left.num_entries();
            left.insert_leaf(pos, key, pks)?;
        }

        // If right had a right sibling, update its left pointer
        if right_header.right_leaf != NULL_PAGE {
            let mut right_right = self.get_page(right_header.right_leaf)?;
            right_right.set_left_leaf(left_id);
            self.put_page(&right_right)?;
        }

        self.put_page(&left)?;

        // Remove separator from parent
        let mut parent = self.get_page(parent_id)?;
        parent.remove_entry(left_idx)?;
        // Also remove right child pointer — it's at position left_idx+1 in children
        // But since we removed the key at left_idx, the child at right_idx is now at left_idx+1
        // Actually, we need to handle the child array. In our slotted layout,
        // internal entries are (key, child_right). The leftmost child is in the header.
        // After removing entry[left_idx], the right child pointer is implicitly removed.
        // But we need to handle the case where right_idx corresponds to first_child.
        // Actually, the structure is: first_child | entry[0]=(key0,c1) | entry[1]=(key1,c2) | ...
        // Children: first_child=0, entry[0].child=1, entry[1].child=2, ...
        // So child at index i is: if i==0 -> first_child, else entry[i-1].child_right
        // When we merge left and right (left_idx, right_idx=left_idx+1):
        // - right is the child at index right_idx
        // - if right_idx == 1, right is entry[0].child_right — removing entry[0] removes it
        // - if right_idx > 1, right is entry[right_idx-1].child_right — removing entry[left_idx] removes it
        // In both cases, removing entry[left_idx] removes the right child reference.
        // The left child stays as either first_child or entry[left_idx-1].child_right.
        self.put_page(&parent)?;

        // Handle parent underflow
        if parent_id != self.meta.root_page && parent.num_entries() < Self::min_internal_entries() {
            self.handle_internal_underflow(parent_id)?;
        }

        Ok(())
    }

    /// Redistributes from left internal sibling.
    fn redistribute_internal_left(&mut self, parent_id: u32, child_idx: u16) -> Result<()> {
        let parent = self.get_page(parent_id)?;
        let left_id = self.get_child(&parent, child_idx - 1)?;
        let node_id = self.get_child(&parent, child_idx)?;

        // Get separator from parent
        let (_, _separator_child) = parent.get_internal(child_idx - 1).ok_or_else(|| {
            onto_core::CoreError::Corruption("failed to read parent entry".into())
        })?;
        // The separator key is at slot child_idx-1
        let separator_key = parent.slot_key(child_idx - 1).unwrap().to_vec();

        let mut left = self.get_page(left_id)?;
        let mut node = self.get_page(node_id)?;

        // Move last entry from left, push separator down
        let last_idx = left.num_entries() - 1;
        let (left_key, left_child) = left.get_internal(last_idx).unwrap();
        left.remove_entry(last_idx)?;

        // The moved child becomes the first_child of node, old first_child becomes entry
        let old_first = node.first_child();
        node.insert_internal(0, &separator_key, old_first)?;
        node.set_first_child(left_child);

        self.put_page(&left)?;
        self.put_page(&node)?;

        // Update parent separator
        let mut parent = self.get_page(parent_id)?;
        parent.remove_entry(child_idx - 1)?;
        parent.insert_internal(child_idx - 1, &left_key, node_id)?;
        self.put_page(&parent)?;

        // Update moved child's parent pointer
        let mut moved = self.get_page(left_child)?;
        moved.set_parent(node_id);
        self.put_page(&moved)?;

        Ok(())
    }

    /// Redistributes from right internal sibling.
    fn redistribute_internal_right(&mut self, parent_id: u32, child_idx: u16) -> Result<()> {
        let parent = self.get_page(parent_id)?;
        let node_id = self.get_child(&parent, child_idx)?;
        let right_id = self.get_child(&parent, child_idx + 1)?;

        let separator_key = parent.slot_key(child_idx).unwrap().to_vec();

        let mut node = self.get_page(node_id)?;
        let mut right = self.get_page(right_id)?;

        // Move first child of right to node, push separator down
        let right_first = right.first_child();
        let pos = node.num_entries();
        node.insert_internal(pos, &separator_key, right_first)?;

        // Move right's first entry up to parent
        let (right_key, right_child) = right.get_internal(0).unwrap();
        right.remove_entry(0)?;
        right.set_first_child(right_child);

        self.put_page(&node)?;
        self.put_page(&right)?;

        // Update parent separator
        let mut parent = self.get_page(parent_id)?;
        parent.remove_entry(child_idx)?;
        parent.insert_internal(child_idx, &right_key, right_id)?;
        self.put_page(&parent)?;

        // Update moved child's parent pointer
        let mut moved = self.get_page(right_first)?;
        moved.set_parent(node_id);
        self.put_page(&moved)?;

        Ok(())
    }

    /// Merges two adjacent internal nodes. left absorbs right.
    fn merge_internals(&mut self, parent_id: u32, left_idx: u16, right_idx: u16) -> Result<()> {
        let parent = self.get_page(parent_id)?;
        let left_id = self.get_child(&parent, left_idx)?;
        let right_id = self.get_child(&parent, right_idx)?;

        let separator_key = parent.slot_key(left_idx).unwrap().to_vec();

        let left_orig = self.get_page(left_id)?;
        let right = self.get_page(right_id)?;

        // Collect all entries: left's first_child + left's entries + separator + right's first_child + right's entries
        let left_first = left_orig.first_child();
        let right_first = right.first_child();

        let mut all_entries: Vec<(Vec<u8>, u32)> = Vec::new();
        // Separator goes first with right_first as its child
        all_entries.push((separator_key.clone(), right_first));

        // Left's existing entries
        let num_left = left_orig.num_entries();
        for i in 0..num_left {
            let (key, child) = left_orig.get_internal(i).unwrap();
            all_entries.push((key, child));
        }

        // Right's entries
        let num_right = right.num_entries();
        for i in 0..num_right {
            let (key, child) = right.get_internal(i).unwrap();
            all_entries.push((key, child));
        }

        // Rebuild left page from scratch
        let left_header = left_orig.header().unwrap();
        let mut left = DiskPage::new(left_id, PageType::Internal);
        left.set_parent(left_header.parent);
        left.set_first_child(left_first);

        for (key, child) in &all_entries {
            let pos = left.num_entries();
            left.insert_internal(pos, key, *child)?;
        }

        // Update parent pointers for all children of the merged node
        // (right_first + all right entries)
        {
            let mut rf = self.get_page(right_first)?;
            rf.set_parent(left_id);
            self.put_page(&rf)?;
        }
        for i in 0..num_right {
            let (_, child) = right.get_internal(i).unwrap();
            let mut c = self.get_page(child)?;
            c.set_parent(left_id);
            self.put_page(&c)?;
        }

        self.put_page(&left)?;

        // Remove separator from parent
        let mut parent = self.get_page(parent_id)?;
        parent.remove_entry(left_idx)?;
        self.put_page(&parent)?;

        // Handle parent underflow
        if parent_id != self.meta.root_page && parent.num_entries() < Self::min_internal_entries() {
            self.handle_internal_underflow(parent_id)?;
        }

        Ok(())
    }

    /// Updates the parent separator key for a leaf whose first key may have changed.
    fn update_parent_separator(&mut self, leaf_id: u32) -> Result<()> {
        let leaf = self.get_page(leaf_id)?;
        let header = leaf.header().unwrap();
        let parent_id = header.parent;
        if parent_id == NULL_PAGE {
            return Ok(());
        }

        // Only update if this leaf is NOT the first child (leftmost)
        let parent = self.get_page(parent_id)?;
        let child_idx = self.find_child_index(&parent, leaf_id)?;
        if child_idx == 0 {
            return Ok(()); // first child doesn't have a separator
        }

        let new_first_key = leaf.get_leaf(0).map(|(k, _)| k).unwrap_or_default();
        let mut parent = self.get_page(parent_id)?;
        // The separator for child_idx is at slot child_idx-1
        // It stores (key, child_right=leaf_id). We need to update the key.
        parent.remove_entry(child_idx - 1)?;
        parent.insert_internal(child_idx - 1, &new_first_key, leaf_id)?;
        self.put_page(&parent)?;

        Ok(())
    }

    /// If root is internal with 0 entries, make its only child the new root.
    fn try_collapse_root(&mut self) -> Result<()> {
        let root = self.get_page(self.meta.root_page)?;
        if root.page_type() != PageType::Internal {
            return Ok(());
        }
        if root.num_entries() != 0 {
            return Ok(());
        }

        let new_root = root.first_child();
        self.meta.root_page = new_root;

        // Clear parent pointer on new root
        let mut new_root_page = self.get_page(new_root)?;
        new_root_page.set_parent(NULL_PAGE);
        self.put_page(&new_root_page)?;

        // Update header
        let cached = self.pool.fetch_mut(HEADER_PAGE_ID, &mut self.file)?;
        self.meta.encode(&mut cached.data);

        Ok(())
    }

    /// Finds the index of a child page within a parent internal node.
    /// Children: first_child=0, entry[0].child=1, entry[1].child=2, ...
    fn find_child_index(&self, parent: &DiskPage, child_id: u32) -> Result<u16> {
        if parent.first_child() == child_id {
            return Ok(0);
        }
        let num = parent.num_entries();
        for i in 0..num {
            let (_, child) = parent.get_internal(i).ok_or_else(|| {
                onto_core::CoreError::Corruption("failed to read internal entry".into())
            })?;
            if child == child_id {
                return Ok(i + 1);
            }
        }
        Err(onto_core::CoreError::Corruption("child not found in parent".into()))
    }

    /// Gets the child page ID at the given index within a parent.
    fn get_child(&self, parent: &DiskPage, child_idx: u16) -> Result<u32> {
        if child_idx == 0 {
            Ok(parent.first_child())
        } else {
            let (_, child) = parent.get_internal(child_idx - 1).ok_or_else(|| {
                onto_core::CoreError::Corruption("failed to read internal entry".into())
            })?;
            Ok(child)
        }
    }

    // ── Insert ───────────────────────────────────────────────────

    /// Inserts a (key, primary_key) pair into the index.
    pub fn insert(&mut self, key: &[u8], pk: Vec<u8>) -> Result<()> {
        let mut leaf = self.find_leaf(key)?;
        let num = leaf.num_entries();

        // Check if key already exists
        match leaf.binary_search(key) {
            Ok(i) => {
                // Key exists — append pk to the existing entry
                let (existing_key, mut pks) = leaf.get_leaf(i).unwrap_or_default();
                if !pks.contains(&pk) {
                    pks.push(pk);
                    // Remove old entry, insert updated one
                    leaf.remove_entry(i)?;
                    leaf.insert_leaf(i, &existing_key, &pks)?;
                }
                self.put_page(&leaf)?;
            }
            Err(insert_pos) => {
                // New key — insert at position
                if num < Self::max_leaf_entries() {
                    // Leaf has space
                    leaf.insert_leaf(insert_pos, key, &[pk])?;
                    self.put_page(&leaf)?;
                } else {
                    // Leaf is full — need to split
                    self.insert_and_split_leaf(leaf, insert_pos, key, pk)?;
                }
            }
        }

        Ok(())
    }

    /// Inserts into a full leaf and handles the split.
    fn insert_and_split_leaf(
        &mut self,
        mut leaf: DiskPage,
        insert_pos: u16,
        key: &[u8],
        pk: Vec<u8>,
    ) -> Result<()> {
        // Save parent before rebuilding
        let parent_id = leaf.header().map(|h| h.parent).unwrap_or(NULL_PAGE);
        let old_right = leaf.header().map(|h| h.right_leaf).unwrap_or(NULL_PAGE);
        let leaf_page_id = leaf.page_id;

        // Collect all entries including the new one
        let mut all_entries: Vec<(Vec<u8>, Vec<Vec<u8>>)> = Vec::new();
        let num = leaf.num_entries();
        let mut inserted = false;

        for i in 0..num {
            let (k, pks) = leaf.get_leaf(i).unwrap_or_default();
            if !inserted && i == insert_pos {
                all_entries.push((key.to_vec(), vec![pk.clone()]));
                inserted = true;
            }
            all_entries.push((k, pks));
        }
        if !inserted {
            all_entries.push((key.to_vec(), vec![pk]));
        }

        // Split: lower half stays, upper half goes to new page
        let mid = all_entries.len() / 2;
        let new_page_id = self.alloc_page()?;

        // Rebuild original leaf with lower half
        let mut new_leaf_page = DiskPage::new(new_page_id, PageType::Leaf);

        // Clear and rewrite original leaf, preserving parent
        leaf = DiskPage::new(leaf_page_id, PageType::Leaf);
        leaf.set_parent(parent_id);
        for (k, pks) in &all_entries[..mid] {
            leaf.append_leaf(k, pks)?;
        }

        // Fill new leaf with upper half, also set parent
        new_leaf_page.set_parent(parent_id);
        for (k, pks) in &all_entries[mid..] {
            new_leaf_page.append_leaf(k, pks)?;
        }

        // Set sibling pointers
        leaf.set_right_leaf(new_page_id);
        new_leaf_page.set_left_leaf(leaf.page_id);
        new_leaf_page.set_right_leaf(old_right);

        // If old_right exists, update its left pointer
        if old_right != NULL_PAGE {
            let mut old_right_page = self.get_page(old_right)?;
            old_right_page.set_left_leaf(new_page_id);
            self.put_page(&old_right_page)?;
        }

        // Get the split key (first key of new page)
        let split_key = all_entries[mid].0.clone();

        // Write both pages
        self.put_page(&leaf)?;
        self.put_page(&new_leaf_page)?;

        // Propagate split to parent
        self.insert_into_parent(&leaf, &split_key, new_page_id)?;

        Ok(())
    }

    /// Inserts a key and child pointer into the parent of `left_child`.
    /// If the parent doesn't exist, creates a new root.
    fn insert_into_parent(
        &mut self,
        left_child: &DiskPage,
        key: &[u8],
        right_child_id: u32,
    ) -> Result<()> {
        let parent_id = left_child.header().map(|h| h.parent).unwrap_or(NULL_PAGE);

        if parent_id == NULL_PAGE {
            // No parent — create new root
            let new_root_id = self.alloc_page()?;
            let mut new_root = DiskPage::new(new_root_id, PageType::Internal);

            // leftmost child = old root, then one entry (key → right_child)
            new_root.set_first_child(left_child.page_id);
            new_root.append_internal(key, right_child_id)?;

            // Update children's parent pointers
            let mut left = self.get_page(left_child.page_id)?;
            left.set_parent(new_root_id);
            self.put_page(&left)?;

            let mut right = self.get_page(right_child_id)?;
            right.set_parent(new_root_id);
            self.put_page(&right)?;

            self.put_page(&new_root)?;

            // Update meta
            self.meta.root_page = new_root_id;
            let cached = self.pool.fetch_mut(HEADER_PAGE_ID, &mut self.file)?;
            self.meta.encode(&mut cached.data);

            return Ok(());
        }

        // Parent exists
        let mut parent = self.get_page(parent_id)?;
        let num = parent.num_entries();

        // Find insertion position
        let insert_pos = match parent.binary_search(key) {
            Ok(i) => i + 1,
            Err(i) => i,
        };

        if num < Self::max_internal_entries() {
            // Parent has space
            parent.insert_internal(insert_pos, key, right_child_id)?;
            self.put_page(&parent)?;
        } else {
            // Parent is full — split parent
            self.split_internal_and_insert(parent, insert_pos, key, right_child_id)?;
        }

        Ok(())
    }

    /// Splits a full internal node and inserts a new key.
    fn split_internal_and_insert(
        &mut self,
        mut parent: DiskPage,
        insert_pos: u16,
        key: &[u8],
        right_child_id: u32,
    ) -> Result<()> {
        let num = parent.num_entries();
        let first_child = parent.first_child();
        let parent_parent = parent.header().map(|h| h.parent).unwrap_or(NULL_PAGE);

        // Collect all entries including the new one
        let mut all_entries: Vec<(Vec<u8>, u32)> = Vec::new();
        let mut inserted = false;

        for i in 0..num {
            let (k, child) = parent.get_internal(i).unwrap();
            if !inserted && i == insert_pos {
                all_entries.push((key.to_vec(), right_child_id));
                inserted = true;
            }
            all_entries.push((k, child));
        }
        if !inserted {
            all_entries.push((key.to_vec(), right_child_id));
        }

        // Split at the middle
        let mid = all_entries.len() / 2;
        let promoted_key = all_entries[mid].0.clone();

        let new_page_id = self.alloc_page()?;
        let mut new_internal = DiskPage::new(new_page_id, PageType::Internal);

        // Rebuild original with entries [0..mid), preserving parent
        parent = DiskPage::new(parent.page_id, PageType::Internal);
        parent.set_parent(parent_parent);
        parent.set_first_child(first_child);
        for (k, child) in &all_entries[..mid] {
            parent.append_internal(k, *child)?;
        }

        // Right page: first_child = promoted entry's child_right, entries [mid+1..)
        new_internal.set_first_child(all_entries[mid].1);
        for (k, child) in &all_entries[mid + 1..] {
            new_internal.append_internal(k, *child)?;
        }

        // Update parent pointers for children of the new internal node
        {
            let mut fc = self.get_page(new_internal.first_child())?;
            fc.set_parent(new_page_id);
            self.put_page(&fc)?;
        }
        for i in 0..new_internal.num_entries() {
            let (_, child_id) = new_internal.get_internal(i).unwrap();
            let mut child = self.get_page(child_id)?;
            child.set_parent(new_page_id);
            self.put_page(&child)?;
        }

        self.put_page(&parent)?;
        self.put_page(&new_internal)?;

        // Propagate to grandparent
        self.insert_into_parent(&parent, &promoted_key, new_page_id)?;

        Ok(())
    }

    // ── Leaf traversal ───────────────────────────────────────────

    /// Traverses from root to the leaf that should contain the given key.
    /// Traverses from root to the leaf that should contain the given key.
    /// Internal node layout: first_child | (key0,c1) | (key1,c2) | ...
    fn find_leaf(&mut self, key: &[u8]) -> Result<DiskPage> {
        let mut page = self.get_page(self.meta.root_page)?;

        loop {
            match page.page_type() {
                PageType::Leaf => return Ok(page),
                PageType::Internal => {
                    let num = page.num_entries();
                    let child_id = if num == 0 {
                        page.first_child()
                    } else {
                        match page.binary_search(key) {
                            Ok(i) => {
                                let (_, child) = page.get_internal(i).unwrap();
                                child
                            }
                            Err(i) => {
                                if i == 0 {
                                    page.first_child()
                                } else {
                                    let (_, child) = page.get_internal(i - 1).unwrap();
                                    child
                                }
                            }
                        }
                    };
                    page = self.get_page(child_id)?;
                }
                _ => return Err(onto_core::CoreError::Corruption("invalid page type".into())),
            }
        }
    }

    /// Finds the leftmost leaf (for unbounded range scans).
    fn find_leftmost_leaf(&mut self) -> Result<DiskPage> {
        let mut page = self.get_page(self.meta.root_page)?;
        loop {
            match page.page_type() {
                PageType::Leaf => return Ok(page),
                PageType::Internal => {
                    let child_id = page.first_child();
                    page = self.get_page(child_id)?;
                }
                _ => return Err(onto_core::CoreError::Corruption("invalid page type".into())),
            }
        }
    }

    // ── Debug ────────────────────────────────────────────────────

    /// Prints the tree structure for debugging.
    #[allow(dead_code)]
    fn debug_print(&mut self) {
        eprintln!("=== BTreeIndex debug ===");
        eprintln!("root_page={}, num_pages={}", self.meta.root_page, self.meta.num_pages);
        self.debug_print_page(self.meta.root_page, 0);
        eprintln!("========================");
    }

    #[allow(dead_code)]
    fn debug_print_page(&mut self, page_id: u32, depth: usize) {
        let indent = "  ".repeat(depth);
        let page = match self.get_page(page_id) {
            Ok(p) => p,
            Err(e) => { eprintln!("{}ERROR reading page {}: {:?}", indent, page_id, e); return; }
        };
        match page.page_type() {
            PageType::Leaf => {
                let num = page.num_entries();
                let first_key = page.get_leaf(0).map(|(k, _)| String::from_utf8_lossy(&k).to_string()).unwrap_or_default();
                let last_key = if num > 0 { page.get_leaf(num - 1).map(|(k, _)| String::from_utf8_lossy(&k).to_string()).unwrap_or_default() } else { String::new() };
                let parent = page.header().map(|h| h.parent).unwrap_or(0);
                eprintln!("{}Leaf[{}] parent={} entries={} keys=[{}..{}]", indent, page_id, parent, num, first_key, last_key);
            }
            PageType::Internal => {
                let num = page.num_entries();
                let fc = page.first_child();
                let parent = page.header().map(|h| h.parent).unwrap_or(0);
                eprintln!("{}Internal[{}] parent={} first_child={} entries={}", indent, page_id, parent, fc, num);
                self.debug_print_page(fc, depth + 1);
                for i in 0..num {
                    let (key, child) = page.get_internal(i).unwrap();
                    eprintln!("{}  key={} -> child={}", indent, String::from_utf8_lossy(&key), child);
                    self.debug_print_page(child, depth + 1);
                }
            }
            _ => eprintln!("{}Unknown page type for page {}", indent, page_id),
        }
    }

    // ── Capacity ─────────────────────────────────────────────────

    /// Maximum number of entries in a leaf page.
    fn max_leaf_entries() -> u16 {
        // Rough estimate: assume average key=20 bytes, 1 pk=10 bytes
        // Entry: 2 + 20 + 2 + 2 + 10 = 36 bytes
        // Slot: 4 bytes
        // Per entry: 36 + 4 = 40 bytes
        // Available: 4096 - 24 = 4072
        // Max: 4072 / 40 ≈ 101
        100
    }

    /// Maximum number of entries in an internal page.
    fn max_internal_entries() -> u16 {
        // Entry: 2 + 20 + 4 = 26 bytes, Slot: 4 bytes → 30 per entry
        // Available: 4072 / 30 ≈ 135
        128
    }
}

// ═══════════════════════════════════════════════════════════════════
//  Tests
// ═══════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_page_header_roundtrip() {
        let header = PageHeader {
            page_type: PageType::Leaf,
            flags: 0,
            num_entries: 42,
            data_start: 3000,
            free_size: 1000,
            parent: 5,
            left_leaf: 3,
            right_leaf: 7,
        };
        let mut buf = [0u8; PAGE_SIZE];
        header.encode(&mut buf);

        let decoded = PageHeader::decode(&buf).unwrap();
        assert_eq!(decoded.page_type, PageType::Leaf);
        assert_eq!(decoded.num_entries, 42);
        assert_eq!(decoded.data_start, 3000);
        assert_eq!(decoded.free_size, 1000);
        assert_eq!(decoded.parent, 5);
        assert_eq!(decoded.left_leaf, 3);
        assert_eq!(decoded.right_leaf, 7);
    }

    #[test]
    fn test_internal_entry_roundtrip() {
        let key = b"hello";
        let child = 42u32;
        let encoded = encode_internal_entry(key, child);
        let (k, c) = decode_internal_entry(&encoded).unwrap();
        assert_eq!(k, key);
        assert_eq!(c, child);
    }

    #[test]
    fn test_leaf_entry_roundtrip() {
        let key = b"price_00000000000000999";
        let pks: Vec<Vec<u8>> = vec![b"pk1".to_vec(), b"pk2".to_vec(), b"pk3".to_vec()];
        let encoded = encode_leaf_entry(key, &pks);
        let (k, decoded_pks) = decode_leaf_entry(&encoded).unwrap();
        assert_eq!(k, key);
        assert_eq!(decoded_pks.len(), 3);
        assert_eq!(decoded_pks[0], b"pk1");
        assert_eq!(decoded_pks[1], b"pk2");
        assert_eq!(decoded_pks[2], b"pk3");
    }

    #[test]
    fn test_leaf_entry_single_pk() {
        let key = b"test_key";
        let pks: Vec<Vec<u8>> = vec![b"only_pk".to_vec()];
        let encoded = encode_leaf_entry(key, &pks);
        let (k, decoded_pks) = decode_leaf_entry(&encoded).unwrap();
        assert_eq!(k, key);
        assert_eq!(decoded_pks, vec![b"only_pk".to_vec()]);
    }

    #[test]
    fn test_disk_page_insert_and_search() {
        let mut page = DiskPage::new(1, PageType::Leaf);

        // Insert entries in sorted order
        page.append_leaf(b"aaa", &[b"pk1".to_vec()]).unwrap();
        page.append_leaf(b"bbb", &[b"pk2".to_vec()]).unwrap();
        page.append_leaf(b"ccc", &[b"pk3".to_vec()]).unwrap();

        assert_eq!(page.num_entries(), 3);

        // Binary search
        assert_eq!(page.binary_search(b"bbb"), Ok(1));
        assert_eq!(page.binary_search(b"aaa"), Ok(0));
        assert_eq!(page.binary_search(b"ccc"), Ok(2));
        assert_eq!(page.binary_search(b"aaa"), Ok(0));
        assert_eq!(page.binary_search(b"bbb"), Ok(1));
        assert_eq!(page.binary_search(b"ccc"), Ok(2));

        // Not found
        assert!(page.binary_search(b"aab").is_err());
        assert!(page.binary_search(b"zzz").is_err());
    }

    #[test]
    fn test_disk_page_insert_ordered() {
        let mut page = DiskPage::new(1, PageType::Internal);

        page.append_internal(b"100", 2).unwrap();
        page.append_internal(b"200", 3).unwrap();
        page.append_internal(b"300", 4).unwrap();

        let (k, c) = page.get_internal(0).unwrap();
        assert_eq!(k, b"100");
        assert_eq!(c, 2);

        let (k, c) = page.get_internal(2).unwrap();
        assert_eq!(k, b"300");
        assert_eq!(c, 4);
    }

    #[test]
    fn test_disk_page_insert_at_position() {
        let mut page = DiskPage::new(1, PageType::Leaf);

        page.append_leaf(b"aaa", &[b"pk1".to_vec()]).unwrap();
        page.append_leaf(b"ccc", &[b"pk3".to_vec()]).unwrap();

        // Insert "bbb" at position 1
        page.insert_leaf(1, b"bbb", &[b"pk2".to_vec()]).unwrap();

        assert_eq!(page.num_entries(), 3);
        assert_eq!(page.binary_search(b"bbb"), Ok(1));

        let (k, _) = page.get_leaf(0).unwrap();
        assert_eq!(k, b"aaa");
        let (k, _) = page.get_leaf(1).unwrap();
        assert_eq!(k, b"bbb");
        let (k, _) = page.get_leaf(2).unwrap();
        assert_eq!(k, b"ccc");
    }

    #[test]
    fn test_disk_page_remove() {
        let mut page = DiskPage::new(1, PageType::Leaf);

        page.append_leaf(b"aaa", &[b"pk1".to_vec()]).unwrap();
        page.append_leaf(b"bbb", &[b"pk2".to_vec()]).unwrap();
        page.append_leaf(b"ccc", &[b"pk3".to_vec()]).unwrap();

        page.remove_entry(1).unwrap();
        assert_eq!(page.num_entries(), 2);

        let (k, _) = page.get_leaf(0).unwrap();
        assert_eq!(k, b"aaa");
        let (k, _) = page.get_leaf(1).unwrap();
        assert_eq!(k, b"ccc");
    }

    #[test]
    fn test_disk_page_many_entries() {
        let mut page = DiskPage::new(1, PageType::Leaf);

        // Insert 100 entries
        for i in 0..100u32 {
            let key = format!("{:010}", i);
            let pk = format!("pk_{:04}", i);
            page.append_leaf(key.as_bytes(), &[pk.into_bytes()]).unwrap();
        }

        assert_eq!(page.num_entries(), 100);

        // Verify all entries are searchable
        for i in 0..100u32 {
            let key = format!("{:010}", i);
            assert!(page.binary_search(key.as_bytes()).is_ok(), "key {} not found", key);
        }

        // Verify sorted order
        for i in 0..99u16 {
            let k1 = page.slot_key(i).unwrap();
            let k2 = page.slot_key(i + 1).unwrap();
            assert!(k1 < k2, "keys not sorted at positions {} and {}", i, i + 1);
        }
    }

    #[test]
    fn test_disk_page_split_leaf() {
        let mut page = DiskPage::new(1, PageType::Leaf);
        let mut new_page = DiskPage::new(2, PageType::Leaf);

        // Insert 50 entries
        for i in 0..50u32 {
            let key = format!("{:010}", i);
            let pk = format!("pk_{}", i);
            page.append_leaf(key.as_bytes(), &[pk.into_bytes()]).unwrap();
        }

        let split_key = page.split_leaf(&mut new_page).unwrap();

        // Split key should be the first key of the new page
        assert!(!split_key.is_empty());

        // Original page should have half the entries
        assert!(page.num_entries() < 50);
        assert!(new_page.num_entries() > 0);
        assert_eq!(page.num_entries() + new_page.num_entries(), 50);

        // Sibling pointers should be set
        let h1 = page.header().unwrap();
        let h2 = new_page.header().unwrap();
        assert_eq!(h1.right_leaf, 2);
        assert_eq!(h2.left_leaf, 1);
    }

    #[test]
    fn test_index_meta_roundtrip() {
        let meta = IndexMeta {
            root_page: 1,
            num_pages: 10,
            free_list_head: 0,
            class: "Product".to_string(),
            column: "price".to_string(),
        };

        let mut buf = [0u8; PAGE_SIZE];
        meta.encode(&mut buf);

        let decoded = IndexMeta::decode(&buf).unwrap();
        assert_eq!(decoded.root_page, 1);
        assert_eq!(decoded.num_pages, 10);
        assert_eq!(decoded.class, "Product");
        assert_eq!(decoded.column, "price");
    }

    #[test]
    fn test_buffer_pool_basic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.idx");
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .unwrap();

        // Write 3 pages
        for i in 0..3u32 {
            let mut data = [0u8; PAGE_SIZE];
            data[0] = i as u8;
            file.write_all(&data).unwrap();
        }

        let mut pool = BufferPool::new(2);

        // Fetch page 0
        {
            let page = pool.fetch(0, &mut file).unwrap();
            assert_eq!(page.data[0], 0);
        }

        // Fetch page 1
        {
            let page = pool.fetch(1, &mut file).unwrap();
            assert_eq!(page.data[0], 1);
        }

        // Fetch page 2 — should evict page 0 (LRU)
        {
            let page = pool.fetch(2, &mut file).unwrap();
            assert_eq!(page.data[0], 2);
        }

        assert_eq!(pool.len(), 2);

        // Re-fetch page 0 — should read from disk again
        {
            let page = pool.fetch(0, &mut file).unwrap();
            assert_eq!(page.data[0], 0);
        }
    }

    #[test]
    fn test_buffer_pool_flush() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test_flush.idx");
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .unwrap();

        // Write a blank page
        let blank = [0u8; PAGE_SIZE];
        file.write_all(&blank).unwrap();

        let mut pool = BufferPool::new(10);

        // Modify page 0
        {
            let page = pool.fetch_mut(0, &mut file).unwrap();
            page.data[0] = 0xFF;
        }

        // Flush
        pool.flush(&mut file).unwrap();

        // Read back from disk
        let mut buf = [0u8; PAGE_SIZE];
        file.seek(SeekFrom::Start(0)).unwrap();
        file.read_exact(&mut buf).unwrap();
        assert_eq!(buf[0], 0xFF);
    }

    #[test]
    fn test_binary_search_edge_cases() {
        let mut page = DiskPage::new(1, PageType::Leaf);

        // Empty page
        assert!(page.binary_search(b"any").is_err());

        // Single entry
        page.append_leaf(b"only", &[b"pk".to_vec()]).unwrap();
        assert_eq!(page.binary_search(b"only"), Ok(0));
        assert!(page.binary_search(b"a").is_err());
        assert!(page.binary_search(b"z").is_err());
    }

    // ── BTreeIndex tests ─────────────────────────────────────────

    #[test]
    fn test_btree_index_create_and_lookup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.idx");

        let mut idx = BTreeIndex::create(&path, "Product", "price").unwrap();

        // Insert entries
        idx.insert(b"100", b"pk1".to_vec()).unwrap();
        idx.insert(b"200", b"pk2".to_vec()).unwrap();
        idx.insert(b"300", b"pk3".to_vec()).unwrap();

        // Lookup
        let pks = idx.lookup(b"200").unwrap();
        assert_eq!(pks.len(), 1);
        assert_eq!(pks[0], b"pk2");

        let pks = idx.lookup(b"100").unwrap();
        assert_eq!(pks.len(), 1);
        assert_eq!(pks[0], b"pk1");

        // Not found
        let pks = idx.lookup(b"999").unwrap();
        assert!(pks.is_empty());
    }

    #[test]
    fn test_btree_index_duplicate_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dup.idx");

        let mut idx = BTreeIndex::create(&path, "Product", "price").unwrap();

        // Same key, different PKs
        idx.insert(b"100", b"pk1".to_vec()).unwrap();
        idx.insert(b"100", b"pk2".to_vec()).unwrap();
        idx.insert(b"100", b"pk3".to_vec()).unwrap();

        let pks = idx.lookup(b"100").unwrap();
        assert_eq!(pks.len(), 3);
        assert!(pks.contains(&b"pk1".to_vec()));
        assert!(pks.contains(&b"pk2".to_vec()));
        assert!(pks.contains(&b"pk3".to_vec()));
    }

    #[test]
    fn test_btree_index_range_scan() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("range.idx");

        let mut idx = BTreeIndex::create(&path, "Product", "price").unwrap();

        for i in 0..20u32 {
            let key = format!("{:04}", i);
            let pk = format!("pk_{}", i);
            idx.insert(key.as_bytes(), pk.into_bytes()).unwrap();
        }

        // Range scan [0005, 0015]
        let pks = idx.range_scan(Some(b"0005"), Some(b"0015")).unwrap();
        assert_eq!(pks.len(), 11); // 5..=15

        // Unbounded low
        let pks = idx.range_scan(None, Some(b"0002")).unwrap();
        assert_eq!(pks.len(), 3); // 0, 1, 2

        // Unbounded high
        let pks = idx.range_scan(Some(b"0018"), None).unwrap();
        assert_eq!(pks.len(), 2); // 18, 19
    }

    #[test]
    fn test_btree_index_split_leaf() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("split.idx");

        let mut idx = BTreeIndex::create(&path, "Product", "price").unwrap();

        // Insert enough entries to trigger a leaf split
        for i in 0..200u32 {
            let key = format!("{:06}", i);
            let pk = format!("pk_{:04}", i);
            idx.insert(key.as_bytes(), pk.into_bytes()).unwrap();
        }

        // Verify all entries are still accessible
        for i in 0..200u32 {
            let key = format!("{:06}", i);
            let pk = format!("pk_{:04}", i);
            let pks = idx.lookup(key.as_bytes()).unwrap();
            assert_eq!(pks.len(), 1, "key {} should have 1 pk", key);
            assert_eq!(pks[0], pk.as_bytes());
        }

        // Range scan across split boundaries
        let pks = idx.range_scan(Some(b"000050"), Some(b"000150")).unwrap();
        assert_eq!(pks.len(), 101); // 50..=150
    }

    #[test]
    fn test_btree_index_split_internal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("split_int.idx");

        let mut idx = BTreeIndex::create(&path, "Product", "price").unwrap();

        // Insert enough entries to trigger multiple splits
        for i in 0..500u32 {
            let key = format!("{:08}", i);
            let pk = format!("pk_{:04}", i);
            idx.insert(key.as_bytes(), pk.into_bytes()).unwrap();
        }

        // Verify all
        for i in 0..500u32 {
            let key = format!("{:08}", i);
            let pks = idx.lookup(key.as_bytes()).unwrap();
            assert_eq!(pks.len(), 1, "key {} lost after splits", key);
        }

        // Range scan
        let pks = idx.range_scan(Some(b"00000100"), Some(b"00000200")).unwrap();
        assert_eq!(pks.len(), 101);
    }

    #[test]
    fn test_btree_index_persistence() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("persist.idx");

        // Write
        {
            let mut idx = BTreeIndex::create(&path, "Product", "price").unwrap();
            for i in 0..100u32 {
                let key = format!("{:06}", i);
                let pk = format!("pk_{}", i);
                idx.insert(key.as_bytes(), pk.into_bytes()).unwrap();
            }
            idx.flush().unwrap();
        }

        // Reopen and verify
        {
            let mut idx = BTreeIndex::open(&path).unwrap();
            assert_eq!(idx.meta().class, "Product");
            assert_eq!(idx.meta().column, "price");

            for i in 0..100u32 {
                let key = format!("{:06}", i);
                let pks = idx.lookup(key.as_bytes()).unwrap();
                assert_eq!(pks.len(), 1, "key {} not found after reopen", key);
            }
        }
    }

    #[test]
    fn test_btree_index_large_dataset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("large.idx");

        let mut idx = BTreeIndex::create(&path, "Product", "price").unwrap();

        let n = 2000u32;
        for i in 0..n {
            let key = format!("{:010}", i);
            let pk = format!("pk_{:08}", i);
            idx.insert(key.as_bytes(), pk.into_bytes()).unwrap();
        }

        // Point lookup
        let pks = idx.lookup(b"0000001000").unwrap();
        assert_eq!(pks.len(), 1);

        // Range scan
        let pks = idx.range_scan(Some(b"0000000500"), Some(b"0000000599")).unwrap();
        assert_eq!(pks.len(), 100);
    }

    #[test]
    fn test_btree_index_remove_basic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("remove_basic.idx");

        let mut idx = BTreeIndex::create(&path, "Product", "price").unwrap();

        idx.insert(b"100", b"pk1".to_vec()).unwrap();
        idx.insert(b"200", b"pk2".to_vec()).unwrap();
        idx.insert(b"300", b"pk3".to_vec()).unwrap();

        // Remove one
        let removed = idx.remove(b"200", b"pk2").unwrap();
        assert!(removed);
        assert!(idx.lookup(b"200").unwrap().is_empty());

        // Others still present
        assert_eq!(idx.lookup(b"100").unwrap().len(), 1);
        assert_eq!(idx.lookup(b"300").unwrap().len(), 1);
    }

    #[test]
    fn test_btree_index_remove_one_of_many_pks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("remove_partial.idx");

        let mut idx = BTreeIndex::create(&path, "Order", "status").unwrap();

        idx.insert(b"active", b"o1".to_vec()).unwrap();
        idx.insert(b"active", b"o2".to_vec()).unwrap();
        idx.insert(b"active", b"o3".to_vec()).unwrap();

        idx.remove(b"active", b"o2").unwrap();
        let pks = idx.lookup(b"active").unwrap();
        assert_eq!(pks.len(), 2);
        assert!(!pks.contains(&b"o2".to_vec()));

        // Remove remaining
        idx.remove(b"active", b"o1").unwrap();
        idx.remove(b"active", b"o3").unwrap();
        assert!(idx.lookup(b"active").unwrap().is_empty());
    }

    #[test]
    fn test_btree_index_remove_nonexistent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("remove_none.idx");

        let mut idx = BTreeIndex::create(&path, "Product", "price").unwrap();
        idx.insert(b"100", b"pk1".to_vec()).unwrap();

        // Remove non-existent key
        let removed = idx.remove(b"999", b"pk999").unwrap();
        assert!(!removed);
        assert_eq!(idx.lookup(b"100").unwrap().len(), 1);

        // Remove non-existent pk on existing key
        let removed = idx.remove(b"100", b"wrong_pk").unwrap();
        assert!(!removed);
        assert_eq!(idx.lookup(b"100").unwrap().len(), 1);
    }

    #[test]
    fn test_btree_index_remove_with_merge() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("remove_merge.idx");

        let mut idx = BTreeIndex::create(&path, "Product", "price").unwrap();

        let n = 300u32;
        for i in 0..n {
            let key = format!("{:010}", i);
            let pk = format!("pk_{}", i);
            idx.insert(key.as_bytes(), pk.into_bytes()).unwrap();
        }

        // Remove most entries to trigger underflow and merges
        for i in 0..(n - 5) {
            let key = format!("{:010}", i);
            idx.remove(key.as_bytes(), format!("pk_{}", i).as_bytes()).unwrap();
        }

        // Verify remaining entries
        for i in (n - 5)..n {
            let key = format!("{:010}", i);
            let pks = idx.lookup(key.as_bytes()).unwrap();
            assert_eq!(pks.len(), 1, "key {} should still exist", key);
        }

        // Range scan should still work
        let all = idx.range_scan(None, None).unwrap();
        assert_eq!(all.len(), 5);
    }

    #[test]
    fn test_btree_index_remove_interleaved() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("remove_interleave.idx");

        let mut idx = BTreeIndex::create(&path, "Product", "price").unwrap();

        for i in 0..200u32 {
            let key = format!("{:010}", i);
            idx.insert(key.as_bytes(), format!("pk_{}", i).into_bytes()).unwrap();
        }

        // Remove even keys
        for i in (0..200u32).step_by(2) {
            let key = format!("{:010}", i);
            idx.remove(key.as_bytes(), format!("pk_{}", i).as_bytes()).unwrap();
        }
        // Verify odd keys remain
        for i in (1..200u32).step_by(2) {
            let key = format!("{:010}", i);
            assert_eq!(idx.lookup(key.as_bytes()).unwrap().len(), 1);
        }
        // Verify even keys are gone
        for i in (0..200u32).step_by(2) {
            let key = format!("{:010}", i);
            assert!(idx.lookup(key.as_bytes()).unwrap().is_empty());
        }

        // Re-insert even keys
        for i in (0..200u32).step_by(2) {
            let key = format!("{:010}", i);
            idx.insert(key.as_bytes(), format!("new_{}", i).into_bytes()).unwrap();
        }
        assert_eq!(idx.range_scan(None, None).unwrap().len(), 200);
    }

    #[test]
    fn test_btree_index_remove_all() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("remove_all.idx");

        let mut idx = BTreeIndex::create(&path, "Product", "price").unwrap();

        for i in 0..100u32 {
            let key = format!("{:010}", i);
            idx.insert(key.as_bytes(), format!("pk_{}", i).into_bytes()).unwrap();
        }

        for i in 0..100u32 {
            let key = format!("{:010}", i);
            idx.remove(key.as_bytes(), format!("pk_{}", i).as_bytes()).unwrap();
        }

        assert!(idx.range_scan(None, None).unwrap().is_empty());
    }
}
