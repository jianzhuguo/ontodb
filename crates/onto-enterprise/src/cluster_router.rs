//! Cluster query router for OntoDB Enterprise.
//!
//! Provides:
//! - Read/write splitting
//! - Query routing based on consistency level
//! - Replica lag monitoring
//! - Automatic fallback to leader

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use parking_lot::RwLock;

/// Query routing strategy.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum RoutingStrategy {
    /// All queries go to leader.
    LeaderOnly,
    /// Reads go to followers, writes go to leader.
    ReadWriteSplit,
    /// Reads go to nearest healthy node, writes go to leader.
    NearestNode,
    /// Round-robin across all healthy nodes.
    RoundRobin,
}

/// Query type for routing decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryType {
    Read,
    Write,
    ReadWrite,
}

/// Replica health status.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplicaHealth {
    /// Node ID.
    pub node_id: u64,
    /// Is the node reachable?
    pub reachable: bool,
    /// Replication lag in milliseconds.
    pub lag_ms: u64,
    /// Last heartbeat timestamp.
    pub last_heartbeat_ms: u64,
    /// Query latency in milliseconds.
    pub latency_ms: f64,
}

/// Query routing result.
#[derive(Debug, Clone)]
pub struct RoutingDecision {
    /// Target node ID.
    pub target_node: u64,
    /// Reason for routing decision.
    pub reason: String,
    /// Fallback node if primary fails.
    pub fallback_node: Option<u64>,
}

/// Cluster query router.
pub struct ClusterRouter {
    strategy: RoutingStrategy,
    leader_id: Arc<RwLock<Option<u64>>>,
    replicas: Arc<RwLock<HashMap<u64, ReplicaHealth>>>,
    max_lag_ms: u64,
    round_robin_index: Arc<RwLock<usize>>,
}

impl ClusterRouter {
    /// Create a new cluster router.
    pub fn new(strategy: RoutingStrategy, max_lag_ms: u64) -> Self {
        Self {
            strategy,
            leader_id: Arc::new(RwLock::new(None)),
            replicas: Arc::new(RwLock::new(HashMap::new())),
            max_lag_ms,
            round_robin_index: Arc::new(RwLock::new(0)),
        }
    }

    /// Set the current leader node.
    pub fn set_leader(&self, node_id: u64) {
        *self.leader_id.write() = Some(node_id);
    }

    /// Get the current leader node.
    pub fn get_leader(&self) -> Option<u64> {
        *self.leader_id.read()
    }

    /// Update replica health.
    pub fn update_replica(&self, health: ReplicaHealth) {
        self.replicas.write().insert(health.node_id, health);
    }

    /// Remove a replica.
    pub fn remove_replica(&self, node_id: u64) {
        self.replicas.write().remove(&node_id);
    }

    /// Route a query to the appropriate node.
    pub fn route(&self, query_type: QueryType) -> Result<RoutingDecision> {
        let leader = *self.leader_id.read();

        match self.strategy {
            RoutingStrategy::LeaderOnly => {
                let target = leader.ok_or_else(|| anyhow::anyhow!("No leader elected"))?;
                Ok(RoutingDecision {
                    target_node: target,
                    reason: "Leader-only strategy".to_string(),
                    fallback_node: None,
                })
            }
            RoutingStrategy::ReadWriteSplit => {
                match query_type {
                    QueryType::Write | QueryType::ReadWrite => {
                        let target = leader.ok_or_else(|| anyhow::anyhow!("No leader elected"))?;
                        Ok(RoutingDecision {
                            target_node: target,
                            reason: "Write routed to leader".to_string(),
                            fallback_node: None,
                        })
                    }
                    QueryType::Read => {
                        // Find best replica (lowest lag, reachable)
                        let replicas = self.replicas.read();
                        let best_replica = replicas.values()
                            .filter(|r| r.reachable && r.lag_ms <= self.max_lag_ms)
                            .min_by_key(|r| r.lag_ms);

                        if let Some(replica) = best_replica {
                            Ok(RoutingDecision {
                                target_node: replica.node_id,
                                reason: format!("Read routed to replica (lag: {}ms)", replica.lag_ms),
                                fallback_node: leader,
                            })
                        } else {
                            // Fallback to leader
                            let target = leader.ok_or_else(|| anyhow::anyhow!("No leader elected"))?;
                            Ok(RoutingDecision {
                                target_node: target,
                                reason: "No healthy replica, fallback to leader".to_string(),
                                fallback_node: None,
                            })
                        }
                    }
                }
            }
            RoutingStrategy::NearestNode => {
                match query_type {
                    QueryType::Write | QueryType::ReadWrite => {
                        let target = leader.ok_or_else(|| anyhow::anyhow!("No leader elected"))?;
                        Ok(RoutingDecision {
                            target_node: target,
                            reason: "Write routed to leader".to_string(),
                            fallback_node: None,
                        })
                    }
                    QueryType::Read => {
                        let replicas = self.replicas.read();
                        let nearest = replicas.values()
                            .filter(|r| r.reachable && r.lag_ms <= self.max_lag_ms)
                            .min_by(|a, b| a.latency_ms.partial_cmp(&b.latency_ms).unwrap_or(std::cmp::Ordering::Equal));

                        if let Some(replica) = nearest {
                            Ok(RoutingDecision {
                                target_node: replica.node_id,
                                reason: format!("Read routed to nearest node (latency: {:.1}ms)", replica.latency_ms),
                                fallback_node: leader,
                            })
                        } else {
                            let target = leader.ok_or_else(|| anyhow::anyhow!("No leader elected"))?;
                            Ok(RoutingDecision {
                                target_node: target,
                                reason: "No healthy replica, fallback to leader".to_string(),
                                fallback_node: None,
                            })
                        }
                    }
                }
            }
            RoutingStrategy::RoundRobin => {
                let replicas = self.replicas.read();
                let healthy: Vec<u64> = replicas.values()
                    .filter(|r| r.reachable && r.lag_ms <= self.max_lag_ms)
                    .map(|r| r.node_id)
                    .collect();

                if healthy.is_empty() {
                    let target = leader.ok_or_else(|| anyhow::anyhow!("No healthy nodes"))?;
                    return Ok(RoutingDecision {
                        target_node: target,
                        reason: "No healthy replicas, fallback to leader".to_string(),
                        fallback_node: None,
                    });
                }

                let mut idx = self.round_robin_index.write();
                let target = healthy[*idx % healthy.len()];
                *idx += 1;

                Ok(RoutingDecision {
                    target_node: target,
                    reason: format!("Round-robin to node {}", target),
                    fallback_node: leader,
                })
            }
        }
    }

