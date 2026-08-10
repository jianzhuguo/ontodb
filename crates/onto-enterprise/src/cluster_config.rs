//! Cluster configuration and metrics aggregation for OntoDB Enterprise.
//!
//! Provides:
//! - Cluster-wide configuration management
//! - Node health aggregation
//! - Cluster metrics collection
//! - Shard rebalancing support

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use parking_lot::RwLock;

/// Cluster-wide configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterWideConfig {
    /// Cluster name.
    pub cluster_name: String,
    /// Replication factor.
    pub replication_factor: u32,
    /// Read consistency level.
    pub read_consistency: ConsistencyLevel,
    /// Write consistency level.
    pub write_consistency: ConsistencyLevel,
    /// Auto-rebalance on node change.
    pub auto_rebalance: bool,
    /// Rebalance threshold (percentage of imbalance).
    pub rebalance_threshold_percent: u32,
    /// Max concurrent rebalancing operations.
    pub max_rebalance_ops: u32,
}

/// Consistency level.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ConsistencyLevel {
    /// Read/write from any single node.
    One,
    /// Read/write from quorum.
    Quorum,
    /// Read/write from all nodes.
    All,
}

impl Default for ClusterWideConfig {
    fn default() -> Self {
        Self {
            cluster_name: "ontodb-cluster".to_string(),
            replication_factor: 3,
            read_consistency: ConsistencyLevel::Quorum,
            write_consistency: ConsistencyLevel::Quorum,
            auto_rebalance: true,
            rebalance_threshold_percent: 20,
            max_rebalance_ops: 4,
        }
    }
}

/// Aggregated cluster health.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterHealth {
    /// Overall cluster status.
    pub status: ClusterStatus,
    /// Total nodes.
    pub total_nodes: u32,
    /// Healthy nodes.
    pub healthy_nodes: u32,
    /// Unhealthy nodes.
    pub unhealthy_nodes: u32,
    /// Is quorum met?
    pub quorum_met: bool,
    /// Leader node ID.
    pub leader_id: Option<u64>,
    /// Cluster generation.
    pub generation: u64,
    /// Uptime in seconds.
    pub uptime_seconds: u64,
}

/// Cluster status.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ClusterStatus {
    /// All nodes healthy.
    Healthy,
    /// Some nodes unhealthy, but quorum met.
    Degraded,
    /// Quorum not met.
    Critical,
    /// Cluster is initializing.
    Initializing,
}

/// Aggregated cluster metrics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterMetrics {
    /// Total queries per second across all nodes.
    pub total_qps: f64,
    /// Average query latency in milliseconds.
    pub avg_latency_ms: f64,
    /// P99 query latency in milliseconds.
    pub p99_latency_ms: f64,
    /// Total storage entries across all nodes.
    pub total_entries: u64,
    /// Total disk usage in bytes.
    pub total_disk_bytes: u64,
    /// Total memory usage in bytes.
    pub total_memory_bytes: u64,
    /// Total connections across all nodes.
    pub total_connections: u64,
    /// Per-node metrics.
    pub node_metrics: HashMap<u64, NodeMetricsSummary>,
}

/// Node metrics summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeMetricsSummary {
    pub qps: f64,
    pub latency_ms: f64,
    pub entries: u64,
    pub disk_bytes: u64,
    pub memory_bytes: u64,
    pub connections: u64,
}

/// Rebalance operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RebalanceOperation {
    /// Operation ID.
    pub id: String,
    /// Status.
    pub status: RebalanceStatus,
    /// Source shard.
    pub source_shard: u32,
    /// Target node.
    pub target_node: u64,
    /// Progress (0-100).
    pub progress: u32,
    /// Entries transferred.
    pub entries_transferred: u64,
    /// Total entries to transfer.
    pub entries_total: u64,
    /// Started at (Unix millis).
    pub started_at: u64,
}

/// Rebalance status.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum RebalanceStatus {
    Pending,
    Running,
    Completed,
    Failed,
}

/// Cluster configuration manager.
pub struct ClusterConfigManager {
    config: Arc<RwLock<ClusterWideConfig>>,
    rebalance_ops: Arc<RwLock<Vec<RebalanceOperation>>>,
}

