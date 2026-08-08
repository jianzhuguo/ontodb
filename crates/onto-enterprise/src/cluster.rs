//! Cluster management module (placeholder).
//!
//! TODO: Implement Raft-based multi-replica cluster management.

/// Cluster configuration placeholder.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ClusterConfig {
    pub enabled: bool,
    pub node_id: u64,
    pub peers: Vec<String>,
}

impl Default for ClusterConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            node_id: 1,
            peers: Vec::new(),
        }
    }
}
