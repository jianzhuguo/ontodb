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

/// Shared authentication state.
#[derive(Clone)]
pub struct AuthState {
    /// Map of API key -> (description, permission, rate_limit)
    keys: Arc<HashMap<String, (String, Permission, Option<u32>)>>,
    /// Whether auth is enabled.
    pub enabled: bool,
    /// Metrics counters for auth events.
    pub metrics: Option<crate::metrics::SharedMetrics>,
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
                ),
            );
        }
        Self {
            keys: Arc::new(keys),
            enabled: config.enabled,
            metrics: None,
        }
    }

    /// Set metrics for auth event tracking.
    pub fn with_metrics(mut self, metrics: crate::metrics::SharedMetrics) -> Self {
        self.metrics = Some(metrics);
        self
    }

    /// Validate an API key and return its permission level.
    pub fn validate(&self, key: &str) -> Option<(String, Permission, Option<u32>)> {
        self.keys.get(key).cloned()
    }

    /// Get the rate limit for a specific key, or None if not configured.
    #[allow(dead_code)]
    pub fn get_rate_limit(&self, key: &str) -> Option<u32> {
        self.keys.get(key).and_then(|(_, _, limit)| *limit)
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
    let (description, permission, rate_limit) = match auth.validate(&key) {
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
pub struct KeyInfo {
    pub key: String,
    pub description: String,
    pub permission: Permission,
    pub rate_limit: Option<u32>,
}
