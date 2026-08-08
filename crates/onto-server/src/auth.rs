//! API Key authentication for OntoDB HTTP API.
//!
//! Supports multiple API keys with configurable permissions.

use axum::{
    extract::Request,
    http::{header, StatusCode},
    middleware::Next,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

/// Permission level for an API key.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Permission {
    /// Read-only access (SELECT, VECTOR SEARCH, schema)
    ReadOnly,
    /// Read-write access (all operations)
    ReadWrite,
    /// Admin access (all operations + schema modifications)
    Admin,
}

impl Permission {
    /// Check if this permission allows the given HTTP method and path.
    pub fn allows(&self, method: &str, path: &str) -> bool {
        match self {
            Permission::ReadOnly => {
                // Allow GET requests and POST to query/search endpoints
                method == "GET"
                    || (method == "POST"
                        && (path.ends_with("/query")
                            || path.ends_with("/vector/search")
                            || path.ends_with("/hybrid/query")))
            }
            Permission::ReadWrite => {
                // Allow all non-admin operations
                !path.ends_with("/admin")
            }
            Permission::Admin => true,
        }
    }
}

/// Configuration for a single API key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKeyConfig {
    /// The API key string.
    pub key: String,
    /// Human-readable description.
    pub description: String,
    /// Permission level.
    pub permission: Permission,
    /// Optional rate limit override (requests per minute). None = use default.
    #[serde(default)]
    pub rate_limit: Option<u32>,
    /// Optional IP whitelist. If set, only these IPs can use this key.
    /// Supports exact IPs ("192.168.1.1") and CIDR notation ("10.0.0.0/8").
    /// Empty or absent = no IP restriction.
    #[serde(default)]
    pub allowed_ips: Option<Vec<String>>,
}

/// Authentication configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthConfig {
    /// Whether authentication is enabled.
    pub enabled: bool,
    /// List of valid API keys.
    pub keys: Vec<ApiKeyConfig>,
    /// Default permission for keys without explicit permission.
    pub default_permission: Permission,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            keys: Vec::new(),
            default_permission: Permission::ReadWrite,
        }
    }
}

/// IP whitelist configuration for the entire server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpWhitelistConfig {
    /// Whether IP whitelisting is enabled.
    pub enabled: bool,
    /// Global allowed IPs (applies to all requests, before API key check).
    /// Supports exact IPs and CIDR notation.
    #[serde(default)]
    pub allowed_ips: Vec<String>,
    /// Allow localhost (127.0.0.1 and ::1) when whitelist is enabled.
    #[serde(default = "default_true")]
    pub allow_localhost: bool,
}

fn default_true() -> bool { true }

impl Default for IpWhitelistConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            allowed_ips: Vec::new(),
            allow_localhost: true,
        }
    }
}

/// Shared authentication state with hot-reload support.
#[derive(Clone)]
pub struct AuthState {
    /// Map of API key -> (description, permission, rate_limit, allowed_ips)
    /// Wrapped in RwLock for hot-reload without server restart.
    keys: Arc<parking_lot::RwLock<HashMap<String, (String, Permission, Option<u32>, Option<Vec<String>>)>>>,
    /// Whether auth is enabled.
    pub enabled: bool,
    /// Metrics counters for auth events.
    pub metrics: Option<crate::metrics::SharedMetrics>,
    /// Global IP whitelist configuration (hot-reloadable).
    pub ip_whitelist: Arc<parking_lot::RwLock<IpWhitelistConfig>>,
    /// Path to the config file for hot-reload.
    config_path: Option<std::path::PathBuf>,
    /// Last known modification time of the config file.
    last_modified: Arc<parking_lot::RwLock<Option<std::time::SystemTime>>>,
}

impl AuthState {
    /// Create a new AuthState from configuration.
    pub fn new(config: &AuthConfig) -> Self {
        let mut keys = HashMap::new();
        for key_config in &config.keys {
            keys.insert(
                key_config.key.clone(),
                (
                    key_config.description.clone(),
                    key_config.permission.clone(),
                    key_config.rate_limit,
                    key_config.allowed_ips.clone(),
                ),
            );
        }
        Self {
            keys: Arc::new(parking_lot::RwLock::new(keys)),
            enabled: config.enabled,
            metrics: None,
            ip_whitelist: Arc::new(parking_lot::RwLock::new(IpWhitelistConfig::default())),
            config_path: None,
            last_modified: Arc::new(parking_lot::RwLock::new(None)),
        }
    }

    /// Set IP whitelist configuration.
    pub fn with_ip_whitelist(self, config: IpWhitelistConfig) -> Self {
        *self.ip_whitelist.write() = config;
        self
    }

    /// Set the config file path for hot-reload.
    pub fn with_config_path(mut self, path: std::path::PathBuf) -> Self {
        self.config_path = Some(path);
        self
    }

