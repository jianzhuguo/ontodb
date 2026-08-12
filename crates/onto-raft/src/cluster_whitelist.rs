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
                    // Safe: just set to empty array above
                    key_config.get_mut("allowed_ips").expect("just set allowed_ips").as_array_mut().expect("just set to array")
                }
            };

            for (_, addr) in new_nodes {
                // Extract IP from "ip:port" format
                // IPv6 addresses are in brackets: "[::1]:9000"
                let ip = if addr.starts_with('[') {
                    // IPv6: extract from "[ip]:port"
                    addr.split(']').next()
                        .map(|s| &s[1..])
                        .unwrap_or(addr)
                } else {
                    // IPv4: extract from "ip:port"
                    addr.split(':').next().unwrap_or(addr)
                };
                // Use /128 for IPv6, /32 for IPv4
                let prefix_len = if ip.contains(':') { 128 } else { 32 };
                let ip_cidr = format!("{}/{}", ip, prefix_len);

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
        let local_hash = format!("{:x}", config_hash(&local_config));
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
    /// NOTE: Currently compares local config only. In production, this should
    /// fetch the remote node's config via an HTTP/RPC health endpoint.
    async fn check_node(&self, addr: &str) -> (bool, String, HashSet<String>) {
        let reachable = check_tcp_reachable(addr).await;
        // TODO: Fetch remote node's config via HTTP health endpoint for real comparison
        let local_config = self.config_store.get_json();
        let hash = format!("{:x}", config_hash(&local_config));
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
        let local_hash = format!("{:x}", config_hash(&local_config));
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
            // Safe: "127.0.0.1:0" is a valid address literal
            let fallback: std::net::SocketAddr = "127.0.0.1:0".parse().expect("valid address");
            let parsed_addr = addr.parse().unwrap_or(fallback);
            let reachable = std::net::TcpStream::connect_timeout(
                &parsed_addr,
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

/// Deterministic config hash using FNV-1a (consistent across processes).
fn config_hash(data: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325; // FNV offset basis
    for byte in data {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3); // FNV prime
    }
    hash
}

/// Simple TCP connectivity check.
async fn check_tcp_reachable(addr: &str) -> bool {
    tokio::net::TcpStream::connect(addr).await.is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_ipv4_valid() {
        assert_eq!(parse_ipv4("192.168.1.1"), Some(192 << 24 | 168 << 16 | 1 << 8 | 1));
        assert_eq!(parse_ipv4("0.0.0.0"), Some(0));
        assert_eq!(parse_ipv4("255.255.255.255"), Some(0xFFFFFFFF));
    }

    #[test]
    fn test_parse_ipv4_invalid() {
        assert_eq!(parse_ipv4("not.an.ip.address"), None);
        assert_eq!(parse_ipv4("256.1.1.1"), None);
        assert_eq!(parse_ipv4("1.1.1"), None);
        assert_eq!(parse_ipv4(""), None);
    }

    #[test]
    fn test_ip_in_cidr_exact() {
        assert!(ip_in_cidr("192.168.1.1", "192.168.1.1/32"));
        assert!(!ip_in_cidr("192.168.1.2", "192.168.1.1/32"));
    }

    #[test]
    fn test_ip_in_cidr_subnet() {
        assert!(ip_in_cidr("192.168.1.100", "192.168.1.0/24"));
        assert!(ip_in_cidr("192.168.1.1", "192.168.0.0/16"));
        assert!(!ip_in_cidr("192.169.1.1", "192.168.0.0/16"));
    }

    #[test]
    fn test_ip_in_cidr_prefix_zero() {
        assert!(ip_in_cidr("8.8.8.8", "0.0.0.0/0"));
    }

    #[test]
    fn test_ip_in_cidr_invalid() {
        assert!(!ip_in_cidr("bad", "192.168.1.0/24"));
        assert!(!ip_in_cidr("192.168.1.1", "bad"));
        assert!(!ip_in_cidr("192.168.1.1", "192.168.1.0/abc"));
    }

    #[test]
    fn test_extract_all_ips() {
        let config = serde_json::json!({
            "keys": [
                {"allowed_ips": ["192.168.1.1/32", "10.0.0.0/8"]},
                {"allowed_ips": ["192.168.1.1/32", "172.16.0.0/12"]}
            ]
        });
        let ips = extract_all_ips(&config);
        assert_eq!(ips.len(), 3); // 192.168.1.1/32 is deduplicated
        assert!(ips.contains("192.168.1.1/32"));
        assert!(ips.contains("10.0.0.0/8"));
        assert!(ips.contains("172.16.0.0/12"));
    }

    #[test]
    fn test_extract_all_ips_empty() {
        let config = serde_json::json!({"keys": []});
        assert!(extract_all_ips(&config).is_empty());
    }

    #[test]
    fn test_extract_all_ips_no_keys() {
        let config = serde_json::json!({});
        assert!(extract_all_ips(&config).is_empty());
    }

    #[test]
    fn test_config_hash_deterministic() {
        let data = b"test data";
        let h1 = config_hash(data);
        let h2 = config_hash(data);
        assert_eq!(h1, h2);
    }

    #[test]
    fn test_config_hash_different_inputs() {
        let h1 = config_hash(b"data1");
        let h2 = config_hash(b"data2");
        assert_ne!(h1, h2);
    }

    #[test]
    fn test_cluster_whitelist_manager_new() {
        let config_store = SharedConfigStore::new(None);
        let manager = ClusterWhitelistManager::new(1, config_store);
        assert_eq!(manager.self_id(), 1);
        assert!(manager.get_nodes().is_empty());
    }

    #[test]
    fn test_cluster_whitelist_update_cluster() {
        let config_store = SharedConfigStore::new(None);
        config_store.apply(b"{\"keys\":[]}").unwrap();
        let manager = ClusterWhitelistManager::new(1, config_store);

        let mut nodes = BTreeMap::new();
        nodes.insert(1, "127.0.0.1:9000".to_string());
        nodes.insert(2, "127.0.0.1:9001".to_string());

        manager.update_cluster(nodes).unwrap();
        assert_eq!(manager.get_nodes().len(), 2);
    }

    #[test]
    fn test_cluster_whitelist_validate_empty_cluster() {
        let config_store = SharedConfigStore::new(None);
        config_store.apply(b"{\"keys\":[]}").unwrap();
        let manager = ClusterWhitelistManager::new(1, config_store);

        let result = manager.validate_cluster_whitelist();
        assert!(result.all_consistent);
        assert!(result.node_results.is_empty());
    }

    #[test]
    fn test_cluster_whitelist_validate_single_node() {
        let config_store = SharedConfigStore::new(None);
        config_store.apply(b"{\"keys\":[]}").unwrap();
        let manager = ClusterWhitelistManager::new(1, config_store);

        let mut nodes = BTreeMap::new();
        nodes.insert(1, "127.0.0.1:9000".to_string());
        manager.update_cluster(nodes).unwrap();

        let result = manager.validate_cluster_whitelist();
        assert_eq!(result.node_results.len(), 1);
        assert!(result.node_results[0].reachable); // self is always reachable
    }

    #[test]
    fn test_sync_peer_ips() {
        let config_store = SharedConfigStore::new(None);
        config_store.apply(b"{\"keys\":[{\"allowed_ips\":[]}]}").unwrap();
        let manager = ClusterWhitelistManager::new(1, config_store.clone());

        let mut nodes = BTreeMap::new();
        nodes.insert(1, "127.0.0.1:9000".to_string());
        nodes.insert(2, "10.0.0.1:9001".to_string());
        manager.update_cluster(nodes).unwrap();

        let result = manager.sync_peer_ips();
        assert_eq!(result["status"], "synced");

        // Verify the peer IP was added to config
        let config_json = String::from_utf8(config_store.get_json()).unwrap();
        assert!(config_json.contains("10.0.0.1/32"));
    }
}
