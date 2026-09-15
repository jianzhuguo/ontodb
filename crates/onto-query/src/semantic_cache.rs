// Copyright (c) 2024-2026 OntoDB Team
// Licensed under the Business Source License 1.1 (BUSL-1.1).
// See LICENSE for details. Change Date: 2031-09-15.

//! Semantic cache — query results cached by semantic similarity, not just text match.
//!
//! Two queries with different text but same semantic meaning hit the same cache entry.
//! Example: "find all sensors" and "list every sensor" are semantically identical.

use std::collections::HashMap;

/// Cache entry with semantic fingerprint.
#[derive(Debug, Clone)]
pub struct SemanticCacheEntry {
    pub query_text: String,
    pub fingerprint: Vec<f32>,
    pub result_summary: String,
    pub hit_count: u64,
    pub created_at: u64,
    pub last_hit_at: u64,
    pub ttl_ms: u64,
}

/// Semantic cache configuration.
#[derive(Debug, Clone)]
pub struct SemanticCacheConfig {
    /// Maximum cache entries.
    pub max_entries: usize,
    /// Similarity threshold for cache hit (0.0-1.0).
    pub similarity_threshold: f32,
    /// Default TTL in milliseconds.
    pub default_ttl_ms: u64,
    /// Enable semantic matching (vs exact text match only).
    pub semantic_matching: bool,
}

impl Default for SemanticCacheConfig {
    fn default() -> Self {
        Self {
            max_entries: 1000,
            similarity_threshold: 0.95,
            default_ttl_ms: 300_000, // 5 minutes
            semantic_matching: true,
        }
    }
}

/// Semantic cache engine.
pub struct SemanticCache {
    entries: HashMap<String, SemanticCacheEntry>,
    config: SemanticCacheConfig,
    hits: u64,
    misses: u64,
}

impl SemanticCache {
    pub fn new(config: SemanticCacheConfig) -> Self {
        Self {
            entries: HashMap::new(),
            config,
            hits: 0,
            misses: 0,
        }
    }

    /// Look up cache by query fingerprint. Returns cached result if semantic match found.
    pub fn lookup(&mut self, query_text: &str, fingerprint: &[f32], now_ms: u64) -> Option<String> {
        // Exact text match first
        if let Some(entry) = self.entries.get_mut(query_text) {
            if now_ms < entry.created_at + entry.ttl_ms {
                entry.hit_count += 1;
                entry.last_hit_at = now_ms;
                self.hits += 1;
                return Some(entry.result_summary.clone());
            }
        }

        // Semantic similarity match
        if self.config.semantic_matching {
            let mut best_match: Option<String> = None;
            let mut best_sim = 0.0f32;

            for (key, entry) in &self.entries {
                if now_ms >= entry.created_at + entry.ttl_ms { continue; }
                let sim = cosine_similarity(fingerprint, &entry.fingerprint);
                if sim >= self.config.similarity_threshold && sim > best_sim {
                    best_sim = sim;
                    best_match = Some(key.clone());
                }
            }

            if let Some(key) = best_match {
                if let Some(entry) = self.entries.get_mut(&key) {
                    entry.hit_count += 1;
                    entry.last_hit_at = now_ms;
                    self.hits += 1;
                    return Some(entry.result_summary.clone());
                }
            }
        }

        self.misses += 1;
        None
    }

    /// Insert a cache entry.
    pub fn insert(&mut self, query_text: &str, fingerprint: Vec<f32>, result_summary: &str, now_ms: u64) {
        // Evict oldest if at capacity
        if self.entries.len() >= self.config.max_entries {
            if let Some(oldest_key) = self.entries.iter()
                .min_by_key(|(_, e)| e.last_hit_at)
                .map(|(k, _)| k.clone())
            {
                self.entries.remove(&oldest_key);
            }
        }

        self.entries.insert(query_text.to_string(), SemanticCacheEntry {
            query_text: query_text.to_string(),
            fingerprint,
            result_summary: result_summary.to_string(),
            hit_count: 0,
            created_at: now_ms,
            last_hit_at: now_ms,
            ttl_ms: self.config.default_ttl_ms,
        });
    }

    /// Invalidate cache entries matching a pattern (e.g., when data changes).
    pub fn invalidate(&mut self, pattern: &str) {
        self.entries.retain(|k, _| !k.contains(pattern));
    }

    /// Clear all entries.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    pub fn stats(&self) -> CacheStats {
        CacheStats {
            entries: self.entries.len(),
            hits: self.hits,
            misses: self.misses,
            hit_rate: if self.hits + self.misses == 0 { 0.0 } else { self.hits as f64 / (self.hits + self.misses) as f64 },
        }
    }

    pub fn entry_count(&self) -> usize { self.entries.len() }
}

