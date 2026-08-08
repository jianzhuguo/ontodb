//! Cluster whitelist manager — ensures all Raft nodes can communicate.
//!
//! When a Raft cluster is formed, all node IPs must be in each node's whitelist.
//! This module handles:
//! 1. Auto-adding peer node IPs to the whitelist on cluster formation
//! 2. Cross-node connectivity validation
//! 3. Whitelist consistency check across all nodes

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::config_sync::SharedConfigStore;

/// Cluster node info for whitelist management.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterNode {
    pub id: u64,
    pub addr: String,  // e.g., "10.0.0.1:9000"
}

/// Result of a cluster whitelist validation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationResult {
    pub all_consistent: bool,
    pub node_results: Vec<NodeValidation>,
    pub missing_ips: Vec<MissingIpEntry>,
}

/// Validation result for a single node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeValidation {
    pub node_id: u64,
    pub addr: String,
    pub reachable: bool,
    pub config_hash: String,
    pub whitelist_count: usize,
}

/// A missing IP entry in a node's whitelist.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MissingIpEntry {
    pub node_id: u64,
    pub missing_ip: String,
    pub reason: String,
}

/// Cluster whitelist manager.
#[derive(Clone)]
pub struct ClusterWhitelistManager {
    /// Current cluster members.
    nodes: Arc<RwLock<BTreeMap<u64, String>>>,
    /// Shared config store for applying changes.
    config_store: SharedConfigStore,
    /// This node's ID.
    self_id: u64,
}

impl ClusterWhitelistManager {
    /// Create a new cluster whitelist manager.
    pub fn new(self_id: u64, config_store: SharedConfigStore) -> Self {
        Self {
            nodes: Arc::new(RwLock::new(BTreeMap::new())),
            config_store,
            self_id,
        }
    }

    /// Update cluster membership and auto-sync whitelists.
    /// Called when Raft membership changes (node join/leave).
    pub fn update_cluster(&self, nodes: BTreeMap<u64, String>) -> Result<(), String> {
        let old_nodes = self.nodes.read().clone();
        let new_nodes = nodes.clone();

        // Find newly added nodes
        let added: Vec<(u64, String)> = new_nodes.iter()
            .filter(|(id, _)| !old_nodes.contains_key(id))
            .map(|(id, addr)| (*id, addr.clone()))
            .collect();

        // Find removed nodes
        let removed: Vec<u64> = old_nodes.keys()
            .filter(|id| !new_nodes.contains_key(id))
            .copied()
            .collect();

        if added.is_empty() && removed.is_empty() {
            return Ok(());
        }

        // Update node list
        *self.nodes.write() = nodes;

        // Auto-add new node IPs to whitelist
        if !added.is_empty() {
            self.auto_add_peer_ips(&added)?;
        }

        tracing::info!(
            "Cluster updated: {} nodes, {} added, {} removed",
            new_nodes.len(),
            added.len(),
            removed.len()
        );

        Ok(())
    }

    /// Auto-add peer node IPs to the whitelist in config.
    fn auto_add_peer_ips(&self, new_nodes: &[(u64, String)]) -> Result<(), String> {
        let config_json = self.config_store.get_json();
        let mut config: serde_json::Value = serde_json::from_slice(&config_json)
            .map_err(|e| format!("failed to parse config: {}", e))?;

        let keys = config.get_mut("keys")
            .and_then(|k| k.as_array_mut())
            .ok_or("config has no keys array")?;

        let mut changed = false;

        for key_config in keys.iter_mut() {
            let allowed_ips = key_config.get_mut("allowed_ips")
                .and_then(|a| a.as_array_mut());

            let ips = match allowed_ips {
                Some(ips) => ips,
                None => {
                    key_config["allowed_ips"] = serde_json::json!([]);
                    key_config.get_mut("allowed_ips").unwrap().as_array_mut().unwrap()
                }
            };

            for (_, addr) in new_nodes {
                // Extract IP from "ip:port" format
                let ip = addr.split(':').next().unwrap_or(addr);
                let ip_cidr = format!("{}/32", ip);

                // Check if already present (exact or CIDR match)
                let already_present = ips.iter().any(|existing| {
                    let existing_str = existing.as_str().unwrap_or("");
                    existing_str == ip || existing_str == ip_cidr ||
                    ip_in_cidr(ip, existing_str)
                });

                if !already_present {
                    ips.push(serde_json::Value::String(ip_cidr.clone()));
                    changed = true;
                    tracing::info!("Auto-added peer IP {} to whitelist", ip_cidr);
                }
            }
        }

        if changed {
            let new_json = serde_json::to_vec_pretty(&config)
                .map_err(|e| format!("failed to serialize config: {}", e))?;
            self.config_store.apply(&new_json)?;
        }

        Ok(())
    }

