// Copyright (c) 2024-2026 Guo Jianzhu (郭建筑)
// Licensed under the Apache License, Version 2.0.
// See LICENSE for details.
//! Admin API for managing API keys and IP whitelists at runtime.
//!
//! Endpoints (require Admin permission):
//! - GET    /api/admin/keys              — List all API keys (masked)
//! - POST   /api/admin/keys              — Add a new API key
//! - PUT    /api/admin/keys/:key         — Update an existing key
//! - DELETE /api/admin/keys/:key         — Delete a key
//! - GET    /api/admin/keys/:key/ips     — List allowed IPs for a key
//! - POST   /api/admin/keys/:key/ips     — Add IP(s) to a key's whitelist
//! - DELETE /api/admin/keys/:key/ips/:ip — Remove an IP from a key's whitelist
//! - POST   /api/admin/reload            — Force config reload from disk

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
use onto_core::policy::{
    password_hash, Action, CredentialRecord, Grant, ObjectRef, Principal, PrincipalKind,
    CRED_SALT_LEN, CRED_SCHEME_BLAKE3,
};

/// Admin API state — holds references to auth config file and auth state.
#[derive(Clone)]
pub struct AdminState {
    pub auth: AuthState,
    pub config_path: Option<PathBuf>,
    pub metrics: SharedMetrics,
    /// Shared config store for cross-node sync (Raft integration).
    #[cfg(feature = "raft")]
    pub config_store: Option<onto_raft::SharedConfigStore>,
    /// Cluster whitelist manager for cross-node validation.
    #[cfg(feature = "raft")]
    pub cluster_manager: Option<onto_raft::ClusterWhitelistManager>,
    /// Audit logger for writing admin operations to audit file.
    pub audit: Option<std::sync::Arc<crate::audit::AuditLogger>>,
    /// RBAC policy service (design §13, M2 T4.4): the users/roles/grants/
    /// tokens-rotate admin endpoints operate on the same policy store the
    /// OntoQL DDL does. `None` = policy plane not configured.
    pub policy: Option<std::sync::Arc<onto_ontology::policy_store::PolicyService>>,
}

/// Request body for creating/updating an API key.
#[derive(Debug, Deserialize)]
pub struct KeyRequest {
    pub key: String,
    pub description: String,
    pub permission: String, // "ReadOnly" | "ReadWrite" | "Admin"
    #[serde(default)]
    pub rate_limit: Option<u32>,
    #[serde(default)]
    pub allowed_ips: Option<Vec<String>>,
    /// Optional namespace scope for this key (None = unscoped).
    #[serde(default)]
    pub namespace: Option<String>,
    /// Optional RBAC principal id (None = legacy key, namespace-ACL only).
    #[serde(default)]
    pub principal: Option<String>,
}

/// Request body for adding IPs.
#[derive(Debug, Deserialize)]
pub struct AddIpRequest {
    pub ip: String, // single IP or CIDR
}

/// Masked API key info for listing.
#[derive(Debug, Serialize)]
pub struct KeyInfo {
    pub key_prefix: String, // first 8 chars + "..."
    pub description: String,
    pub permission: String,
    pub rate_limit: Option<u32>,
    pub allowed_ips: Vec<String>,
    /// Namespace scope of the key (None = unscoped).
    pub namespace: Option<String>,
    /// RBAC principal id (None = legacy key).
    pub principal: Option<String>,
}

/// Build the admin API router (keys plane + RBAC policy plane, design §13).
/// Returns an un-stated router; the caller applies `.with_state(...)` so the
/// surrounding router's state type can be inferred (e.g. `AppState`).
pub fn admin_router() -> Router<AdminState> {
    let router = Router::new()
        .route("/api/admin/keys", get(list_keys).post(add_key))
        .route("/api/admin/keys/{key}", put(update_key).delete(delete_key))
        .route("/api/admin/keys/{key}/ips", get(list_ips).post(add_ips))
        .route("/api/admin/keys/{key}/ips/{ip}", delete(remove_ip))
        .route("/api/admin/reload", post(force_reload))
        // RBAC policy plane (M2 T4.4, design §13) — ADMIN only.
        .route("/api/admin/users", post(admin_create_user))
        .route("/api/admin/roles", post(admin_create_role))
        .route(
            "/api/admin/grants",
            get(admin_list_grants).post(admin_create_grant),
        )
        .route("/api/admin/grants/{id}", delete(admin_delete_grant))
        .route("/api/admin/tokens/rotate", post(admin_rotate_token));

    #[cfg(feature = "raft")]
    let router = router
        .route("/api/admin/cluster/validate", get(validate_cluster))
        .route("/api/admin/cluster/sync", post(sync_cluster))
        .route("/api/admin/cluster/nodes", get(list_cluster_nodes));

    router
}

/// GET /api/admin/keys — List all API keys (key value masked).
pub async fn list_keys(
    axum::extract::State(state): axum::extract::State<AdminState>,
) -> impl IntoResponse {
    let config = match load_config(&state) {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"success": false, "error": e})),
            )
        }
    };

    let keys: Vec<KeyInfo> = config
        .keys
        .iter()
        .map(|k| {
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
                namespace: k.namespace.clone(),
                principal: k.principal.clone(),
            }
        })
        .collect();

    (
        StatusCode::OK,
        Json(json!({"success": true, "keys": keys, "count": keys.len()})),
    )
}

/// POST /api/admin/keys — Add a new API key.
pub async fn add_key(
    axum::extract::State(state): axum::extract::State<AdminState>,
    Json(req): Json<KeyRequest>,
) -> impl IntoResponse {
    // Validate permission
    let permission = match parse_permission(&req.permission) {
        Some(p) => p,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(
                    json!({"success": false, "error": "invalid permission, use ReadOnly/ReadWrite/Admin"}),
                ),
            )
        }
    };

    // Validate IPs if provided
    if let Some(ref ips) = req.allowed_ips {
        for ip in ips {
            if !is_valid_ip_or_cidr(ip) {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(
                        json!({"success": false, "error": format!("invalid IP or CIDR: '{}'", ip)}),
                    ),
                );
            }
        }
    }

    let mut config = match load_config(&state) {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"success": false, "error": e})),
            )
        }
    };

    // Check duplicate
    if config.keys.iter().any(|k| k.key == req.key) {
        return (
            StatusCode::CONFLICT,
            Json(json!({"success": false, "error": "key already exists"})),
        );
    }

    let key_display = mask_key(&req.key);
    config.keys.push(crate::auth::ApiKeyConfig {
        key: req.key.clone(),
        description: req.description.clone(),
        permission: permission.clone(),
        rate_limit: req.rate_limit,
        allowed_ips: req.allowed_ips.clone(),
        namespace: req.namespace.clone(),
        principal: req.principal.clone(),
    });

    if let Err(e) = save_config(&state, &config) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"success": false, "error": e})),
        );
    }

    // Apply to live state immediately
    apply_config_to_state(&state, &config);

    audit_log(
        &state,
        "add_key",
        &format!(
            "key={}, desc={}, perm={:?}",
            key_display, req.description, permission
        ),
    );

    (
        StatusCode::CREATED,
        Json(json!({"success": true, "message": format!("key '{}' added", key_display)})),
    )
}

