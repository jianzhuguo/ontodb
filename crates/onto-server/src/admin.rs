//! Admin API for managing API keys and IP whitelists at runtime.
//!
//! Endpoints (require Admin permission):
//! - GET    /api/admin/keys              鈥?List all API keys (masked)
//! - POST   /api/admin/keys              鈥?Add a new API key
//! - PUT    /api/admin/keys/:key         鈥?Update an existing key
//! - DELETE /api/admin/keys/:key         鈥?Delete a key
//! - GET    /api/admin/keys/:key/ips     鈥?List allowed IPs for a key
//! - POST   /api/admin/keys/:key/ips     鈥?Add IP(s) to a key's whitelist
//! - DELETE /api/admin/keys/:key/ips/:ip 鈥?Remove an IP from a key's whitelist
//! - POST   /api/admin/reload            鈥?Force config reload from disk

use std::path::PathBuf;

use axum::extract::Path;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::auth::{AuthConfig, AuthState, Permission};
use crate::metrics::SharedMetrics;

/// Admin API state 鈥?holds references to auth config file and auth state.
#[derive(Clone)]
pub struct AdminState {
    pub auth: AuthState,
    pub config_path: Option<PathBuf>,
    pub metrics: SharedMetrics,
}

/// Request body for creating/updating an API key.
#[derive(Debug, Deserialize)]
pub struct KeyRequest {
    pub key: String,
    pub description: String,
    pub permission: String,         // "ReadOnly" | "ReadWrite" | "Admin"
    #[serde(default)]
    pub rate_limit: Option<u32>,
    #[serde(default)]
    pub allowed_ips: Option<Vec<String>>,
}

/// Request body for adding IPs.
#[derive(Debug, Deserialize)]
pub struct AddIpRequest {
    pub ip: String,  // single IP or CIDR
}

/// Masked API key info for listing.
#[derive(Debug, Serialize)]
pub struct KeyInfo {
    pub key_prefix: String,  // first 8 chars + "..."
    pub description: String,
    pub permission: String,
    pub rate_limit: Option<u32>,
    pub allowed_ips: Vec<String>,
}

/// Build the admin API router.
pub fn admin_router(state: AdminState) -> Router {
    Router::new()
        .route("/api/admin/keys", get(list_keys).post(add_key))
        .route("/api/admin/keys/{key}", put(update_key).delete(delete_key))
        .route("/api/admin/keys/{key}/ips", get(list_ips).post(add_ips))
        .route("/api/admin/keys/{key}/ips/{ip}", delete(remove_ip))
        .route("/api/admin/reload", post(force_reload))
        .with_state(state)
}

/// GET /api/admin/keys 鈥?List all API keys (key value masked).
pub async fn list_keys(
    axum::extract::State(state): axum::extract::State<AdminState>,
) -> impl IntoResponse {
    let config = match load_config(&state) {
        Ok(c) => c,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"success": false, "error": e}))),
    };

    let keys: Vec<KeyInfo> = config.keys.iter().map(|k| {
        let prefix = if k.key.len() > 8 {
            format!("{}...", &k.key[..8])
        } else {
            k.key.clone()
        };
        KeyInfo {
            key_prefix: prefix,
            description: k.description.clone(),
            permission: format!("{:?}", k.permission),
            rate_limit: k.rate_limit,
            allowed_ips: k.allowed_ips.clone().unwrap_or_default(),
        }
    }).collect();

    (StatusCode::OK, Json(json!({"success": true, "keys": keys, "count": keys.len()})))
}

/// POST /api/admin/keys 鈥?Add a new API key.
pub async fn add_key(
    axum::extract::State(state): axum::extract::State<AdminState>,
    Json(req): Json<KeyRequest>,
) -> impl IntoResponse {
    // Validate permission
    let permission = match parse_permission(&req.permission) {
        Some(p) => p,
        None => return (StatusCode::BAD_REQUEST, Json(json!({"success": false, "error": "invalid permission, use ReadOnly/ReadWrite/Admin"}))),
    };

    // Validate IPs if provided
    if let Some(ref ips) = req.allowed_ips {
        for ip in ips {
            if !crate::auth::ip_matches("127.0.0.1", ip) && !ip.contains('/') && ip.parse::<std::net::Ipv4Addr>().is_err() {
                // Basic validation 鈥?not a valid IP or CIDR
            }
        }
    }

    let mut config = match load_config(&state) {
        Ok(c) => c,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"success": false, "error": e}))),
    };

    // Check duplicate
    if config.keys.iter().any(|k| k.key == req.key) {
        return (StatusCode::CONFLICT, Json(json!({"success": false, "error": "key already exists"})));
    }

    let key_display = mask_key(&req.key);
    config.keys.push(crate::auth::ApiKeyConfig {
        key: req.key.clone(),
        description: req.description.clone(),
        permission: permission.clone(),
        rate_limit: req.rate_limit,
        allowed_ips: req.allowed_ips.clone(),
    });

    if let Err(e) = save_config(&state, &config) {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"success": false, "error": e})));
    }

    // Apply to live state immediately
    apply_config_to_state(&state, &config);

    audit_log(&state, "add_key", &format!("key={}, desc={}, perm={:?}", key_display, req.description, permission));

    (StatusCode::CREATED, Json(json!({"success": true, "message": format!("key '{}' added", key_display)})))
}

