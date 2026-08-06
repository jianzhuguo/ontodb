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
pub struct BlockCache {
    /// Cached blocks keyed by block offset.
    entries: HashMap<u64, CacheEntry>,
    /// Maximum number of blocks to cache.
    capacity: usize,
    /// Monotonic counter for LRU tracking.
    counter: u64,
}

impl BlockCache {
    /// Creates a new block cache with the given capacity.
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: HashMap::with_capacity(capacity),
            capacity,
            counter: 0,
        }
    }

    /// Gets a cached block by offset. Returns None on cache miss.
    pub fn get(&mut self, offset: u64) -> Option<&[u8]> {
        self.counter += 1;
        if let Some(entry) = self.entries.get_mut(&offset) {
            entry.access_counter = self.counter;
            Some(&entry.data)
        } else {
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