/// PUT /api/admin/keys/:key — Update an existing key.
pub async fn update_key(
    axum::extract::State(state): axum::extract::State<AdminState>,
    Path(key_id): Path<String>,
    Json(req): Json<KeyRequest>,
) -> impl IntoResponse {
    let permission = match parse_permission(&req.permission) {
        Some(p) => p,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"success": false, "error": "invalid permission"})),
            )
        }
    };

    let mut config = match load_config(&state) {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"success": false, "error": e})),
            )
        }
    };

    let idx = config.keys.iter().position(|k| k.key == key_id);
    let idx = match idx {
        Some(i) => i,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"success": false, "error": "key not found"})),
            )
        }
    };

    config.keys[idx] = crate::auth::ApiKeyConfig {
        key: req.key.clone(),
        description: req.description.clone(),
        permission: permission.clone(),
        rate_limit: req.rate_limit,
        allowed_ips: req.allowed_ips.clone(),
        namespace: req.namespace.clone(),
        principal: req.principal.clone(),
    };

    if let Err(e) = save_config(&state, &config) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"success": false, "error": e})),
        );
    }

    apply_config_to_state(&state, &config);
    audit_log(&state, "update_key", &format!("key={}", mask_key(&key_id)));

    (
        StatusCode::OK,
        Json(json!({"success": true, "message": format!("key '{}' updated", mask_key(&key_id))})),
    )
}

/// DELETE /api/admin/keys/:key — Delete a key.
pub async fn delete_key(
    axum::extract::State(state): axum::extract::State<AdminState>,
    Path(key_id): Path<String>,
) -> impl IntoResponse {
    let mut config = match load_config(&state) {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"success": false, "error": e})),
            )
        }
    };

    let before = config.keys.len();
    config.keys.retain(|k| k.key != key_id);
    if config.keys.len() == before {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"success": false, "error": "key not found"})),
        );
    }

    if let Err(e) = save_config(&state, &config) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"success": false, "error": e})),
        );
    }

    apply_config_to_state(&state, &config);
    audit_log(&state, "delete_key", &format!("key={}", mask_key(&key_id)));

    (
        StatusCode::OK,
        Json(json!({"success": true, "message": format!("key '{}' deleted", mask_key(&key_id))})),
    )
}

/// GET /api/admin/keys/:key/ips — List allowed IPs for a key.
pub async fn list_ips(
    axum::extract::State(state): axum::extract::State<AdminState>,
    Path(key_id): Path<String>,
) -> impl IntoResponse {
    let config = match load_config(&state) {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"success": false, "error": e})),
            )
        }
    };

    match config.keys.iter().find(|k| k.key == key_id) {
        Some(key) => {
            let ips = key.allowed_ips.clone().unwrap_or_default();
            (
                StatusCode::OK,
                Json(
                    json!({"success": true, "key": mask_key(&key_id), "allowed_ips": ips, "count": ips.len()}),
                ),
            )
        }
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({"success": false, "error": "key not found"})),
        ),
    }
}

/// POST /api/admin/keys/:key/ips — Add IP(s) to a key's whitelist.
pub async fn add_ips(
    axum::extract::State(state): axum::extract::State<AdminState>,
    Path(key_id): Path<String>,
    Json(req): Json<AddIpRequest>,
) -> impl IntoResponse {
    let mut config = match load_config(&state) {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"success": false, "error": e})),
            )
        }
    };

    let key = match config.keys.iter_mut().find(|k| k.key == key_id) {
        Some(k) => k,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"success": false, "error": "key not found"})),
            )
        }
    };

    if !is_valid_ip_or_cidr(&req.ip) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"success": false, "error": "invalid IP or CIDR format"})),
        );
    }

    let ips = key.allowed_ips.get_or_insert_with(Vec::new);
    if ips.contains(&req.ip) {
        return (
            StatusCode::CONFLICT,
            Json(json!({"success": false, "error": "IP already in whitelist"})),
        );
    }
    ips.push(req.ip.clone());

    if let Err(e) = save_config(&state, &config) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"success": false, "error": e})),
        );
    }

    apply_config_to_state(&state, &config);
    audit_log(
        &state,
        "add_ip",
        &format!("key={}, ip={}", mask_key(&key_id), req.ip),
    );

    (
        StatusCode::OK,
        Json(
            json!({"success": true, "message": format!("IP '{}' added to key '{}'", req.ip, mask_key(&key_id))}),
        ),
    )
}

/// DELETE /api/admin/keys/:key/ips/:ip — Remove an IP from a key's whitelist.
pub async fn remove_ip(
    axum::extract::State(state): axum::extract::State<AdminState>,
    Path(key_id): Path<String>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let ip = match params.get("ip") {
        Some(i) => i.clone(),
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"success": false, "error": "missing 'ip' query parameter"})),
            )
        }
    };
    let mut config = match load_config(&state) {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"success": false, "error": e})),
            )
        }
    };

    let key = match config.keys.iter_mut().find(|k| k.key == key_id) {
        Some(k) => k,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"success": false, "error": "key not found"})),
            )
        }
    };

    match key.allowed_ips.as_mut() {
        Some(ips) => {
            let before = ips.len();
            ips.retain(|i| i != &ip);
            if ips.len() == before {
                return (
                    StatusCode::NOT_FOUND,
                    Json(json!({"success": false, "error": "IP not in whitelist"})),
                );
            }
        }
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"success": false, "error": "no whitelist configured for this key"})),
            )
        }
    }

    if let Err(e) = save_config(&state, &config) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"success": false, "error": e})),
        );
    }

    apply_config_to_state(&state, &config);
    audit_log(
        &state,
        "remove_ip",
        &format!("key={}, ip={}", mask_key(&key_id), ip),
    );

    (
        StatusCode::OK,
        Json(
            json!({"success": true, "message": format!("IP '{}' removed from key '{}'", ip, mask_key(&key_id))}),
        ),
    )
}

