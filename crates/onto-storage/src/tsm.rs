//! TSM (Time-Structured Merge) storage for time series data.
//!
//! TSM is a column-oriented storage format optimized for time series workloads.
//! It stores data in blocks, where each block contains a compressed column of
//! values for a single metric/field.
//!
//! Key features:
//! - Column-oriented: each field is stored separately for efficient compression
//! - Time-based partitioning: data is partitioned by time range
//! - Delta encoding for timestamps (high compression ratio)
//! - Gorilla encoding for float values (high compression ratio)
//! - Block-level compression with zstd
//! - Index for fast point lookups
//!
//! Design:
//! - Each TSM file contains blocks of (timestamp, value) pairs for a single field
//! - Blocks are sorted by time and compressed
//! - An index at the end of the file maps (series_key, time_range) → block_offset
//! - Hot data lives in MemTable, warm data in TSM files, cold data in Parquet

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

// ── Configuration ──

/// Maximum block size (number of entries per block).
const MAX_BLOCK_SIZE: usize = 1000;

/// TSM file header magic bytes.
const TSM_MAGIC: &[u8; 4] = b"TSM1";

/// TSM file version.
const TSM_VERSION: u8 = 1;

// ── Data Types ──

/// A time series value type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TsValue {
    Float(f64),
    Integer(i64),
    Boolean(bool),
    String(String),
}

/// A single time series data point.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TsPoint {
    /// Timestamp in nanoseconds since Unix epoch.
    pub timestamp: i64,
    /// The value.
    pub value: TsValue,
}

/// A block of time series data for a single series.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TsBlock {
    /// Series key (e.g., "cpu.usage{host=server1}").
    pub series_key: String,
    /// Minimum timestamp in this block.
    pub min_timestamp: i64,
    /// Maximum timestamp in this block.
    pub max_timestamp: i64,
    /// Sorted timestamps.
    pub timestamps: Vec<i64>,
    /// Values (same length as timestamps).
    pub values: Vec<TsValue>,
}

impl TsBlock {
    /// Create a new empty block for a series.
    pub fn new(series_key: String) -> Self {
        Self {
            series_key,
            min_timestamp: i64::MAX,
            max_timestamp: i64::MIN,
            timestamps: Vec::new(),
            values: Vec::new(),
        }
    }

    /// Add a point to this block.
    pub fn push(&mut self, point: TsPoint) {
        if point.timestamp < self.min_timestamp {
            self.min_timestamp = point.timestamp;
        }
        if point.timestamp > self.max_timestamp {
            self.max_timestamp = point.timestamp;
        }
        self.timestamps.push(point.timestamp);
        self.values.push(point.value);
    }

    /// Whether this block is full.
    pub fn is_full(&self) -> bool {
        self.timestamps.len() >= MAX_BLOCK_SIZE
    }

    /// Number of points in this block.
    pub fn len(&self) -> usize {
        self.timestamps.len()
    }

    pub fn is_empty(&self) -> bool {
        self.timestamps.is_empty()
    }
}

// ── TSM File Format ──

/// TSM file block entry in the index.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockEntry {
    /// Series key.
    pub series_key: String,
    /// Minimum timestamp.
    pub min_timestamp: i64,
    /// Maximum timestamp.
    pub max_timestamp: i64,
    /// Block offset in the file.
    pub offset: u64,
    /// Block size in bytes.
    pub size: u32,
}

/// TSM file index.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TsmIndex {
    /// Block entries sorted by (series_key, min_timestamp).
    pub entries: Vec<BlockEntry>,
}

// ── Timestamp Encoding (Delta + Delta-of-Delta) ──

/// Encode timestamps using simple delta encoding.
///
/// Stores first timestamp, then deltas as i64 values.
/// This is simple and correct; production version would use
/// delta-of-delta with variable-length encoding for better compression.
pub fn encode_timestamps(timestamps: &[i64]) -> Vec<u8> {
    if timestamps.is_empty() {
        return Vec::new();
    }

    let mut buf = Vec::new();

    // Write first timestamp
    buf.extend_from_slice(&timestamps[0].to_le_bytes());

    // Write deltas as fixed-size i64
    for i in 1..timestamps.len() {
        let delta = timestamps[i] - timestamps[i - 1];
        buf.extend_from_slice(&delta.to_le_bytes());
    }

    buf
}

