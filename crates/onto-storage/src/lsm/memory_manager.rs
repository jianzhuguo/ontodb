//! Dynamic memory manager for OntoDB storage engine.
//!
//! Provides:
//! - Memory usage tracking across components
//! - Dynamic MemTable size adjustment based on write rate
//! - Adaptive Block Cache sizing based on hit rate
//! - Memory pressure detection and response

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;
use parking_lot::RwLock;

/// Memory manager configuration.
#[derive(Debug, Clone)]
pub struct MemoryManagerConfig {
    /// Minimum MemTable size in bytes.
    pub min_memtable_size: usize,
    /// Maximum MemTable size in bytes.
    pub max_memtable_size: usize,
    /// Minimum Block Cache size in bytes.
    pub min_block_cache_size: usize,
    /// Maximum Block Cache size in bytes.
    pub max_block_cache_size: usize,
    /// Memory pressure threshold (0.0 - 1.0).
    /// When system memory usage exceeds this, trigger shrinking.
    pub pressure_threshold: f64,
    /// Adjustment interval in seconds.
    pub adjustment_interval_secs: u64,
}

impl Default for MemoryManagerConfig {
    fn default() -> Self {
        Self {
            min_memtable_size: 4 * 1024 * 1024,      // 4 MB
            max_memtable_size: 256 * 1024 * 1024,     // 256 MB
            min_block_cache_size: 16 * 1024 * 1024,   // 16 MB
            max_block_cache_size: 1024 * 1024 * 1024,  // 1 GB
            pressure_threshold: 0.85,
            adjustment_interval_secs: 30,
        }
    }
}

/// Component memory statistics.
#[derive(Debug, Clone, Default)]
pub struct MemoryStats {
    /// Current MemTable size in bytes.
    pub memtable_size: usize,
    /// Current Block Cache size in bytes.
    pub block_cache_size: usize,
    /// Current Block Cache entries.
    pub block_cache_entries: usize,
    /// Block Cache hit rate (0.0 - 1.0).
    pub block_cache_hit_rate: f64,
    /// WAL buffer size in bytes.
    pub wal_buffer_size: usize,
    /// Total tracked memory in bytes.
    pub total_tracked: usize,
    /// System memory usage estimate.
    pub system_memory_usage: f64,
}

/// Write rate tracker for adaptive sizing.
struct WriteRateTracker {
    /// Recent write counts per interval.
    write_counts: Vec<u64>,
    /// Current interval start.
    interval_start: Instant,
    /// Current interval count.
    current_count: u64,
    /// Maximum intervals to track.
    max_intervals: usize,
}

impl WriteRateTracker {
    fn new(max_intervals: usize) -> Self {
        Self {
            write_counts: Vec::with_capacity(max_intervals),
            interval_start: Instant::now(),
            current_count: 0,
            max_intervals,
        }
    }

    fn record_write(&mut self) {
        self.current_count += 1;
    }

    fn maybe_advance(&mut self, interval_secs: u64) {
        if self.interval_start.elapsed().as_secs() >= interval_secs {
            if self.write_counts.len() >= self.max_intervals {
                self.write_counts.remove(0);
            }
            self.write_counts.push(self.current_count);
            self.current_count = 0;
            self.interval_start = Instant::now();
        }
    }

    fn writes_per_second(&self) -> f64 {
        if self.write_counts.is_empty() {
            return 0.0;
        }
        let total: u64 = self.write_counts.iter().sum();
        let intervals = self.write_counts.len() as f64;
        total as f64 / intervals
    }
}

/// Dynamic memory manager.
pub struct MemoryManager {
    config: MemoryManagerConfig,
    /// Initial MemTable size (from options).
    initial_memtable_size: usize,
    /// Current MemTable size limit.
    memtable_size: AtomicUsize,
    /// Current Block Cache size limit.
    block_cache_size: AtomicUsize,
    /// Write rate tracker.
    write_tracker: RwLock<WriteRateTracker>,
    /// Memory stats.
    stats: RwLock<MemoryStats>,
    /// Last adjustment time.
    last_adjustment: RwLock<Instant>,
    /// Cache hit counter.
    cache_hits: AtomicU64,
    /// Cache miss counter.
    cache_misses: AtomicU64,
    /// Atomic write counter for minimal overhead.
    write_counter: AtomicU64,
}

impl MemoryManager {
    /// Create a new memory manager.
    pub fn new(config: MemoryManagerConfig) -> Self {
        let initial_memtable = (config.min_memtable_size + config.max_memtable_size) / 2;
        let initial_cache = (config.min_block_cache_size + config.max_block_cache_size) / 2;
        Self::with_initial_sizes(config, initial_memtable, initial_cache)
    }

    /// Create with specific initial sizes (from StorageOptions).
    pub fn with_initial_sizes(config: MemoryManagerConfig, memtable_size: usize, cache_size: usize) -> Self {
        Self {
            initial_memtable_size: memtable_size,
            config,
            memtable_size: AtomicUsize::new(memtable_size),
            block_cache_size: AtomicUsize::new(cache_size),
            write_tracker: RwLock::new(WriteRateTracker::new(10)),
            stats: RwLock::new(MemoryStats::default()),
            last_adjustment: RwLock::new(Instant::now()),
            cache_hits: AtomicU64::new(0),
            cache_misses: AtomicU64::new(0),
            write_counter: AtomicU64::new(0),
        }
    }

    /// Get current MemTable size limit.
    pub fn memtable_size(&self) -> usize {
        self.memtable_size.load(Ordering::Relaxed)
    }