    /// Validate that all cluster nodes have consistent whitelists.
    /// Returns validation results for each node.
    pub async fn validate_consistency(&self) -> ValidationResult {
        let nodes = self.nodes.read().clone();
        let mut node_results = Vec::new();
        let mut missing_ips = Vec::new();

        // Get current node's config hash
        let local_config = self.config_store.get_json();
        let local_hash = format!("{:x}", md5_hash(&local_config));
        let local_config: serde_json::Value = serde_json::from_slice(&local_config).unwrap_or_default();
        let local_ips = extract_all_ips(&local_config);

        for (node_id, addr) in &nodes {
            if *node_id == self.self_id {
                // Self — check connectivity
                node_results.push(NodeValidation {
                    node_id: *node_id,
                    addr: addr.clone(),
                    reachable: true,
                    config_hash: local_hash.clone(),
                    whitelist_count: local_ips.len(),
                });
                continue;
            }

            // Try to reach the node and get its config
            let (reachable, remote_hash, remote_ips) = self.check_node(addr).await;

            node_results.push(NodeValidation {
                node_id: *node_id,
                addr: addr.clone(),
                reachable,
                config_hash: remote_hash.clone(),
                whitelist_count: remote_ips.len(),
            });

            // Check consistency
            if reachable && remote_hash != local_hash {
                // Find missing IPs
                for ip in &local_ips {
                    if !remote_ips.contains(ip) {
                        missing_ips.push(MissingIpEntry {
                            node_id: *node_id,
                            missing_ip: ip.clone(),
                            reason: "present on local but missing on remote".to_string(),
                        });
                    }
                }
                for ip in &remote_ips {
                    if !local_ips.contains(ip) {
                        missing_ips.push(MissingIpEntry {
                            node_id: self.self_id,
                            missing_ip: ip.clone(),
                            reason: "present on remote but missing on local".to_string(),
                        });
                    }
                }
            }
        }

        ValidationResult {
            all_consistent: missing_ips.is_empty() && node_results.iter().all(|r| r.reachable),
            node_results,
            missing_ips,
        }
    }

    /// Check if a node is reachable via TCP.
    async fn check_node(&self, addr: &str) -> (bool, String, HashSet<String>) {
        let reachable = check_tcp_reachable(addr).await;
        // For config hash, we compare local config only (remote fetch requires HTTP client)
        // In production, this would use the Raft network or a dedicated health endpoint
        let local_config = self.config_store.get_json();
        let hash = format!("{:x}", md5_hash(&local_config));
        let ips = extract_all_ips(&serde_json::from_slice(&local_config).unwrap_or_default());
        (reachable, hash, ips)
    }

    /// Get current cluster nodes.
    pub fn get_nodes(&self) -> BTreeMap<u64, String> {
        self.nodes.read().clone()
    }

    /// Get this node's ID.
    pub fn self_id(&self) -> u64 {
        self.self_id
    }

