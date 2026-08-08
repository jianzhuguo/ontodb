//! Rolling upgrade module for cross-version compatibility.
//!
//! Provides:
//! - Version negotiation between nodes
//! - Data format compatibility checks
//! - Upgrade coordination

use std::collections::HashMap;
use std::sync::Arc;
use parking_lot::RwLock;

/// Version information.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl Version {
    pub fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self { major, minor, patch }
    }

    /// Check if this version is compatible with another version.
    /// Compatible means same major version.
    pub fn is_compatible_with(&self, other: &Version) -> bool {
        self.major == other.major
    }

    /// Get the data format version for this release.
    pub fn data_format_version(&self) -> u32 {
        // Major version changes may introduce breaking data format changes
        self.major * 1000 + self.minor
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Rolling upgrade configuration.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RollingUpgradeConfig {
    /// Enable rolling upgrade support.
    pub enabled: bool,
    /// Current node version.
    pub current_version: Version,
    /// Minimum compatible version (for downgrade protection).
    pub min_compatible_version: Version,
    /// Maximum version skew allowed between nodes.
    pub max_version_skew: u32,
}

impl Default for RollingUpgradeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            current_version: Version::new(0, 1, 0),
            min_compatible_version: Version::new(0, 1, 0),
            max_version_skew: 1,
        }
    }
}

/// Node version info for cluster.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct NodeVersionInfo {
    pub node_id: u64,
    pub version: Version,
    pub data_format_version: u32,
    pub upgrade_ready: bool,
}

/// Rolling upgrade manager.
#[derive(Clone)]
pub struct RollingUpgradeManager {
    config: RollingUpgradeConfig,
    /// Known node versions in the cluster.
    node_versions: Arc<RwLock<HashMap<u64, Version>>>,
    /// Upgrade state.
    upgrade_state: Arc<RwLock<UpgradeState>>,
}

/// Upgrade state.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum UpgradeState {
    /// No upgrade in progress.
    Idle,
    /// Upgrade in progress — waiting for all nodes to be ready.
    Preparing {
        target_version: Version,
        ready_nodes: Vec<u64>,
        pending_nodes: Vec<u64>,
    },
    /// All nodes ready — performing rolling restart.
    InProgress {
        target_version: Version,
        completed_nodes: Vec<u64>,
        remaining_nodes: Vec<u64>,
    },
}

impl RollingUpgradeManager {
    /// Create a new rolling upgrade manager.
    pub fn new(config: RollingUpgradeConfig) -> Self {
        Self {
            config,
            node_versions: Arc::new(RwLock::new(HashMap::new())),
            upgrade_state: Arc::new(RwLock::new(UpgradeState::Idle)),
        }
    }

    /// Register a node's version.
    pub fn register_node(&self, node_id: u64, version: Version) {
        self.node_versions.write().insert(node_id, version);
    }

    /// Remove a node.
    pub fn unregister_node(&self, node_id: u64) {
        self.node_versions.write().remove(&node_id);
    }

    /// Check if the cluster is ready for upgrade to target version.
    pub fn check_upgrade_readiness(&self, target: &Version) -> UpgradeReadiness {
        if !self.config.enabled {
            return UpgradeReadiness {
                ready: false,
                reason: "Rolling upgrade not enabled".to_string(),
                incompatible_nodes: Vec::new(),
            };
        }

        // Check if target version is compatible with current
        if !self.config.current_version.is_compatible_with(target) {
            return UpgradeReadiness {
                ready: false,
                reason: format!(
                    "Version {} is not compatible with current {}",
                    target, self.config.current_version
                ),
                incompatible_nodes: Vec::new(),
            };
        }

        // Check all nodes in cluster
        let versions = self.node_versions.read();
        let mut incompatible = Vec::new();

        for (node_id, version) in versions.iter() {
            if !version.is_compatible_with(&self.config.current_version) {
                incompatible.push(*node_id);
            }
        }

        if !incompatible.is_empty() {
            return UpgradeReadiness {
                ready: false,
                reason: format!("{} nodes have incompatible versions", incompatible.len()),
                incompatible_nodes: incompatible,
            };
        }

        UpgradeReadiness {
            ready: true,
            reason: "Cluster ready for upgrade".to_string(),
            incompatible_nodes: Vec::new(),
        }
    }