/// POST /api/admin/reload — Force config reload from disk.
pub async fn force_reload(
    axum::extract::State(state): axum::extract::State<AdminState>,
) -> impl IntoResponse {
    // Use force_reload to bypass modification time check
    let changed = state.auth.force_reload();
    audit_log(&state, "force_reload", &format!("changed={}", changed));
    (
        StatusCode::OK,
        Json(json!({"success": true, "config_changed": changed})),
    )
}

/// GET /api/admin/cluster/validate — Validate cluster whitelist consistency.
#[cfg(feature = "raft")]
async fn validate_cluster(
    axum::extract::State(state): axum::extract::State<AdminState>,
) -> impl IntoResponse {
    let manager = match &state.cluster_manager {
        Some(m) => m,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"success": false, "error": "cluster mode not enabled"})),
            )
        }
    };

    let result = manager.validate_cluster_whitelist();
    audit_log(
        &state,
        "cluster_validate",
        &format!("consistent={}", result.all_consistent),
    );
    (
        StatusCode::OK,
        Json(json!({"success": true, "validation": result})),
    )
}

/// POST /api/admin/cluster/sync — Force sync whitelists to all cluster nodes.
#[cfg(feature = "raft")]
async fn sync_cluster(
    axum::extract::State(state): axum::extract::State<AdminState>,
) -> impl IntoResponse {
    let manager = match &state.cluster_manager {
        Some(m) => m,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"success": false, "error": "cluster mode not enabled"})),
            )
        }
    };

    // Auto-add all peer IPs to whitelist
    let nodes = manager.get_nodes();
    let result = manager.sync_peer_ips();
    audit_log(&state, "cluster_sync", &format!("nodes={}", nodes.len()));

    (
        StatusCode::OK,
        Json(json!({"success": true, "nodes": nodes.len(), "sync_result": result})),
    )
}

/// GET /api/admin/cluster/nodes — List cluster nodes.
#[cfg(feature = "raft")]
async fn list_cluster_nodes(
    axum::extract::State(state): axum::extract::State<AdminState>,
) -> impl IntoResponse {
    let manager = match &state.cluster_manager {
        Some(m) => m,
        None => {
            return (
                StatusCode::OK,
                Json(json!({"success": true, "mode": "standalone", "nodes": []})),
            )
        }
    };

    let nodes = manager.get_nodes();
    let node_list: Vec<serde_json::Value> = nodes
        .iter()
        .map(|(id, addr)| json!({"id": id, "addr": addr}))
        .collect();

    (
        StatusCode::OK,
        Json(json!({
            "success": true,
            "mode": "cluster",
            "self_id": manager.self_id(),
            "nodes": node_list,
        })),
    )
}

// ── Helpers ────────────────────────────────────────────────────

fn load_config(state: &AdminState) -> Result<AuthConfig, String> {
    let path = state
        .config_path
        .as_ref()
        .ok_or("no config file path configured")?;
    let data =
        std::fs::read_to_string(path).map_err(|e| format!("failed to read config: {}", e))?;
    serde_json::from_str(&data).map_err(|e| format!("failed to parse config: {}", e))
}

fn save_config(state: &AdminState, config: &AuthConfig) -> Result<(), String> {
    let json =
        serde_json::to_string_pretty(config).map_err(|e| format!("serialization error: {}", e))?;

    // If SharedConfigStore is available, use it (handles disk + Raft replication)
    #[cfg(feature = "raft")]
    if let Some(ref store) = state.config_store {
        store.apply(json.as_bytes())?;
        tracing::info!("Config saved via SharedConfigStore (Raft replication if active)");
    } else {
        // Fallback: direct file write
        let path = state
            .config_path
            .as_ref()
            .ok_or("no config file path configured")?;
        std::fs::write(path, &json).map_err(|e| format!("failed to write config: {}", e))?;
    }

    #[cfg(not(feature = "raft"))]
    {
        let path = state
            .config_path
            .as_ref()
            .ok_or("no config file path configured")?;
        std::fs::write(path, &json).map_err(|e| format!("failed to write config: {}", e))?;
    }

    Ok(())
}

fn apply_config_to_state(state: &AdminState, config: &AuthConfig) {
    // Update keys in memory
    let mut keys = std::collections::HashMap::new();
    for k in &config.keys {
        keys.insert(
            k.key.clone(),
            (
                k.description.clone(),
                k.permission.clone(),
                k.rate_limit,
                k.allowed_ips.clone(),
            ),
        );
    }
    // Access the auth state's internal RwLock
    state.auth.reload();
}

