// Copyright (c) 2024-2026 OntoDB Team
// Licensed under the Business Source License 1.1 (BUSL-1.1).
// See LICENSE for details. Change Date: 2031-09-15.
// On the Change Date, this file will be licensed under Apache License 2.0.
//! Shard lifecycle management.
//!
//! The ShardManager handles shard creation, migration, and rebalancing.
//! It persists shard metadata and coordinates shard state changes.

use std::collections::HashMap;

use crate::types::{ShardConfig, ShardId, ShardMap, ShardStrategy};

/// Manages shard lifecycle: creation, configuration, and metadata.
pub struct ShardManager {
    shard_map: ShardMap,
    /// Shard status tracking.
    shard_status: HashMap<ShardId, ShardStatus>,
    /// Active migration tasks.
    migrations: HashMap<String, MigrationTask>,
    /// Rebalance history.
    rebalance_history: Vec<RebalanceRecord>,
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
    /// Shard is being rebalanced.
    Rebalancing,
}

/// Migration task status.
#[derive(Debug, Clone, PartialEq)]
pub enum MigrationStatus {
    /// Migration is pending.
    Pending,
    /// Migration is in progress.
    InProgress {
        /// Total records to migrate.
        total_records: u64,
        /// Records migrated so far.
        migrated_records: u64,
    },
    /// Migration completed successfully.
    Completed,
    /// Migration failed.
    Failed { error: String },
    /// Migration was cancelled.
    Cancelled,
}

/// A migration task for moving data between shards.
#[derive(Debug, Clone)]
pub struct MigrationTask {
    /// Unique migration ID.
    pub id: String,
    /// Source shard ID.
    pub source_shard: ShardId,
    /// Target shard ID.
    pub target_shard: ShardId,
    /// Class (table) being migrated.
    pub class: String,
    /// Migration status.
    pub status: MigrationStatus,
    /// Key range to migrate (for range-based migration).
    pub key_range: Option<(Vec<u8>, Vec<u8>)>,
    /// Created timestamp.
    pub created_at: u64,
    /// Completed timestamp.
    pub completed_at: Option<u64>,
}

/// Record of a rebalance operation.
#[derive(Debug, Clone)]
pub struct RebalanceRecord {
    /// Rebalance ID.
    pub id: String,
    /// Timestamp.
    pub timestamp: u64,
    /// Number of shards before rebalance.
    pub shards_before: usize,
    /// Number of shards after rebalance.
    pub shards_after: usize,
    /// Classes rebalanced.
    pub classes_rebalanced: Vec<String>,
    /// Status.
    pub status: String,
}

/// Result of a migration operation.
#[derive(Debug, Clone)]
pub struct MigrationResult {
    /// Migration ID.
    pub migration_id: String,
    /// Source shard.
    pub source_shard: ShardId,
    /// Target shard.
    pub target_shard: ShardId,
    /// Number of records migrated.
    pub records_migrated: u64,
    /// Status.
    pub status: MigrationStatus,
}

/// Result of a rebalance operation.
#[derive(Debug, Clone)]
pub struct RebalanceResult {
    /// Rebalance ID.
    pub rebalance_id: String,
    /// Classes rebalanced.
    pub classes_rebalanced: Vec<String>,
    /// New shard assignments.
    pub new_assignments: HashMap<String, ShardId>,
    /// Status.
    pub status: String,
}

