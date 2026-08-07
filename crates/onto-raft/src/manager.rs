//! Raft node manager for OntoDB distributed cluster.

use std::collections::BTreeMap;

use openraft::BasicNode;

use crate::error::RaftError;
use crate::types::NodeId;

/// Configuration for a Raft node.
#[derive(Debug, Clone)]
pub struct RaftNodeConfig {
    /// This node's ID.
    pub node_id: NodeId,
    /// Address to listen on for Raft RPCs (e.g., "127.0.0.1:9000").
    pub listen_addr: String,
    /// Initial cluster members (id -> address).
    pub initial_members: BTreeMap<NodeId, String>,
}

/// Manages a Raft node in the OntoDB cluster.
pub struct RaftNodeManager {
    config: RaftNodeConfig,
    members: BTreeMap<NodeId, BasicNode>,
}

impl RaftNodeManager {
    /// Create a new Raft node manager.
    pub fn new(config: RaftNodeConfig) -> Self {
        let mut members = BTreeMap::new();
        for (id, addr) in &config.initial_members {
            members.insert(*id, BasicNode { addr: addr.clone() });
        }
        members.insert(config.node_id, BasicNode {
            addr: config.listen_addr.clone(),
        });

        Self { config, members }
    }

    /// Get the current cluster members.
    pub fn members(&self) -> &BTreeMap<NodeId, BasicNode> {
        &self.members
    }

    /// Add a member to the cluster configuration.
    pub fn add_member(&mut self, node_id: NodeId, addr: String) {
        self.members.insert(node_id, BasicNode { addr });
    }

    /// Remove a member from the cluster configuration.
    pub fn remove_member(&mut self, node_id: NodeId) {
        self.members.remove(&node_id);
    }

    /// Get this node's ID.
    pub fn node_id(&self) -> NodeId {
        self.config.node_id
    }

    /// Get this node's listen address.
    pub fn listen_addr(&self) -> &str {
        &self.config.listen_addr
    }

    /// Export cluster config as JSON.
    pub fn export_config(&self) -> String {
        serde_json::to_string_pretty(&serde_json::json!({
            "node_id": self.config.node_id,
            "listen_addr": self.config.listen_addr,
            "members": self.members.iter().map(|(id, node)| {
                serde_json::json!({
                    "id": id,
                    "addr": node.addr,
                })
            }).collect::<Vec<_>>(),
        })).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_raft_manager() {
        let mut initial = BTreeMap::new();
        initial.insert(2, "127.0.0.1:9001".to_string());

        let config = RaftNodeConfig {
            node_id: 1,
            listen_addr: "127.0.0.1:9000".to_string(),
            initial_members: initial,
        };
        let mut manager = RaftNodeManager::new(config);
        assert_eq!(manager.node_id(), 1);
        assert_eq!(manager.members().len(), 2);

        manager.add_member(3, "127.0.0.1:9002".to_string());
        assert_eq!(manager.members().len(), 3);

        manager.remove_member(2);
        assert_eq!(manager.members().len(), 2);
    }

    #[test]
    fn test_export_config() {
        let config = RaftNodeConfig {
            node_id: 1,
            listen_addr: "127.0.0.1:9000".to_string(),
            initial_members: BTreeMap::new(),
        };
        let manager = RaftNodeManager::new(config);
        let json = manager.export_config();
        assert!(json.contains("\"node_id\": 1"));
    }
}