/// Validate that a string is a valid IPv4/IPv6 address or CIDR notation.
fn is_valid_ip_or_cidr(s: &str) -> bool {
    if let Some((ip, prefix)) = s.split_once('/') {
        // CIDR notation: validate IP and prefix length
        if ip.parse::<std::net::Ipv4Addr>().is_ok() {
            return prefix.parse::<u32>().is_ok_and(|p| p <= 32);
        }
        if ip.parse::<std::net::Ipv6Addr>().is_ok() {
            return prefix.parse::<u32>().is_ok_and(|p| p <= 128);
        }
        return false;
    }
    // Plain IP address
    s.parse::<std::net::Ipv4Addr>().is_ok() || s.parse::<std::net::Ipv6Addr>().is_ok()
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

/// Extract client IP from request (X-Forwarded-For or X-Real-IP).
fn extract_client_ip_from_state() -> String {
    // In admin handlers we don't have direct access to request headers,
    // so we use "admin-api" as the source identifier.
    // The actual client IP is logged at the middleware level.
    "admin-api".to_string()
}

fn audit_log(state: &AdminState, action: &str, detail: &str) {
    // Always log to tracing
    tracing::warn!(
        target: "audit",
        action = action,
        detail = detail,
        "admin operation"
    );

    // Write to audit file if logger is available
    if let Some(ref logger) = state.audit {
        let client_ip = extract_client_ip_from_state();
        // We don't have the API key here, but we know it's an Admin key
        let entry = logger.create_admin_entry(&client_ip, None, action, detail, true, None);
        logger.log(entry);
    }
}

// ─── RBAC policy-plane admin API (M2 T4.4, design §13) ─────────────────────
//
// Six endpoints over the same policy store the OntoQL policy DDL mutates.
// POST/DELETE on /api/admin/grants mirror GRANT/REVOKE semantics: REVOKE
// plants an explicit deny (design §7.1) so a removed grant cannot silently
// re-inherit through the `is_a` line, and GET lists ALLOW and DENY rows the
// way SHOW GRANTS does. Every mutation audits the `grant_change` /
// `token_rotate` events the DDL emits; the minted token never reaches the
// audit trail.

/// Request body for `POST /api/admin/users` (design §13: `{name, kind, password}`).
#[derive(Debug, Deserialize)]
pub struct AdminCreateUserRequest {
    pub name: String,
    /// `"user"` (default) | `"service_account"`.
    #[serde(default)]
    pub kind: Option<String>,
    /// Optional login credential. Omit for a token-only principal (mint one
    /// with `POST /api/admin/tokens/rotate`).
    #[serde(default)]
    pub password: Option<String>,
}

/// Request body for `POST /api/admin/roles`.
#[derive(Debug, Deserialize)]
pub struct AdminCreateRoleRequest {
    pub name: String,
}

/// Request body for `POST /api/admin/grants` (design §13: `{role, action, object}`).
#[derive(Debug, Deserialize)]
pub struct AdminCreateGrantRequest {
    pub role: String,
    /// `READ` | `WRITE` | `DDL` | `INFER`.
    pub action: String,
    /// Object in [`onto_core::policy::ObjectRef::display`] form:
    /// `"NAMESPACE *"`, `"CLASS ns.Cls"`, `"PROPERTY ns.Cls.prop"`, `"RULE r"`.
    pub object: String,
    /// `GRANT ... ONLY` (design §7.1): covers the class itself, not its
    /// subclasses.
    #[serde(default)]
    pub only: Option<bool>,
    /// `GRANT ... WITH GRANT OPTION` (design §5, T4.5): the grantee may
    /// delegate this grant.
    #[serde(default)]
    pub with_grant_option: Option<bool>,
}

/// Query for `GET /api/admin/grants?role=analyst`.
#[derive(Debug, Deserialize)]
pub struct AdminGrantListQuery {
    #[serde(default)]
    pub role: Option<String>,
}

/// Request body for `POST /api/admin/tokens/rotate`.
#[derive(Debug, Deserialize)]
pub struct AdminRotateTokenRequest {
    pub principal: String,
}

/// Error from a policy-plane op: HTTP status + JSON body.
type PolicyOpErr = (StatusCode, serde_json::Value);

fn op_bad(msg: impl Into<String>) -> PolicyOpErr {
    (
        StatusCode::BAD_REQUEST,
        json!({"success": false, "error": msg.into()}),
    )
}

fn op_conflict(msg: impl Into<String>) -> PolicyOpErr {
    (
        StatusCode::CONFLICT,
        json!({"success": false, "error": msg.into()}),
    )
}

fn op_not_found(msg: impl Into<String>) -> PolicyOpErr {
    (
        StatusCode::NOT_FOUND,
        json!({"success": false, "error": msg.into()}),
    )
}

fn op_internal(msg: impl Into<String>) -> PolicyOpErr {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        json!({"success": false, "error": msg.into()}),
    )
}

/// ADMIN gate for the policy plane (design §13 "(ADMIN)"). The auth
/// middleware already restricts /admin/ writes to Permission::Admin keys,
/// but ReadOnly keys pass GET — this closes that hole for the six
/// policy-plane endpoints. A `None` caller means auth is disabled, which the
/// query plane treats as admin (see `QueryActor::anonymous`).
fn require_policy_admin(
    state: &AdminState,
    caller: Option<&crate::auth::KeyInfo>,
) -> Result<(), PolicyOpErr> {
    match caller {
        None => Ok(()),
        Some(k) if k.permission == Permission::Admin => Ok(()),
        Some(_) => {
            policy_audit(
                state,
                "authz_deny",
                "policy admin API requires Admin",
                false,
                Some("Admin API key required".to_string()),
            );
            Err((
                StatusCode::FORBIDDEN,
                json!({
                    "success": false,
                    "error": "policy admin API requires an Admin API key (or auth disabled)",
                    // Design §13 "行为变化": 403 bodies carry the required
                    // action/object for client-side troubleshooting.
                    "required_action": "ADMIN",
                    "object": "admin",
                }),
            ))
        }
    }
}

/// Audit one policy-plane outcome. `event` is the design §12 event type
/// (`grant_change` / `token_rotate` / `authz_deny`); `detail` must never
/// carry secrets.
fn policy_audit(
    state: &AdminState,
    event: &str,
    detail: &str,
    success: bool,
    error: Option<String>,
) {
    tracing::warn!(
        target: "audit",
        action = event,
        detail = detail,
        success = success,
        "admin policy operation"
    );
    if let Some(ref logger) = state.audit {
        let entry = logger.create_admin_entry(
            &extract_client_ip_from_state(),
            None,
            event,
            detail,
            success,
            error,
        );
        logger.log(entry);
    }
}

fn policy_svc(state: &AdminState) -> Result<&std::sync::Arc<onto_ontology::policy_store::PolicyService>, PolicyOpErr> {
    state.policy.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"success": false, "error": "RBAC policy service is not configured"}),
        )
    })
}

/// Parses an action word — the OntoQL GRANT verb set.
fn parse_action_word(s: &str) -> Result<Action, String> {
    match s.trim().to_ascii_uppercase().as_str() {
        "READ" => Ok(Action::Read),
        "WRITE" => Ok(Action::Write),
        "DDL" => Ok(Action::Ddl),
        "INFER" => Ok(Action::Infer),
        other => Err(format!(
            "unknown action '{other}' (expected READ|WRITE|DDL|INFER)"
        )),
    }
}

/// Qualified name → (ns, class), matching the PEP's `split_once('.')`
/// convention: a bare name lands in `_default`.
fn split_ns(ref_: &str) -> Result<(String, String), String> {
    match ref_.split_once('.') {
        Some((ns, class)) if !ns.is_empty() && !class.is_empty() => {
            Ok((ns.to_string(), class.to_string()))
        }
        _ if !ref_.is_empty() => Ok(("_default".to_string(), ref_.to_string())),
        _ => Err("empty class reference".to_string()),
    }
}