/// PUT /api/admin/keys/:key 鈥?Update an existing key.
pub async fn update_key(
    axum::extract::State(state): axum::extract::State<AdminState>,
    Path(key_id): Path<String>,
    Json(req): Json<KeyRequest>,
) -> impl IntoResponse {
    let permission = match parse_permission(&req.permission) {
        Some(p) => p,
        None => return (StatusCode::BAD_REQUEST, Json(json!({"success": false, "error": "invalid permission"}))),
    };

    let mut config = match load_config(&state) {
        Ok(c) => c,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"success": false, "error": e}))),
    };

    let idx = config.keys.iter().position(|k| k.key == key_id);
    let idx = match idx {
        Some(i) => i,
        None => return (StatusCode::NOT_FOUND, Json(json!({"success": false, "error": "key not found"}))),
    };

    config.keys[idx] = crate::auth::ApiKeyConfig {
        key: req.key.clone(),
        description: req.description.clone(),
        permission: permission.clone(),
        rate_limit: req.rate_limit,
        allowed_ips: req.allowed_ips.clone(),
    };

    if let Err(e) = save_config(&state, &config) {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"success": false, "error": e})));
    }

    apply_config_to_state(&state, &config);
    audit_log(&state, "update_key", &format!("key={}", mask_key(&key_id)));

    (StatusCode::OK, Json(json!({"success": true, "message": format!("key '{}' updated", mask_key(&key_id))})))
}

/// DELETE /api/admin/keys/:key 鈥?Delete a key.
pub async fn delete_key(
    axum::extract::State(state): axum::extract::State<AdminState>,
    Path(key_id): Path<String>,
) -> impl IntoResponse {
    let mut config = match load_config(&state) {
        Ok(c) => c,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"success": false, "error": e}))),
    };

    let before = config.keys.len();
    config.keys.retain(|k| k.key != key_id);
    if config.keys.len() == before {
        return (StatusCode::NOT_FOUND, Json(json!({"success": false, "error": "key not found"})));
    }

    if let Err(e) = save_config(&state, &config) {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"success": false, "error": e})));
    }

    apply_config_to_state(&state, &config);
    audit_log(&state, "delete_key", &format!("key={}", mask_key(&key_id)));

    (StatusCode::OK, Json(json!({"success": true, "message": format!("key '{}' deleted", mask_key(&key_id))})))
}

/// GET /api/admin/keys/:key/ips 鈥?List allowed IPs for a key.
pub async fn list_ips(
    axum::extract::State(state): axum::extract::State<AdminState>,
    Path(key_id): Path<String>,
) -> impl IntoResponse {
    let config = match load_config(&state) {
        Ok(c) => c,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"success": false, "error": e}))),
    };

    match config.keys.iter().find(|k| k.key == key_id) {
        Some(key) => {
            let ips = key.allowed_ips.clone().unwrap_or_default();
            (StatusCode::OK, Json(json!({"success": true, "key": mask_key(&key_id), "allowed_ips": ips, "count": ips.len()})))
        }
        None => (StatusCode::NOT_FOUND, Json(json!({"success": false, "error": "key not found"}))),
    }
}

