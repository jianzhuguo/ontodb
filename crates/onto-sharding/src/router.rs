// Copyright (c) 2024-2026 OntoDB Team
// Licensed under the Business Source License 1.1 (BUSL-1.1).
// See LICENSE for details. Change Date: 2031-09-15.
// On the Change Date, this file will be licensed under Apache License 2.0.
//! Shard router for query execution.
//!
//! The ShardRouter determines which shard(s) a query should be routed to,
//! based on the target class and filter conditions.

use crate::strategy::ShardTarget;
use crate::types::{ShardId, ShardMap};

/// Routes queries to the correct shard(s).
///
/// The router is stateless — it reads from a ShardMap and returns routing decisions.
/// It is designed to be embedded in the query executor.
pub struct ShardRouter {
    shard_map: ShardMap,
    /// The local node's shard assignments.
    local_shards: Vec<ShardId>,
}

impl ShardRouter {
    pub fn new(shard_map: ShardMap, local_shards: Vec<ShardId>) -> Self {
        Self {
            shard_map,
            local_shards,
        }
    }

    /// Route a point operation (GET/PUT/DELETE) by class and key.
    pub fn route_key(&self, class: &str, key: &[u8]) -> ShardTarget {
        let strategy = self.shard_map.get_strategy(class);
        strategy.route_key(key)
    }

    /// Route a full table scan for a class.
    pub fn route_scan(&self, class: &str) -> ShardTarget {
        let strategy = self.shard_map.get_strategy(class);
        strategy.route_scan()
    }

    /// Route a range query for a class.
    pub fn route_range(&self, class: &str, start: &[u8], end: &[u8]) -> ShardTarget {
        let strategy = self.shard_map.get_strategy(class);
        strategy.route_range(start, end)
    }

    /// Check if a shard target is local (handled by this node).
    pub fn is_local(&self, shard: ShardId) -> bool {
        self.local_shards.contains(&shard)
    }

    /// Filter a ShardTarget to only include local shards.
    pub fn filter_local(&self, target: ShardTarget) -> ShardTarget {
        match target {
            ShardTarget::Single(shard) => {
                if self.is_local(shard) {
                    ShardTarget::Single(shard)
                } else {
                    ShardTarget::Multi(vec![]) // not local
                }
            }
            ShardTarget::Multi(shards) => {
                let local: Vec<ShardId> =
                    shards.into_iter().filter(|s| self.is_local(*s)).collect();
                ShardTarget::Multi(local)
            }
            ShardTarget::All => ShardTarget::Multi(self.local_shards.clone()),
        }
    }

    /// Get the shard map.
    pub fn shard_map(&self) -> &ShardMap {
        &self.shard_map
    }

    /// Get local shards.
    pub fn local_shards(&self) -> &[ShardId] {
        &self.local_shards
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ShardMap;

    fn make_router() -> ShardRouter {
        let mut map = ShardMap::new(0);
        map.shard_class("User", 1);
        map.shard_class("Order", 2);
        map.shard_class("Product", 0);
        ShardRouter::new(map, vec![0, 1]) // this node has shards 0 and 1
    }

    #[test]
    fn test_route_key_class_based() {
        let router = make_router();
        assert_eq!(router.route_key("User", b"u1"), ShardTarget::Single(1));
        assert_eq!(router.route_key("Order", b"o1"), ShardTarget::Single(2));
        assert_eq!(router.route_key("Product", b"p1"), ShardTarget::Single(0));
    }

    #[test]
    fn test_route_scan() {
        let router = make_router();
        assert_eq!(router.route_scan("User"), ShardTarget::Single(1));
        assert_eq!(router.route_scan("Product"), ShardTarget::Single(0));
    }

    #[test]
    fn test_filter_local() {
        let router = make_router();
        // Shard 1 is local
        assert_eq!(
            router.filter_local(ShardTarget::Single(1)),
            ShardTarget::Single(1)
        );
        // Shard 2 is NOT local
        assert_eq!(
            router.filter_local(ShardTarget::Single(2)),
            ShardTarget::Multi(vec![])
        );
        // All → only local shards
        assert_eq!(
            router.filter_local(ShardTarget::All),
            ShardTarget::Multi(vec![0, 1])
        );
    }

    #[test]
    fn test_is_local() {
        let router = make_router();
        assert!(router.is_local(0));
        assert!(router.is_local(1));
        assert!(!router.is_local(2));
    }
}