    /// Get current Block Cache size limit.
    pub fn block_cache_size(&self) -> usize {
        self.block_cache_size.load(Ordering::Relaxed)
    }

    /// Record a write operation.
    /// Uses atomic counter for minimal overhead.
    pub fn record_write(&self) {
        self.write_counter.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a cache hit.
    pub fn record_cache_hit(&self) {
        self.cache_hits.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a cache miss.
    pub fn record_cache_miss(&self) {
        self.cache_misses.fetch_add(1, Ordering::Relaxed);
    }

    /// Get cache hit rate.
    pub fn cache_hit_rate(&self) -> f64 {
        let hits = self.cache_hits.load(Ordering::Relaxed);
        let misses = self.cache_misses.load(Ordering::Relaxed);
        let total = hits + misses;
        if total == 0 { 0.0 } else { hits as f64 / total as f64 }
    }

    /// Periodic adjustment based on workload.
    /// Should be called from a background thread.
    pub fn adjust(&self) {
        let mut last = self.last_adjustment.write();
        if last.elapsed().as_secs() < self.config.adjustment_interval_secs {
            return;
        }
        *last = Instant::now();

        // Advance write tracker
        let mut tracker = self.write_tracker.write();
        tracker.maybe_advance(self.config.adjustment_interval_secs);
        let wps = tracker.writes_per_second();
        drop(tracker);

        // Adjust MemTable size based on write rate
        let current_memtable = self.memtable_size.load(Ordering::Relaxed);
        let new_memtable = if wps > 100_000.0 {
            // High write rate: increase MemTable to reduce flush frequency
            (current_memtable * 2).min(self.config.max_memtable_size)
        } else if wps < 10_000.0 {
            // Low write rate: decrease MemTable to save memory
            (current_memtable / 2).max(self.config.min_memtable_size)
        } else {
            current_memtable
        };
        self.memtable_size.store(new_memtable, Ordering::Relaxed);

        // Adjust Block Cache size based on hit rate
        let hit_rate = self.cache_hit_rate();
        let current_cache = self.block_cache_size.load(Ordering::Relaxed);
        let new_cache = if hit_rate > 0.9 {
            // High hit rate: increase cache for better performance
            (current_cache * 3 / 2).min(self.config.max_block_cache_size)
        } else if hit_rate < 0.5 {
            // Low hit rate: decrease cache, data is too large
            (current_cache * 3 / 4).max(self.config.min_block_cache_size)
        } else {
            current_cache
        };
        self.block_cache_size.store(new_cache, Ordering::Relaxed);

        // Update stats
        let mut stats = self.stats.write();
        stats.memtable_size = new_memtable;
        stats.block_cache_size = new_cache;
        stats.block_cache_hit_rate = hit_rate;
    }

    /// Respond to memory pressure (called when system memory is low).
    pub fn shrink(&self) {
        // Shrink MemTable to minimum
        self.memtable_size.store(self.config.min_memtable_size, Ordering::Relaxed);
        // Shrink Block Cache to minimum
        self.block_cache_size.store(self.config.min_block_cache_size, Ordering::Relaxed);
    }

    /// Get current memory stats.
    pub fn stats(&self) -> MemoryStats {
        self.stats.read().clone()
    }

    /// Check if memory pressure is high.
    pub fn is_under_pressure(&self) -> bool {
        let stats = self.stats.read();
        stats.system_memory_usage > self.config.pressure_threshold
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_memory_manager_new() {
        let config = MemoryManagerConfig::default();
        let mm = MemoryManager::new(config);
        assert!(mm.memtable_size() > 0);
        assert!(mm.block_cache_size() > 0);
    }

    #[test]
    fn test_cache_hit_rate() {
        let config = MemoryManagerConfig::default();
        let mm = MemoryManager::new(config);
        assert_eq!(mm.cache_hit_rate(), 0.0);

        mm.record_cache_hit();
        mm.record_cache_hit();
        mm.record_cache_miss();
        assert!((mm.cache_hit_rate() - 0.666).abs() < 0.01);
    }

    #[test]
    fn test_adjust_high_write_rate() {
        let config = MemoryManagerConfig {
            adjustment_interval_secs: 0, // Immediate adjustment
            ..Default::default()
        };
        let mm = MemoryManager::new(config);
        let initial = mm.memtable_size();

        // Simulate high write rate
        {
            let mut tracker = mm.write_tracker.write();
            tracker.write_counts = vec![200_000; 10]; // 200K writes per interval
        }

        mm.adjust();
        assert!(mm.memtable_size() > initial);
    }

    #[test]
    fn test_adjust_low_write_rate() {
        let config = MemoryManagerConfig {
            adjustment_interval_secs: 0,
            ..Default::default()
        };
        let mm = MemoryManager::new(config);
        let initial = mm.memtable_size();

        // Simulate low write rate
        {
            let mut tracker = mm.write_tracker.write();
            tracker.write_counts = vec![100; 10]; // 100 writes per interval
        }

        mm.adjust();
        assert!(mm.memtable_size() < initial);
    }

    #[test]
    fn test_shrink() {
        let config = MemoryManagerConfig::default();
        let min_memtable = config.min_memtable_size;
        let min_cache = config.min_block_cache_size;
        let mm = MemoryManager::new(config);
        mm.shrink();
        assert_eq!(mm.memtable_size(), min_memtable);
        assert_eq!(mm.block_cache_size(), min_cache);
    }
}