/// Parses an object in `ObjectRef::display()` form — the round-trip the GET
/// rows speak: `"NAMESPACE ns"`, `"CLASS ns.Cls"`, `"PROPERTY ns.Cls.prop"`,
/// `"RULE r"`.
fn parse_object_ref(s: &str) -> Result<ObjectRef, String> {
    let s = s.trim();
    let (kind, rest) = s.split_once(' ').ok_or_else(|| {
        format!(
            "object '{s}' must be '<KIND> <ref>' \
             (NAMESPACE ns | CLASS ns.Cls | PROPERTY ns.Cls.prop | RULE r)"
        )
    })?;
    let rest = rest.trim();
    match kind.to_ascii_uppercase().as_str() {
        "NAMESPACE" if !rest.is_empty() => Ok(ObjectRef::Namespace(rest.to_string())),
        "CLASS" => {
            let (ns, class) = split_ns(rest)?;
            Ok(ObjectRef::Class { ns, class })
        }
        "PROPERTY" => {
            let (head, prop) = rest
                .rsplit_once('.')
                .ok_or_else(|| format!("property '{rest}' must be 'ns.Cls.prop' or 'Cls.prop'"))?;
            let (ns, class) = split_ns(head)?;
            Ok(ObjectRef::Property {
                ns,
                class,
                prop: prop.to_string(),
            })
        }
        "RULE" if !rest.is_empty() => Ok(ObjectRef::Rule(rest.to_string())),
        other => Err(format!(
            "unknown object kind '{other}' in '{s}' \
             (expected NAMESPACE|CLASS|PROPERTY|RULE)"
        )),
    }
}

/// Stable, URL-safe id of one policy row: `{role}:{ACTION}:{KIND}:{ref}`.
/// The policy store keeps one entry per (role, action, object) key, so the
/// key identifies the row — and survives the ALLOW→DENY transition a DELETE
/// performs.
fn grant_row_id(role: &str, action: Action, object: &ObjectRef) -> String {
    let (kind, ref_) = match object {
        ObjectRef::Namespace(ns) => ("NAMESPACE", ns.clone()),
        ObjectRef::Class { ns, class } => ("CLASS", format!("{ns}.{class}")),
        ObjectRef::Property { ns, class, prop } => {
            ("PROPERTY", format!("{ns}.{class}.{prop}"))
        }
        ObjectRef::Rule(r) => ("RULE", r.clone()),
    };
    format!("{role}:{}:{kind}:{ref_}", action.as_str())
}

/// Inverse of [`grant_row_id`].
fn parse_grant_id(id: &str) -> Result<(String, Action, ObjectRef), String> {
    let mut parts = id.splitn(4, ':');
    let (Some(role), Some(action), Some(kind), Some(ref_)) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(format!(
            "malformed grant id '{id}' (expected role:ACTION:KIND:ref)"
        ));
    };
    if role.is_empty() {
        return Err(format!("malformed grant id '{id}': empty role"));
    }
    let action = parse_action_word(action)?;
    let object = match kind {
        "NAMESPACE" if !ref_.is_empty() => ObjectRef::Namespace(ref_.to_string()),
        "CLASS" => {
            let (ns, class) = split_ns(ref_)?;
            ObjectRef::Class { ns, class }
        }
        "PROPERTY" => {
            let (head, prop) = ref_.rsplit_once('.').ok_or_else(|| {
                format!("malformed grant id '{id}': property ref must be 'ns.Cls.prop'")
            })?;
            let (ns, class) = split_ns(head)?;
            ObjectRef::Property {
                ns,
                class,
                prop: prop.to_string(),
            }
        }
        "RULE" if !ref_.is_empty() => ObjectRef::Rule(ref_.to_string()),
        other => return Err(format!("unknown object kind '{other}' in grant id '{id}'")),
    };
    Ok((role.to_string(), action, object))
}

/// One row of `GET /api/admin/grants` — same shape as `SHOW GRANTS` plus the
/// row id used by DELETE.
fn grant_row_json(g: &onto_core::policy::Grant, effect: &str) -> serde_json::Value {
    json!({
        "id": grant_row_id(&g.role, g.action, &g.object),
        "role": g.role,
        "action": g.action.as_str(),
        "object": g.object.display(),
        "effect": effect,
        "only": g.options.only,
        "with_grant_option": g.options.with_grant_option,
    })
}

// ── core ops (sync, tested directly; handlers are thin wrappers) ───────────

fn op_create_user(
    svc: &onto_ontology::policy_store::PolicyService,
    name: &str,
    kind: Option<&str>,
    password: Option<&str>,
) -> Result<serde_json::Value, PolicyOpErr> {
    let name = name.trim();
    if name.is_empty() {
        return Err(op_bad("name is required"));
    }
    let kind = match kind.unwrap_or("user").trim().to_ascii_lowercase().as_str() {
        "user" => PrincipalKind::User,
        "service_account" | "service" | "serviceaccount" => PrincipalKind::ServiceAccount,
        other => {
            return Err(op_bad(format!(
                "unknown kind '{other}' (expected user|service_account)"
            )))
        }
    };
    // Create-only: ALTER USER is the update path (and bumps token_epoch).
    // Mirrors the DDL plane's split between CREATE/ALTER.
    if svc.engine.principal(name).is_some() {
        return Err(op_conflict(format!("user '{name}' already exists")));
    }
    svc.upsert_principal(Principal {
        id: name.to_string(),
        kind,
        roles: Default::default(),
        token_epoch: 0,
        disabled: false,
        cred_scheme: CRED_SCHEME_BLAKE3.to_string(),
    })
    .map_err(|e| op_internal(e.to_string()))?;
    if let Some(pw) = password {
        let mut salt = [0u8; CRED_SALT_LEN];
        rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut salt);
        let pepper = std::env::var("ONTODB_CRED_PEPPER").ok();
        svc.put_credential(CredentialRecord {
            principal_id: name.to_string(),
            salt,
            hash: password_hash(pw, &salt, pepper.as_ref().map(|s| s.as_bytes())),
            cred_scheme: CRED_SCHEME_BLAKE3.to_string(),
        })
        .map_err(|e| op_internal(e.to_string()))?;
    }
    Ok(json!({
        "success": true,
        "name": name,
        "kind": match kind { PrincipalKind::User => "user", PrincipalKind::ServiceAccount => "service_account" },
        "message": format!("user '{name}' created"),
    }))
}

