// Copyright (c) 2024-2026 OntoDB Team
// Licensed under the Business Source License 1.1 (BUSL-1.1).
// See LICENSE for details. Change Date: 2031-09-15.
// On the Change Date, this file will be licensed under Apache License 2.0.
//! Core types for data sharding.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Unique identifier for a shard.
pub type ShardId = u32;

/// Sharding strategy for a class.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ShardStrategy {
    /// Entire class lives on one shard.
    ClassBased {
        /// The shard this class is assigned to.
        shard: ShardId,
    },
    /// Class is split by primary key range.
    RangeBased {
        /// Sorted range boundaries: [(end_key, shard_id), ...]
        /// All keys < end_key[0] go to shard[0], keys in [end_key[i-1], end_key[i]) go to shard[i], etc.
        ranges: Vec<RangeShard>,
    },
    /// Class is split by hash of primary key.
    HashBased {
        /// Number of virtual shards (must be power of 2 for consistent hashing).
        num_shards: u32,
        /// Mapping from hash slot to physical shard.
        slot_map: Vec<ShardId>,
    },
}

/// A range shard boundary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RangeShard {
    /// Upper bound of this range (exclusive).
    pub end_key: Vec<u8>,
    /// Shard for keys in this range.
    pub shard: ShardId,
}

/// Configuration for a single shard.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShardConfig {
    /// Unique shard ID.
    pub id: ShardId,
    /// Human-readable name (e.g., "shard-us-east-1").
    pub name: String,
    /// Raft group ID for this shard (if replicated).
    pub raft_group: Option<u64>,
    /// Node IDs that hold replicas of this shard.
    pub replicas: Vec<u64>,
    /// Whether this shard is the primary (leader).
    pub is_primary: bool,
}

/// Complete shard map: maps classes to their sharding strategy.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ShardMap {
    /// Class name → sharding strategy.
    pub class_strategies: HashMap<String, ShardStrategy>,
    /// Shard ID → shard config.
    pub shards: HashMap<ShardId, ShardConfig>,
    /// Default shard for classes without explicit sharding.
    pub default_shard: ShardId,
}

impl ShardMap {
    pub fn new(default_shard: ShardId) -> Self {
        let mut shards = HashMap::new();
        shards.insert(
            default_shard,
            ShardConfig {
                id: default_shard,
                name: "default".to_string(),
                raft_group: None,
                replicas: vec![0],
                is_primary: true,
            },
        );
        Self {
            class_strategies: HashMap::new(),
            shards,
            default_shard,
        }
    }

    /// Add a class-based shard assignment.
    pub fn shard_class(&mut self, class: &str, shard: ShardId) {
        self.class_strategies
            .insert(class.to_string(), ShardStrategy::ClassBased { shard });
    }

    /// Add a range-based shard assignment.
    pub fn shard_class_range(&mut self, class: &str, ranges: Vec<RangeShard>) {
        self.class_strategies
            .insert(class.to_string(), ShardStrategy::RangeBased { ranges });
    }

    /// Add a hash-based shard assignment.
    pub fn shard_class_hash(&mut self, class: &str, num_shards: u32, slot_map: Vec<ShardId>) {
        self.class_strategies.insert(
            class.to_string(),
            ShardStrategy::HashBased {
                num_shards,
                slot_map,
            },
        );
    }

    /// Register a shard configuration.
    pub fn add_shard(&mut self, config: ShardConfig) {
        self.shards.insert(config.id, config);
    }

    /// Get the strategy for a class, or default class-based on the default shard.
    pub fn get_strategy(&self, class: &str) -> ShardStrategy {
        self.class_strategies
            .get(class)
            .cloned()
            .unwrap_or(ShardStrategy::ClassBased {
                shard: self.default_shard,
            })
    }
}