impl ShardManager {
    pub fn new(default_shard: ShardId) -> Self {
        let shard_map = ShardMap::new(default_shard);
        let mut shard_status = HashMap::new();
        shard_status.insert(default_shard, ShardStatus::Active);
        Self {
            shard_map,
            shard_status,
            migrations: HashMap::new(),
            rebalance_history: Vec::new(),
        }
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

    /// Get a mutable reference to the shard map.
    pub fn shard_map_mut(&mut self) -> &mut ShardMap {
        &mut self.shard_map
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
        self.shard_status
            .iter()
            .filter(|(_, status)| **status == ShardStatus::Active)
            .map(|(id, _)| *id)
            .collect()
    }

    /// Mark a shard as migrating.
    pub fn start_migration(&mut self, shard: ShardId, target_node: u64) -> Result<(), String> {
        match self.shard_status.get(&shard) {
            Some(ShardStatus::Active) => {
                self.shard_status
                    .insert(shard, ShardStatus::Migrating { target_node });
                Ok(())
            }
            Some(status) => Err(format!(
                "Shard {} is not active (status: {:?})",
                shard, status
            )),
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
        let shard_status: HashMap<ShardId, ShardStatus> = shard_map
            .shards
            .keys()
            .map(|id| (*id, ShardStatus::Active))
            .collect();
        Ok(Self {
            shard_map,
            shard_status,
            migrations: HashMap::new(),
            rebalance_history: Vec::new(),
        })
    }

    // ── Migration Operations ─────────────────────────────────────────

    /// Create a new migration task.
    pub fn create_migration(
        &mut self,
        source_shard: ShardId,
        target_shard: ShardId,
        class: &str,
        key_range: Option<(Vec<u8>, Vec<u8>)>,
    ) -> Result<String, String> {
        // Validate source shard exists and is active
        match self.shard_status.get(&source_shard) {
            Some(ShardStatus::Active) => {}
            Some(status) => {
                return Err(format!(
                    "Source shard {} is not active (status: {:?})",
                    source_shard, status
                ))
            }
            None => return Err(format!("Source shard {} does not exist", source_shard)),
        }

        // Validate target shard exists
        if !self.shard_map.shards.contains_key(&target_shard) {
            return Err(format!("Target shard {} does not exist", target_shard));
        }

        // Generate migration ID
        let migration_id = format!(
            "mig_{}_{}_{}",
            source_shard,
            target_shard,
            self.migrations.len()
        );

        let task = MigrationTask {
            id: migration_id.clone(),
            source_shard,
            target_shard,
            class: class.to_string(),
            status: MigrationStatus::Pending,
            key_range,
            created_at: self.current_timestamp(),
            completed_at: None,
        };

        self.migrations.insert(migration_id.clone(), task);

        // Mark source shard as migrating
        self.shard_status.insert(
            source_shard,
            ShardStatus::Migrating {
                target_node: target_shard as u64,
            },
        );

        Ok(migration_id)
    }

    /// Update migration progress.
    pub fn update_migration_progress(
        &mut self,
        migration_id: &str,
        total_records: u64,
        migrated_records: u64,
    ) -> Result<(), String> {
        let task = self
            .migrations
            .get_mut(migration_id)
            .ok_or_else(|| format!("Migration {} not found", migration_id))?;

        task.status = MigrationStatus::InProgress {
            total_records,
            migrated_records,
        };

        Ok(())
    }

    /// Complete a migration.
    pub fn complete_migration_task(
        &mut self,
        migration_id: &str,
    ) -> Result<MigrationResult, String> {
        let timestamp = self.current_timestamp();
        let task = self
            .migrations
            .get_mut(migration_id)
            .ok_or_else(|| format!("Migration {} not found", migration_id))?;

        task.status = MigrationStatus::Completed;
        task.completed_at = Some(timestamp);

        // Mark source shard as active again
        self.shard_status
            .insert(task.source_shard, ShardStatus::Active);

        Ok(MigrationResult {
            migration_id: migration_id.to_string(),
            source_shard: task.source_shard,
            target_shard: task.target_shard,
            records_migrated: match &task.status {
                MigrationStatus::InProgress {
                    migrated_records, ..
                } => *migrated_records,
                _ => 0,
            },
            status: task.status.clone(),
        })
    }

    /// Fail a migration.
    pub fn fail_migration(&mut self, migration_id: &str, error: &str) -> Result<(), String> {
        let timestamp = self.current_timestamp();
        let task = self
            .migrations
            .get_mut(migration_id)
            .ok_or_else(|| format!("Migration {} not found", migration_id))?;

        task.status = MigrationStatus::Failed {
            error: error.to_string(),
        };
        task.completed_at = Some(timestamp);

        // Mark source shard as active again
        self.shard_status
            .insert(task.source_shard, ShardStatus::Active);

        Ok(())
    }

    /// Cancel a migration.
    pub fn cancel_migration(&mut self, migration_id: &str) -> Result<(), String> {
        let timestamp = self.current_timestamp();
        let task = self
            .migrations
            .get_mut(migration_id)
            .ok_or_else(|| format!("Migration {} not found", migration_id))?;

        match &task.status {
            MigrationStatus::Pending | MigrationStatus::InProgress { .. } => {
                task.status = MigrationStatus::Cancelled;
                task.completed_at = Some(timestamp);

                // Mark source shard as active again
                self.shard_status
                    .insert(task.source_shard, ShardStatus::Active);
                Ok(())
            }
            _ => Err(format!(
                "Migration {} cannot be cancelled (status: {:?})",
                migration_id, task.status
            )),
        }
    }

    /// Get migration status.
    pub fn get_migration(&self, migration_id: &str) -> Option<&MigrationTask> {
        self.migrations.get(migration_id)
    }

    /// List all migrations.
    pub fn list_migrations(&self) -> Vec<&MigrationTask> {
        self.migrations.values().collect()
    }

    /// List active migrations.
    pub fn active_migrations(&self) -> Vec<&MigrationTask> {
        self.migrations
            .values()
            .filter(|t| {
                matches!(
                    t.status,
                    MigrationStatus::Pending | MigrationStatus::InProgress { .. }
                )
            })
            .collect()
    }

    // ── Rebalance Operations ─────────────────────────────────────────

    /// Rebalance classes across shards.
    /// This redistributes classes evenly across available shards.
    pub fn rebalance_classes(&mut self, classes: Vec<String>) -> Result<RebalanceResult, String> {
        let active_shards = self.active_shards();
        if active_shards.is_empty() {
            return Err("No active shards available for rebalancing".to_string());
        }

        let rebalance_id = format!("rebal_{}", self.rebalance_history.len());
        let shards_before = self.shard_map.shards.len();

        let mut new_assignments = HashMap::new();

        // Distribute classes evenly across shards
        for (i, class) in classes.iter().enumerate() {
            let shard_idx = i % active_shards.len();
            let target_shard = active_shards[shard_idx];
            self.shard_map.shard_class(class, target_shard);
            new_assignments.insert(class.clone(), target_shard);
        }

        let record = RebalanceRecord {
            id: rebalance_id.clone(),
            timestamp: self.current_timestamp(),
            shards_before,
            shards_after: self.shard_map.shards.len(),
            classes_rebalanced: classes.clone(),
            status: "completed".to_string(),
        };

        self.rebalance_history.push(record);

        Ok(RebalanceResult {
            rebalance_id,
            classes_rebalanced: classes,
            new_assignments,
            status: "completed".to_string(),
        })
    }

    // ── Scale Operations ─────────────────────────────────────────────

    /// Add a new shard and optionally rebalance.
    pub fn add_shard_and_rebalance(
        &mut self,
        config: ShardConfig,
        rebalance_classes: bool,
    ) -> Result<RebalanceResult, String> {
        let shard_id = config.id;
        let shard_name = config.name.clone();

        // Create the new shard
        self.create_shard(config);

        if rebalance_classes {
            // Get all classes and rebalance
            let classes: Vec<String> = self.shard_map.class_strategies.keys().cloned().collect();
            if !classes.is_empty() {
                return self.rebalance_classes(classes);
            }
        }

        Ok(RebalanceResult {
            rebalance_id: format!("scale_{}", shard_id),
            classes_rebalanced: Vec::new(),
            new_assignments: HashMap::new(),
            status: format!("Shard {} ({}) added successfully", shard_id, shard_name),
        })
    }

    /// Remove a shard and migrate its data to another shard.
    pub fn remove_shard_with_migration(
        &mut self,
        shard_id: ShardId,
        target_shard: ShardId,
    ) -> Result<String, String> {
        // Validate shard exists
        if !self.shard_map.shards.contains_key(&shard_id) {
            return Err(format!("Shard {} does not exist", shard_id));
        }

        // Cannot remove default shard
        if shard_id == self.shard_map.default_shard {
            return Err("Cannot remove the default shard".to_string());
        }

        // Validate target shard exists
        if !self.shard_map.shards.contains_key(&target_shard) {
            return Err(format!("Target shard {} does not exist", target_shard));
        }

        // Find all classes assigned to this shard and reassign
        let classes_to_migrate: Vec<String> = self
            .shard_map
            .class_strategies
            .iter()
            .filter_map(|(class, strategy)| match strategy {
                ShardStrategy::ClassBased { shard } if *shard == shard_id => Some(class.clone()),
                _ => None,
            })
            .collect();

        for class in &classes_to_migrate {
            self.shard_map.shard_class(class, target_shard);
        }

        // Remove the shard
        self.shard_map.shards.remove(&shard_id);
        self.shard_status.remove(&shard_id);

        Ok(format!(
            "Shard {} removed, {} classes migrated to shard {}",
            shard_id,
            classes_to_migrate.len(),
            target_shard
        ))
    }

    // ── Split Operations ─────────────────────────────────────────────

    /// Split a shard into multiple new shards.
    pub fn split_shard(
        &mut self,
        source_shard: ShardId,
        new_shards: Vec<ShardConfig>,
        strategy: SplitStrategy,
    ) -> Result<Vec<MigrationTask>, String> {
        // Validate source shard
        match self.shard_status.get(&source_shard) {
            Some(ShardStatus::Active) => {}
            Some(status) => {
                return Err(format!(
                    "Source shard {} is not active (status: {:?})",
                    source_shard, status
                ))
            }
            None => return Err(format!("Source shard {} does not exist", source_shard)),
        }

        // Create new shards
        for config in &new_shards {
            self.create_shard(config.clone());
        }

        // Mark source shard as splitting
        let new_shard_ids: Vec<ShardId> = new_shards.iter().map(|s| s.id).collect();
        self.shard_status.insert(
            source_shard,
            ShardStatus::Splitting {
                new_shards: new_shard_ids.clone(),
            },
        );

        // Create migration tasks based on strategy
        let mut migrations = Vec::new();
        match strategy {
            SplitStrategy::Even => {
                // Distribute evenly across new shards
                for new_shard in new_shard_ids.iter() {
                    let migration_id =
                        self.create_migration(source_shard, *new_shard, "*", None)?;
                    if let Some(task) = self.migrations.get(&migration_id) {
                        migrations.push(task.clone());
                    }
                }
            }
            SplitStrategy::RangeBased { ranges } => {
                // Create range-based migrations
                for (i, range) in ranges.iter().enumerate() {
                    if i < new_shard_ids.len() {
                        let migration_id = self.create_migration(
                            source_shard,
                            new_shard_ids[i],
                            "*",
                            Some((range.0.clone(), range.1.clone())),
                        )?;
                        if let Some(task) = self.migrations.get(&migration_id) {
                            migrations.push(task.clone());
                        }
                    }
                }
            }
            SplitStrategy::HashBased => {
                // Hash-based split: distribute based on key hash
                for new_shard in &new_shard_ids {
                    let migration_id =
                        self.create_migration(source_shard, *new_shard, "*", None)?;
                    if let Some(task) = self.migrations.get(&migration_id) {
                        migrations.push(task.clone());
                    }
                }
            }
        }

        Ok(migrations)
    }

    // ── Helper Methods ───────────────────────────────────────────────

    fn current_timestamp(&self) -> u64 {
        // Simple timestamp based on system time
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }

    /// Get migration history.
    pub fn migration_history(&self) -> Vec<&MigrationTask> {
        self.migrations.values().collect()
    }

    /// Get rebalance history.
    pub fn rebalance_history(&self) -> &[RebalanceRecord] {
        &self.rebalance_history
    }

    /// Get shard statistics.
    pub fn shard_statistics(&self) -> HashMap<ShardId, ShardStatistics> {
        let mut stats = HashMap::new();
        for (id, status) in &self.shard_status {
            stats.insert(
                *id,
                ShardStatistics {
                    shard_id: *id,
                    status: status.clone(),
                    classes_count: self
                        .shard_map
                        .class_strategies
                        .values()
                        .filter(|s| match s {
                            ShardStrategy::ClassBased { shard } => *shard == *id,
                            _ => false,
                        })
                        .count(),
                },
            );
        }
        stats
    }
}

/// Strategy for splitting a shard.
#[derive(Debug, Clone)]
pub enum SplitStrategy {
    /// Distribute evenly across new shards.
    Even,
    /// Split based on key ranges.
    RangeBased { ranges: Vec<(Vec<u8>, Vec<u8>)> },
    /// Split based on key hash.
    HashBased,
}

/// Statistics for a shard.
#[derive(Debug, Clone)]
pub struct ShardStatistics {
    pub shard_id: ShardId,
    pub status: ShardStatus,
    pub classes_count: usize,
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
        assert_eq!(
            mgr.shard_status(1),
            Some(&ShardStatus::Migrating { target_node: 2 })
        );

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
        assert_eq!(
            restored.shard_map().get_strategy("User"),
            crate::types::ShardStrategy::ClassBased { shard: 1 }
        );
    }
}