fn op_create_role(
    svc: &onto_ontology::policy_store::PolicyService,
    name: &str,
) -> Result<serde_json::Value, PolicyOpErr> {
    let name = name.trim();
    if name.is_empty() {
        return Err(op_bad("name is required"));
    }
    if !svc.create_role(name).map_err(|e| op_internal(e.to_string()))? {
        return Err(op_conflict(format!("role '{name}' already exists")));
    }
    Ok(json!({
        "success": true,
        "name": name,
        "message": format!("role '{name}' created"),
    }))
}

fn op_create_grant(
    svc: &onto_ontology::policy_store::PolicyService,
    role: &str,
    action: &str,
    object: &str,
    only: bool,
    with_grant_option: bool,
) -> Result<serde_json::Value, PolicyOpErr> {
    let role = role.trim();
    if role.is_empty() {
        return Err(op_bad("role is required"));
    }
    if !svc.engine.role_exists(role) {
        return Err(op_bad(format!(
            "no such role '{role}' (CREATE ROLE first)"
        )));
    }
    let action = parse_action_word(action).map_err(op_bad)?;
    let object = parse_object_ref(object).map_err(op_bad)?;
    let options = onto_core::policy::GrantOptions {
        only,
        with_grant_option,
    };
    svc.grant(Grant {
        role: role.to_string(),
        action,
        object: object.clone(),
        options,
    })
    .map_err(|e| op_internal(e.to_string()))?;
    Ok(json!({
        "success": true,
        "id": grant_row_id(role, action, &object),
        "role": role,
        "action": action.as_str(),
        "object": object.display(),
        "effect": "ALLOW",
        "only": only,
        "with_grant_option": with_grant_option,
        "message": format!(
            "granted {} on {} to '{}'{}{}",
            action.as_str(),
            object.display(),
            role,
            if only { " ONLY" } else { "" },
            if with_grant_option { " WITH GRANT OPTION" } else { "" }
        ),
    }))
}

fn op_delete_grant(
    svc: &onto_ontology::policy_store::PolicyService,
    id: &str,
) -> Result<serde_json::Value, PolicyOpErr> {
    let (role, action, object) = parse_grant_id(id).map_err(op_bad)?;
    // The key identifies the row; reuse the row's own options so an ONLY
    // grant is carved out with an ONLY deny (the deny must not widen past
    // what the allow covered).
    let mut options = onto_core::policy::GrantOptions::default();
    let mut found = false;
    for g in svc
        .engine
        .grants_for_role(&role)
        .into_iter()
        .chain(svc.engine.denies_for_role(&role))
    {
        if g.action == action && g.object == object {
            options = g.options;
            found = true;
            break;
        }
    }
    if !found {
        return Err(op_not_found(format!("no such grant '{id}'")));
    }
    // REVOKE semantics (design §7.1): remove the allow and plant an explicit
    // deny so subclasses cannot re-inherit the removed grant. The row
    // transitions ALLOW → DENY under the same id.
    let removed = svc
        .revoke(&Grant {
            role: role.clone(),
            action,
            object: object.clone(),
            options,
        })
        .map_err(|e| op_internal(e.to_string()))?;
    Ok(json!({
        "success": true,
        "id": id,
        "role": role,
        "action": action.as_str(),
        "object": object.display(),
        "effect": "DENY",
        "only": options.only,
        "revoked": removed,
        "message": format!(
            "{} on {} {} role '{}'{}",
            action.as_str(),
            object.display(),
            if removed { "revoked from" } else { "denied for" },
            role,
            if options.only { " ONLY" } else { "" }
        ),
    }))
}

fn op_list_grants(
    svc: &onto_ontology::policy_store::PolicyService,
    role: Option<&str>,
) -> Result<serde_json::Value, PolicyOpErr> {
    let mut rows = match role {
        Some(r) => svc.engine.grants_for_role(r),
        None => svc.engine.grants(),
    }
    .iter()
    .map(|g| grant_row_json(g, "ALLOW"))
    .collect::<Vec<_>>();
    rows.extend(
        match role {
            Some(r) => svc.engine.denies_for_role(r),
            None => svc.engine.denies(),
        }
        .iter()
        .map(|g| grant_row_json(g, "DENY")),
    );
    Ok(json!({"success": true, "grants": rows, "count": rows.len()}))
}

fn op_rotate_token(
    svc: &onto_ontology::policy_store::PolicyService,
    principal: &str,
) -> Result<serde_json::Value, PolicyOpErr> {
    let principal = principal.trim();
    if principal.is_empty() {
        return Err(op_bad("principal is required"));
    }
    // Epoch bump + replacement token (design §10) — identical to the ROTATE
    // TOKEN DDL. The minted secret goes to the caller only.
    let mut raw = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut raw);
    let token: String = raw.iter().map(|b| format!("{b:02x}")).collect();
    if !svc
        .rotate_token(principal, &token)
        .map_err(|e| op_internal(e.to_string()))?
    {
        return Err(op_not_found(format!("no such principal '{principal}'")));
    }
    Ok(json!({
        "success": true,
        "principal": principal,
        "token": token,
        "message": format!("token rotated for '{principal}'"),
    }))
}

// ── handlers ───────────────────────────────────────────────────────────────

/// POST /api/admin/users — create a user/service-account principal.
pub async fn admin_create_user(
    axum::extract::State(state): axum::extract::State<AdminState>,
    caller: Option<axum::Extension<crate::auth::KeyInfo>>,
    Json(req): Json<AdminCreateUserRequest>,
) -> impl IntoResponse {
    if let Err((st, v)) = require_policy_admin(&state, caller.as_ref().map(|e| &e.0)) {
        return (st, Json(v));
    }
    let svc = match policy_svc(&state) {
        Ok(s) => s,
        Err((st, v)) => return (st, Json(v)),
    };
    let op_detail = format!("CREATE USER {}", req.name);
    match op_create_user(svc, &req.name, req.kind.as_deref(), req.password.as_deref()) {
        Ok(v) => {
            policy_audit(&state, "grant_change", &format!("{op_detail} -> {}", v["message"]), true, None);
            (StatusCode::CREATED, Json(v))
        }
        Err((st, v)) => {
            policy_audit(
                &state,
                "grant_change",
                &format!("{op_detail} -> error: {}", v["error"]),
                false,
                Some(v["error"].to_string()),
            );
            (st, Json(v))
        }
    }
}