    /// Reload configuration from disk. Returns true if config changed.
    pub fn reload(&self) -> bool {
        let path = match &self.config_path {
            Some(p) => p.clone(),
            None => return false,
        };

        // Check modification time
        let meta = match std::fs::metadata(&path) {
            Ok(m) => m,
            Err(_) => return false,
        };
        let mod_time = match meta.modified() {
            Ok(t) => t,
            Err(_) => return false,
        };

        // Skip if unchanged
        {
            let last = self.last_modified.read();
            if let Some(prev) = *last {
                if mod_time <= prev {
                    return false;
                }
            }
        }

        // Read and parse config
        let data = match std::fs::read_to_string(&path) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("Auth reload failed to read {}: {}", path.display(), e);
                return false;
            }
        };

        let config: AuthConfig = match serde_json::from_str(&data) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("Auth reload failed to parse {}: {}", path.display(), e);
                return false;
            }
        };

        // Update keys
        let mut new_keys = HashMap::new();
        for key_config in &config.keys {
            new_keys.insert(
                key_config.key.clone(),
                (
                    key_config.description.clone(),
                    key_config.permission.clone(),
                    key_config.rate_limit,
                    key_config.allowed_ips.clone(),
                ),
            );
        }

        *self.keys.write() = new_keys;
        *self.last_modified.write() = Some(mod_time);

        eprintln!("Auth config reloaded from {} ({} keys)", path.display(), config.keys.len());
        true
    }

    /// Start a background task that watches the config file for changes.
    /// Checks every `interval` seconds. Returns a JoinHandle.
    pub fn start_reload_watcher(self, interval_secs: u64) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut timer = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
            loop {
                timer.tick().await;
                self.reload();
            }
        })
    }

    /// Set metrics for auth event tracking.
    pub fn with_metrics(mut self, metrics: crate::metrics::SharedMetrics) -> Self {
        self.metrics = Some(metrics);
        self
    }

    /// Validate an API key and return its permission level.
    pub fn validate(&self, key: &str) -> Option<(String, Permission, Option<u32>, Option<Vec<String>>)> {
        self.keys.read().get(key).cloned()
    }

    /// Check if a client IP is allowed by the global IP whitelist.
    pub fn is_ip_allowed(&self, client_ip: &str) -> bool {
        let wl = self.ip_whitelist.read();
        if !wl.enabled {
            return true;
        }
        if wl.allow_localhost && (client_ip == "127.0.0.1" || client_ip == "::1" || client_ip.starts_with("127.")) {
            return true;
        }
        if wl.allowed_ips.is_empty() {
            return true;
        }
        for allowed in &wl.allowed_ips {
            if ip_matches(client_ip, allowed) {
                return true;
            }
        }
        false
    }

    /// Check if a client IP is allowed for a specific API key.
    pub fn is_ip_allowed_for_key(&self, key: &str, client_ip: &str) -> bool {
        let keys = self.keys.read();
        if let Some((_, _, _, Some(ref allowed_ips))) = keys.get(key) {
            if allowed_ips.is_empty() {
                return true;
            }
            for allowed in allowed_ips {
                if ip_matches(client_ip, allowed) {
                    return true;
                }
            }
            return false;
        }
        true
    }

    /// Get the rate limit for a specific key, or None if not configured.
    #[allow(dead_code)]
    pub fn get_rate_limit(&self, key: &str) -> Option<u32> {
        self.keys.read().get(key).and_then(|(_, _, limit, _)| *limit)
    }
}

