// Copyright (c) 2024-2026 OntoDB Team
// Licensed under the Business Source License 1.1 (BUSL-1.1).
// See LICENSE for details. Change Date: 2031-09-15.
// On the Change Date, this file will be licensed under Apache License 2.0.
//! Query and plan caching for OntoDB.
//!
//! Provides LRU-based caching for query results and execution plans
//! to avoid redundant computation.

use crate::optimizer::ExecutionPlan;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

/// A cached query result with metadata.
#[derive(Debug, Clone)]
pub struct CachedResult {
    /// The cached rows.
    pub rows: Vec<Map<String, Value>>,
    /// When this entry was cached.
    pub cached_at: Instant,
    /// Time-to-live for this entry.
    pub ttl: Duration,
    /// Number of times this cache entry was hit.
    pub hit_count: u64,
    /// Last access time for LRU eviction.
    pub last_accessed: Instant,
}

impl CachedResult {
    /// Check if this cache entry is still valid.
    pub fn is_valid(&self) -> bool {
        self.cached_at.elapsed() < self.ttl
    }
}

/// A cached execution plan.
#[derive(Debug, Clone)]
pub struct CachedPlan {
    /// The cached execution plan.
    pub plan: ExecutionPlan,
    /// When this entry was cached.
    pub cached_at: Instant,
    /// Number of times this cache entry was hit.
    pub hit_count: u64,
    /// Last access time for LRU eviction.
    pub last_accessed: Instant,
}

/// Cache key based on query hash.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CacheKey {
    /// Normalized query string hash.
    pub hash: u64,
    /// Query type (SELECT, INSERT, etc.)
    pub query_type: String,
}

/// LRU cache statistics.
#[derive(Debug, Clone, Default)]
pub struct CacheStats {
    /// Total cache lookups.
    pub lookups: u64,
    /// Cache hits.
    pub hits: u64,
    /// Cache misses.
    pub misses: u64,
    /// Total entries evicted.
    pub evictions: u64,
    /// Current cache size.
    pub size: usize,
}

impl CacheStats {
    /// Calculate hit rate as a percentage.
    pub fn hit_rate(&self) -> f64 {
        if self.lookups == 0 {
            0.0
        } else {
            (self.hits as f64 / self.lookups as f64) * 100.0
        }
    }
}

/// Query result cache with LRU eviction.
///
/// Uses a BTreeMap<counter, hash> for O(log n) eviction (finding the
/// minimum counter is `first_key_value()` on a sorted tree).
pub struct QueryCache {
    /// Cached results indexed by query hash.
    entries: HashMap<u64, CachedResult>,
    /// Sorted access order: counter → hash. Eviction picks the smallest counter.
    access_order: BTreeMap<u64, u64>,
    /// Reverse map: hash → current counter (for updating on access).
    hash_to_counter: HashMap<u64, u64>,
    /// Monotonically increasing counter.
    counter: u64,
    /// Maximum number of entries.
    max_size: usize,
    /// Default TTL for cache entries.
    default_ttl: Duration,
    /// Cache statistics.
    stats: CacheStats,
}

impl QueryCache {
    /// Create a new query cache with the given maximum size and TTL.
    pub fn new(max_size: usize, default_ttl: Duration) -> Self {
        Self {
            entries: HashMap::with_capacity(max_size),
            access_order: BTreeMap::new(),
            hash_to_counter: HashMap::with_capacity(max_size),
            counter: 0,
            max_size,
            default_ttl,
            stats: CacheStats::default(),
        }
    }

    /// Get a cached result for the given query hash.
    pub fn get(&mut self, query_hash: u64) -> Option<Vec<Map<String, Value>>> {
        self.stats.lookups += 1;

        if let Some(entry) = self.entries.get_mut(&query_hash) {
            if entry.is_valid() {
                self.stats.hits += 1;
                entry.last_accessed = Instant::now();
                let rows = entry.rows.clone();
                // Inline touch to avoid double mutable borrow
                if let Some(old_counter) = self.hash_to_counter.remove(&query_hash) {
                    self.access_order.remove(&old_counter);
                }
                self.counter += 1;
                self.access_order.insert(self.counter, query_hash);
                self.hash_to_counter.insert(query_hash, self.counter);
                return Some(rows);
            }
        }

        self.stats.misses += 1;
        None
    }

    /// Insert a query result into the cache.
    pub fn insert(&mut self, query_hash: u64, rows: Vec<Map<String, Value>>) {
        // Evict if at capacity
        if self.entries.len() >= self.max_size {
            self.evict_lru();
        }

        let now = Instant::now();
        let entry = CachedResult {
            rows,
            cached_at: now,
            ttl: self.default_ttl,
            hit_count: 0,
            last_accessed: now,
        };

        self.entries.insert(query_hash, entry);
        self.touch(query_hash);
        self.stats.size = self.entries.len();
    }

    /// Insert a query result with custom TTL.
    pub fn insert_with_ttl(
        &mut self,
        query_hash: u64,
        rows: Vec<Map<String, Value>>,
        ttl: Duration,
    ) {
        if self.entries.len() >= self.max_size {
            self.evict_lru();
        }

        let now = Instant::now();
        let entry = CachedResult {
            rows,
            cached_at: now,
            ttl,
            hit_count: 0,
            last_accessed: now,
        };

        self.entries.insert(query_hash, entry);
        self.touch(query_hash);
        self.stats.size = self.entries.len();
    }

    /// Invalidate cache entries related to a table.
    /// This is called when a write operation occurs on the table.
    pub fn invalidate_table(&mut self, _table: &str) {
        // For simplicity, clear all cache on any write
        // In production, we'd track which queries touch which tables
        self.clear();
    }