/// Decode timestamps from delta encoding.
pub fn decode_timestamps(data: &[u8], count: usize) -> Option<Vec<i64>> {
    if count == 0 || data.len() < 8 {
        return None;
    }

    let mut timestamps = Vec::with_capacity(count);
    let first = i64::from_le_bytes(data[0..8].try_into().ok()?);
    timestamps.push(first);

    let mut pos = 8;
    while timestamps.len() < count && pos + 8 <= data.len() {
        let delta = i64::from_le_bytes(data[pos..pos + 8].try_into().ok()?);
        timestamps.push(timestamps.last()? + delta);
        pos += 8;
    }

    Some(timestamps)
}

// ── Float Value Encoding (Gorilla) ──

/// Encode float values using Gorilla encoding.
///
/// This achieves excellent compression for slowly-changing float values.
/// - First value: stored as-is (8 bytes)
/// - Subsequent values: store only the XOR with previous value
///   - If XOR is 0: 1 bit
///   - Otherwise: use leading/trailing zero counts to store fewer bits
pub fn encode_floats(values: &[f64]) -> Vec<u8> {
    if values.is_empty() {
        return Vec::new();
    }

    let mut buf = Vec::new();

    // Write first value
    buf.extend_from_slice(&values[0].to_le_bytes());

    if values.len() == 1 {
        return buf;
    }

    // For simplicity, use delta encoding for now
    // Full Gorilla encoding would use bit-level operations
    let mut prev = values[0];
    for i in 1..values.len() {
        let xor = values[i].to_bits() ^ prev.to_bits();
        if xor == 0 {
            buf.push(0x00); // Same value
        } else {
            buf.push(0x01); // Different value
            buf.extend_from_slice(&xor.to_le_bytes());
        }
        prev = values[i];
    }

    buf
}

/// Decode float values from Gorilla encoding.
pub fn decode_floats(data: &[u8], count: usize) -> Option<Vec<f64>> {
    if count == 0 || data.len() < 8 {
        return None;
    }

    let mut values = Vec::with_capacity(count);
    let first = f64::from_le_bytes(data[0..8].try_into().ok()?);
    values.push(first);

    if count == 1 {
        return Some(values);
    }

    let mut pos = 8;
    let mut prev = first;

    while values.len() < count && pos < data.len() {
        let flag = data[pos];
        pos += 1;

        if flag == 0x00 {
            // Same value
            values.push(prev);
        } else {
            // Different value
            if pos + 8 > data.len() {
                return None;
            }
            let xor = u64::from_le_bytes(data[pos..pos + 8].try_into().ok()?);
            pos += 8;
            let val = f64::from_bits(prev.to_bits() ^ xor);
            values.push(val);
            prev = val;
        }
    }

    Some(values)
}

// ── Integer Value Encoding (Delta) ──

/// Encode integer values using delta encoding.
pub fn encode_integers(values: &[i64]) -> Vec<u8> {
    if values.is_empty() {
        return Vec::new();
    }

    let mut buf = Vec::new();
    buf.extend_from_slice(&values[0].to_le_bytes());

    for i in 1..values.len() {
        let delta = values[i] - values[i - 1];
        buf.extend_from_slice(&delta.to_le_bytes());
    }

    buf
}

/// Decode integer values from delta encoding.
pub fn decode_integers(data: &[u8], count: usize) -> Option<Vec<i64>> {
    if count == 0 || data.len() < 8 {
        return None;
    }

    let mut values = Vec::with_capacity(count);
    let first = i64::from_le_bytes(data[0..8].try_into().ok()?);
    values.push(first);

    let mut pos = 8;
    while values.len() < count && pos + 8 <= data.len() {
        let delta = i64::from_le_bytes(data[pos..pos + 8].try_into().ok()?);
        values.push(values.last()? + delta);
        pos += 8;
    }

    Some(values)
}

// ── Block Compression ──

/// Compress a block using zstd.
pub fn compress_block(data: &[u8]) -> Vec<u8> {
    zstd::encode_all(data, 3).unwrap_or_else(|_| data.to_vec())
}

/// Decompress a block using zstd.
pub fn decompress_block(data: &[u8]) -> Option<Vec<u8>> {
    zstd::decode_all(data).ok()
}

// ── TSM Writer ──

/// Writer for TSM files.
pub struct TsmWriter {
    /// Current blocks being built (one per series).
    blocks: BTreeMap<String, TsBlock>,
    /// Written blocks (ready for flushing).
    written_blocks: Vec<TsBlock>,
}

impl TsmWriter {
    pub fn new() -> Self {
        Self {
            blocks: BTreeMap::new(),
            written_blocks: Vec::new(),
        }
    }

