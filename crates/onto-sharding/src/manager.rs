//! Shard lifecycle management.
//!
//! The ShardManager handles shard creation, migration, and rebalancing.
//! It persists shard metadata and coordinates shard state changes.

use std::collections::HashMap;

use crate::types::{ShardConfig, ShardId, ShardMap};

/// Manages shard lifecycle: creation, configuration, and metadata.
pub struct ShardManager {
    shard_map: ShardMap,
    /// Shard status tracking.
    shard_status: HashMap<ShardId, ShardStatus>,
}

/// Status of a shard.
#[derive(Debug, Clone, PartialEq)]
pub enum ShardStatus {
    /// Shard is active and serving requests.
    Active,
    /// Shard is being migrated to another node.
    Migrating { target_node: u64 },
    /// Shard is offline (e.g., all replicas down).
    Offline,
    /// Shard is being split into multiple shards.
    Splitting { new_shards: Vec<ShardId> },
}

impl ShardManager {
    pub fn new(default_shard: ShardId) -> Self {
        let shard_map = ShardMap::new(default_shard);
        let mut shard_status = HashMap::new();
        shard_status.insert(default_shard, ShardStatus::Active);
        Self { shard_map, shard_status }
    }

    /// Create a new shard.
    pub fn create_shard(&mut self, config: ShardConfig) {
        let id = config.id;
        self.shard_map.add_shard(config);
        self.shard_status.insert(id, ShardStatus::Active);
    }

    /// Assign a class to a shard (class-based sharding).
    pub fn assign_class(&mut self, class: &str, shard: ShardId) -> Result<(), String> {
        if !self.shard_map.shards.contains_key(&shard) {
            return Err(format!("Shard {} does not exist", shard));
        }
        self.shard_map.shard_class(class, shard);
        Ok(())
    }

    /// Get the shard map (for sharing with router).
    pub fn shard_map(&self) -> &ShardMap {
        &self.shard_map
    }

    /// Get a reference to the owned shard map.
    pub fn into_shard_map(self) -> ShardMap {
        self.shard_map
    }

    /// Get shard status.
    pub fn shard_status(&self, shard: ShardId) -> Option<&ShardStatus> {
        self.shard_status.get(&shard)
    }

    /// List all active shards.
    pub fn active_shards(&self) -> Vec<ShardId> {
        self.shard_status.iter()
            .filter(|(_, status)| **status == ShardStatus::Active)
            .map(|(id, _)| *id)
            .collect()
    }

    /// Mark a shard as migrating.
    pub fn start_migration(&mut self, shard: ShardId, target_node: u64) -> Result<(), String> {
        match self.shard_status.get(&shard) {
            Some(ShardStatus::Active) => {
                self.shard_status.insert(shard, ShardStatus::Migrating { target_node });
                Ok(())
            }
            Some(status) => Err(format!("Shard {} is not active (status: {:?})", shard, status)),
            None => Err(format!("Shard {} does not exist", shard)),
        }
    }

    /// Complete migration — mark shard as active again.
    pub fn complete_migration(&mut self, shard: ShardId) {
        self.shard_status.insert(shard, ShardStatus::Active);
    }

    /// Mark a shard as offline.
    pub fn mark_offline(&mut self, shard: ShardId) {
        self.shard_status.insert(shard, ShardStatus::Offline);
    }

    /// Serialize shard map to JSON for persistence.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(&self.shard_map)
    }

    /// Load shard map from JSON.
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        let shard_map: ShardMap = serde_json::from_str(json)?;
        let shard_status: HashMap<ShardId, ShardStatus> = shard_map.shards.keys()
            .map(|id| (*id, ShardStatus::Active))
            .collect();
        Ok(Self { shard_map, shard_status })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ShardConfig;

    fn make_shard(id: ShardId, name: &str) -> ShardConfig {
        ShardConfig {
            id,
            name: name.to_string(),
            raft_group: Some(id as u64),
            replicas: vec![0],
            is_primary: true,
        }
    }

    #[test]
    fn test_create_shard() {
        let mut mgr = ShardManager::new(0);
        mgr.create_shard(make_shard(1, "shard-1"));
        assert_eq!(mgr.active_shards().len(), 2); // default + new
    }

    #[test]
    fn test_assign_class() {
        let mut mgr = ShardManager::new(0);
        mgr.create_shard(make_shard(1, "shard-1"));
        assert!(mgr.assign_class("User", 1).is_ok());
        assert!(mgr.assign_class("User", 99).is_err()); // shard doesn't exist
    }

    #[test]
    fn test_migration_lifecycle() {
        let mut mgr = ShardManager::new(0);
        mgr.create_shard(make_shard(1, "shard-1"));

        // Start migration
        assert!(mgr.start_migration(1, 2).is_ok());
        assert_eq!(mgr.shard_status(1), Some(&ShardStatus::Migrating { target_node: 2 }));

        // Can't migrate again while migrating
        assert!(mgr.start_migration(1, 3).is_err());

        // Complete migration
        mgr.complete_migration(1);
        assert_eq!(mgr.shard_status(1), Some(&ShardStatus::Active));
    }

    #[test]
    fn test_json_roundtrip() {
        let mut mgr = ShardManager::new(0);
        mgr.create_shard(make_shard(1, "shard-1"));
        mgr.assign_class("User", 1).unwrap();

        let json = mgr.to_json().unwrap();
        let restored = ShardManager::from_json(&json).unwrap();
        assert_eq!(restored.shard_map().shards.len(), 2);
        assert_eq!(restored.shard_map().get_strategy("User"), crate::types::ShardStrategy::ClassBased { shard: 1 });
    }
}
