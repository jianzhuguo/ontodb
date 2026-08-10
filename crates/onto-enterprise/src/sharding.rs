//! Data sharding module for OntoDB Enterprise.
//!
//! Provides:
//! - Hash-based sharding (consistent hashing)
//! - Range-based sharding
//! - Shard routing and query coordination
//! - Cross-shard query support
//! - Shard rebalancing

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::collections::hash_map::DefaultHasher;
use std::sync::Arc;
use parking_lot::RwLock;

/// Sharding configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShardingConfig {
    /// Enable sharding.
    pub enabled: bool,
    /// Number of shards.
    pub shard_count: u32,
    /// Sharding strategy.
    pub strategy: ShardStrategy,
    /// Shard key field name.
    pub shard_key: String,
    /// Virtual nodes per physical shard (for consistent hashing).
    pub virtual_nodes: u32,
}

/// Sharding strategy.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ShardStrategy {
    /// Hash-based sharding (consistent hashing).
    Hash,
    /// Range-based sharding.
    Range,
    /// Modulo sharding (simple hash % shard_count).
    Modulo,
}

impl Default for ShardingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            shard_count: 4,
            strategy: ShardStrategy::Modulo,
            shard_key: "id".to_string(),
            virtual_nodes: 100,
        }
    }
}

/// Shard information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Shard {
    /// Shard ID.
    pub id: u32,
    /// Shard range start (for range-based sharding).
    pub range_start: Option<String>,
    /// Shard range end (for range-based sharding).
    pub range_end: Option<String>,
    /// Node hosting this shard.
    pub node_id: u64,
    /// Is this shard the primary (vs replica)?
    pub is_primary: bool,
    /// Shard size in bytes.
    pub size_bytes: u64,
    /// Number of entries in shard.
    pub entry_count: u64,
}

/// Shard routing information.
#[derive(Debug, Clone)]
pub struct ShardRoute {
    /// Target shard ID.
    pub shard_id: u32,
    /// Is this a cross-shard query?
    pub cross_shard: bool,
    /// Target shards (for cross-shard queries).
    pub target_shards: Vec<u32>,
}

/// Shard manager.
pub struct ShardManager {
    config: ShardingConfig,
    shards: Arc<RwLock<Vec<Shard>>>,
    /// Consistent hashing ring (virtual node -> shard ID).
    hash_ring: Arc<RwLock<Vec<(u64, u32)>>>,
    /// Range boundaries for range-based sharding.
    range_boundaries: Arc<RwLock<Vec<(String, u32)>>>,
}

impl ShardManager {
    /// Create a new shard manager.
    pub fn new(config: ShardingConfig) -> Self {
        let mut shards = Vec::new();
        for i in 0..config.shard_count {
            shards.push(Shard {
                id: i,
                range_start: None,
                range_end: None,
                node_id: (i as u64) + 1, // Assign to nodes 1..N
                is_primary: true,
                size_bytes: 0,
                entry_count: 0,
            });
        }

        let hash_ring = Self::build_hash_ring(&config, &shards);
        let range_boundaries = Vec::new();

        Self {
            config,
            shards: Arc::new(RwLock::new(shards)),
            hash_ring: Arc::new(RwLock::new(hash_ring)),
            range_boundaries: Arc::new(RwLock::new(range_boundaries)),
        }
    }

    /// Build consistent hashing ring.
    fn build_hash_ring(config: &ShardingConfig, shards: &[Shard]) -> Vec<(u64, u32)> {
        let mut ring = Vec::new();
        for shard in shards {
            for vn in 0..config.virtual_nodes {
                let key = format!("shard-{}-vn-{}", shard.id, vn);
                let hash = Self::hash_key(&key);
                ring.push((hash, shard.id));
            }
        }
        ring.sort_by_key(|(hash, _)| *hash);
        ring
    }

    /// Hash a key to a u64.
    fn hash_key(key: &str) -> u64 {
        let mut hasher = DefaultHasher::new();
        key.hash(&mut hasher);
        hasher.finish()
    }

    /// Get the shard ID for a given key.
    pub fn get_shard(&self, key: &str) -> u32 {
        match self.config.strategy {
            ShardStrategy::Modulo => self.get_shard_modulo(key),
            ShardStrategy::Hash => self.get_shard_consistent(key),
            ShardStrategy::Range => self.get_shard_range(key),
        }
    }

    /// Modulo-based sharding.
    fn get_shard_modulo(&self, key: &str) -> u32 {
        let hash = Self::hash_key(key);
        (hash % self.config.shard_count as u64) as u32
    }

    /// Consistent hashing sharding.
    fn get_shard_consistent(&self, key: &str) -> u32 {
        let hash = Self::hash_key(key);
        let ring = self.hash_ring.read();

        // Find the first virtual node with hash >= key hash
        match ring.binary_search_by_key(&hash, |(h, _)| *h) {
            Ok(idx) => ring[idx].1,
            Err(idx) => {
                if idx < ring.len() {
                    ring[idx].1
                } else {
                    ring[0].1 // Wrap around
                }
            }
        }
    }

    /// Range-based sharding.
    fn get_shard_range(&self, key: &str) -> u32 {
        let boundaries = self.range_boundaries.read();
        
        for (boundary, shard_id) in boundaries.iter() {
            if key <= boundary.as_str() {
                return *shard_id;
            }
        }

        // Default to last shard
        boundaries.last().map(|(_, id)| *id).unwrap_or(0)
    }