#[derive(Debug)]
pub struct CacheStats {
    pub entries: usize,
    pub hits: u64,
    pub misses: u64,
    pub hit_rate: f64,
}

/// Cosine similarity between two vectors.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() { return 0.0; }
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 { 0.0 } else { dot / (norm_a * norm_b) }
}

/// Simple text fingerprint (bag-of-words hash vector).
pub fn text_fingerprint(text: &str) -> Vec<f32> {
    let mut vec = vec![0.0f32; 128];
    for word in text.to_lowercase().split_whitespace() {
        let hash = simple_hash(word) % 128;
        vec[hash] += 1.0;
    }
    // L2 normalize
    let norm: f32 = vec.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 { vec.iter_mut().for_each(|x| *x /= norm); }
    vec
}

fn simple_hash(s: &str) -> usize {
    let mut h: usize = 5381;
    for b in s.bytes() {
        h = h.wrapping_mul(33).wrapping_add(b as usize);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_exact_hit() {
        let mut cache = SemanticCache::new(SemanticCacheConfig::default());
        let fp = text_fingerprint("SELECT * FROM sensors");
        cache.insert("SELECT * FROM sensors", fp.clone(), "10 rows", 1000);

        let result = cache.lookup("SELECT * FROM sensors", &fp, 2000);
        assert_eq!(result, Some("10 rows".to_string()));
        assert_eq!(cache.stats().hits, 1);
    }

    #[test]
    fn test_cache_miss() {
        let mut cache = SemanticCache::new(SemanticCacheConfig::default());
        let fp = text_fingerprint("SELECT * FROM sensors");
        cache.insert("SELECT * FROM sensors", fp, "10 rows", 1000);

        let fp2 = text_fingerprint("DELETE FROM users");
        let result = cache.lookup("DELETE FROM users", &fp2, 2000);
        assert_eq!(result, None);
        assert_eq!(cache.stats().misses, 1);
    }

    #[test]
    fn test_cache_semantic_hit() {
        let config = SemanticCacheConfig {
            similarity_threshold: 0.8,
            semantic_matching: true,
            ..Default::default()
        };
        let mut cache = SemanticCache::new(config);
        let fp1 = text_fingerprint("find all sensors in building A");
        cache.insert("find all sensors in building A", fp1.clone(), "5 rows", 1000);

        // Similar query
        let fp2 = text_fingerprint("list all sensors in building A");
        let result = cache.lookup("list all sensors in building A", &fp2, 2000);
        // Should hit because fingerprints are very similar
        assert!(result.is_some());
    }

    #[test]
    fn test_cache_ttl_expiry() {
        let config = SemanticCacheConfig { default_ttl_ms: 1000, ..Default::default() };
        let mut cache = SemanticCache::new(config);
        let fp = text_fingerprint("test");
        cache.insert("test", fp.clone(), "result", 1000);

        assert!(cache.lookup("test", &fp, 1500).is_some()); // within TTL
        assert!(cache.lookup("test", &fp, 2500).is_none()); // expired
    }

    #[test]
    fn test_cache_eviction() {
        let config = SemanticCacheConfig { max_entries: 3, ..Default::default() };
        let mut cache = SemanticCache::new(config);
        for i in 0..5 {
            let q = format!("query {}", i);
            let fp = text_fingerprint(&q);
            cache.insert(&q, fp, &format!("result {}", i), 1000);
        }
        assert_eq!(cache.entry_count(), 3);
    }

    #[test]
    fn test_cache_invalidate() {
        let mut cache = SemanticCache::new(SemanticCacheConfig::default());
        let fp1 = text_fingerprint("SELECT * FROM sensors");
        let fp2 = text_fingerprint("SELECT * FROM users");
        cache.insert("SELECT * FROM sensors", fp1, "r1", 1000);
        cache.insert("SELECT * FROM users", fp2, "r2", 1000);

        cache.invalidate("sensors");
        assert_eq!(cache.entry_count(), 1);
    }

    #[test]
    fn test_cosine_similarity() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        assert!((cosine_similarity(&a, &b) - 1.0).abs() < 0.001);

        let c = vec![0.0, 1.0, 0.0];
        assert!((cosine_similarity(&a, &c)).abs() < 0.001);
    }

    #[test]
    fn test_text_fingerprint() {
        let fp1 = text_fingerprint("hello world");
        let fp2 = text_fingerprint("hello world");
        let fp3 = text_fingerprint("goodbye universe");
        assert_eq!(fp1, fp2);
        assert_ne!(fp1, fp3);
    }
}