    /// Start upgrade preparation.
    pub fn start_upgrade(&self, target: Version) -> anyhow::Result<()> {
        let readiness = self.check_upgrade_readiness(&target);
        if !readiness.ready {
            anyhow::bail!("Cluster not ready: {}", readiness.reason);
        }

        let target_version_str = target.to_string();
        let node_ids: Vec<u64> = self.node_versions.read().keys().copied().collect();

        *self.upgrade_state.write() = UpgradeState::Preparing {
            target_version: target,
            ready_nodes: Vec::new(),
            pending_nodes: node_ids,
        };

        tracing::info!("Upgrade preparation started for version {}", target_version_str);
        Ok(())
    }

    /// Mark a node as ready for upgrade.
    pub fn mark_node_ready(&self, node_id: u64) {
        let mut state = self.upgrade_state.write();
        if let UpgradeState::Preparing {
            ref target_version,
            ref mut ready_nodes,
            ref mut pending_nodes,
        } = *state
        {
            if let Some(pos) = pending_nodes.iter().position(|id| *id == node_id) {
                pending_nodes.remove(pos);
                ready_nodes.push(node_id);
            }

            if pending_nodes.is_empty() {
                tracing::info!(
                    "All nodes ready for upgrade to {}",
                    target_version
                );
            }
        }
    }

    /// Get current upgrade state.
    pub fn upgrade_state(&self) -> UpgradeState {
        self.upgrade_state.read().clone()
    }

    /// Get cluster version info.
    pub fn cluster_version_info(&self) -> Vec<NodeVersionInfo> {
        self.node_versions
            .read()
            .iter()
            .map(|(id, version)| NodeVersionInfo {
                node_id: *id,
                version: version.clone(),
                data_format_version: version.data_format_version(),
                upgrade_ready: false,
            })
            .collect()
    }
}

/// Upgrade readiness check result.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct UpgradeReadiness {
    pub ready: bool,
    pub reason: String,
    pub incompatible_nodes: Vec<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version_compatibility() {
        let v1 = Version::new(1, 0, 0);
        let v2 = Version::new(1, 1, 0);
        let v3 = Version::new(2, 0, 0);

        assert!(v1.is_compatible_with(&v2));
        assert!(!v1.is_compatible_with(&v3));
    }

    #[test]
    fn test_data_format_version() {
        let v = Version::new(1, 2, 3);
        assert_eq!(v.data_format_version(), 1002);
    }

    #[test]
    fn test_upgrade_readiness() {
        let config = RollingUpgradeConfig {
            enabled: true,
            current_version: Version::new(1, 0, 0),
            ..Default::default()
        };
        let manager = RollingUpgradeManager::new(config);

        manager.register_node(1, Version::new(1, 0, 0));
        manager.register_node(2, Version::new(1, 0, 0));

        let readiness = manager.check_upgrade_readiness(&Version::new(1, 1, 0));
        assert!(readiness.ready);
    }

    #[test]
    fn test_incompatible_version() {
        let config = RollingUpgradeConfig {
            enabled: true,
            current_version: Version::new(1, 0, 0),
            ..Default::default()
        };
        let manager = RollingUpgradeManager::new(config);

        manager.register_node(1, Version::new(1, 0, 0));
        manager.register_node(2, Version::new(2, 0, 0)); // Incompatible

        let readiness = manager.check_upgrade_readiness(&Version::new(1, 1, 0));
        assert!(!readiness.ready);
        assert_eq!(readiness.incompatible_nodes.len(), 1);
    }
}