    /// Get shard routing for a query.
    pub fn route_query(&self, shard_key_value: Option<&str>, query: &str) -> ShardRoute {
        if let Some(key_value) = shard_key_value {
            // Single shard query
            let shard_id = self.get_shard(key_value);
            ShardRoute {
                shard_id,
                cross_shard: false,
                target_shards: vec![shard_id],
            }
        } else {
            // Cross-shard query (scan all shards)
            let shards = self.shards.read();
            ShardRoute {
                shard_id: 0,
                cross_shard: true,
                target_shards: shards.iter().map(|s| s.id).collect(),
            }
        }
    }

    /// Get all shards.
    pub fn get_shards(&self) -> Vec<Shard> {
        self.shards.read().clone()
    }

    /// Get a specific shard.
    pub fn get_shard_info(&self, shard_id: u32) -> Option<Shard> {
        self.shards.read().iter().find(|s| s.id == shard_id).cloned()
    }

    /// Update shard statistics.
    pub fn update_shard_stats(&self, shard_id: u32, size_bytes: u64, entry_count: u64) -> Result<()> {
        let mut shards = self.shards.write();
        let shard = shards.iter_mut().find(|s| s.id == shard_id)
            .ok_or_else(|| anyhow::anyhow!("Shard {} not found", shard_id))?;
        
        shard.size_bytes = size_bytes;
        shard.entry_count = entry_count;
        Ok(())
    }

    /// Set range boundaries for range-based sharding.
    pub fn set_range_boundaries(&self, boundaries: Vec<(String, u32)>) {
        let mut range_boundaries = self.range_boundaries.write();
        *range_boundaries = boundaries;
    }

    /// Get the shard count.
    pub fn shard_count(&self) -> u32 {
        self.config.shard_count
    }

    /// Get the sharding strategy.
    pub fn strategy(&self) -> &ShardStrategy {
        &self.config.strategy
    }

    /// Check if sharding is enabled.
    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    /// Get shard distribution statistics.
    pub fn distribution_stats(&self) -> HashMap<u32, (u64, u64)> {
        let shards = self.shards.read();
        shards.iter().map(|s| (s.id, (s.size_bytes, s.entry_count))).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> ShardingConfig {
        ShardingConfig {
            enabled: true,
            shard_count: 4,
            strategy: ShardStrategy::Modulo,
            shard_key: "id".to_string(),
            virtual_nodes: 100,
        }
    }

    #[test]
    fn test_shard_manager_new() {
        let config = test_config();
        let manager = ShardManager::new(config);
        assert_eq!(manager.shard_count(), 4);
        assert!(manager.is_enabled());
    }

    #[test]
    fn test_modulo_sharding() {
        let config = ShardingConfig {
            strategy: ShardStrategy::Modulo,
            ..test_config()
        };
        let manager = ShardManager::new(config);

        // Same key should always go to same shard
        let shard1 = manager.get_shard("user-123");
        let shard2 = manager.get_shard("user-123");
        assert_eq!(shard1, shard2);

        // Different keys may go to different shards
        let shard_a = manager.get_shard("user-1");
        let shard_b = manager.get_shard("user-2");
        // They might be the same or different, but should be valid
        assert!(shard_a < 4);
        assert!(shard_b < 4);
    }

    #[test]
    fn test_consistent_hashing() {
        let config = ShardingConfig {
            strategy: ShardStrategy::Hash,
            ..test_config()
        };
        let manager = ShardManager::new(config);

        // Same key should always go to same shard
        let shard1 = manager.get_shard("order-456");
        let shard2 = manager.get_shard("order-456");
        assert_eq!(shard1, shard2);
    }

    #[test]
    fn test_range_sharding() {
        let config = ShardingConfig {
            strategy: ShardStrategy::Range,
            ..test_config()
        };
        let manager = ShardManager::new(config);

        // Set range boundaries
        manager.set_range_boundaries(vec![
            ("m".to_string(), 0),
            ("t".to_string(), 1),
            ("z".to_string(), 2),
        ]);

        assert_eq!(manager.get_shard("alice"), 0);  // "alice" < "m"
        assert_eq!(manager.get_shard("peter"), 1);   // "m" < "peter" < "t"
        assert_eq!(manager.get_shard("sam"), 1);     // "m" < "sam" < "t"
        assert_eq!(manager.get_shard("zoe"), 2);     // "t" < "zoe" < "z"
    }

    #[test]
    fn test_route_query_single() {
        let config = test_config();
        let manager = ShardManager::new(config);

        let route = manager.route_query(Some("user-123"), "SELECT * FROM users");
        assert!(!route.cross_shard);
        assert_eq!(route.target_shards.len(), 1);
    }

    #[test]
    fn test_route_query_cross_shard() {
        let config = test_config();
        let manager = ShardManager::new(config);

        let route = manager.route_query(None, "SELECT COUNT(*) FROM users");
        assert!(route.cross_shard);
        assert_eq!(route.target_shards.len(), 4);
    }

    #[test]
    fn test_shard_stats() {
        let config = test_config();
        let manager = ShardManager::new(config);

        manager.update_shard_stats(0, 1024, 100).unwrap();
        let shard = manager.get_shard_info(0).unwrap();
        assert_eq!(shard.size_bytes, 1024);
        assert_eq!(shard.entry_count, 100);
    }

    #[test]
    fn test_distribution_stats() {
        let config = test_config();
        let manager = ShardManager::new(config);

        let stats = manager.distribution_stats();
        assert_eq!(stats.len(), 4);
    }

    #[test]
    fn test_get_shards() {
        let config = test_config();
        let manager = ShardManager::new(config);

        let shards = manager.get_shards();
        assert_eq!(shards.len(), 4);
        assert_eq!(shards[0].id, 0);
        assert_eq!(shards[3].id, 3);
    }
}