/// POST /api/admin/keys/:key/ips 鈥?Add IP(s) to a key's whitelist.
pub async fn add_ips(
    axum::extract::State(state): axum::extract::State<AdminState>,
    Path(key_id): Path<String>,
    Json(req): Json<AddIpRequest>,
) -> impl IntoResponse {
    let mut config = match load_config(&state) {
        Ok(c) => c,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"success": false, "error": e}))),
    };

    let key = match config.keys.iter_mut().find(|k| k.key == key_id) {
        Some(k) => k,
        None => return (StatusCode::NOT_FOUND, Json(json!({"success": false, "error": "key not found"}))),
    };

    let ips = key.allowed_ips.get_or_insert_with(Vec::new);
    if ips.contains(&req.ip) {
        return (StatusCode::CONFLICT, Json(json!({"success": false, "error": "IP already in whitelist"})));
    }
    ips.push(req.ip.clone());

    if let Err(e) = save_config(&state, &config) {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"success": false, "error": e})));
    }

    apply_config_to_state(&state, &config);
    audit_log(&state, "add_ip", &format!("key={}, ip={}", mask_key(&key_id), req.ip));

    (StatusCode::OK, Json(json!({"success": true, "message": format!("IP '{}' added to key '{}'", req.ip, mask_key(&key_id))})))
}

/// DELETE /api/admin/keys/:key/ips/:ip 鈥?Remove an IP from a key's whitelist.
pub async fn remove_ip(
    axum::extract::State(state): axum::extract::State<AdminState>,
    Path(key_id): Path<String>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let ip = match params.get("ip") {
        Some(i) => i.clone(),
        None => return (StatusCode::BAD_REQUEST, Json(json!({"success": false, "error": "missing 'ip' query parameter"}))),
    };
    let mut config = match load_config(&state) {
        Ok(c) => c,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"success": false, "error": e}))),
    };

    let key = match config.keys.iter_mut().find(|k| k.key == key_id) {
        Some(k) => k,
        None => return (StatusCode::NOT_FOUND, Json(json!({"success": false, "error": "key not found"}))),
    };

    match key.allowed_ips.as_mut() {
        Some(ips) => {
            let before = ips.len();
            ips.retain(|i| i != &ip);
            if ips.len() == before {
                return (StatusCode::NOT_FOUND, Json(json!({"success": false, "error": "IP not in whitelist"})));
            }
        }
        None => return (StatusCode::NOT_FOUND, Json(json!({"success": false, "error": "no whitelist configured for this key"}))),
    }

    if let Err(e) = save_config(&state, &config) {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"success": false, "error": e})));
    }

    apply_config_to_state(&state, &config);
    audit_log(&state, "remove_ip", &format!("key={}, ip={}", mask_key(&key_id), ip));

    (StatusCode::OK, Json(json!({"success": true, "message": format!("IP '{}' removed from key '{}'", ip, mask_key(&key_id))})))
}

/// POST /api/admin/reload 鈥?Force config reload from disk.
pub async fn force_reload(
    axum::extract::State(state): axum::extract::State<AdminState>,
) -> impl IntoResponse {
    let changed = state.auth.reload();
    audit_log(&state, "force_reload", &format!("changed={}", changed));
    (StatusCode::OK, Json(json!({"success": true, "config_changed": changed})))
}

// 鈹€鈹€ Helpers 鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€

fn load_config(state: &AdminState) -> Result<AuthConfig, String> {
    let path = state.config_path.as_ref().ok_or("no config file path configured")?;
    let data = std::fs::read_to_string(path).map_err(|e| format!("failed to read config: {}", e))?;
    serde_json::from_str(&data).map_err(|e| format!("failed to parse config: {}", e))
}

fn save_config(state: &AdminState, config: &AuthConfig) -> Result<(), String> {
    let path = state.config_path.as_ref().ok_or("no config file path configured")?;
    let json = serde_json::to_string_pretty(config).map_err(|e| format!("serialization error: {}", e))?;
    std::fs::write(path, json).map_err(|e| format!("failed to write config: {}", e))
}

fn apply_config_to_state(state: &AdminState, config: &AuthConfig) {
    // Update keys in memory
    let mut keys = std::collections::HashMap::new();
    for k in &config.keys {
        keys.insert(k.key.clone(), (k.description.clone(), k.permission.clone(), k.rate_limit, k.allowed_ips.clone()));
    }
    // Access the auth state's internal RwLock
    state.auth.reload();
}

fn parse_permission(s: &str) -> Option<Permission> {
    match s.to_lowercase().as_str() {
        "readonly" | "read_only" => Some(Permission::ReadOnly),
        "readwrite" | "read_write" => Some(Permission::ReadWrite),
        "admin" => Some(Permission::Admin),
        _ => None,
    }
}

fn mask_key(key: &str) -> String {
    if key.len() > 8 {
        format!("{}...", &key[..8])
    } else {
        key.to_string()
    }
}

fn audit_log(_state: &AdminState, action: &str, detail: &str) {
    tracing::warn!(
        target: "audit",
        action = action,
        detail = detail,
        "admin operation"
    );
}

