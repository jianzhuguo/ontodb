// Copyright (c) 2024-2026 OntoDB Team
// Licensed under the Business Source License 1.1 (BUSL-1.1).
// See LICENSE for details. Change Date: 2031-09-15.
// On the Change Date, this file will be licensed under Apache License 2.0.
//! Block cache for SSTable data blocks.
//!
//! Caches decompressed data blocks in memory using LRU eviction.
//! Each SSTable owns a BlockCache to avoid cross-file contention.
//! Cache key is the block offset within the file.

use std::collections::HashMap;

/// A cached data block entry.
struct CacheEntry {
    /// The decompressed block data.
    data: Vec<u8>,
    /// Monotonic access counter for LRU eviction.
    access_counter: u64,
}

/// LRU block cache for SSTable data blocks.
///
/// Caches decompressed blocks keyed by their offset in the SSTable file.
/// Uses a monotonic counter for O(1) LRU touch and O(n) eviction
/// (n = cache capacity, typically small ~64-256 blocks).
///
/// Supports prefetching adjacent blocks for sequential access patterns.
/// Tracks cache hit/miss statistics for monitoring.
pub struct BlockCache {
    /// Cached blocks keyed by block offset.
    entries: HashMap<u64, CacheEntry>,
    /// Maximum number of blocks to cache.
    capacity: usize,
    /// Monotonic counter for LRU tracking.
    counter: u64,
    /// Prefetch window size (number of adjacent blocks to prefetch).
    prefetch_window: usize,
    /// Number of cache hits.
    hits: u64,
    /// Number of cache misses.
    misses: u64,
}

/// Cache statistics for monitoring.
#[derive(Debug, Clone)]
pub struct CacheStats {
    /// Current number of cached entries.
    pub size: usize,
    /// Maximum capacity.
    pub capacity: usize,
    /// Number of cache hits.
    pub hits: u64,
    /// Number of cache misses.
    pub misses: u64,
    /// Hit rate (0.0 to 1.0).
    pub hit_rate: f64,
}

impl BlockCache {
    /// Creates a new block cache with the given capacity.
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: HashMap::with_capacity(capacity),
            capacity,
            counter: 0,
            prefetch_window: 2, // Prefetch 2 adjacent blocks
            hits: 0,
            misses: 0,
        }
    }

    /// Sets the prefetch window size.
    pub fn set_prefetch_window(&mut self, window: usize) {
        self.prefetch_window = window;
    }

    /// Gets a cached block by offset. Returns None on cache miss.
    pub fn get(&mut self, offset: u64) -> Option<&[u8]> {
        self.counter += 1;
        if let Some(entry) = self.entries.get_mut(&offset) {
            entry.access_counter = self.counter;
            self.hits += 1;
            Some(&entry.data)
        } else {
            self.misses += 1;
            None
        }
    }

    /// Inserts a block into the cache. May evict the LRU entry if full.
    pub fn put(&mut self, offset: u64, data: Vec<u8>) {
        self.counter += 1;

        // If at capacity, evict the least recently used entry
        if self.entries.len() >= self.capacity && !self.entries.contains_key(&offset) {
            self.evict_lru();
        }

        self.entries.insert(
            offset,
            CacheEntry {
                data,
                access_counter: self.counter,
            },
        );
    }

    /// Returns cache statistics for monitoring.
    pub fn stats(&self) -> CacheStats {
        let total = self.hits + self.misses;
        CacheStats {
            size: self.entries.len(),
            capacity: self.capacity,
            hits: self.hits,
            misses: self.misses,
            hit_rate: if total > 0 {
                self.hits as f64 / total as f64
            } else {
                0.0
            },
        }
    }

    /// Resets hit/miss counters.
    pub fn reset_stats(&mut self) {
        self.hits = 0;
        self.misses = 0;
    }

    /// Evicts the least recently used entry.
    fn evict_lru(&mut self) {
        if let Some((&lru_offset, _)) = self
            .entries
            .iter()
            .min_by_key(|(_, entry)| entry.access_counter)
        {
            self.entries.remove(&lru_offset);
        }
    }

    /// Returns the current number of cached entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns offsets that should be prefetched based on a cache miss.
    /// When a block at `offset` is accessed and not in cache, this returns
    /// the offsets of adjacent blocks that are likely to be accessed next.
    pub fn prefetch_offsets(&self, offset: u64, block_size: u64, file_size: u64) -> Vec<u64> {
        let mut offsets = Vec::new();
        for i in 1..=self.prefetch_window as u64 {
            let next_offset = offset + i * block_size;
            if next_offset < file_size && !self.entries.contains_key(&next_offset) {
                offsets.push(next_offset);
            }
        }
        offsets
    }

    /// Returns true if the given offset is in cache.
    pub fn contains(&self, offset: u64) -> bool {
        self.entries.contains_key(&offset)
    }

    /// Returns true if the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Clears all cached entries.
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_block_cache_basic() {
        let mut cache = BlockCache::new(3);

        cache.put(0, vec![1, 2, 3]);
        cache.put(1, vec![4, 5, 6]);
        cache.put(2, vec![7, 8, 9]);

        assert_eq!(cache.get(0), Some(vec![1, 2, 3].as_slice()));
        assert_eq!(cache.get(1), Some(vec![4, 5, 6].as_slice()));
        assert_eq!(cache.get(2), Some(vec![7, 8, 9].as_slice()));
        assert_eq!(cache.get(3), None);
    }

    #[test]
    fn test_block_cache_lru_eviction() {
        let mut cache = BlockCache::new(2);

        cache.put(0, vec![1, 2, 3]);
        cache.put(1, vec![4, 5, 6]);

        // Access offset 0 to make it more recently used
        cache.get(0);

        // Insert offset 2 — should evict offset 1 (LRU)
        cache.put(2, vec![7, 8, 9]);

        assert_eq!(cache.get(0), Some(vec![1, 2, 3].as_slice()));
        assert_eq!(cache.get(1), None); // Evicted
        assert_eq!(cache.get(2), Some(vec![7, 8, 9].as_slice()));
    }

    #[test]
    fn test_block_cache_overwrite() {
        let mut cache = BlockCache::new(2);

        cache.put(0, vec![1, 2, 3]);
        cache.put(0, vec![4, 5, 6]); // Overwrite

        assert_eq!(cache.get(0), Some(vec![4, 5, 6].as_slice()));
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn test_block_cache_clear() {
        let mut cache = BlockCache::new(10);

        for i in 0..5u64 {
            cache.put(i, vec![i as u8]);
        }
        assert_eq!(cache.len(), 5);

        cache.clear();
        assert!(cache.is_empty());
    }
}