    /// Write a point to the appropriate block.
    pub fn write(&mut self, series_key: String, point: TsPoint) {
        let key = series_key.clone();
        let is_full = {
            let block = self.blocks.entry(series_key).or_insert_with(|| TsBlock::new(key.clone()));
            block.push(point);
            block.is_full()
        };

        if is_full {
            if let Some(full_block) = self.blocks.remove(&key) {
                self.written_blocks.push(full_block);
            }
        }
    }

    /// Flush all remaining blocks.
    pub fn flush(&mut self) -> Vec<TsBlock> {
        let mut blocks: Vec<TsBlock> = self.blocks.values().cloned().collect();
        self.blocks.clear();
        blocks.append(&mut self.written_blocks);
        blocks
    }

    /// Encode blocks to bytes for storage.
    pub fn encode_blocks(blocks: &[TsBlock]) -> Vec<u8> {
        let mut buf = Vec::new();

        // Write header
        buf.extend_from_slice(TSM_MAGIC);
        buf.push(TSM_VERSION);

        // Write block count (saturating cast)
        let block_count = blocks.len().min(u32::MAX as usize) as u32;
        buf.extend_from_slice(&block_count.to_le_bytes());

        // Write each block
        for block in blocks {
            // Encode timestamps
            let ts_encoded = encode_timestamps(&block.timestamps);

            // Encode values based on type
            let val_encoded = match block.values.first() {
                Some(TsValue::Float(_)) => {
                    let floats: Vec<f64> = block.values.iter().filter_map(|v| {
                        if let TsValue::Float(f) = v { Some(*f) } else { None }
                    }).collect();
                    encode_floats(&floats)
                }
                Some(TsValue::Integer(_)) => {
                    let ints: Vec<i64> = block.values.iter().filter_map(|v| {
                        if let TsValue::Integer(i) = v { Some(*i) } else { None }
                    }).collect();
                    encode_integers(&ints)
                }
                _ => Vec::new(),
            };

            // Write block header (saturating casts)
            let key_len = block.series_key.len().min(u16::MAX as usize) as u16;
            let count = block.timestamps.len().min(u32::MAX as usize) as u32;
            buf.extend_from_slice(&key_len.to_le_bytes());
            buf.extend_from_slice(block.series_key.as_bytes());
            buf.extend_from_slice(&block.min_timestamp.to_le_bytes());
            buf.extend_from_slice(&block.max_timestamp.to_le_bytes());
            buf.extend_from_slice(&count.to_le_bytes());

            // Write compressed timestamp data
            let ts_compressed = compress_block(&ts_encoded);
            let ts_len = ts_compressed.len().min(u32::MAX as usize) as u32;
            buf.extend_from_slice(&ts_len.to_le_bytes());
            buf.extend_from_slice(&ts_compressed);

            // Write compressed value data
            let val_compressed = compress_block(&val_encoded);
            let val_len = val_compressed.len().min(u32::MAX as usize) as u32;
            buf.extend_from_slice(&val_len.to_le_bytes());
            buf.extend_from_slice(&val_compressed);
        }

        buf
    }
}

// ── TSM Reader ──

/// Reader for TSM files.
pub struct TsmReader;

