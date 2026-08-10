//! Cluster management module for OntoDB Enterprise.
//!
//! Provides:
//! - Cluster node management
//! - Health monitoring and heartbeats
//! - Automatic failover coordination
//! - Configuration synchronization
//! - Cluster-wide metrics aggregation

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use parking_lot::RwLock;

/// Cluster configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterConfig {
    /// Enable cluster mode.
    pub enabled: bool,
    /// This node's ID (must be unique in cluster).
    pub node_id: u64,
    /// Peer nodes (format: "node_id@host:port").
    pub peers: Vec<String>,
    /// Heartbeat interval in milliseconds.
    pub heartbeat_interval_ms: u64,
    /// Node timeout in milliseconds (considered dead after this).
    pub node_timeout_ms: u64,
    /// Enable automatic failover.
    pub auto_failover: bool,
    /// Minimum nodes required for quorum.
    pub min_quorum: usize,
}

impl Default for ClusterConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            node_id: 1,
            peers: Vec::new(),
            heartbeat_interval_ms: 1000,
            node_timeout_ms: 5000,
            auto_failover: true,
            min_quorum: 2,
        }
    }
}

/// Node status in the cluster.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeStatus {
    /// Node is healthy and responsive.
    Healthy,
    /// Node is degraded (slow responses).
    Degraded,
    /// Node is unreachable.
    Unreachable,
    /// Node is in maintenance mode.
    Maintenance,
    /// Node has left the cluster.
    Left,
}

/// Cluster node information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterNode {
    /// Node ID.
    pub id: u64,
    /// Node address (host:port).
    pub address: String,
    /// Current status.
    pub status: NodeStatus,
    /// Is this node the leader?
    pub is_leader: bool,
    /// Is this node a voter (vs observer)?
    pub is_voter: bool,
    /// Last heartbeat timestamp (Unix millis).
    pub last_heartbeat_ms: u64,
    /// Node metrics.
    pub metrics: NodeMetrics,
}

/// Node-level metrics.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NodeMetrics {
    /// CPU usage percentage (0-100).
    pub cpu_percent: f64,
    /// Memory usage percentage (0-100).
    pub memory_percent: f64,
    /// Disk usage percentage (0-100).
    pub disk_percent: f64,
    /// Active connections.
    pub connections: u64,
    /// Queries per second.
    pub qps: f64,
    /// Storage entries count.
    pub storage_entries: u64,
}

/// Cluster state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterState {
    /// Cluster name/ID.
    pub cluster_id: String,
    /// Current leader node ID.
    pub leader_id: Option<u64>,
    /// All nodes in the cluster.
    pub nodes: Vec<ClusterNode>,
    /// Cluster generation (increments on topology changes).
    pub generation: u64,
    /// Is the cluster in a healthy state?
    pub healthy: bool,
}

/// Cluster event for monitoring.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClusterEvent {
    /// Node joined the cluster.
    NodeJoined { node_id: u64, address: String },
    /// Node left the cluster.
    NodeLeft { node_id: u64, reason: String },
    /// Node status changed.
    NodeStatusChanged { node_id: u64, old: NodeStatus, new: NodeStatus },
    /// Leader changed.
    LeaderChanged { old_leader: Option<u64>, new_leader: u64 },
    /// Failover occurred.
    Failover { failed_node: u64, new_leader: u64 },
}

/// Cluster manager.
pub struct ClusterManager {
    config: ClusterConfig,
    nodes: Arc<RwLock<HashMap<u64, ClusterNode>>>,
    leader_id: Arc<RwLock<Option<u64>>>,
    generation: Arc<RwLock<u64>>,
    events: Arc<RwLock<Vec<ClusterEvent>>>,
}