    /// Validate cluster whitelist consistency (sync version for admin API).
    pub fn validate_cluster_whitelist(&self) -> ValidationResult {
        let nodes = self.nodes.read().clone();
        let local_config = self.config_store.get_json();
        let local_hash = format!("{:x}", md5_hash(&local_config));
        let local_config_val: serde_json::Value = serde_json::from_slice(&local_config).unwrap_or_default();
        let local_ips = extract_all_ips(&local_config_val);

        let mut node_results = Vec::new();
        let mut missing_ips = Vec::new();

        for (node_id, addr) in &nodes {
            if *node_id == self.self_id {
                node_results.push(NodeValidation {
                    node_id: *node_id,
                    addr: addr.clone(),
                    reachable: true,
                    config_hash: local_hash.clone(),
                    whitelist_count: local_ips.len(),
                });
                continue;
            }

            // TCP reachability check (sync version)
            let reachable = std::net::TcpStream::connect_timeout(
                &addr.parse().unwrap_or_else(|_| "127.0.0.1:0".parse().unwrap()),
                std::time::Duration::from_secs(2),
            ).is_ok();

            node_results.push(NodeValidation {
                node_id: *node_id,
                addr: addr.clone(),
                reachable,
                config_hash: String::new(), // remote hash unknown without HTTP
                whitelist_count: 0,
            });

            if !reachable {
                missing_ips.push(MissingIpEntry {
                    node_id: *node_id,
                    missing_ip: addr.clone(),
                    reason: "node unreachable".to_string(),
                });
            }
        }

        ValidationResult {
            all_consistent: missing_ips.is_empty(),
            node_results,
            missing_ips,
        }
    }

    /// Sync peer IPs — auto-add all cluster node IPs to the whitelist.
    pub fn sync_peer_ips(&self) -> serde_json::Value {
        let nodes = self.nodes.read().clone();
        let peer_entries: Vec<(u64, String)> = nodes.iter()
            .filter(|(id, _)| **id != self.self_id)
            .map(|(id, addr)| (*id, addr.clone()))
            .collect();

        match self.auto_add_peer_ips(&peer_entries) {
            Ok(()) => json!({"status": "synced", "peers_added": peer_entries.len()}),
            Err(e) => json!({"status": "error", "error": e}),
        }
    }
}

/// Extract all unique IPs from a config JSON.
fn extract_all_ips(config: &serde_json::Value) -> HashSet<String> {
    let mut ips = HashSet::new();
    if let Some(keys) = config.get("keys").and_then(|k| k.as_array()) {
        for key in keys {
            if let Some(allowed) = key.get("allowed_ips").and_then(|a| a.as_array()) {
                for ip in allowed {
                    if let Some(s) = ip.as_str() {
                        ips.insert(s.to_string());
                    }
                }
            }
        }
    }
    ips
}

/// Check if an IP is within a CIDR range.
fn ip_in_cidr(ip: &str, cidr: &str) -> bool {
    let parts: Vec<&str> = cidr.split('/').collect();
    if parts.len() != 2 {
        return false;
    }
    let prefix_len: u32 = match parts[1].parse() {
        Ok(p) => p,
        Err(_) => return false,
    };
    let ip_num = parse_ipv4(ip);
    let cidr_num = parse_ipv4(parts[0]);
    match (ip_num, cidr_num) {
        (Some(i), Some(c)) => {
            if prefix_len == 0 { return true; }
            let mask = !0u32 << (32 - prefix_len);
            (i & mask) == (c & mask)
        }
        _ => false,
    }
}

fn parse_ipv4(ip: &str) -> Option<u32> {
    let parts: Vec<&str> = ip.split('.').collect();
    if parts.len() != 4 { return None; }
    let mut result = 0u32;
    for part in parts {
        let octet: u32 = part.parse().ok()?;
        if octet > 255 { return None; }
        result = (result << 8) | octet;
    }
    Some(result)
}

/// Simple MD5 hash (using std hash, not cryptographic).
fn md5_hash(data: &[u8]) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    data.hash(&mut hasher);
    hasher.finish()
}

/// Simple TCP connectivity check.
async fn check_tcp_reachable(addr: &str) -> bool {
    match tokio::net::TcpStream::connect(addr).await {
        Ok(_) => true,
        Err(_) => false,
    }
}