    /// Clear all cache entries.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.access_order.clear();
        self.hash_to_counter.clear();
        self.stats.size = 0;
    }

    /// Get cache statistics.
    pub fn stats(&self) -> &CacheStats {
        &self.stats
    }

    /// Evict the least recently used entry. O(log n) via BTreeMap.
    fn evict_lru(&mut self) {
        // The smallest counter in the BTreeMap is the LRU entry
        if let Some((&_counter, &victim_hash)) = self.access_order.first_key_value() {
            self.access_order.pop_first();
            self.hash_to_counter.remove(&victim_hash);
            if self.entries.remove(&victim_hash).is_some() {
                self.stats.evictions += 1;
                self.stats.size = self.entries.len();
            }
        }
    }

    /// Records a page access. O(log n) operation.
    fn touch(&mut self, hash: u64) {
        // Remove old counter from BTreeMap if present
        if let Some(old_counter) = self.hash_to_counter.remove(&hash) {
            self.access_order.remove(&old_counter);
        }
        self.counter += 1;
        self.access_order.insert(self.counter, hash);
        self.hash_to_counter.insert(hash, self.counter);
    }
}

/// Execution plan cache with O(log n) LRU eviction.
///
/// Uses a BTreeMap<counter, hash> for O(log n) eviction.
pub struct PlanCache {
    /// Cached plans indexed by normalized query hash.
    entries: HashMap<u64, CachedPlan>,
    /// Sorted access order: counter → hash.
    access_order: BTreeMap<u64, u64>,
    /// Reverse map: hash → current counter.
    hash_to_counter: HashMap<u64, u64>,
    /// Monotonically increasing counter.
    counter: u64,
    /// Maximum number of entries.
    max_size: usize,
    /// Cache statistics.
    stats: CacheStats,
}

impl PlanCache {
    /// Create a new plan cache.
    pub fn new(max_size: usize) -> Self {
        Self {
            entries: HashMap::with_capacity(max_size),
            access_order: BTreeMap::new(),
            hash_to_counter: HashMap::with_capacity(max_size),
            counter: 0,
            max_size,
            stats: CacheStats::default(),
        }
    }

    /// Get a cached plan for the given query hash.
    pub fn get(&mut self, query_hash: u64) -> Option<ExecutionPlan> {
        self.stats.lookups += 1;

        if let Some(entry) = self.entries.get_mut(&query_hash) {
            self.stats.hits += 1;
            entry.last_accessed = Instant::now();
            let plan = entry.plan.clone();
            // Inline touch to avoid double mutable borrow
            if let Some(old_counter) = self.hash_to_counter.remove(&query_hash) {
                self.access_order.remove(&old_counter);
            }
            self.counter += 1;
            self.access_order.insert(self.counter, query_hash);
            self.hash_to_counter.insert(query_hash, self.counter);
            return Some(plan);
        }

        self.stats.misses += 1;
        None
    }

    /// Insert a plan into the cache.
    pub fn insert(&mut self, query_hash: u64, plan: ExecutionPlan) {
        if self.entries.len() >= self.max_size {
            self.evict_lru();
        }

        let now = Instant::now();
        let entry = CachedPlan {
            plan,
            cached_at: now,
            hit_count: 0,
            last_accessed: now,
        };

        self.entries.insert(query_hash, entry);
        self.touch(query_hash);
        self.stats.size = self.entries.len();
    }

    /// Clear all cached plans.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.access_order.clear();
        self.hash_to_counter.clear();
        self.stats.size = 0;
    }

    /// Get cache statistics.
    pub fn stats(&self) -> &CacheStats {
        &self.stats
    }

    /// Evict the least recently used entry. O(log n) via BTreeMap.
    fn evict_lru(&mut self) {
        if let Some((&_counter, &victim_hash)) = self.access_order.first_key_value() {
            self.access_order.pop_first();
            self.hash_to_counter.remove(&victim_hash);
            if self.entries.remove(&victim_hash).is_some() {
                self.stats.evictions += 1;
                self.stats.size = self.entries.len();
            }
        }
    }

    /// Records a page access. O(log n) operation.
    fn touch(&mut self, hash: u64) {
        // Remove old counter from BTreeMap if present
        if let Some(old_counter) = self.hash_to_counter.remove(&hash) {
            self.access_order.remove(&old_counter);
        }
        self.counter += 1;
        self.access_order.insert(self.counter, hash);
        self.hash_to_counter.insert(hash, self.counter);
    }
}

/// Compute a hash for a query string.
pub fn hash_query(query: &str) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    let mut hasher = DefaultHasher::new();
    query.to_lowercase().trim().hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_query_cache_basic() {
        let mut cache = QueryCache::new(10, Duration::from_secs(60));
        let hash = hash_query("SELECT * FROM Product");

        // Cache miss
        assert!(cache.get(hash).is_none());

        // Insert
        let rows = vec![Map::new()];
        cache.insert(hash, rows.clone());

        // Cache hit
        let result = cache.get(hash);
        assert!(result.is_some());
        assert_eq!(result.unwrap().len(), 1);
    }

    #[test]
    fn test_query_cache_eviction() {
        let mut cache = QueryCache::new(2, Duration::from_secs(60));

        cache.insert(1, vec![]);
        cache.insert(2, vec![]);
        cache.insert(3, vec![]); // Should evict 1

        assert!(cache.get(1).is_none());
        assert!(cache.get(2).is_some());
        assert!(cache.get(3).is_some());
    }

    #[test]
    fn test_plan_cache() {
        let mut cache = PlanCache::new(10);
        let hash = hash_query("SELECT * FROM Product");

        // Cache miss
        assert!(cache.get(hash).is_none());

        // Would insert a plan here in real usage
        // cache.insert(hash, plan);
    }
}