impl ClusterManager {
    /// Create a new cluster manager.
    pub fn new(config: ClusterConfig) -> Self {
        let mut nodes = HashMap::new();
        
        // Register self
        nodes.insert(config.node_id, ClusterNode {
            id: config.node_id,
            address: format!("localhost:7912"), // Will be configured
            status: NodeStatus::Healthy,
            is_leader: false,
            is_voter: true,
            last_heartbeat_ms: current_timestamp_ms(),
            metrics: NodeMetrics::default(),
        });

        Self {
            config,
            nodes: Arc::new(RwLock::new(nodes)),
            leader_id: Arc::new(RwLock::new(None)),
            generation: Arc::new(RwLock::new(1)),
            events: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Get the current cluster state.
    pub fn state(&self) -> ClusterState {
        let nodes = self.nodes.read();
        let leader_id = *self.leader_id.read();
        let generation = *self.generation.read();

        ClusterState {
            cluster_id: format!("ontodb-cluster-{}", self.config.node_id),
            leader_id,
            nodes: nodes.values().cloned().collect(),
            generation,
            healthy: self.is_healthy(),
        }
    }

    /// Check if the cluster is healthy (quorum met).
    pub fn is_healthy(&self) -> bool {
        let nodes = self.nodes.read();
        let healthy_voters = nodes.values()
            .filter(|n| n.is_voter && n.status == NodeStatus::Healthy)
            .count();
        healthy_voters >= self.config.min_quorum
    }

    /// Get a specific node.
    pub fn get_node(&self, node_id: u64) -> Option<ClusterNode> {
        self.nodes.read().get(&node_id).cloned()
    }

    /// Get all nodes.
    pub fn get_nodes(&self) -> Vec<ClusterNode> {
        self.nodes.read().values().cloned().collect()
    }

    /// Get the current leader node.
    pub fn get_leader(&self) -> Option<ClusterNode> {
        let leader_id = *self.leader_id.read();
        leader_id.and_then(|id| self.nodes.read().get(&id).cloned())
    }

    /// Add a node to the cluster.
    pub fn add_node(&self, id: u64, address: String, is_voter: bool) -> Result<()> {
        let mut nodes = self.nodes.write();
        
        if nodes.contains_key(&id) {
            anyhow::bail!("Node {} already exists", id);
        }

        nodes.insert(id, ClusterNode {
            id,
            address,
            status: NodeStatus::Healthy,
            is_leader: false,
            is_voter,
            last_heartbeat_ms: current_timestamp_ms(),
            metrics: NodeMetrics::default(),
        });

        // Increment generation
        let mut gen = self.generation.write();
        *gen += 1;

        // Record event
        self.events.write().push(ClusterEvent::NodeJoined {
            node_id: id,
            address: nodes.get(&id).unwrap().address.clone(),
        });

        tracing::info!("Node {} joined cluster", id);
        Ok(())
    }

    /// Remove a node from the cluster.
    pub fn remove_node(&self, node_id: u64) -> Result<()> {
        if node_id == self.config.node_id {
            anyhow::bail!("Cannot remove self from cluster");
        }

        let mut nodes = self.nodes.write();
        let node = nodes.remove(&node_id)
            .ok_or_else(|| anyhow::anyhow!("Node {} not found", node_id))?;

        // Increment generation
        let mut gen = self.generation.write();
        *gen += 1;

        // Record event
        self.events.write().push(ClusterEvent::NodeLeft {
            node_id,
            reason: "removed".to_string(),
        });

        // If removed node was leader, clear leader
        if Some(node_id) == *self.leader_id.read() {
            *self.leader_id.write() = None;
        }

        tracing::info!("Node {} removed from cluster", node_id);
        Ok(())
    }

    /// Update node status.
    pub fn update_node_status(&self, node_id: u64, status: NodeStatus) -> Result<()> {
        let mut nodes = self.nodes.write();
        let node = nodes.get_mut(&node_id)
            .ok_or_else(|| anyhow::anyhow!("Node {} not found", node_id))?;

        let old_status = node.status;
        if old_status != status {
            node.status = status;
            
            self.events.write().push(ClusterEvent::NodeStatusChanged {
                node_id,
                old: old_status,
                new: status,
            });

            tracing::info!("Node {} status: {:?} -> {:?}", node_id, old_status, status);
        }

        Ok(())
    }

    /// Update node metrics.
    pub fn update_node_metrics(&self, node_id: u64, metrics: NodeMetrics) -> Result<()> {
        let mut nodes = self.nodes.write();
        let node = nodes.get_mut(&node_id)
            .ok_or_else(|| anyhow::anyhow!("Node {} not found", node_id))?;

        node.metrics = metrics;
        node.last_heartbeat_ms = current_timestamp_ms();
        Ok(())
    }

    /// Set the leader node.
    pub fn set_leader(&self, node_id: u64) -> Result<()> {
        let old_leader = *self.leader_id.read();
        
        // Clear old leader flag
        if let Some(old_id) = old_leader {
            if let Some(node) = self.nodes.write().get_mut(&old_id) {
                node.is_leader = false;
            }
        }

        // Set new leader
        {
            let mut nodes = self.nodes.write();
            let node = nodes.get_mut(&node_id)
                .ok_or_else(|| anyhow::anyhow!("Node {} not found", node_id))?;
            node.is_leader = true;
        }

        *self.leader_id.write() = Some(node_id);

        // Record event
        self.events.write().push(ClusterEvent::LeaderChanged {
            old_leader,
            new_leader: node_id,
        });

        tracing::info!("Leader changed: {:?} -> {}", old_leader, node_id);
        Ok(())
    }

    /// Process heartbeat from a node.
    pub fn heartbeat(&self, node_id: u64, metrics: Option<NodeMetrics>) -> Result<()> {
        let mut nodes = self.nodes.write();
        let node = nodes.get_mut(&node_id)
            .ok_or_else(|| anyhow::anyhow!("Node {} not found", node_id))?;

        node.last_heartbeat_ms = current_timestamp_ms();
        
        if let Some(m) = metrics {
            node.metrics = m;
        }

        // If node was unreachable, mark as healthy
        if node.status == NodeStatus::Unreachable {
            node.status = NodeStatus::Healthy;
            tracing::info!("Node {} recovered", node_id);
        }

        Ok(())
    }

    /// Check for timed-out nodes (should be called periodically).
    pub fn check_timeouts(&self) -> Vec<u64> {
        let now = current_timestamp_ms();
        let timeout_ms = self.config.node_timeout_ms;
        let mut timed_out = Vec::new();

        let mut nodes = self.nodes.write();
        for node in nodes.values_mut() {
            if node.id == self.config.node_id {
                continue; // Skip self
            }
            if node.status == NodeStatus::Healthy || node.status == NodeStatus::Degraded {
                if now - node.last_heartbeat_ms > timeout_ms {
                    node.status = NodeStatus::Unreachable;
                    timed_out.push(node.id);
                    tracing::warn!("Node {} timed out", node.id);
                }
            }
        }

        timed_out
    }

    /// Attempt automatic failover if leader is unreachable.
    pub fn try_failover(&self) -> Option<u64> {
        if !self.config.auto_failover {
            return None;
        }

        let leader_id = *self.leader_id.read();
        
        // Check if current leader is unreachable
        if let Some(lid) = leader_id {
            let nodes = self.nodes.read();
            if let Some(leader) = nodes.get(&lid) {
                if leader.status != NodeStatus::Unreachable {
                    return None; // Leader is fine
                }
            }
        }

        // Find best candidate (healthy voter with lowest ID)
        let nodes = self.nodes.read();
        let candidate = nodes.values()
            .filter(|n| n.is_voter && n.status == NodeStatus::Healthy && n.id != self.config.node_id)
            .min_by_key(|n| n.id);

        if let Some(new_leader) = candidate {
            let new_leader_id = new_leader.id;
            drop(nodes);

            // Set new leader
            if self.set_leader(new_leader_id).is_ok() {
                self.events.write().push(ClusterEvent::Failover {
                    failed_node: leader_id.unwrap_or(0),
                    new_leader: new_leader_id,
                });
                return Some(new_leader_id);
            }
        }

        None
    }

    /// Get recent cluster events.
    pub fn get_events(&self, limit: usize) -> Vec<ClusterEvent> {
        let events = self.events.read();
        let start = if events.len() > limit { events.len() - limit } else { 0 };
        events[start..].to_vec()
    }

    /// Get cluster configuration.
    pub fn config(&self) -> &ClusterConfig {
        &self.config
    }

    /// Get the node ID of this node.
    pub fn node_id(&self) -> u64 {
        self.config.node_id
    }
}

/// Get current timestamp in milliseconds.
fn current_timestamp_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> ClusterConfig {
        ClusterConfig {
            enabled: true,
            node_id: 1,
            peers: vec!["2@localhost:7913".to_string(), "3@localhost:7914".to_string()],
            heartbeat_interval_ms: 1000,
            node_timeout_ms: 5000,
            auto_failover: true,
            min_quorum: 2,
        }
    }

    #[test]
    fn test_cluster_manager_new() {
        let config = test_config();
        let manager = ClusterManager::new(config);
        assert_eq!(manager.node_id(), 1);
        assert_eq!(manager.get_nodes().len(), 1);
    }

    #[test]
    fn test_add_node() {
        let config = test_config();
        let manager = ClusterManager::new(config);

        manager.add_node(2, "localhost:7913".to_string(), true).unwrap();
        assert_eq!(manager.get_nodes().len(), 2);
        
        let node = manager.get_node(2).unwrap();
        assert_eq!(node.status, NodeStatus::Healthy);
        assert!(node.is_voter);
    }

    #[test]
    fn test_remove_node() {
        let config = test_config();
        let manager = ClusterManager::new(config);

        manager.add_node(2, "localhost:7913".to_string(), true).unwrap();
        assert_eq!(manager.get_nodes().len(), 2);

        manager.remove_node(2).unwrap();
        assert_eq!(manager.get_nodes().len(), 1);
    }

    #[test]
    fn test_remove_self_fails() {
        let config = test_config();
        let manager = ClusterManager::new(config);

        let result = manager.remove_node(1);
        assert!(result.is_err());
    }

    #[test]
    fn test_set_leader() {
        let config = test_config();
        let manager = ClusterManager::new(config);

        manager.add_node(2, "localhost:7913".to_string(), true).unwrap();
        manager.set_leader(2).unwrap();

        let leader = manager.get_leader().unwrap();
        assert_eq!(leader.id, 2);
        assert!(leader.is_leader);
    }

    #[test]
    fn test_heartbeat() {
        let config = test_config();
        let manager = ClusterManager::new(config);

        manager.add_node(2, "localhost:7913".to_string(), true).unwrap();
        manager.heartbeat(2, None).unwrap();

        let node = manager.get_node(2).unwrap();
        assert_eq!(node.status, NodeStatus::Healthy);
    }

    #[test]
    fn test_cluster_state() {
        let config = test_config();
        let manager = ClusterManager::new(config);

        manager.add_node(2, "localhost:7913".to_string(), true).unwrap();
        
        let state = manager.state();
        assert_eq!(state.nodes.len(), 2);
        assert!(state.healthy);
    }

    #[test]
    fn test_is_healthy() {
        let config = ClusterConfig {
            min_quorum: 3,
            ..test_config()
        };
        let manager = ClusterManager::new(config);

        // Only 1 node, need 3 for quorum
        assert!(!manager.is_healthy());

        manager.add_node(2, "localhost:7913".to_string(), true).unwrap();
        assert!(!manager.is_healthy());

        manager.add_node(3, "localhost:7914".to_string(), true).unwrap();
        assert!(manager.is_healthy());
    }

    #[test]
    fn test_events() {
        let config = test_config();
        let manager = ClusterManager::new(config);

        manager.add_node(2, "localhost:7913".to_string(), true).unwrap();
        manager.set_leader(2).unwrap();

        let events = manager.get_events(10);
        assert!(events.len() >= 2); // NodeJoined + LeaderChanged
    }
}