/// Authentication middleware.
///
/// Extracts the API key from the `Authorization: Bearer <key>` header
/// or the `X-API-Key` header and validates it.
pub async fn auth_middleware(
    axum::extract::State(auth): axum::extract::State<AuthState>,
    request: Request,
    next: Next,
) -> impl IntoResponse {
    // Extract client IP (from X-Forwarded-For or socket addr)
    let client_ip = extract_client_ip(&request);

    // Check global IP whitelist (before auth)
    if !auth.is_ip_allowed(&client_ip) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({
                "success": false,
                "error": format!("IP {} is not in the server whitelist", client_ip)
            })),
        ).into_response();
    }

    // Skip auth if disabled
    if !auth.enabled {
        return next.run(request).await;
    }

    // Skip auth for health check endpoints
    let path = request.uri().path();
    if path == "/api/health" || path == "/api/health/ready" || path == "/api/health/live" {
        return next.run(request).await;
    }

    // Extract API key from headers
    let api_key = extract_api_key(&request);

    // Record auth attempt
    if let Some(ref metrics) = auth.metrics {
        metrics.auth_attempts.inc();
    }

    let key = match api_key {
        Some(k) => k,
        None => {
            if let Some(ref metrics) = auth.metrics {
                metrics.auth_failures.inc();
            }
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({
                    "success": false,
                    "error": "Missing API key. Provide via 'Authorization: Bearer <key>' or 'X-API-Key: <key>' header"
                })),
            )
                .into_response();
        }
    };

    // Validate the key
    let (description, permission, rate_limit, _allowed_ips) = match auth.validate(&key) {
        Some(info) => info,
        None => {
            if let Some(ref metrics) = auth.metrics {
                metrics.auth_failures.inc();
            }
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({
                    "success": false,
                    "error": "Invalid API key"
                })),
            )
                .into_response();
        }
    };

    // Check per-key IP restriction
    if !auth.is_ip_allowed_for_key(&key, &client_ip) {
        if let Some(ref metrics) = auth.metrics {
            metrics.auth_failures.inc();
        }
        return (
            StatusCode::FORBIDDEN,
            Json(json!({
                "success": false,
                "error": format!("IP {} is not allowed for this API key", client_ip)
            })),
        ).into_response();
    }

    // Check permission for this endpoint
    let method = request.method().as_str();
    let path = request.uri().path();

    if !permission.allows(method, path) {
        if let Some(ref metrics) = auth.metrics {
            metrics.auth_failures.inc();
        }
        return (
            StatusCode::FORBIDDEN,
            Json(json!({
                "success": false,
                "error": format!("Insufficient permissions. Required: {:?}, your key: '{}'", permission, description)
            })),
        )
            .into_response();
    }

    // Record auth success
    if let Some(ref metrics) = auth.metrics {
        metrics.auth_successes.inc();
    }

    // Add key info to request extensions for downstream handlers
    let mut request = request;
    request.extensions_mut().insert(KeyInfo {
        key: key.clone(),
        description,
        permission,
        rate_limit,
    });

    next.run(request).await
}

/// Extract client IP from request headers (X-Forwarded-For) or default.
fn extract_client_ip(request: &Request) -> String {
    // Try X-Forwarded-For first (for proxied requests)
    if let Some(forwarded) = request.headers().get("X-Forwarded-For") {
        if let Ok(val) = forwarded.to_str() {
            // Take the first IP (original client)
            if let Some(first) = val.split(',').next() {
                return first.trim().to_string();
            }
        }
    }
    // Try X-Real-IP
    if let Some(real_ip) = request.headers().get("X-Real-IP") {
        if let Ok(val) = real_ip.to_str() {
            return val.trim().to_string();
        }
    }
    // Default to unknown (will be allowed if whitelist is disabled)
    "unknown".to_string()
}

/// Extract API key from request headers.
fn extract_api_key(request: &Request) -> Option<String> {
    // Try Authorization: Bearer <key> first
    if let Some(auth_header) = request.headers().get(header::AUTHORIZATION) {
        if let Ok(auth_str) = auth_header.to_str() {
            if let Some(key) = auth_str.strip_prefix("Bearer ") {
                return Some(key.trim().to_string());
            }
        }
    }

    // Try X-API-Key header
    if let Some(api_key_header) = request.headers().get("X-API-Key") {
        if let Ok(key) = api_key_header.to_str() {
            return Some(key.trim().to_string());
        }
    }

    // Try query parameter (less secure, but convenient for testing)
    if let Some(query) = request.uri().query() {
        for param in query.split('&') {
            if let Some((key, value)) = param.split_once('=') {
                if key == "api_key" {
                    return Some(value.to_string());
                }
            }
        }
    }

    None
}

/// Information about the authenticated API key.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct KeyInfo {
    pub key: String,
    pub description: String,
    pub permission: Permission,
    pub rate_limit: Option<u32>,
}

/// Check if a client IP matches an allowed entry (exact match or CIDR).
pub fn ip_matches(client_ip: &str, allowed: &str) -> bool {
    // Exact match
    if client_ip == allowed {
        return true;
    }

    // CIDR match (e.g., "10.0.0.0/8", "192.168.1.0/24")
    if let Some((subnet, prefix_len)) = allowed.split_once('/') {
        if let Ok(prefix) = prefix_len.parse::<u32>() {
            return cidr_match(client_ip, subnet, prefix);
        }
    }

    false
}

/// Check if an IPv4 address is within a CIDR range.
fn cidr_match(client_ip: &str, subnet: &str, prefix_len: u32) -> bool {
    let client = parse_ipv4(client_ip);
    let network = parse_ipv4(subnet);

    match (client, network) {
        (Some(c), Some(n)) => {
            if prefix_len == 0 {
                return true;
            }
            let mask = !0u32 << (32 - prefix_len);
            (c & mask) == (n & mask)
        }
        _ => false,
    }
}

/// Parse an IPv4 address string into a u32.
fn parse_ipv4(ip: &str) -> Option<u32> {
    let parts: Vec<&str> = ip.split('.').collect();
    if parts.len() != 4 {
        return None;
    }
    let mut result = 0u32;
    for part in parts {
        let octet: u32 = part.parse().ok()?;
        if octet > 255 {
            return None;
        }
        result = (result << 8) | octet;
    }
    Some(result)
}