    /// Get all replica health statuses.
    pub fn get_replica_health(&self) -> Vec<ReplicaHealth> {
        self.replicas.read().values().cloned().collect()
    }

    /// Get healthy replica count.
    pub fn healthy_replica_count(&self) -> usize {
        self.replicas.read().values()
            .filter(|r| r.reachable && r.lag_ms <= self.max_lag_ms)
            .count()
    }

    /// Check if any replica exceeds lag threshold.
    pub fn has_lagging_replicas(&self) -> bool {
        self.replicas.read().values()
            .any(|r| r.reachable && r.lag_ms > self.max_lag_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_router(strategy: RoutingStrategy) -> ClusterRouter {
        let router = ClusterRouter::new(strategy, 1000);
        router.set_leader(1);
        router.update_replica(ReplicaHealth {
            node_id: 2,
            reachable: true,
            lag_ms: 10,
            last_heartbeat_ms: 1000,
            latency_ms: 5.0,
        });
        router.update_replica(ReplicaHealth {
            node_id: 3,
            reachable: true,
            lag_ms: 50,
            last_heartbeat_ms: 1000,
            latency_ms: 8.0,
        });
        router
    }

    #[test]
    fn test_leader_only() {
        let router = test_router(RoutingStrategy::LeaderOnly);
        let decision = router.route(QueryType::Read).unwrap();
        assert_eq!(decision.target_node, 1);
    }

    #[test]
    fn test_read_write_split_write() {
        let router = test_router(RoutingStrategy::ReadWriteSplit);
        let decision = router.route(QueryType::Write).unwrap();
        assert_eq!(decision.target_node, 1); // Leader
    }

    #[test]
    fn test_read_write_split_read() {
        let router = test_router(RoutingStrategy::ReadWriteSplit);
        let decision = router.route(QueryType::Read).unwrap();
        assert_eq!(decision.target_node, 2); // Best replica (lowest lag)
    }

    #[test]
    fn test_nearest_node() {
        let router = test_router(RoutingStrategy::NearestNode);
        let decision = router.route(QueryType::Read).unwrap();
        assert_eq!(decision.target_node, 2); // Lowest latency
    }

    #[test]
    fn test_round_robin() {
        let router = test_router(RoutingStrategy::RoundRobin);
        let d1 = router.route(QueryType::Read).unwrap();
        let d2 = router.route(QueryType::Read).unwrap();
        let d3 = router.route(QueryType::Read).unwrap();
        // Should cycle through replicas
        assert_ne!(d1.target_node, d2.target_node);
    }

    #[test]
    fn test_fallback_to_leader() {
        let router = ClusterRouter::new(RoutingStrategy::ReadWriteSplit, 1000);
        router.set_leader(1);
        // No replicas available
        let decision = router.route(QueryType::Read).unwrap();
        assert_eq!(decision.target_node, 1); // Fallback to leader
    }

    #[test]
    fn test_lag_threshold() {
        let router = ClusterRouter::new(RoutingStrategy::ReadWriteSplit, 100);
        router.set_leader(1);
        router.update_replica(ReplicaHealth {
            node_id: 2,
            reachable: true,
            lag_ms: 200, // Exceeds threshold
            last_heartbeat_ms: 1000,
            latency_ms: 5.0,
        });
        let decision = router.route(QueryType::Read).unwrap();
        assert_eq!(decision.target_node, 1); // Fallback to leader due to lag
    }

    #[test]
    fn test_healthy_replica_count() {
        let router = test_router(RoutingStrategy::LeaderOnly);
        assert_eq!(router.healthy_replica_count(), 2);
    }

    #[test]
    fn test_has_lagging_replicas() {
        let router = ClusterRouter::new(RoutingStrategy::ReadWriteSplit, 100);
        router.update_replica(ReplicaHealth {
            node_id: 2,
            reachable: true,
            lag_ms: 200,
            last_heartbeat_ms: 1000,
            latency_ms: 5.0,
        });
        assert!(router.has_lagging_replicas());
    }
}
