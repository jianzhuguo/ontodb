// Copyright (c) 2024-2026 OntoDB Team
// Licensed under the Business Source License 1.1 (BUSL-1.1).
// See LICENSE for details. Change Date: 2031-09-15.
// On the Change Date, this file will be licensed under Apache License 2.0.
//! Sharding strategy implementations.
//!
//! Each strategy determines which shard(s) a given key belongs to.

use crate::types::{ShardId, ShardStrategy};

/// Result of a shard lookup — may target one or multiple shards.
#[derive(Debug, Clone, PartialEq)]
pub enum ShardTarget {
    /// Operation targets a single shard.
    Single(ShardId),
    /// Operation must fan out to all shards (e.g., unbounded scan).
    All,
    /// Operation targets a specific set of shards.
    Multi(Vec<ShardId>),
}

impl ShardStrategy {
    /// Determine which shard a primary key belongs to.
    pub fn route_key(&self, key: &[u8]) -> ShardTarget {
        match self {
            ShardStrategy::ClassBased { shard } => ShardTarget::Single(*shard),
            ShardStrategy::RangeBased { ranges } => {
                for range in ranges {
                    if key < range.end_key.as_slice() {
                        return ShardTarget::Single(range.shard);
                    }
                }
                // Key is beyond all ranges — use the last shard
                ranges
                    .last()
                    .map(|r| ShardTarget::Single(r.shard))
                    .unwrap_or(ShardTarget::All)
            }
            ShardStrategy::HashBased {
                num_shards,
                slot_map,
            } => {
                let hash = Self::hash_key(key);
                let slot = (hash % *num_shards as u64) as usize;
                let shard = slot_map.get(slot).copied().unwrap_or(0);
                ShardTarget::Single(shard)
            }
        }
    }

    /// Determine which shards to scan for a full table scan.
    pub fn route_scan(&self) -> ShardTarget {
        match self {
            ShardStrategy::ClassBased { shard } => ShardTarget::Single(*shard),
            ShardStrategy::RangeBased { ranges } => {
                let shards: Vec<ShardId> = ranges.iter().map(|r| r.shard).collect();
                if shards.is_empty() {
                    ShardTarget::All
                } else {
                    ShardTarget::Multi(shards)
                }
            }
            ShardStrategy::HashBased { slot_map, .. } => {
                let mut shards: Vec<ShardId> = slot_map.clone();
                shards.sort();
                shards.dedup();
                ShardTarget::Multi(shards)
            }
        }
    }

    /// Determine which shards a range query [start, end) targets.
    pub fn route_range(&self, start: &[u8], end: &[u8]) -> ShardTarget {
        match self {
            ShardStrategy::ClassBased { shard } => ShardTarget::Single(*shard),
            ShardStrategy::RangeBased { ranges } => {
                let mut target_shards = Vec::new();
                for range in ranges {
                    // Include shard if its range overlaps with [start, end)
                    if start < range.end_key.as_slice()
                        && end > ranges.first().map(|r| r.end_key.as_slice()).unwrap_or(&[])
                        && !target_shards.contains(&range.shard)
                    {
                        target_shards.push(range.shard);
                    }
                }
                if target_shards.is_empty() {
                    ShardTarget::All
                } else if target_shards.len() == 1 {
                    ShardTarget::Single(target_shards[0])
                } else {
                    ShardTarget::Multi(target_shards)
                }
            }
            ShardStrategy::HashBased { .. } => {
                // Hash-based can't do range routing — must scan all shards
                ShardTarget::All
            }
        }
    }

    /// Simple hash function for keys (FNV-1a).
    fn hash_key(key: &[u8]) -> u64 {
        let mut hash: u64 = 0xcbf29ce484222325;
        for byte in key {
            hash ^= *byte as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        hash
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{RangeShard, ShardMap};

    #[test]
    fn test_class_based_routing() {
        let strategy = ShardStrategy::ClassBased { shard: 1 };
        assert_eq!(strategy.route_key(b"any_key"), ShardTarget::Single(1));
        assert_eq!(strategy.route_scan(), ShardTarget::Single(1));
    }

    #[test]
    fn test_range_based_routing() {
        let strategy = ShardStrategy::RangeBased {
            ranges: vec![
                RangeShard {
                    end_key: b"m".to_vec(),
                    shard: 0,
                },
                RangeShard {
                    end_key: b"t".to_vec(),
                    shard: 1,
                },
                RangeShard {
                    end_key: b"z".to_vec(),
                    shard: 2,
                },
            ],
        };
        // "a" < "m" → shard 0
        assert_eq!(strategy.route_key(b"a"), ShardTarget::Single(0));
        // "n" >= "m" but < "t" → shard 1
        assert_eq!(strategy.route_key(b"n"), ShardTarget::Single(1));
        // "u" >= "t" but < "z" → shard 2
        assert_eq!(strategy.route_key(b"u"), ShardTarget::Single(2));
        // Scan hits all shards
        assert_eq!(strategy.route_scan(), ShardTarget::Multi(vec![0, 1, 2]));
    }

    #[test]
    fn test_hash_based_routing() {
        let strategy = ShardStrategy::HashBased {
            num_shards: 4,
            slot_map: vec![0, 0, 1, 1], // slots 0,1 → shard 0; slots 2,3 → shard 1
        };
        // Deterministic — same key always goes to same shard
        let target = strategy.route_key(b"test_key");
        assert!(matches!(target, ShardTarget::Single(_)));
        // Scan hits both shards
        assert_eq!(strategy.route_scan(), ShardTarget::Multi(vec![0, 1]));
    }

    #[test]
    fn test_shard_map_default() {
        let map = ShardMap::new(0);
        let strategy = map.get_strategy("unknown_class");
        assert_eq!(strategy, ShardStrategy::ClassBased { shard: 0 });
    }

    #[test]
    fn test_shard_map_class_assignment() {
        let mut map = ShardMap::new(0);
        map.shard_class("User", 1);
        map.shard_class("Order", 2);

        assert_eq!(
            map.get_strategy("User"),
            ShardStrategy::ClassBased { shard: 1 }
        );
        assert_eq!(
            map.get_strategy("Order"),
            ShardStrategy::ClassBased { shard: 2 }
        );
        assert_eq!(
            map.get_strategy("Product"),
            ShardStrategy::ClassBased { shard: 0 }
        ); // default
    }

    #[test]
    fn test_hash_deterministic() {
        let strategy = ShardStrategy::HashBased {
            num_shards: 8,
            slot_map: vec![0, 1, 2, 3, 4, 5, 6, 7],
        };
        // Same key should always route to same shard
        let t1 = strategy.route_key(b"user_123");
        let t2 = strategy.route_key(b"user_123");
        assert_eq!(t1, t2);
    }
}