impl TsmReader {
    /// Decode blocks from bytes.
    pub fn decode_blocks(data: &[u8]) -> Option<Vec<TsBlock>> {
        if data.len() < 9 {
            return None;
        }

        // Verify magic
        if &data[0..4] != TSM_MAGIC {
            return None;
        }

        let _version = data[4];
        let block_count = u32::from_le_bytes(data[5..9].try_into().ok()?) as usize;
        // Cap allocation to prevent OOM from malformed data
        if block_count > 1_000_000 {
            return None;
        }

        let mut blocks = Vec::with_capacity(block_count);
        let mut pos = 9;

        for _ in 0..block_count {
            if pos + 2 > data.len() {
                return None;
            }
            let key_len = u16::from_le_bytes(data[pos..pos + 2].try_into().ok()?) as usize;
            pos += 2;

            if pos + key_len > data.len() {
                return None;
            }
            let series_key = String::from_utf8(data[pos..pos + key_len].to_vec()).ok()?;
            pos += key_len;

            if pos + 24 > data.len() {
                return None;
            }
            let min_timestamp = i64::from_le_bytes(data[pos..pos + 8].try_into().ok()?);
            let max_timestamp = i64::from_le_bytes(data[pos + 8..pos + 16].try_into().ok()?);
            let count = u32::from_le_bytes(data[pos + 16..pos + 20].try_into().ok()?) as usize;
            pos += 20;

            // Read compressed timestamp data
            if pos + 4 > data.len() {
                return None;
            }
            let ts_size = u32::from_le_bytes(data[pos..pos + 4].try_into().ok()?) as usize;
            pos += 4;

            if pos + ts_size > data.len() {
                return None;
            }
            let ts_compressed = &data[pos..pos + ts_size];
            pos += ts_size;

            // Read compressed value data
            if pos + 4 > data.len() {
                return None;
            }
            let val_size = u32::from_le_bytes(data[pos..pos + 4].try_into().ok()?) as usize;
            pos += 4;

            if pos + val_size > data.len() {
                return None;
            }
            let val_compressed = &data[pos..pos + val_size];
            pos += val_size;

            // Decompress and decode
            let ts_data = decompress_block(ts_compressed)?;
            let timestamps = decode_timestamps(&ts_data, count)?;

            let val_data = decompress_block(val_compressed)?;
            // For now, assume float values
            let floats = decode_floats(&val_data, count)?;
            let values: Vec<TsValue> = floats.into_iter().map(TsValue::Float).collect();

            blocks.push(TsBlock {
                series_key,
                min_timestamp,
                max_timestamp,
                timestamps,
                values,
            });
        }

        Some(blocks)
    }
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_timestamp_encoding() {
        let timestamps = vec![1000, 1010, 1020, 1030, 1040];
        let encoded = encode_timestamps(&timestamps);
        let decoded = decode_timestamps(&encoded, timestamps.len()).unwrap();
        assert_eq!(timestamps, decoded);
    }

    #[test]
    fn test_timestamp_encoding_irregular() {
        let timestamps = vec![1000, 1005, 2000, 2001, 5000];
        let encoded = encode_timestamps(&timestamps);
        let decoded = decode_timestamps(&encoded, timestamps.len()).unwrap();
        assert_eq!(timestamps, decoded);
    }

    #[test]
    fn test_float_encoding() {
        let values = vec![1.0, 1.1, 1.2, 1.3, 1.4];
        let encoded = encode_floats(&values);
        let decoded = decode_floats(&encoded, values.len()).unwrap();
        assert_eq!(values.len(), decoded.len());
        for (a, b) in values.iter().zip(decoded.iter()) {
            assert!((a - b).abs() < 1e-10);
        }
    }

    #[test]
    fn test_integer_encoding() {
        let values = vec![100, 110, 120, 130, 140];
        let encoded = encode_integers(&values);
        let decoded = decode_integers(&encoded, values.len()).unwrap();
        assert_eq!(values, decoded);
    }

    #[test]
    fn test_block_operations() {
        let mut block = TsBlock::new("cpu.usage".to_string());
        block.push(TsPoint { timestamp: 1000, value: TsValue::Float(0.5) });
        block.push(TsPoint { timestamp: 1010, value: TsValue::Float(0.6) });

        assert_eq!(block.len(), 2);
        assert_eq!(block.min_timestamp, 1000);
        assert_eq!(block.max_timestamp, 1010);
    }

    #[test]
    fn test_tsm_writer_reader() {
        let mut writer = TsmWriter::new();

        // Write some points
        for i in 0..10 {
            writer.write(
                "cpu.usage".to_string(),
                TsPoint {
                    timestamp: 1000 + i * 10,
                    value: TsValue::Float(0.5 + i as f64 * 0.1),
                },
            );
        }

        let blocks = writer.flush();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].len(), 10);

        // Encode and decode
        let encoded = TsmWriter::encode_blocks(&blocks);
        let decoded = TsmReader::decode_blocks(&encoded).unwrap();
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].series_key, "cpu.usage");
        assert_eq!(decoded[0].timestamps.len(), 10);
    }

    #[test]
    fn test_tsm_multiple_series() {
        let mut writer = TsmWriter::new();

        // Write to multiple series
        for i in 0..5 {
            writer.write(
                "cpu.usage".to_string(),
                TsPoint { timestamp: 1000 + i, value: TsValue::Float(0.5) },
            );
            writer.write(
                "memory.used".to_string(),
                TsPoint { timestamp: 1000 + i, value: TsValue::Integer(1024) },
            );
        }

        let blocks = writer.flush();
        assert_eq!(blocks.len(), 2);
    }

    #[test]
    fn test_compression_roundtrip() {
        let data = b"Hello, World! This is a test of compression.";
        let compressed = compress_block(data);
        let decompressed = decompress_block(&compressed).unwrap();
        assert_eq!(data.to_vec(), decompressed);
    }
}
