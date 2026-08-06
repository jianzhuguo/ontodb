//! Query and plan caching for OntoDB.
//!
//! Provides LRU-based caching for query results and execution plans
//! to avoid redundant computation.

use crate::optimizer::ExecutionPlan;
use crate::parser::QueryAst;
use serde_json::{Map, Value};
use std::collections::HashMap;
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
pub struct QueryCache {
    /// Cached results indexed by query hash.
    entries: HashMap<u64, CachedResult>,
    /// Maximum number of entries.
    max_size: usize,
    /// Default TTL for cache entries.
    default_ttl: Duration,
    /// Cache statistics.
    stats: CacheStats,
    /// Access order for LRU eviction.
    access_order: Vec<u64>,
}

impl QueryCache {
    /// Create a new query cache with the given maximum size and TTL.
    pub fn new(max_size: usize, default_ttl: Duration) -> Self {
        Self {
            entries: HashMap::with_capacity(max_size),
            max_size,
            default_ttl,
            stats: CacheStats::default(),
            access_order: Vec::with_capacity(max_size),
        }
    }

    /// Get a cached result for the given query hash.
    pub fn get(&mut self, query_hash: u64) -> Option<Vec<Map<String, Value>>> {
        self.stats.lookups += 1;

        if let Some(entry) = self.entries.get(&query_hash) {
            if entry.is_valid() {
                self.stats.hits += 1;
                // Update access order
                self.access_order.retain(|&h| h != query_hash);
                self.access_order.push(query_hash);
                return Some(entry.rows.clone());
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

        let entry = CachedResult {
            rows,
            cached_at: Instant::now(),
            ttl: self.default_ttl,
            hit_count: 0,
        };

        self.entries.insert(query_hash, entry);
        self.access_order.push(query_hash);
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

        let entry = CachedResult {
            rows,
            cached_at: Instant::now(),
            ttl,
            hit_count: 0,
        };

        self.entries.insert(query_hash, entry);
        self.access_order.push(query_hash);
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
        self.stats.size = 0;
    }

    /// Get cache statistics.
    pub fn stats(&self) -> &CacheStats {
        &self.stats
    }

    /// Evict the least recently used entry.
    fn evict_lru(&mut self) {
        if let Some(oldest_hash) = self.access_order.first().cloned() {
            self.entries.remove(&oldest_hash);
            self.access_order.retain(|&h| h != oldest_hash);
            self.stats.evictions += 1;
            self.stats.size = self.entries.len();
        }
    }
}

/// Execution plan cache.
pub struct PlanCache {
    /// Cached plans indexed by normalized query hash.
    entries: HashMap<u64, CachedPlan>,
    /// Maximum number of entries.
    max_size: usize,
    /// Cache statistics.
    stats: CacheStats,
    /// Access order for LRU eviction.
    access_order: Vec<u64>,
}

impl PlanCache {
    /// Create a new plan cache.
    pub fn new(max_size: usize) -> Self {
        Self {
            entries: HashMap::with_capacity(max_size),
            max_size,
            stats: CacheStats::default(),
            access_order: Vec::with_capacity(max_size),
        }
    }

    /// Get a cached plan for the given query hash.
    pub fn get(&mut self, query_hash: u64) -> Option<ExecutionPlan> {
        self.stats.lookups += 1;

        if let Some(entry) = self.entries.get(&query_hash) {
            self.stats.hits += 1;
            self.access_order.retain(|&h| h != query_hash);
            self.access_order.push(query_hash);
            return Some(entry.plan.clone());
        }

        self.stats.misses += 1;
        None
    }

    /// Insert a plan into the cache.
    pub fn insert(&mut self, query_hash: u64, plan: ExecutionPlan) {
        if self.entries.len() >= self.max_size {
            self.evict_lru();
        }

        let entry = CachedPlan {
            plan,
            cached_at: Instant::now(),
            hit_count: 0,
        };

        self.entries.insert(query_hash, entry);
        self.access_order.push(query_hash);
        self.stats.size = self.entries.len();
    }

    /// Clear all cached plans.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.access_order.clear();
        self.stats.size = 0;
    }

    /// Get cache statistics.
    pub fn stats(&self) -> &CacheStats {
        &self.stats
    }

    /// Evict the least recently used entry.
    fn evict_lru(&mut self) {
        if let Some(oldest_hash) = self.access_order.first().cloned() {
            self.entries.remove(&oldest_hash);
            self.access_order.retain(|&h| h != oldest_hash);
            self.stats.evictions += 1;
            self.stats.size = self.entries.len();
        }
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
