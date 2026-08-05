//! Storage engine configuration options.

use std::path::PathBuf;

/// Configuration for the LSM-Tree storage engine.
#[derive(Debug, Clone)]
pub struct StorageOptions {
    /// Root directory for all data files.
    pub data_dir: PathBuf,

    /// Maximum size of the MemTable before flushing to SSTable (bytes).
    pub memtable_size_limit: usize,

    /// Size of each SSTable data block (bytes).
    pub block_size: usize,

    /// Number of levels in the LSM-Tree.
    pub num_levels: usize,

    /// Size ratio between levels (level N is `size_ratio` times larger than level N-1).
    pub size_ratio: usize,

    /// Whether to use bloom filters for point lookups.
    pub use_bloom_filter: bool,

    /// False positive rate for bloom filters.
    pub bloom_filter_fp_rate: f64,

    /// Whether to fsync WAL after each transaction commit.
    /// When true, committed data survives OS crashes (strongest durability).
    /// When false, only process crashes are survived (better performance).
    pub sync_wal_on_commit: bool,
}

impl Default for StorageOptions {
    fn default() -> Self {
        Self {
            data_dir: PathBuf::from("./ontodb_data"),
            memtable_size_limit: 4 * 1024 * 1024, // 4MB
            block_size: 4096,                       // 4KB
            num_levels: 7,
            size_ratio: 10,
            use_bloom_filter: true,
            bloom_filter_fp_rate: 0.01,
            sync_wal_on_commit: true, // Strong durability by default
        }
    }
}