impl ClusterConfigManager {
    /// Create a new cluster config manager.
    pub fn new(config: ClusterWideConfig) -> Self {
        Self {
            config: Arc::new(RwLock::new(config)),
            rebalance_ops: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Get current cluster configuration.
    pub fn get_config(&self) -> ClusterWideConfig {
        self.config.read().clone()
    }

    /// Update cluster configuration.
    pub fn update_config(&self, new_config: ClusterWideConfig) -> Result<()> {
        let mut config = self.config.write();
        *config = new_config;
        Ok(())
    }

    /// Update replication factor.
    pub fn set_replication_factor(&self, factor: u32) -> Result<()> {
        if factor == 0 {
            anyhow::bail!("Replication factor must be > 0");
        }
        self.config.write().replication_factor = factor;
        Ok(())
    }

    /// Update consistency levels.
    pub fn set_consistency(&self, read: ConsistencyLevel, write: ConsistencyLevel) {
        let mut config = self.config.write();
        config.read_consistency = read;
        config.write_consistency = write;
    }

    /// Aggregate health from multiple nodes.
    pub fn aggregate_health(&self, node_healths: &[(u64, bool)], leader_id: Option<u64>, generation: u64) -> ClusterHealth {
        let total = node_healths.len() as u32;
        let healthy = node_healths.iter().filter(|(_, h)| *h).count() as u32;
        let unhealthy = total - healthy;

        let config = self.config.read();
        let quorum_size = (config.replication_factor / 2) + 1;
        let quorum_met = healthy >= quorum_size;

        let status = if healthy == total {
            ClusterStatus::Healthy
        } else if quorum_met {
            ClusterStatus::Degraded
        } else {
            ClusterStatus::Critical
        };

        ClusterHealth {
            status,
            total_nodes: total,
            healthy_nodes: healthy,
            unhealthy_nodes: unhealthy,
            quorum_met,
            leader_id,
            generation,
            uptime_seconds: 0,
        }
    }

    /// Aggregate metrics from multiple nodes.
    pub fn aggregate_metrics(&self, node_metrics: &[(u64, NodeMetricsSummary)]) -> ClusterMetrics {
        let mut total_qps = 0.0;
        let mut total_latency = 0.0;
        let mut total_entries = 0u64;
        let mut total_disk = 0u64;
        let mut total_memory = 0u64;
        let mut total_connections = 0u64;
        let mut metrics_map = HashMap::new();

        for (node_id, metrics) in node_metrics {
            total_qps += metrics.qps;
            total_latency += metrics.latency_ms;
            total_entries += metrics.entries;
            total_disk += metrics.disk_bytes;
            total_memory += metrics.memory_bytes;
            total_connections += metrics.connections;
            metrics_map.insert(*node_id, metrics.clone());
        }

        let avg_latency = if node_metrics.is_empty() { 0.0 } else { total_latency / node_metrics.len() as f64 };

        ClusterMetrics {
            total_qps,
            avg_latency_ms: avg_latency,
            p99_latency_ms: avg_latency * 1.5, // Estimate
            total_entries,
            total_disk_bytes: total_disk,
            total_memory_bytes: total_memory,
            total_connections,
            node_metrics: metrics_map,
        }
    }

    /// Check if rebalancing is needed.
    pub fn needs_rebalance(&self, shard_sizes: &[(u32, u64)]) -> bool {
        if shard_sizes.len() < 2 {
            return false;
        }

        let config = self.config.read();
        if !config.auto_rebalance {
            return false;
        }

        let avg_size: f64 = shard_sizes.iter().map(|(_, s)| *s as f64).sum::<f64>() / shard_sizes.len() as f64;
        let threshold = avg_size * config.rebalance_threshold_percent as f64 / 100.0;

        shard_sizes.iter().any(|(_, size)| {
            let diff = (*size as f64 - avg_size).abs();
            diff > threshold
        })
    }

    /// Get rebalance operations.
    pub fn get_rebalance_ops(&self) -> Vec<RebalanceOperation> {
        self.rebalance_ops.read().clone()
    }

    /// Add a rebalance operation.
    pub fn add_rebalance_op(&self, op: RebalanceOperation) {
        self.rebalance_ops.write().push(op);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cluster_config_default() {
        let config = ClusterWideConfig::default();
        assert_eq!(config.cluster_name, "ontodb-cluster");
        assert_eq!(config.replication_factor, 3);
        assert_eq!(config.read_consistency, ConsistencyLevel::Quorum);
        assert_eq!(config.write_consistency, ConsistencyLevel::Quorum);
        assert!(config.auto_rebalance);
    }

    #[test]
    fn test_set_replication_factor() {
        let manager = ClusterConfigManager::new(ClusterWideConfig::default());
        manager.set_replication_factor(5).unwrap();
        assert_eq!(manager.get_config().replication_factor, 5);
    }

    #[test]
    fn test_set_replication_factor_zero() {
        let manager = ClusterConfigManager::new(ClusterWideConfig::default());
        assert!(manager.set_replication_factor(0).is_err());
    }

    #[test]
    fn test_aggregate_health_all_healthy() {
        let manager = ClusterConfigManager::new(ClusterWideConfig::default());
        let health = manager.aggregate_health(&[(1, true), (2, true), (3, true)], Some(1), 1);
        assert_eq!(health.status, ClusterStatus::Healthy);
        assert_eq!(health.healthy_nodes, 3);
        assert_eq!(health.unhealthy_nodes, 0);
        assert!(health.quorum_met);
    }

    #[test]
    fn test_aggregate_health_degraded() {
        let manager = ClusterConfigManager::new(ClusterWideConfig::default());
        let health = manager.aggregate_health(&[(1, true), (2, true), (3, false)], Some(1), 1);
        assert_eq!(health.status, ClusterStatus::Degraded);
        assert_eq!(health.healthy_nodes, 2);
        assert_eq!(health.unhealthy_nodes, 1);
        assert!(health.quorum_met);
    }

    #[test]
    fn test_aggregate_health_critical() {
        let manager = ClusterConfigManager::new(ClusterWideConfig::default());
        let health = manager.aggregate_health(&[(1, true), (2, false), (3, false)], Some(1), 1);
        assert_eq!(health.status, ClusterStatus::Critical);
        assert_eq!(health.healthy_nodes, 1);
        assert_eq!(health.unhealthy_nodes, 2);
        assert!(!health.quorum_met);
    }

    #[test]
    fn test_aggregate_metrics() {
        let manager = ClusterConfigManager::new(ClusterWideConfig::default());
        let metrics = manager.aggregate_metrics(&[
            (1, NodeMetricsSummary { qps: 1000.0, latency_ms: 5.0, entries: 10000, disk_bytes: 1024, memory_bytes: 512, connections: 10 }),
            (2, NodeMetricsSummary { qps: 800.0, latency_ms: 8.0, entries: 8000, disk_bytes: 800, memory_bytes: 400, connections: 8 }),
        ]);
        assert_eq!(metrics.total_qps, 1800.0);
        assert_eq!(metrics.total_entries, 18000);
        assert_eq!(metrics.total_connections, 18);
    }

    #[test]
    fn test_needs_rebalance() {
        let manager = ClusterConfigManager::new(ClusterWideConfig {
            auto_rebalance: true,
            rebalance_threshold_percent: 20,
            ..Default::default()
        });

        // Balanced
        assert!(!manager.needs_rebalance(&[(0, 100), (1, 100), (2, 100)]));

        // Imbalanced (>20% deviation)
        assert!(manager.needs_rebalance(&[(0, 100), (1, 100), (2, 150)]));
    }

    #[test]
    fn test_needs_rebalance_disabled() {
        let manager = ClusterConfigManager::new(ClusterWideConfig {
            auto_rebalance: false,
            ..Default::default()
        });
        assert!(!manager.needs_rebalance(&[(0, 100), (1, 200)]));
    }

    #[test]
    fn test_update_config() {
        let manager = ClusterConfigManager::new(ClusterWideConfig::default());
        let mut new_config = ClusterWideConfig::default();
        new_config.replication_factor = 5;
        manager.update_config(new_config).unwrap();
        assert_eq!(manager.get_config().replication_factor, 5);
    }
}