/// POST /api/admin/roles — create a role.
pub async fn admin_create_role(
    axum::extract::State(state): axum::extract::State<AdminState>,
    caller: Option<axum::Extension<crate::auth::KeyInfo>>,
    Json(req): Json<AdminCreateRoleRequest>,
) -> impl IntoResponse {
    if let Err((st, v)) = require_policy_admin(&state, caller.as_ref().map(|e| &e.0)) {
        return (st, Json(v));
    }
    let svc = match policy_svc(&state) {
        Ok(s) => s,
        Err((st, v)) => return (st, Json(v)),
    };
    let op_detail = format!("CREATE ROLE {}", req.name);
    match op_create_role(svc, &req.name) {
        Ok(v) => {
            policy_audit(&state, "grant_change", &format!("{op_detail} -> {}", v["message"]), true, None);
            (StatusCode::CREATED, Json(v))
        }
        Err((st, v)) => {
            policy_audit(
                &state,
                "grant_change",
                &format!("{op_detail} -> error: {}", v["error"]),
                false,
                Some(v["error"].to_string()),
            );
            (st, Json(v))
        }
    }
}

/// POST /api/admin/grants — GRANT semantics: insert the allow, clear a
/// same-key deny.
pub async fn admin_create_grant(
    axum::extract::State(state): axum::extract::State<AdminState>,
    caller: Option<axum::Extension<crate::auth::KeyInfo>>,
    Json(req): Json<AdminCreateGrantRequest>,
) -> impl IntoResponse {
    if let Err((st, v)) = require_policy_admin(&state, caller.as_ref().map(|e| &e.0)) {
        return (st, Json(v));
    }
    let svc = match policy_svc(&state) {
        Ok(s) => s,
        Err((st, v)) => return (st, Json(v)),
    };
    let only = req.only.unwrap_or(false);
    let with_grant_option = req.with_grant_option.unwrap_or(false);
    let op_detail = format!(
        "GRANT {} ON {}{}{} TO {}",
        req.action.trim().to_ascii_uppercase(),
        req.object.trim(),
        if only { " ONLY" } else { "" },
        if with_grant_option {
            " WITH GRANT OPTION"
        } else {
            ""
        },
        req.role
    );
    match op_create_grant(
        svc,
        &req.role,
        &req.action,
        &req.object,
        only,
        with_grant_option,
    ) {
        Ok(v) => {
            policy_audit(&state, "grant_change", &format!("{op_detail} -> {}", v["message"]), true, None);
            (StatusCode::CREATED, Json(v))
        }
        Err((st, v)) => {
            policy_audit(
                &state,
                "grant_change",
                &format!("{op_detail} -> error: {}", v["error"]),
                false,
                Some(v["error"].to_string()),
            );
            (st, Json(v))
        }
    }
}

/// DELETE /api/admin/grants/{id} — REVOKE semantics (design §7.1): the row
/// transitions ALLOW → DENY under the same id.
pub async fn admin_delete_grant(
    axum::extract::State(state): axum::extract::State<AdminState>,
    caller: Option<axum::Extension<crate::auth::KeyInfo>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> impl IntoResponse {
    if let Err((st, v)) = require_policy_admin(&state, caller.as_ref().map(|e| &e.0)) {
        return (st, Json(v));
    }
    let svc = match policy_svc(&state) {
        Ok(s) => s,
        Err((st, v)) => return (st, Json(v)),
    };
    let op_detail = format!("REVOKE {id}");
    match op_delete_grant(svc, &id) {
        Ok(v) => {
            policy_audit(&state, "grant_change", &format!("{op_detail} -> {}", v["message"]), true, None);
            (StatusCode::OK, Json(v))
        }
        Err((st, v)) => {
            policy_audit(
                &state,
                "grant_change",
                &format!("{op_detail} -> error: {}", v["error"]),
                false,
                Some(v["error"].to_string()),
            );
            (st, Json(v))
        }
    }
}

/// GET /api/admin/grants?role=analyst — list ALLOW and DENY rows.
pub async fn admin_list_grants(
    axum::extract::State(state): axum::extract::State<AdminState>,
    caller: Option<axum::Extension<crate::auth::KeyInfo>>,
    axum::extract::Query(q): axum::extract::Query<AdminGrantListQuery>,
) -> impl IntoResponse {
    if let Err((st, v)) = require_policy_admin(&state, caller.as_ref().map(|e| &e.0)) {
        return (st, Json(v));
    }
    let svc = match policy_svc(&state) {
        Ok(s) => s,
        Err((st, v)) => return (st, Json(v)),
    };
    match op_list_grants(svc, q.role.as_deref()) {
        Ok(v) => (StatusCode::OK, Json(v)),
        Err((st, v)) => (st, Json(v)),
    }
}

/// POST /api/admin/tokens/rotate — epoch bump + replacement token (the
/// minted secret is returned once and never audited).
pub async fn admin_rotate_token(
    axum::extract::State(state): axum::extract::State<AdminState>,
    caller: Option<axum::Extension<crate::auth::KeyInfo>>,
    Json(req): Json<AdminRotateTokenRequest>,
) -> impl IntoResponse {
    if let Err((st, v)) = require_policy_admin(&state, caller.as_ref().map(|e| &e.0)) {
        return (st, Json(v));
    }
    let svc = match policy_svc(&state) {
        Ok(s) => s,
        Err((st, v)) => return (st, Json(v)),
    };
    let op_detail = format!("ROTATE TOKEN {}", req.principal);
    match op_rotate_token(svc, &req.principal) {
        Ok(v) => {
            // The DDL audits "token rotated" — never the minted secret.
            policy_audit(
                &state,
                "token_rotate",
                &format!("{op_detail} -> token rotated"),
                true,
                None,
            );
            (StatusCode::OK, Json(v))
        }
        Err((st, v)) => {
            policy_audit(
                &state,
                "token_rotate",
                &format!("{op_detail} -> error: {}", v["error"]),
                false,
                Some(v["error"].to_string()),
            );
            (st, Json(v))
        }
    }
}

#[cfg(test)]
mod policy_tests {
    use super::*;
    use onto_core::policy::Action;
    use onto_ontology::policy_store::PolicyStore;
    use onto_storage::{LsmEngine, StorageOptions};
    use std::sync::Arc;

    fn service(dir: &std::path::Path) -> Arc<onto_ontology::policy_store::PolicyService> {
        let engine = Arc::new(
            LsmEngine::open(StorageOptions {
                data_dir: dir.to_path_buf(),
                ..Default::default()
            })
            .unwrap(),
        );
        onto_ontology::policy_store::PolicyService::open(PolicyStore::new(engine)).unwrap()
    }

    #[test]
    fn object_ref_display_round_trips() {
        // Bare class/property names normalize to the `_default` namespace the
        // way the PEP splits qualified names, so display() is the canonical
        // qualified form — round-trip means parse∘display is a fixed point.
        for (input, expected_display) in [
            ("NAMESPACE *", "NAMESPACE *"),
            ("NAMESPACE sembio", "NAMESPACE sembio"),
            ("CLASS zoo.Dog", "CLASS zoo.Dog"),
            ("CLASS Dog", "CLASS _default.Dog"),
            ("PROPERTY zoo.Dog.name", "PROPERTY zoo.Dog.name"),
            ("PROPERTY Dog.name", "PROPERTY _default.Dog.name"),
            ("RULE CaxSco", "RULE CaxSco"),
            ("RULE cax-sco", "RULE cax-sco"),
        ] {
            let obj = parse_object_ref(input).unwrap_or_else(|e| panic!("parse '{input}': {e}"));
            assert_eq!(obj.display(), expected_display, "display of '{input}'");
            let again = parse_object_ref(obj.display().as_str())
                .unwrap_or_else(|e| panic!("re-parse '{}': {e}", obj.display()));
            assert_eq!(again, obj, "display is a fixed point for '{input}'");
            let id = grant_row_id("analyst", Action::Read, &obj);
            let (role, action, back) =
                parse_grant_id(&id).unwrap_or_else(|e| panic!("id '{id}': {e}"));
            assert_eq!(role, "analyst");
            assert_eq!(action, Action::Read);
            assert_eq!(back, obj, "id round-trip '{input}'");
        }
    }

    #[test]
    fn action_word_and_object_reject_garbage() {
        assert!(parse_action_word("read").is_ok());
        assert!(parse_action_word("INFER").is_ok());
        assert!(parse_action_word("ADMIN").is_err());
        assert!(parse_action_word("").is_err());
        assert!(parse_object_ref("CLASS").is_err());
        assert!(parse_object_ref("WIDGET x").is_err());
        assert!(parse_object_ref("PROPERTY x").is_err());
        assert!(parse_object_ref("").is_err());
        assert!(parse_grant_id("nonsense").is_err());
        assert!(parse_grant_id("r:BOGUS:CLASS:x").is_err());
    }

    #[test]
    fn admin_policy_flow_user_role_grant_delete_rotate() {
        let dir = tempfile::tempdir().unwrap();
        let svc = service(dir.path());

        // Users: create-only, 409 on replay (ALTER USER is the update path).
        op_create_user(&svc, "alice", Some("user"), Some("pw")).unwrap();
        let pepper = std::env::var("ONTODB_CRED_PEPPER").ok();
        let p = svc.engine.principal("alice").expect("alice exists");
        assert!(matches!(p.kind, onto_core::policy::PrincipalKind::User));
        assert!(svc
            .engine
            .authenticate_password("alice", "pw", pepper.as_ref().map(|s| s.as_bytes()))
            .is_some());
        assert!(matches!(
            op_create_user(&svc, "alice", None, None),
            Err((StatusCode::CONFLICT, _))
        ));

        // Service-account kind is accepted and recorded.
        op_create_user(&svc, "ingest", Some("service_account"), None).unwrap();
        let p = svc.engine.principal("ingest").unwrap();
        assert!(matches!(
            p.kind,
            onto_core::policy::PrincipalKind::ServiceAccount
        ));

        // Roles: create-only, same as the CREATE ROLE DDL.
        op_create_role(&svc, "analyst").unwrap();
        assert!(svc.engine.role_exists("analyst"));
        assert!(matches!(
            op_create_role(&svc, "analyst"),
            Err((StatusCode::CONFLICT, _))
        ));

        // Grants: unknown role fails, then a real grant lands and lists.
        assert!(op_create_grant(&svc, "ghost", "READ", "CLASS zoo.Dog", false, false).is_err());
        let g = op_create_grant(&svc, "analyst", "read", "CLASS zoo.Dog", true, true).unwrap();
        assert_eq!(g["effect"], "ALLOW");
        assert_eq!(g["with_grant_option"], true);
        let id = g["id"].as_str().unwrap().to_string();
        assert_eq!(id, "analyst:READ:CLASS:zoo.Dog");
        let list = op_list_grants(&svc, Some("analyst")).unwrap();
        assert_eq!(list["count"], 1);
        assert_eq!(list["grants"][0]["id"], id.as_str());
        assert_eq!(list["grants"][0]["only"], true);
        assert_eq!(list["grants"][0]["with_grant_option"], true);
        // The grant actually authorizes once the principal holds the role
        // (ONLY semantics: the class itself is covered).
        svc.grant_role("alice", "analyst").unwrap();
        assert!(svc.engine.check_class("alice", Action::Read, "zoo", "Dog"));

        // DELETE = REVOKE: the row transitions ALLOW → DENY under the same
        // id, and the deny blocks the class (design §7.1 carve-out).
        let d = op_delete_grant(&svc, &id).unwrap();
        assert_eq!(d["effect"], "DENY");
        assert_eq!(d["revoked"], true);
        let list = op_list_grants(&svc, Some("analyst")).unwrap();
        assert_eq!(list["count"], 1);
        assert_eq!(list["grants"][0]["effect"], "DENY");
        assert_eq!(list["grants"][0]["id"], id.as_str());
        assert!(!svc
            .engine
            .check_class("alice", Action::Read, "zoo", "Dog"));
        // Deleting again is a no-op deny (no allow left to remove).
        let d = op_delete_grant(&svc, &id).unwrap();
        assert_eq!(d["revoked"], false);
        assert!(op_delete_grant(&svc, "ghost:READ:CLASS:zoo.Dog").is_err());

        // Tokens: rotate mints a fresh token, the principal's epoch bumps,
        // and the secret is returned to the caller only.
        let t = op_rotate_token(&svc, "alice").unwrap();
        let token = t["token"].as_str().unwrap().to_string();
        assert_eq!(token.len(), 64);
        assert!(svc.engine.authenticate_token(&token).is_some());
        let t2 = op_rotate_token(&svc, "alice").unwrap();
        let token2 = t2["token"].as_str().unwrap().to_string();
        assert_ne!(token, token2);
        assert!(svc.engine.authenticate_token(&token).is_none(), "old token dies with the epoch bump");
        assert!(svc.engine.authenticate_token(&token2).is_some());
        assert!(matches!(
            op_rotate_token(&svc, "ghost"),
            Err((StatusCode::NOT_FOUND, _))
        ));
    }
}
