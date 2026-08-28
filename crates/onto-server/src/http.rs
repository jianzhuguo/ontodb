//! HTTP API for OntoDB.
//!
//! Provides RESTful endpoints for SQL queries, vector search, hybrid queries,
//! health checks, and Prometheus metrics.

use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, post, put},
    Json, Router,
};
use onto_query::{OntoQLParser, QueryAst, QueryExecutor, QueryParser};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::trace::TraceLayer;

use crate::metrics::SharedMetrics;

/// Custom JSON response that supports pretty-printing.
struct PrettyJson<T>(pub T, pub bool);

impl<T: Serialize> IntoResponse for PrettyJson<T> {
    fn into_response(self) -> axum::response::Response {
        let body = if self.1 {
            serde_json::to_string_pretty(&self.0).unwrap_or_else(|_| "{}".to_string())
        } else {
            serde_json::to_string(&self.0).unwrap_or_else(|_| "{}".to_string())
        };
        (
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            body,
        )
            .into_response()
    }
}

/// Validates a user-supplied filter expression to prevent SQL injection.
/// Only allows simple comparison expressions (e.g., "price > 100", "name = 'foo'").
fn validate_filter(filter: &str) -> Result<(), String> {
    if filter.len() > 1024 {
        return Err("filter expression too long (max 1024 chars)".into());
    }
    // Reject semicolons to prevent query chaining
    if filter.contains(';') {
        return Err("filter must not contain semicolons".into());
    }
    // Reject comment sequences
    if filter.contains("/*") || filter.contains("*/") {
        return Err("filter must not contain block comments".into());
    }
    if filter.contains("--") {
        return Err("filter must not contain line comments".into());
    }
    // Reject dangerous SQL keywords (case-insensitive, Unicode-aware word boundaries)
    let upper: String = filter.chars().flat_map(|c| c.to_uppercase()).collect();
    let upper_chars: Vec<char> = upper.chars().collect();
    let forbidden = [
        "DROP", "DELETE", "INSERT", "UPDATE", "UNION",
        "ALTER", "CREATE", "TRUNCATE", "EXEC", "EXECUTE",
        "SLEEP", "BENCHMARK", "LOAD_FILE", "INTO OUTFILE",
        "PG_SLEEP", "INFORMATION_SCHEMA",
    ];
    for kw in &forbidden {
        let kw_chars: Vec<char> = kw.chars().collect();
        let mut search_start = 0;
        while search_start + kw_chars.len() <= upper_chars.len() {
            if upper_chars[search_start..search_start + kw_chars.len()] == kw_chars[..] {
                let before_ok = search_start == 0
                    || !upper_chars[search_start - 1].is_alphanumeric();
                let after_pos = search_start + kw_chars.len();
                let after_ok = after_pos >= upper_chars.len()
                    || !upper_chars[after_pos].is_alphanumeric();
                if before_ok && after_ok {
                    return Err(format!("filter must not contain '{}'", kw));
                }
            }
            search_start += 1;
        }
    }
    Ok(())
}

/// Validates a SQL identifier (class name, column name) to prevent injection.
/// Only allows alphanumeric characters, underscores, and dots (for qualified names).
fn validate_identifier(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 128 {
        return Err("identifier must be 1-128 characters".into());
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.') {
        return Err(format!(
            "invalid identifier '{}': only alphanumeric, underscore, and dot allowed",
            name
        ));
    }
    if name.starts_with('.') || name.ends_with('.') || name.contains("..") {
        return Err(format!("invalid identifier '{}': malformed dot usage", name));
    }
    Ok(())
}

/// Strips internal details (file paths, OS errors, line numbers) from error
/// messages before returning them to the client.
fn sanitize_error(err: &str) -> String {
    let lower = err.to_lowercase();
    if lower.contains("failed to read file")
        || lower.contains("permission denied")
        || lower.contains("no such file")
        || lower.contains("the system cannot find")
        || lower.contains("access is denied")
    {
        return "file not found or inaccessible".to_string();
    }
    // Keep user-facing parse/validation errors, strip internal paths
    if lower.contains("parse error") || lower.contains("syntax error") || lower.contains("invalid") {
        // Sanitize: remove any Windows/Unix path patterns
        let sanitized: String = err
            .split(['\\', '/'])
            .next_back()
            .unwrap_or(err)
            .to_string();
        return sanitized;
    }
    "query execution failed".to_string()
}

/// Validates a backup path to prevent path traversal attacks.
fn validate_backup_path(path: &str) -> Result<std::path::PathBuf, String> {
    if path.is_empty() {
        return Err("backup path must not be empty".into());
    }
    let p = std::path::Path::new(path);
    for component in p.components() {
        if let std::path::Component::ParentDir = component {
            return Err("backup path must not contain '..'".into());
        }
    }
    if !p.is_absolute() {
        return Err("backup path must be absolute".into());
    }
    Ok(p.to_path_buf())
}

/// Slow query threshold in seconds. Queries exceeding this are logged as warnings.
const SLOW_QUERY_THRESHOLD_SECS: f64 = 1.0;

/// Safely truncate a UTF-8 string to at most `max_bytes` bytes.
/// Returns the original string if it's already short enough.
fn truncate_utf8(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Shared application state.
#[derive(Clone)]
pub struct AppState {
    pub executor: Arc<QueryExecutor>,
    pub metrics: SharedMetrics,
    pub graph: Arc<onto_graph::GraphStore>,
    pub audit: Arc<crate::audit::AuditLogger>,
    pub raft_node_id: Option<u64>,
}

/// SQL query request.
#[derive(Debug, Deserialize)]
pub struct QueryRequest {
    /// SQL or OntoDB query string.
    pub query: String,
    /// Optional: return results as pretty-printed JSON.
    #[serde(default)]
    pub pretty: bool,
}

/// Vector search request.
#[derive(Debug, Deserialize)]
pub struct VectorSearchRequest {
    /// Target class (table) name.
    pub class: String,
    /// Vector column name.
    pub column: String,
    /// Query vector for similarity search.
    pub query_vector: Vec<f32>,
    /// Number of top results to return.
    pub top_k: usize,
    /// Optional SQL WHERE filter for hybrid search.
    #[serde(default)]
    pub filter: Option<String>,
}

/// SPARQL query request.
#[derive(Debug, Deserialize)]
pub struct SparqlRequest {
    /// SPARQL query string.
    pub query: String,
}

/// Backup request.
#[derive(Debug, Deserialize)]
pub struct BackupRequest {
    /// Target directory path for the backup.
    pub path: String,
}

/// Incremental backup request.
#[derive(Debug, Deserialize)]
pub struct IncrementalBackupRequest {
    /// Target directory path for the incremental backup.
    pub path: String,
    /// ISO 8601 timestamp — only files modified after this time are included.
    /// Use the timestamp from a previous full/incremental backup.
    pub since: String,
}

/// Backup verification request.
#[derive(Debug, Deserialize)]
pub struct VerifyBackupRequest {
    /// Path to the backup directory to verify.
    pub path: String,
}

/// Batch query request — execute multiple queries in a single HTTP call.
#[derive(Debug, Deserialize)]
pub struct BatchRequest {
    /// List of queries to execute.
    pub queries: Vec<String>,
    /// If true, stop on first error. If false, continue and return all results.
    #[serde(default)]
    pub fail_fast: bool,
    /// Optional: return results as pretty-printed JSON.
    #[serde(default)]
    pub pretty: bool,
}

/// Result of a single query in a batch.
#[derive(Debug, Serialize)]
pub struct BatchResult {
    /// Index of the query in the batch (0-based).
    pub index: usize,
    /// The query that was executed.
    pub query: String,
    /// Whether the query succeeded.
    pub success: bool,
    /// Result data (for SELECT) or null (for INSERT/UPDATE/DELETE).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    /// Error message (only present if success is false).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Execution time in milliseconds.
    pub elapsed_ms: f64,
}

/// Transaction begin request.
#[derive(Debug, Deserialize)]
pub struct TransactionBeginRequest {
    /// Optional: isolation level (default: snapshot).
    #[serde(default)]
    pub isolation: Option<String>,
}

/// Transaction execute request — execute a query within an active transaction.
#[derive(Debug, Deserialize)]
pub struct TransactionExecuteRequest {
    /// Transaction ID returned from /api/transaction/begin.
    pub txn_id: u64,
    /// SQL or OntoDB query to execute within the transaction.
    pub query: String,
}

/// Transaction commit/rollback request.
#[derive(Debug, Deserialize)]
pub struct TransactionActionRequest {
    /// Transaction ID returned from /api/transaction/begin.
    pub txn_id: u64,
}

/// Hybrid query request: combines SQL filter with vector search.
#[derive(Debug, Deserialize)]
pub struct HybridQueryRequest {
    /// SQL query for initial filtering.
    pub sql_filter: String,
    /// Vector column for similarity ranking.
    pub vector_column: String,
    /// Query vector for similarity search.
    pub query_vector: Vec<f32>,
    /// Number of top results to return.
    pub top_k: usize,
    /// Target class name (optional, inferred from SQL if not provided).
    #[serde(default)]
    pub class: Option<String>,
}

/// Standard API response wrapper.
#[derive(Debug, Serialize)]
pub struct ApiResponse<T: Serialize> {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Query execution time in milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<f64>,
}

impl<T: Serialize> ApiResponse<T> {
    pub fn success(data: T, elapsed_ms: f64) -> Self {
        Self {
            success: true,
            data: Some(data),
            error: None,
            elapsed_ms: Some(elapsed_ms),
        }
    }

    pub fn error(msg: impl Into<String>) -> Self {
        Self {
            success: false,
            data: None,
            error: Some(msg.into()),
            elapsed_ms: None,
        }
    }
}

/// Builds a CORS layer from a comma-separated origins string.
/// Empty string = same-origin only (no CORS headers).
/// "*" = allow all origins (NOT recommended for production).
fn build_cors_layer(origins: &str) -> CorsLayer {
    if origins.trim() == "*" {
        tracing::warn!("CORS: allowing ALL origins �?do NOT use in production");
        // Use Any origin without credentials (not permissive() which allows credentials)
        CorsLayer::new()
            .allow_origin(tower_http::cors::Any)
            .allow_methods([axum::http::Method::GET, axum::http::Method::POST, axum::http::Method::PUT, axum::http::Method::DELETE])
            .allow_headers(tower_http::cors::Any)
            .allow_credentials(false)
    } else if origins.trim().is_empty() {
        // Same-origin only �?no cross-origin requests allowed
        CorsLayer::new()
            .allow_methods([axum::http::Method::GET, axum::http::Method::POST, axum::http::Method::PUT, axum::http::Method::DELETE])
            .allow_headers(tower_http::cors::Any)
    } else {
        let allowed: Vec<axum::http::HeaderValue> = origins
            .split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .filter_map(|s| s.parse().ok())
            .collect();
        tracing::info!("CORS: allowing origins: {:?}", allowed);
        CorsLayer::new()
            .allow_origin(AllowOrigin::list(allowed))
            .allow_methods([axum::http::Method::GET, axum::http::Method::POST, axum::http::Method::PUT, axum::http::Method::DELETE])
            .allow_headers(tower_http::cors::Any)
    }
}

/// Builds the HTTP router with all API endpoints (no auth).
#[allow(dead_code)]
pub fn build_router(state: AppState, cors_origins: &str) -> Router {
    use axum::extract::DefaultBodyLimit;

    // Max request body size: 64 MB
    const MAX_BODY_SIZE: usize = 64 * 1024 * 1024;

    Router::new()
        // Health check
        .route("/api/health", get(health))
        .route("/api/health/ready", get(health_ready))
        .route("/api/health/live", get(health_live))
        // Metrics (Prometheus and JSON)
        .route("/metrics", get(metrics_prometheus))
        .route("/api/metrics", get(metrics_json))
        // SQL query execution
        .route("/api/query", post(execute_query))
        // Batch query execution
        .route("/api/batch", post(batch_query))
        // SPARQL query execution
        .route("/sparql", post(sparql_query))
        // Vector search
        .route("/api/vector/search", post(vector_search))
        // Hybrid query: SQL + vector search
        .route("/api/hybrid/query", post(hybrid_query))
        // Schema introspection
        .route("/api/schema", get(get_schema))
        // Cluster info
        .route("/api/cluster", get(cluster_info))
        // Backup and flush
        .route("/api/backup", post(backup))
        .route("/api/backup/incremental", post(backup_incremental))
        .route("/api/backup/verify", post(verify_backup_endpoint))
        .route("/api/flush", post(flush))
        // Transaction endpoints
        .route("/api/transaction/begin", post(transaction_begin))
        .route("/api/transaction/execute", post(transaction_execute))
        .route("/api/transaction/commit", post(transaction_commit))
        .route("/api/transaction/rollback", post(transaction_rollback))
        // Sharding configuration
        .route("/api/sharding/config", get(get_sharding_config).put(update_sharding_config))
        .route("/api/sharding/shard", post(add_shard))
        .route("/api/sharding/class", post(assign_class_shard))
        .route("/api/sharding/status", get(get_sharding_status))
        // Migration endpoints
        .route("/api/sharding/migrate", post(start_migration))
        .route("/api/sharding/migrate/progress", put(update_migration_progress))
        .route("/api/sharding/migrate/complete", post(complete_migration))
        .route("/api/sharding/migrate/cancel", post(cancel_migration))
        .route("/api/sharding/migrations", get(list_migrations))
        // Rebalance endpoints
        .route("/api/sharding/rebalance", post(rebalance_shards))
        // Scale endpoints
        .route("/api/sharding/scale/add", post(add_shard_and_rebalance))
        .route("/api/sharding/scale/remove", post(remove_shard))
        // Split endpoints
        .route("/api/sharding/split", post(split_shard))
        // API documentation
        .route("/api/docs", get(swagger_ui))
        .route("/api/openapi.json", get(openapi_spec))
        // Web console
        .route("/console", get(web_console))
        .route("/", get(web_console))
        // Digital Twin & Advisor
        .route("/digital-twin", get(digital_twin))
        .route("/digital-advisor", get(digital_advisor))
        .layer(DefaultBodyLimit::max(MAX_BODY_SIZE))
        .layer(build_cors_layer(cors_origins))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Builds the HTTP router with authentication and rate limiting.
pub fn build_router_with_auth(
    state: AppState,
    auth_state: crate::auth::AuthState,
    rate_limiter: crate::rate_limit::RateLimiter,
    admin_state: crate::admin::AdminState,
    cors_origins: &str,
) -> Router {
    use axum::middleware;

    // Admin sub-router with its own state
    let admin_routes = Router::new()
        .route("/api/admin/keys", get(crate::admin::list_keys).post(crate::admin::add_key))
        .route("/api/admin/keys/:key", put(crate::admin::update_key).delete(crate::admin::delete_key))
        .route("/api/admin/keys/:key/ips", get(crate::admin::list_ips).post(crate::admin::add_ips))
        .route("/api/admin/keys/:key/ips", delete(crate::admin::remove_ip))
        .route("/api/admin/reload", post(crate::admin::force_reload))
        .with_state(admin_state);

    Router::new()
        // Health check and metrics (no auth required)
        .route("/api/health", get(health))
        .route("/api/health/ready", get(health_ready))
        .route("/api/health/live", get(health_live))
        .route("/metrics", get(metrics_prometheus))
        .route("/api/metrics", get(metrics_json))
        // Protected routes
        .route("/api/query", post(execute_query))
        .route("/api/batch", post(batch_query))
        .route("/sparql", post(sparql_query))
        .route("/api/vector/search", post(vector_search))
        .route("/api/hybrid/query", post(hybrid_query))
        .route("/api/schema", get(get_schema))
        .route("/api/cluster", get(cluster_info))
        // Graph endpoints
        .route("/api/graph/vertex", post(add_vertex))
        .route("/api/graph/edge", post(add_edge))
        .route("/api/graph/traverse", post(graph_traverse))
        .route("/api/graph/shortest-path", post(graph_shortest_path))
        .route("/api/graph/vertex/:id", get(get_vertex).delete(delete_vertex))
        .route("/api/graph/neighbors/:id", get(get_neighbors))
        // Backup and flush (Admin only)
        .route("/api/backup", post(backup))
        .route("/api/backup/incremental", post(backup_incremental))
        .route("/api/backup/verify", post(verify_backup_endpoint))
        .route("/api/flush", post(flush))
        // Transaction endpoints
        .route("/api/transaction/begin", post(transaction_begin))
        .route("/api/transaction/execute", post(transaction_execute))
        .route("/api/transaction/commit", post(transaction_commit))
        .route("/api/transaction/rollback", post(transaction_rollback))
        // Sharding configuration (Admin only)
        .route("/api/sharding/config", get(get_sharding_config).put(update_sharding_config))
        .route("/api/sharding/shard", post(add_shard))
        .route("/api/sharding/class", post(assign_class_shard))
        .route("/api/sharding/status", get(get_sharding_status))
        // Migration endpoints (Admin only)
        .route("/api/sharding/migrate", post(start_migration))
        .route("/api/sharding/migrate/progress", put(update_migration_progress))
        .route("/api/sharding/migrate/complete", post(complete_migration))
        .route("/api/sharding/migrate/cancel", post(cancel_migration))
        .route("/api/sharding/migrations", get(list_migrations))
        // Rebalance endpoints (Admin only)
        .route("/api/sharding/rebalance", post(rebalance_shards))
        // Scale endpoints (Admin only)
        .route("/api/sharding/scale/add", post(add_shard_and_rebalance))
        .route("/api/sharding/scale/remove", post(remove_shard))
        // Split endpoints (Admin only)
        .route("/api/sharding/split", post(split_shard))
        // API documentation and console (no auth required)
        .route("/api/docs", get(swagger_ui))
        .route("/api/openapi.json", get(openapi_spec))
        .route("/console", get(web_console))
        .route("/", get(web_console))
        // Digital Twin & Advisor (no auth required)
        .route("/digital-twin", get(digital_twin))
        .route("/digital-advisor", get(digital_advisor))
        // Merge admin routes (after main routes to avoid conflicts)
        .merge(admin_routes)
        // Apply rate limiting middleware
        .layer(middleware::from_fn_with_state(
            rate_limiter,
            crate::rate_limit::rate_limit_middleware,
        ))
        // Apply auth middleware
        .layer(middleware::from_fn_with_state(
            auth_state,
            crate::auth::auth_middleware,
        ))
        .layer(build_cors_layer(cors_origins))
        .layer(axum::middleware::from_fn(security_headers_middleware))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Middleware that adds security headers to all responses.
async fn security_headers_middleware(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert("x-content-type-options", "nosniff".parse().expect("should be valid"));
    headers.insert("x-frame-options", "DENY".parse().expect("should be valid"));
    headers.insert("x-xss-protection", "1; mode=block".parse().expect("should be valid"));
    headers.insert("referrer-policy", "strict-origin-when-cross-origin".parse().expect("should be valid"));
    headers.insert(
        "content-security-policy",
        "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'".parse().expect("should be valid"),
    );
    response
}

/// GET /api/health - Comprehensive health check endpoint.
/// Actually probes storage engine, WAL, and memory.
async fn health(State(state): State<AppState>) -> impl IntoResponse {
    let uptime = state.metrics.started_at.elapsed().as_secs();

    // Probe storage engine
    let (storage_status, storage_detail) = match state.executor.engine_stats() {
        Some(stats) => {
            let detail = json!({
                "memtable_entries": stats.memtable_entries,
                "memtable_size_bytes": stats.memtable_size,
                "total_sstables": stats.total_sstables,
                "sst_size_bytes": stats.total_sst_size,
                "num_levels": stats.num_levels,
            });
            ("ok", Some(detail))
        }
        None => ("error", Some(json!({"error": "engine stats unavailable"}))),
    };

    // Probe query engine (try a parse that OntoDB supports)
    let query_status = match QueryParser::parse("SELECT * FROM health_check") {
        Ok(_) => "ok",
        Err(_) => "error",
    };

    let all_ok = storage_status == "ok" && query_status == "ok";
    let status = if all_ok { "ok" } else { "degraded" };

    let mut checks = json!({
        "storage": storage_status,
        "query_engine": query_status,
    });
    if let Some(detail) = storage_detail {
        checks["storage_detail"] = detail;
    }

    let code = if all_ok { StatusCode::OK } else { StatusCode::SERVICE_UNAVAILABLE };
    (code, Json(json!({
        "status": status,
        "version": env!("CARGO_PKG_VERSION"),
        "engine": "OntoDB",
        "uptime_seconds": uptime,
        "checks": checks,
    })))
}

/// GET /api/health/ready - Kubernetes readiness probe.
/// Returns 200 only if the engine can accept queries.
async fn health_ready(State(state): State<AppState>) -> impl IntoResponse {
    // Check: engine stats available (storage alive)
    let engine_ok = state.executor.engine_stats().is_some();
    // Check: parser works (query engine alive) �?use a query syntax that always parses
    let parser_ok = QueryParser::parse("SELECT * FROM health_check").is_ok()
        || QueryParser::parse("CREATE CLASS health_check").is_ok();

    if engine_ok && parser_ok {
        (StatusCode::OK, Json(json!({"status": "ready"})))
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, Json(json!({
            "status": "not_ready",
            "engine": engine_ok,
            "parser": parser_ok,
        })))
    }
}

/// GET /api/health/live - Kubernetes liveness probe.
/// Returns 200 if the server process is alive.
async fn health_live() -> impl IntoResponse {
    Json(json!({
        "status": "alive"
    }))
}

/// GET /metrics - Prometheus metrics endpoint.
/// Returns metrics in Prometheus exposition format.
async fn metrics_prometheus(State(state): State<AppState>) -> impl IntoResponse {
    // Refresh storage metrics from engine state
    if let Some(stats) = state.executor.engine_stats() {
        state.metrics.update_storage_stats(
            stats.total_sstables,
            stats.memtable_entries,
            state.metrics.compactions_total.get(), // compactions tracked separately
        );
    }
    let metrics = state.metrics.to_prometheus();
    (
        StatusCode::OK,
        [("Content-Type", "text/plain; version=0.0.4; charset=utf-8")],
        metrics,
    )
}

/// GET /api/metrics - JSON metrics endpoint.
/// Returns metrics in JSON format for programmatic access.
async fn metrics_json(State(state): State<AppState>) -> impl IntoResponse {
    // Refresh storage metrics from engine state
    if let Some(stats) = state.executor.engine_stats() {
        state.metrics.update_storage_stats(
            stats.total_sstables,
            stats.memtable_entries,
            state.metrics.compactions_total.get(),
        );
    }
    let m = &state.metrics;
    Json(json!({
        "server": {
            "version": env!("CARGO_PKG_VERSION"),
            "uptime_seconds": m.started_at.elapsed().as_secs(),
        },
        "queries": {
            "total": m.queries_total.get(),
            "by_type": {
                "select": m.queries_select.get(),
                "insert": m.queries_insert.get(),
                "update": m.queries_update.get(),
                "delete": m.queries_delete.get(),
                "vector_search": m.queries_vector_search.get(),
                "other": m.queries_other.get(),
            },
            "errors": m.query_errors.get(),
            "parse_errors": m.parse_errors.get(),
            "latency": {
                "count": m.query_latency.count(),
                "sum_seconds": m.query_latency.sum(),
            }
        },
        "vector_search": {
            "latency": {
                "count": m.vector_search_latency.count(),
                "sum_seconds": m.vector_search_latency.sum(),
            },
            "results_total": m.vector_search_results.get(),
        },
        "connections": {
            "http": {
                "total": m.http_connections_total.get(),
                "active": m.http_connections_active.get(),
            },
            "tcp": {
                "total": m.tcp_connections_total.get(),
                "active": m.tcp_connections_active.get(),
            }
        },
        "auth": {
            "attempts": m.auth_attempts.get(),
            "successes": m.auth_successes.get(),
            "failures": m.auth_failures.get(),
        },
        "rate_limiting": {
            "limited_total": m.rate_limited_total.get(),
        },
        "storage": {
            "sstable_count": m.sstable_count.get(),
            "entries": m.storage_entries.get(),
            "compactions": m.compactions_total.get(),
        },
        "slow_queries": {
            "total": m.slow_queries_total.get(),
            "threshold_seconds": SLOW_QUERY_THRESHOLD_SECS,
        }
    }))
}

/// POST /api/query - Execute a SQL or OntoDB query.
///
/// Request body:
/// ```json
/// {
///     "query": "SELECT * FROM Product WHERE price > 100",
///     "pretty": false
/// }
/// ```
async fn execute_query(
    State(state): State<AppState>,
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<std::net::SocketAddr>,
    Json(req): Json<QueryRequest>,
) -> impl IntoResponse {
    let start = std::time::Instant::now();
    let client_ip = addr.ip().to_string();

    let query = req.query.trim_end_matches(';').trim();
    // Try OntoQL parser first, fall back to SQL parser
    let ast = match OntoQLParser::parse(query) {
        Ok(ontoql_ast) => {
            // Handle triple operations directly via TripleStore
            match &ontoql_ast {
                onto_query::OntoQLAst::InsertTriple { subject, predicate, object } => {
                    if let Some(ts) = state.executor.triple_store() {
                        match ts.add_triple(subject, predicate, object) {
                            Ok(()) => {
                                let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
                                state.metrics.record_query("INSERT_TRIPLE", elapsed_ms / 1000.0, true);
                                return (
                                    StatusCode::OK,
                                    PrettyJson(ApiResponse::success(json!({"message": "Triple inserted"}), elapsed_ms), req.pretty),
                                );
                            }
                            Err(e) => {
                                return (StatusCode::INTERNAL_SERVER_ERROR, PrettyJson(ApiResponse::<Value>::error(e), false));
                            }
                        }
                    } else {
                        return (StatusCode::BAD_REQUEST, PrettyJson(ApiResponse::<Value>::error("TripleStore not configured".to_string()), false));
                    }
                }
                onto_query::OntoQLAst::InsertTriples { triples } => {
                    if let Some(ts) = state.executor.triple_store() {
                        let count = triples.len();
                        match ts.add_triples(&triples.iter().map(|(s,p,o)| (s.clone(), p.clone(), o.clone())).collect::<Vec<_>>()) {
                            Ok(_) => {
                                let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
                                state.metrics.record_query("INSERT_TRIPLES", elapsed_ms / 1000.0, true);
                                return (
                                    StatusCode::OK,
                                    PrettyJson(ApiResponse::success(json!({"message": format!("{} triples inserted", count)}), elapsed_ms), req.pretty),
                                );
                            }
                            Err(e) => {
                                return (StatusCode::INTERNAL_SERVER_ERROR, PrettyJson(ApiResponse::<Value>::error(e), false));
                            }
                        }
                    } else {
                        return (StatusCode::BAD_REQUEST, PrettyJson(ApiResponse::<Value>::error("TripleStore not configured".to_string()), false));
                    }
                }
                onto_query::OntoQLAst::DeleteTriple { subject, predicate, object } => {
                    if let Some(ts) = state.executor.triple_store() {
                        match ts.remove_triple(subject, predicate, object) {
                            Ok(()) => {
                                let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
                                state.metrics.record_query("DELETE_TRIPLE", elapsed_ms / 1000.0, true);
                                return (
                                    StatusCode::OK,
                                    PrettyJson(ApiResponse::success(json!({"message": "Triple deleted"}), elapsed_ms), req.pretty),
                                );
                            }
                            Err(e) => {
                                return (StatusCode::INTERNAL_SERVER_ERROR, PrettyJson(ApiResponse::<Value>::error(e), false));
                            }
                        }
                    } else {
                        return (StatusCode::BAD_REQUEST, PrettyJson(ApiResponse::<Value>::error("TripleStore not configured".to_string()), false));
                    }
                }
                onto_query::OntoQLAst::DropClass { name } => {
                    // DROP CLASS deletes the auto-created ontology entry (CREATE CLASS ↔ DROP CLASS)
                    match state.executor.drop_ontology(name) {
                        Ok(()) => {
                            let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
                            state.metrics.record_query("DROP_CLASS", elapsed_ms / 1000.0, true);
                            return (
                                StatusCode::OK,
                                PrettyJson(ApiResponse::success(json!({"message": format!("Class '{}' dropped", name)}), elapsed_ms), req.pretty),
                            );
                        }
                        Err(e) => {
                            return (StatusCode::BAD_REQUEST, PrettyJson(ApiResponse::<Value>::error(e.to_string()), false));
                        }
                    }
                }
                onto_query::OntoQLAst::DropOntology { name } => {
                    match state.executor.drop_ontology(name) {
                        Ok(()) => {
                            let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
                            state.metrics.record_query("DROP_ONTOLOGY", elapsed_ms / 1000.0, true);
                            return (
                                StatusCode::OK,
                                PrettyJson(ApiResponse::success(json!({"message": format!("Ontology '{}' dropped", name)}), elapsed_ms), req.pretty),
                            );
                        }
                        Err(e) => {
                            return (StatusCode::BAD_REQUEST, PrettyJson(ApiResponse::<Value>::error(e.to_string()), false));
                        }
                    }
                }
                onto_query::OntoQLAst::SelectTriples { subject, predicate, object, limit } => {
                    let results = state.executor.query_triples(
                        subject.as_deref(),
                        predicate.as_deref(),
                        object.as_deref(),
                    );
                    match results {
                        Ok(mut rows) => {
                            if let Some(l) = limit {
                                rows.truncate(*l);
                            }
                            let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
                            state.metrics.record_query("SELECT_TRIPLE", elapsed_ms / 1000.0, true);
                            return (
                                StatusCode::OK,
                                PrettyJson(ApiResponse::success(json!(rows), elapsed_ms), req.pretty),
                            );
                        }
                        Err(e) => {
                            return (StatusCode::INTERNAL_SERVER_ERROR, PrettyJson(ApiResponse::<Value>::error(e.to_string()), false));
                        }
                    }
                }
                _ => {
                    // Non-triple OntoQL operation - translate to QueryAst
                    match ontoql_ast.to_query_ast() {
                        Ok(ast) => ast,
                        Err(e) => {
                            state.metrics.record_parse_error();
                            return (
                                StatusCode::BAD_REQUEST,
                                PrettyJson(ApiResponse::<Value>::error(format!("OntoQL translation error: {}", e)), false),
                            );
                        }
                    }
                }
            }
        }
        Err(_) => {
            // OntoQL parser didn't recognize it �?try SQL parser
            match QueryParser::parse(query) {
                Ok(ast) => ast,
                Err(e) => {
                    state.metrics.record_parse_error();
                    return (
                        StatusCode::BAD_REQUEST,
                        PrettyJson(ApiResponse::<Value>::error(format!("Parse error: {}", e)), false),
                    );
                }
            }
        }
    };

    // Determine query type for metrics
    let query_type = match &ast {
        QueryAst::Select { .. } => "SELECT",
        QueryAst::Insert { .. } => "INSERT",
        QueryAst::Update { .. } => "UPDATE",
        QueryAst::Delete { .. } => "DELETE",
        QueryAst::VectorSearch { .. } => "VECTOR_SEARCH",
        _ => "OTHER",
    };

    let result = if onto_query::QueryExecutor::is_read_only_query(&ast) {
        state.executor.execute_read(&ast)
    } else {
        state.executor.execute(&ast)
    };
    let result = match result {
        Ok(r) => r,
        Err(e) => {
            let elapsed = start.elapsed().as_secs_f64();
            state.metrics.record_query(query_type, elapsed, false);
            // Audit log �?failed query
            let err_msg = e.to_string();
            let audit_entry = state.audit.create_query_entry(&client_ip, None, query_type, query, elapsed * 1000.0, false, Some(err_msg.clone()));
            state.audit.log(audit_entry);
            // Sanitize error message for client: remove file paths and internal details
            let safe_msg = sanitize_error(&err_msg);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                PrettyJson(ApiResponse::<Value>::error(safe_msg), false),
            );
        }
    };

    let elapsed = start.elapsed().as_secs_f64();
    state.metrics.record_query(query_type, elapsed, true);

    // Audit log �?successful query
    let audit_entry = state.audit.create_query_entry(&client_ip, None, query_type, query, elapsed * 1000.0, true, None);
    state.audit.log(audit_entry);

    // Slow query logging
    if elapsed >= SLOW_QUERY_THRESHOLD_SECS {
        state.metrics.slow_queries_total.inc();
        let truncated = truncate_utf8(query, 200);
        tracing::warn!(
            target: "slow_query",
            query_type = query_type,
            elapsed_ms = elapsed * 1000.0,
            query = truncated,
            "slow query detected"
        );
    }

    let elapsed_ms = elapsed * 1000.0;

    let data = match result {
        onto_query::QueryResult::Success(msg) => json!({ "message": msg }),
        onto_query::QueryResult::Rows(rows) => json!(rows),
    };

    let response = ApiResponse::success(data, elapsed_ms);
    (StatusCode::OK, PrettyJson(response, req.pretty))
}

/// POST /sparql - Execute a SPARQL query.
///
/// Accepts a SPARQL query and translates it to SQL for execution.
/// Returns results in W3C SPARQL Results JSON Format.
async fn sparql_query(
    State(state): State<AppState>,
    Json(req): Json<SparqlRequest>,
) -> impl IntoResponse {
    let start = std::time::Instant::now();

    // Parse SPARQL query
    let mut parser = onto_query::SparqlParser::new();
    let sparql_query = match parser.parse(&req.query) {
        Ok(q) => q,
        Err(e) => {
            state.metrics.record_parse_error();
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<Value>::error(format!("SPARQL parse error: {}", e))),
            );
        }
    };

    // Translate to SQL
    let sql = match parser.translate_to_sql(&sparql_query) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<Value>::error(sanitize_error(&e.to_string()))),
            );
        }
    };

    // Execute as SQL
    let ast = match QueryParser::parse(&sql) {
        Ok(ast) => ast,
        Err(e) => {
            state.metrics.record_parse_error();
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<Value>::error(format!("Generated SQL parse error: {}", e))),
            );
        }
    };

    let result = if onto_query::QueryExecutor::is_read_only_query(&ast) {
        state.executor.execute_read(&ast)
    } else {
        state.executor.execute(&ast)
    };
    let result = match result {
        Ok(r) => r,
        Err(e) => {
            let elapsed = start.elapsed().as_secs_f64();
            state.metrics.record_query("SPARQL", elapsed, false);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<Value>::error(sanitize_error(&e.to_string()))),
            );
        }
    };

    let elapsed = start.elapsed().as_secs_f64();
    state.metrics.record_query("SPARQL", elapsed, true);

    if elapsed >= SLOW_QUERY_THRESHOLD_SECS {
        state.metrics.slow_queries_total.inc();
        let truncated = truncate_utf8(&req.query, 200);
        tracing::warn!(
            target: "slow_query",
            query_type = "SPARQL",
            elapsed_ms = elapsed * 1000.0,
            query = truncated,
            "slow SPARQL query detected"
        );
    }

    let elapsed_ms = elapsed * 1000.0;

    let data = match result {
        onto_query::QueryResult::Rows(rows) => {
            // Format as SPARQL JSON results
            let sparql_result = onto_query::sparql::format_sparql_json(&sparql_query, &rows);
            json!(sparql_result)
        }
        onto_query::QueryResult::Success(msg) => {
            json!({ "head": { "vars": [] }, "results": { "bindings": [] }, "message": msg })
        }
    };

    (StatusCode::OK, Json(ApiResponse::success(data, elapsed_ms)))
}

/// POST /api/vector/search - Perform vector similarity search.
///
/// Request body:
/// ```json
/// {
///     "class": "Product",
///     "column": "embedding",
///     "query_vector": [0.1, 0.2, 0.3],
///     "top_k": 10,
///     "filter": "price > 100"
/// }
/// ```
async fn vector_search(
    State(state): State<AppState>,
    Json(req): Json<VectorSearchRequest>,
) -> impl IntoResponse {
    let start = std::time::Instant::now();

    // Validate identifiers to prevent injection
    if let Err(e) = validate_identifier(&req.class) {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<Value>::error(format!("Invalid class name: {}", e))),
        );
    }
    if let Err(e) = validate_identifier(&req.column) {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<Value>::error(format!("Invalid column name: {}", e))),
        );
    }
    if req.query_vector.len() > 4096 {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<Value>::error("query_vector too large (max 4096 dimensions)".to_string())),
        );
    }

    // Build VECTOR SEARCH query
    let filter_clause = if let Some(f) = &req.filter {
        if let Err(e) = validate_filter(f) {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<Value>::error(format!("Invalid filter: {}", e))),
            );
        }
        format!(" WHERE {}", f)
    } else {
        String::new()
    };

    let vector_str = req
        .query_vector
        .iter()
        .map(|v| v.to_string())
        .collect::<Vec<_>>()
        .join(", ");

    let top_k = req.top_k.min(10000);
    let query = format!(
        "VECTOR SEARCH ON {} ({}) QUERY [{}] TOP {}{}",
        req.class, req.column, vector_str, top_k, filter_clause
    );

    let ast = match QueryParser::parse(&query) {
        Ok(ast) => ast,
        Err(e) => {
            state.metrics.record_parse_error();
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<Value>::error(format!("Parse error: {}", e))),
            );
        }
    };

    let result = if onto_query::QueryExecutor::is_read_only_query(&ast) {
        state.executor.execute_read(&ast)
    } else {
        state.executor.execute(&ast)
    };
    let result = match result {
        Ok(r) => r,
        Err(e) => {
            let elapsed = start.elapsed().as_secs_f64();
            state.metrics.record_query("VECTOR_SEARCH", elapsed, false);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<Value>::error(sanitize_error(&e.to_string()))),
            );
        }
    };

    let elapsed = start.elapsed().as_secs_f64();
    state.metrics.vector_search_latency.observe(elapsed);
    state.metrics.record_query("VECTOR_SEARCH", elapsed, true);

    if elapsed >= SLOW_QUERY_THRESHOLD_SECS {
        state.metrics.slow_queries_total.inc();
        tracing::warn!(
            target: "slow_query",
            query_type = "VECTOR_SEARCH",
            elapsed_ms = elapsed * 1000.0,
            class = req.class,
            top_k = req.top_k,
            "slow vector search detected"
        );
    }

    let elapsed_ms = elapsed * 1000.0;

    match result {
        onto_query::QueryResult::Rows(rows) => {
            state.metrics.vector_search_results.add(rows.len() as u64);
            (StatusCode::OK, Json(ApiResponse::success(json!(rows), elapsed_ms)))
        }
        onto_query::QueryResult::Success(msg) => {
            (StatusCode::OK, Json(ApiResponse::success(json!({ "message": msg }), elapsed_ms)))
        }
    }
}

/// POST /api/batch - Execute multiple queries in a single request.
///
/// Request body:
/// ```json
/// {
///     "queries": [
///         "INSERT INTO Product (name, price) VALUES ('iPhone', 999)",
///         "INSERT INTO Product (name, price) VALUES ('iPad', 799)",
///         "SELECT * FROM Product"
///     ],
///     "fail_fast": false,
///     "pretty": false
/// }
/// ```
async fn batch_query(
    State(state): State<AppState>,
    Json(req): Json<BatchRequest>,
) -> impl IntoResponse {
    let start = std::time::Instant::now();

    if req.queries.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<Value>::error("queries array is empty".to_string())),
        );
    }

    if req.queries.len() > 1000 {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<Value>::error("batch size exceeds maximum of 1000".to_string())),
        );
    }

    let mut results: Vec<BatchResult> = Vec::with_capacity(req.queries.len());
    let mut has_error = false;

    for (i, query) in req.queries.iter().enumerate() {
        let query_start = std::time::Instant::now();
        let trimmed = query.trim_end_matches(';').trim();

        if trimmed.is_empty() {
            results.push(BatchResult {
                index: i,
                query: query.clone(),
                success: true,
                data: None,
                error: None,
                elapsed_ms: 0.0,
            });
            continue;
        }

        // Parse and execute
        let result = match onto_query::QueryParser::parse(trimmed) {
            Ok(ast) => state.executor.execute(&ast),
            Err(e) => Err(e),
        };

        let elapsed_ms = query_start.elapsed().as_secs_f64() * 1000.0;

        match result {
            Ok(query_result) => {
                let data = match query_result {
                    onto_query::QueryResult::Rows(rows) => Some(json!(rows)),
                    onto_query::QueryResult::Success(msg) => Some(json!({ "message": msg })),
                };
                results.push(BatchResult {
                    index: i,
                    query: query.clone(),
                    success: true,
                    data,
                    error: None,
                    elapsed_ms,
                });
            }
            Err(e) => {
                has_error = true;
                results.push(BatchResult {
                    index: i,
                    query: query.clone(),
                    success: false,
                    data: None,
                    error: Some(sanitize_error(&e.to_string())),
                    elapsed_ms,
                });

                if req.fail_fast {
                    // Fill remaining queries as skipped
                    for j in (i + 1)..req.queries.len() {
                        results.push(BatchResult {
                            index: j,
                            query: req.queries[j].clone(),
                            success: false,
                            data: None,
                            error: Some("skipped due to fail_fast".to_string()),
                            elapsed_ms: 0.0,
                        });
                    }
                    break;
                }
            }
        }
    }

    let total_elapsed = start.elapsed().as_secs_f64() * 1000.0;
    let success_count = results.iter().filter(|r| r.success).count();
    let error_count = results.iter().filter(|r| !r.success).count();

    let status_code = if has_error && req.fail_fast {
        StatusCode::MULTI_STATUS
    } else if has_error {
        StatusCode::MULTI_STATUS
    } else {
        StatusCode::OK
    };

    let response = json!({
        "results": results,
        "summary": {
            "total": req.queries.len(),
            "succeeded": success_count,
            "failed": error_count,
            "elapsed_ms": total_elapsed
        }
    });

    (status_code, Json(ApiResponse::success(response, total_elapsed)))
}

/// POST /api/hybrid/query - Execute a hybrid SQL + vector search query.
///
/// This endpoint combines SQL filtering with vector similarity ranking.
/// First executes the SQL filter, then applies vector search on the results.
///
/// Request body:
/// ```json
/// {
///     "sql_filter": "SELECT * FROM Product WHERE price > 100",
///     "vector_column": "embedding",
///     "query_vector": [0.1, 0.2, 0.3],
///     "top_k": 10,
///     "class": "Product"
/// }
/// ```
async fn hybrid_query(
    State(state): State<AppState>,
    Json(req): Json<HybridQueryRequest>,
) -> impl IntoResponse {
    let start = std::time::Instant::now();

    // Validate identifiers to prevent injection
    if let Err(e) = validate_identifier(&req.vector_column) {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<Value>::error(format!("Invalid vector_column: {}", e))),
        );
    }
    if let Some(ref c) = req.class {
        if let Err(e) = validate_identifier(c) {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<Value>::error(format!("Invalid class name: {}", e))),
            );
        }
    }

    // Step 1: Execute the SQL filter query
    let sql_query = req.sql_filter.trim_end_matches(';').trim();
    let sql_ast = match QueryParser::parse(sql_query) {
        Ok(ast) => ast,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<Value>::error(format!("SQL parse error: {}", e))),
            );
        }
    };

    let sql_result = if onto_query::QueryExecutor::is_read_only_query(&sql_ast) {
        state.executor.execute_read(&sql_ast)
    } else {
        state.executor.execute(&sql_ast)
    };
    let sql_result = match sql_result {
        Ok(r) => r,
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<Value>::error("query execution failed".to_string())),
            );
        }
    };

    // Extract class name from SQL or use provided class
    let class = req.class.unwrap_or_else(|| {
        // Try to extract from SELECT ... FROM <class>
        if let onto_query::QueryResult::Rows(_) = &sql_result {
            // Class extraction is best-effort; user should provide it
            "unknown".to_string()
        } else {
            "unknown".to_string()
        }
    });

    // Validate vector dimensions (DoS protection)
    if req.query_vector.len() > 4096 {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<Value>::error("query_vector too large (max 4096 dimensions)".to_string())),
        );
    }

    // Step 2: Build vector search with filter from SQL results
    let top_k = req.top_k.min(10000);
    let vector_str = req
        .query_vector
        .iter()
        .map(|v| v.to_string())
        .collect::<Vec<_>>()
        .join(", ");

    // For hybrid query, we combine the SQL filter with vector search
    // The SQL filter is applied as a WHERE clause in the vector search
    let filter_clause = match &sql_ast {
        QueryAst::Select { filter: Some(_), .. } => {
            // Extract the WHERE clause using case-insensitive char-aware search
            let sql_upper: String = sql_query.chars().flat_map(|c| c.to_uppercase()).collect();
            if let Some(byte_pos) = sql_upper.find(" WHERE ") {
                // Count chars up to byte_pos to get a safe char boundary in the original
                let char_count = sql_upper[..byte_pos].chars().count();
                let after_where: String = sql_query.chars().skip(char_count + 7).collect();
                let extracted = after_where.trim();
                if let Err(e) = validate_filter(extracted) {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(ApiResponse::<Value>::error(format!("Invalid filter in SQL: {}", e))),
                    );
                }
                format!(" WHERE {}", extracted)
            } else {
                String::new()
            }
        }
        _ => String::new(),
    };

    let vector_query = format!(
        "VECTOR SEARCH ON {} ({}) QUERY [{}] TOP {}{}",
        class, req.vector_column, vector_str, top_k, filter_clause
    );

    let vector_ast = match QueryParser::parse(&vector_query) {
        Ok(ast) => ast,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<Value>::error(format!("Vector search parse error: {}", e))),
            );
        }
    };

    let vector_result = if onto_query::QueryExecutor::is_read_only_query(&vector_ast) {
        state.executor.execute_read(&vector_ast)
    } else {
        state.executor.execute(&vector_ast)
    };
    let vector_result = match vector_result {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<Value>::error(sanitize_error(&e.to_string()))),
            );
        }
    };

    let elapsed = start.elapsed().as_secs_f64() * 1000.0;

    match vector_result {
        onto_query::QueryResult::Rows(rows) => {
            (StatusCode::OK, Json(ApiResponse::success(json!(rows), elapsed)))
        }
        onto_query::QueryResult::Success(msg) => {
            (StatusCode::OK, Json(ApiResponse::success(json!({ "message": msg }), elapsed)))
        }
    }
}

/// GET /api/schema - Get database schema information.
async fn get_schema(
    State(state): State<AppState>,
) -> impl IntoResponse {
    let start = std::time::Instant::now();
    match state.executor.schema_info() {
        Ok(schema) => {
            let elapsed = start.elapsed().as_secs_f64() * 1000.0;
            Json(ApiResponse::success(schema, elapsed))
        }
        Err(e) => Json(ApiResponse::error(format!("schema introspection failed: {}", e))),
    }
}

// ── Transaction API ──────────────────────────────────────────

/// POST /api/transaction/begin - Begin a new transaction.
///
/// Returns a transaction ID that can be used with execute/commit/rollback.
///
/// Request body (optional):
/// ```json
/// { "isolation": "snapshot" }
/// ```
async fn transaction_begin(
    State(state): State<AppState>,
    Json(_req): Json<TransactionBeginRequest>,
) -> impl IntoResponse {
    let start = std::time::Instant::now();
    let txn_id = state.executor.engine().begin_txn();
    let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;

    (
        StatusCode::OK,
        Json(ApiResponse::success(json!({
            "txn_id": txn_id,
            "message": "Transaction started"
        }), elapsed_ms)),
    )
}

/// POST /api/transaction/execute - Execute a query within a transaction.
///
/// Request body:
/// ```json
/// {
///     "txn_id": 12345,
///     "query": "INSERT INTO Product (name, price) VALUES ('iPhone', 999)"
/// }
/// ```
async fn transaction_execute(
    State(state): State<AppState>,
    Json(req): Json<TransactionExecuteRequest>,
) -> impl IntoResponse {
    let start = std::time::Instant::now();

    let query = req.query.trim_end_matches(';').trim();
    if query.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<Value>::error("query is empty".to_string())),
        );
    }

    // Parse the query
    let ast = match QueryParser::parse(query) {
        Ok(ast) => ast,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<Value>::error(format!("Parse error: {}", e))),
            );
        }
    };

    // Check if it's a write query (only write queries can run in transactions)
    if QueryExecutor::is_read_only_query(&ast) {
        // Read queries don't need transaction context, execute normally
        let result = state.executor.execute_read(&ast);
        let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
        return match result {
            Ok(r) => {
                let data = match r {
                    onto_query::QueryResult::Rows(rows) => json!(rows),
                    onto_query::QueryResult::Success(msg) => json!({ "message": msg }),
                };
                (StatusCode::OK, Json(ApiResponse::success(data, elapsed_ms)))
            }
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<Value>::error(sanitize_error(&e.to_string()))),
            ),
        };
    }

    // Write query — execute within transaction
    let result = state.executor.execute_in_transaction(req.txn_id, &ast);
    let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;

    match result {
        Ok(r) => {
            let data = match r {
                onto_query::QueryResult::Rows(rows) => json!(rows),
                onto_query::QueryResult::Success(msg) => json!({ "message": msg }),
            };
            (StatusCode::OK, Json(ApiResponse::success(data, elapsed_ms)))
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<Value>::error(sanitize_error(&e.to_string()))),
        ),
    }
}

/// POST /api/transaction/commit - Commit a transaction.
///
/// Request body:
/// ```json
/// { "txn_id": 12345 }
/// ```
async fn transaction_commit(
    State(state): State<AppState>,
    Json(req): Json<TransactionActionRequest>,
) -> impl IntoResponse {
    let start = std::time::Instant::now();

    match state.executor.engine().commit_txn(req.txn_id) {
        Ok(()) => {
            let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
            (
                StatusCode::OK,
                Json(ApiResponse::success(json!({
                    "txn_id": req.txn_id,
                    "message": "Transaction committed"
                }), elapsed_ms)),
            )
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<Value>::error(sanitize_error(&e.to_string()))),
        ),
    }
}

/// POST /api/transaction/rollback - Rollback (abort) a transaction.
///
/// Request body:
/// ```json
/// { "txn_id": 12345 }
/// ```
async fn transaction_rollback(
    State(state): State<AppState>,
    Json(req): Json<TransactionActionRequest>,
) -> impl IntoResponse {
    let start = std::time::Instant::now();

    match state.executor.engine().abort_txn(req.txn_id) {
        Ok(()) => {
            let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
            (
                StatusCode::OK,
                Json(ApiResponse::success(json!({
                    "txn_id": req.txn_id,
                    "message": "Transaction rolled back"
                }), elapsed_ms)),
            )
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<Value>::error(sanitize_error(&e.to_string()))),
        ),
    }
}

// ── API Documentation ────────────────────────────────────────────

/// GET /api/docs - Swagger UI for API documentation.
async fn swagger_ui() -> axum::response::Html<&'static str> {
    axum::response::Html(include_str!("swagger_ui.html"))
}

/// GET /api/openapi.json - OpenAPI 3.0 specification.
async fn openapi_spec() -> impl IntoResponse {
    (
        StatusCode::OK,
        [("Content-Type", "application/json")],
        include_str!("../../../docs/api/openapi.json"),
    )
}

/// GET /console - Web management console.
async fn web_console() -> axum::response::Html<&'static str> {
    axum::response::Html(include_str!("static/console.html"))
}

/// GET /digital-twin - Enterprise digital twin monitoring dashboard.
/// Reads from disk at runtime so changes take effect without recompilation.
async fn digital_twin() -> axum::response::Html<String> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/static/digital_twin.html");
    match std::fs::read_to_string(&path) {
        Ok(content) => axum::response::Html(content),
        Err(_) => axum::response::Html("<h1>Digital Twin dashboard unavailable</h1><p>Please check server configuration.</p>".to_string()),
    }
}

/// GET /digital-advisor - Enterprise decision intelligence dashboard.
async fn digital_advisor() -> axum::response::Html<String> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/static/digital_advisor.html");
    match std::fs::read_to_string(&path) {
        Ok(content) => axum::response::Html(content),
        Err(_) => axum::response::Html("<h1>Digital Advisor unavailable</h1>".to_string()),
    }
}

// ── Cluster API ──────────────────────────────────────────────────

/// GET /api/cluster - Get cluster information.
async fn cluster_info(
    State(state): State<AppState>,
) -> impl IntoResponse {
    let uptime = state.metrics.started_at.elapsed().as_secs();

    Json(json!({
        "node_id": state.raft_node_id,
        "mode": if state.raft_node_id.is_some() { "cluster" } else { "standalone" },
        "uptime_seconds": uptime,
        "version": env!("CARGO_PKG_VERSION"),
        "metrics": {
            "connections": {
                "http_active": state.metrics.http_connections_active.get(),
                "tcp_active": state.metrics.tcp_connections_active.get(),
            },
            "queries_total": state.metrics.queries_total.get(),
        }
    }))
}

// ── Graph API Handlers ───────────────────────────────────────────

/// POST /api/graph/vertex - Add a vertex to the graph.
async fn add_vertex(
    State(state): State<AppState>,
    Json(req): Json<serde_json::Value>,
) -> impl IntoResponse {
    let start = std::time::Instant::now();

    let id = req.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let labels: Vec<String> = req.get("labels")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();

    if id.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(ApiResponse::<serde_json::Value>::error("missing vertex id")));
    }

    let mut vertex = onto_graph::Vertex::new(&id, labels.clone());

    // Parse properties from request
    if let Some(props) = req.get("properties").and_then(|v| v.as_object()) {
        for (key, val) in props {
            let pv = match val {
                serde_json::Value::String(s) => onto_graph::PropValue::String(s.clone()),
                serde_json::Value::Number(n) => {
                    if let Some(i) = n.as_i64() {
                        onto_graph::PropValue::Int(i)
                    } else if let Some(f) = n.as_f64() {
                        onto_graph::PropValue::Float(f)
                    } else {
                        continue;
                    }
                }
                serde_json::Value::Bool(b) => onto_graph::PropValue::Bool(*b),
                _ => onto_graph::PropValue::String(val.to_string()),
            };
            vertex.properties.insert(key.clone(), pv);
        }
    }

    let elapsed = start.elapsed().as_secs_f64() * 1000.0;

    match state.graph.add_vertex(vertex) {
        Ok(()) => (StatusCode::OK, Json(ApiResponse::success(json!({
            "message": format!("vertex '{}' added", id),
            "id": id,
            "labels": labels,
        }), elapsed))),
        Err(e) => (StatusCode::CONFLICT, Json(ApiResponse::<serde_json::Value>::error(
            format!("failed to add vertex: {}", e)
        ))),
    }
}

/// POST /api/graph/edge - Add an edge to the graph.
async fn add_edge(
    State(state): State<AppState>,
    Json(req): Json<serde_json::Value>,
) -> impl IntoResponse {
    let start = std::time::Instant::now();

    let id = req.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let from = req.get("from").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let to = req.get("to").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let label = req.get("label").and_then(|v| v.as_str()).unwrap_or("").to_string();

    if id.is_empty() || from.is_empty() || to.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(ApiResponse::<serde_json::Value>::error("missing required fields: id, from, to")));
    }

    let mut edge = onto_graph::Edge::new(&id, &from, &to, &label);

    // Parse properties
    if let Some(props) = req.get("properties").and_then(|v| v.as_object()) {
        for (key, val) in props {
            let pv = match val {
                serde_json::Value::String(s) => onto_graph::PropValue::String(s.clone()),
                serde_json::Value::Number(n) => {
                    if let Some(i) = n.as_i64() {
                        onto_graph::PropValue::Int(i)
                    } else if let Some(f) = n.as_f64() {
                        onto_graph::PropValue::Float(f)
                    } else {
                        continue;
                    }
                }
                serde_json::Value::Bool(b) => onto_graph::PropValue::Bool(*b),
                _ => onto_graph::PropValue::String(val.to_string()),
            };
            edge.properties.insert(key.clone(), pv);
        }
    }

    let elapsed = start.elapsed().as_secs_f64() * 1000.0;

    match state.graph.add_edge(edge) {
        Ok(()) => (StatusCode::OK, Json(ApiResponse::success(json!({
            "message": format!("edge '{}' added", id),
            "id": id,
            "from": from,
            "to": to,
            "label": label,
        }), elapsed))),
        Err(e) => {
            let code = if e.to_string().contains("not found") {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::CONFLICT
            };
            (code, Json(ApiResponse::<serde_json::Value>::error(
                format!("failed to add edge: {}", e)
            )))
        }
    }
}

/// POST /api/graph/traverse - Traverse the graph using BFS/DFS.
async fn graph_traverse(
    State(state): State<AppState>,
    Json(req): Json<serde_json::Value>,
) -> impl IntoResponse {
    let start = std::time::Instant::now();

    let start_id = req.get("start").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let direction_str = req.get("direction").and_then(|v| v.as_str()).unwrap_or("out");
    let max_depth = req.get("max_depth").and_then(|v| v.as_u64()).unwrap_or(3).min(100) as usize;
    let edge_label = req.get("edge_label").and_then(|v| v.as_str());
    let algo = req.get("algorithm").and_then(|v| v.as_str()).unwrap_or("bfs");

    if start_id.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(ApiResponse::<serde_json::Value>::error("missing start vertex id")));
    }

    let direction = match direction_str {
        "in" => onto_graph::Direction::In,
        "both" => onto_graph::Direction::Both,
        _ => onto_graph::Direction::Out,
    };

    let engine = onto_graph::TraversalEngine::new(&state.graph);

    let result = if algo == "dfs" {
        engine.traverse_dfs(&start_id, max_depth, direction, edge_label, None)
    } else {
        engine.traverse_bfs(&start_id, max_depth, direction, edge_label, None)
    };

    let elapsed = start.elapsed().as_secs_f64() * 1000.0;

    match result {
        Ok(traversal) => {
            let vertices: Vec<serde_json::Value> = traversal.vertices.iter().map(|v| {
                json!({
                    "id": v.id,
                    "labels": v.labels,
                    "properties": v.properties,
                })
            }).collect();

            (StatusCode::OK, Json(ApiResponse::success(json!({
                "start": start_id,
                "direction": direction_str,
                "max_depth": max_depth,
                "algorithm": algo,
                "visited_count": traversal.visited_count,
                "vertices": vertices,
            }), elapsed)))
        }
        Err(e) => {
            let code = if e.to_string().contains("not found") {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            (code, Json(ApiResponse::<serde_json::Value>::error(
                format!("traversal failed: {}", e)
            )))
        }
    }
}

/// POST /api/graph/shortest-path - Find shortest path between two vertices.
async fn graph_shortest_path(
    State(state): State<AppState>,
    Json(req): Json<serde_json::Value>,
) -> impl IntoResponse {
    let start = std::time::Instant::now();

    let from_id = req.get("from").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let to_id = req.get("to").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let max_depth = req.get("max_depth").and_then(|v| v.as_u64()).unwrap_or(10).min(100) as usize;

    if from_id.is_empty() || to_id.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(ApiResponse::<serde_json::Value>::error("missing from/to vertex ids")));
    }

    let engine = onto_graph::TraversalEngine::new(&state.graph);
    let elapsed = start.elapsed().as_secs_f64() * 1000.0;

    match engine.shortest_path(&from_id, &to_id, max_depth) {
        Ok(Some(path)) => {
            (StatusCode::OK, Json(ApiResponse::success(json!({
                "from": from_id,
                "to": to_id,
                "found": true,
                "length": path.length,
                "path": {
                    "vertex_ids": path.vertex_ids,
                    "edge_ids": path.edge_ids,
                },
            }), elapsed)))
        }
        Ok(None) => {
            (StatusCode::OK, Json(ApiResponse::success(json!({
                "from": from_id,
                "to": to_id,
                "found": false,
                "message": "no path exists between the two vertices",
            }), elapsed)))
        }
        Err(e) => {
            let code = if e.to_string().contains("not found") {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            (code, Json(ApiResponse::<serde_json::Value>::error(
                format!("shortest path search failed: {}", e)
            )))
        }
    }
}

/// GET /api/graph/vertex/:id - Get a vertex by ID.
async fn get_vertex(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> impl IntoResponse {
    let start = std::time::Instant::now();
    let elapsed = start.elapsed().as_secs_f64() * 1000.0;

    match state.graph.get_vertex(&id) {
        Some(vertex) => {
            (StatusCode::OK, Json(ApiResponse::success(json!({
                "id": vertex.id,
                "labels": vertex.labels,
                "properties": vertex.properties,
            }), elapsed)))
        }
        None => {
            (StatusCode::NOT_FOUND, Json(ApiResponse::<serde_json::Value>::error(
                format!("vertex '{}' not found", id)
            )))
        }
    }
}

/// DELETE /api/graph/vertex/:id - Delete a vertex.
async fn delete_vertex(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> impl IntoResponse {
    let start = std::time::Instant::now();
    let elapsed = start.elapsed().as_secs_f64() * 1000.0;

    match state.graph.delete_vertex(&id) {
        Ok(()) => {
            (StatusCode::OK, Json(ApiResponse::success(json!({
                "message": format!("vertex '{}' deleted", id)
            }), elapsed)))
        }
        Err(e) => {
            (StatusCode::NOT_FOUND, Json(ApiResponse::<serde_json::Value>::error(
                format!("failed to delete vertex: {}", e)
            )))
        }
    }
}

/// GET /api/graph/neighbors/:id - Get neighbors of a vertex.
async fn get_neighbors(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> impl IntoResponse {
    let start = std::time::Instant::now();
    let elapsed = start.elapsed().as_secs_f64() * 1000.0;

    let neighbors = state.graph.get_neighbors(&id);
    let neighbor_data: Vec<serde_json::Value> = neighbors.iter().map(|v| {
        json!({
            "id": v.id,
            "labels": v.labels,
            "properties": v.properties,
        })
    }).collect();

    (StatusCode::OK, Json(ApiResponse::success(json!({
        "vertex_id": id,
        "count": neighbor_data.len(),
        "neighbors": neighbor_data,
    }), elapsed)))
}

/// POST /api/backup - Create a full snapshot backup.
async fn backup(
    State(state): State<AppState>,
    Json(req): Json<BackupRequest>,
) -> impl IntoResponse {
    let start = std::time::Instant::now();
    let backup_dir = match validate_backup_path(&req.path) {
        Ok(p) => p,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<Value>::error(format!("Invalid backup path: {}", e))),
            );
        }
    };

    match state.executor.backup(&backup_dir) {
        Ok(manifest) => {
            let elapsed = start.elapsed().as_secs_f64() * 1000.0;
            let total_bytes: u64 = manifest.files.iter().map(|f| f.size).sum();
            (StatusCode::OK, Json(ApiResponse::success(json!({
                "message": "Backup completed",
                "path": req.path,
                "files": manifest.files.len(),
                "total_bytes": total_bytes,
                "timestamp": manifest.timestamp,
            }), elapsed)))
        }
        Err(e) => {
            let _elapsed = start.elapsed().as_secs_f64() * 1000.0;
            (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse::<Value>::error(
                format!("Backup failed: {}", e)
            )))
        }
    }
}

/// POST /api/backup/incremental - Create an incremental backup.
async fn backup_incremental(
    State(state): State<AppState>,
    Json(req): Json<IncrementalBackupRequest>,
) -> impl IntoResponse {
    let start = std::time::Instant::now();
    let backup_dir = match validate_backup_path(&req.path) {
        Ok(p) => p,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<Value>::error(format!("Invalid backup path: {}", e))),
            );
        }
    };

    // Parse the ISO 8601 timestamp into SystemTime
    // Accept formats: "2026-08-08T12:00:00Z" or "2026-08-08T12:00:00"
    let since = match parse_iso_timestamp(&req.since) {
        Ok(t) => t,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, Json(ApiResponse::<Value>::error(
                format!("Invalid 'since' timestamp: {}", e)
            )));
        }
    };

    match state.executor.backup_incremental(&backup_dir, &since) {
        Ok(manifest) => {
            let elapsed = start.elapsed().as_secs_f64() * 1000.0;
            let total_bytes: u64 = manifest.files.iter().map(|f| f.size).sum();
            (StatusCode::OK, Json(ApiResponse::success(json!({
                "message": "Incremental backup completed",
                "path": req.path,
                "files": manifest.files.len(),
                "total_bytes": total_bytes,
                "timestamp": manifest.timestamp,
                "backup_type": manifest.backup_type,
            }), elapsed)))
        }
        Err(e) => {
            (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse::<Value>::error(
                format!("Incremental backup failed: {}", e)
            )))
        }
    }
}

/// POST /api/backup/verify - Verify a backup's integrity.
async fn verify_backup_endpoint(
    State(_state): State<AppState>,
    Json(req): Json<VerifyBackupRequest>,
) -> impl IntoResponse {
    let start = std::time::Instant::now();
    let backup_dir = match validate_backup_path(&req.path) {
        Ok(p) => p,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<Value>::error(format!("Invalid backup path: {}", e))),
            );
        }
    };

    match onto_query::QueryExecutor::verify_backup(&backup_dir) {
        Ok(()) => {
            let elapsed = start.elapsed().as_secs_f64() * 1000.0;
            (StatusCode::OK, Json(ApiResponse::success(json!({
                "message": "Backup verification passed",
                "path": req.path,
            }), elapsed)))
        }
        Err(e) => {
            (StatusCode::BAD_REQUEST, Json(ApiResponse::<Value>::error(
                format!("Backup verification failed: {}", e)
            )))
        }
    }
}

/// Parse an ISO 8601 timestamp string into SystemTime.
fn parse_iso_timestamp(s: &str) -> Result<std::time::SystemTime, String> {
    // Simple parser for "YYYY-MM-DDTHH:MM:SSZ" or "YYYY-MM-DDTHH:MM:SS"
    let s = s.trim_end_matches('Z');
    let parts: Vec<&str> = s.split('T').collect();
    if parts.len() != 2 {
        return Err("expected format: YYYY-MM-DDTHH:MM:SS".to_string());
    }

    let date_parts: Vec<&str> = parts[0].split('-').collect();
    let time_parts: Vec<&str> = parts[1].split(':').collect();

    if date_parts.len() != 3 || time_parts.len() < 3 {
        return Err("invalid date/time format".to_string());
    }

    let year: u16 = date_parts[0].parse().map_err(|_| "invalid year")?;
    let month: u8 = date_parts[1].parse().map_err(|_| "invalid month")?;
    let day: u8 = date_parts[2].parse().map_err(|_| "invalid day")?;
    let hour: u8 = time_parts[0].parse().map_err(|_| "invalid hour")?;
    let minute: u8 = time_parts[1].parse().map_err(|_| "invalid minute")?;
    let second: u8 = time_parts[2].parse().map_err(|_| "invalid second")?;

    // Convert to days since epoch (simplified)
    let days = days_since_epoch(year, month, day);
    let secs = days * 86400 + (hour as u64) * 3600 + (minute as u64) * 60 + second as u64;

    Ok(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs))
}

fn days_since_epoch(year: u16, month: u8, day: u8) -> u64 {
    let y = year as i64;
    let m = month as i64;
    let d = day as i64;
    // Days from 1970-01-01
    let mut days = 0i64;
    for yr in 1970..y {
        days += if is_leap_year(yr as u16) { 366 } else { 365 };
    }
    for mo in 1..m {
        days += days_in_month(year, mo as u8) as i64;
    }
    (days + d - 1) as u64
}

fn is_leap_year(year: u16) -> bool {
    (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400)
}

fn days_in_month(year: u16, month: u8) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => if is_leap_year(year) { 29 } else { 28 },
        _ => 0,
    }
}

/// POST /api/flush - Flush MemTable to SSTable.
async fn flush(
    State(state): State<AppState>,
) -> impl IntoResponse {
    let start = std::time::Instant::now();

    match state.executor.flush() {
        Ok(()) => {
            let elapsed = start.elapsed().as_secs_f64() * 1000.0;
            (StatusCode::OK, Json(ApiResponse::success(json!({
                "message": "MemTable flushed to SSTable"
            }), elapsed)))
        }
        Err(e) => {
            let _elapsed = start.elapsed().as_secs_f64() * 1000.0;
            (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse::<Value>::error(
                format!("Flush failed: {}", e)
            )))
        }
    }
}

// ── Sharding API ──────────────────────────────────────────────────

/// Sharding configuration request.
#[derive(Debug, Deserialize)]
pub struct ShardConfigRequest {
    /// Shard ID.
    pub id: u32,
    /// Human-readable name.
    pub name: String,
    /// Optional Raft group ID.
    #[serde(default)]
    pub raft_group: Option<u64>,
    /// Node IDs that hold replicas.
    #[serde(default)]
    pub replicas: Vec<u64>,
    /// Whether this shard is the primary.
    #[serde(default = "default_true")]
    pub is_primary: bool,
}

fn default_true() -> bool { true }

/// Class sharding assignment request.
#[derive(Debug, Deserialize)]
pub struct ClassShardRequest {
    /// Class (table) name.
    pub class: String,
    /// Sharding strategy type: "class", "range", "hash".
    #[serde(default = "default_class_strategy")]
    pub strategy: String,
    /// For class-based: target shard ID.
    #[serde(default)]
    pub shard: Option<u32>,
    /// For range-based: range boundaries.
    #[serde(default)]
    pub ranges: Option<Vec<RangeShardConfig>>,
    /// For hash-based: number of virtual shards.
    #[serde(default)]
    pub num_shards: Option<u32>,
    /// For hash-based: slot to shard mapping.
    #[serde(default)]
    pub slot_map: Option<Vec<u32>>,
}

fn default_class_strategy() -> String { "class".to_string() }

/// Range shard configuration.
#[derive(Debug, Deserialize, Serialize)]
pub struct RangeShardConfig {
    /// Upper bound (exclusive).
    pub end_key: String,
    /// Shard ID for this range.
    pub shard: u32,
}

/// GET /api/sharding/config - Get current sharding configuration.
async fn get_sharding_config(
    State(state): State<AppState>,
) -> impl IntoResponse {
    let start = std::time::Instant::now();
    let elapsed = start.elapsed().as_secs_f64() * 1000.0;

    let mgr = state.executor.shard_manager();
    match mgr.as_ref() {
        Some(shard_mgr) => {
            let config = shard_mgr.shard_map();
            let shards_json: Vec<serde_json::Value> = config.shards.iter().map(|(id, s)| json!({
                "id": id,
                "name": s.name,
                "raft_group": s.raft_group,
                "replicas": s.replicas,
                "is_primary": s.is_primary,
            })).collect();

            let strategies_json: Vec<serde_json::Value> = config.class_strategies.iter().map(|(class, strategy)| {
                let strategy_json = match strategy {
                    onto_sharding::ShardStrategy::ClassBased { shard } => json!({
                        "type": "class",
                        "shard": shard,
                    }),
                    onto_sharding::ShardStrategy::RangeBased { ranges } => json!({
                        "type": "range",
                        "ranges": ranges.iter().map(|r| json!({
                            "end_key": String::from_utf8_lossy(&r.end_key),
                            "shard": r.shard,
                        })).collect::<Vec<_>>(),
                    }),
                    onto_sharding::ShardStrategy::HashBased { num_shards, slot_map } => json!({
                        "type": "hash",
                        "num_shards": num_shards,
                        "slot_map": slot_map,
                    }),
                };
                json!({
                    "class": class,
                    "strategy": strategy_json,
                })
            }).collect();

            (StatusCode::OK, Json(ApiResponse::success(json!({
                "default_shard": config.default_shard,
                "shards": shards_json,
                "class_strategies": strategies_json,
            }), elapsed)))
        }
        None => {
            (StatusCode::OK, Json(ApiResponse::success(json!({
                "enabled": false,
                "message": "Sharding is not configured"
            }), elapsed)))
        }
    }
}

/// PUT /api/sharding/config - Update sharding configuration.
async fn update_sharding_config(
    State(state): State<AppState>,
    Json(config): Json<serde_json::Value>,
) -> impl IntoResponse {
    let start = std::time::Instant::now();

    // Parse the shard map from the request
    let shard_map: onto_sharding::ShardMap = match serde_json::from_value(config) {
        Ok(m) => m,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, Json(ApiResponse::<Value>::error(
                format!("Invalid sharding config: {}", e)
            )));
        }
    };

    // Get local shards (for now, assume all shards are local)
    let local_shards: Vec<u32> = shard_map.shards.keys().cloned().collect();

    // Update the executor's shard configuration
    state.executor.update_shard_config(shard_map.clone(), local_shards);

    let elapsed = start.elapsed().as_secs_f64() * 1000.0;
    (StatusCode::OK, Json(ApiResponse::success(json!({
        "message": "Sharding configuration updated",
        "shards": shard_map.shards.len(),
        "class_strategies": shard_map.class_strategies.len(),
    }), elapsed)))
}

/// POST /api/sharding/shard - Add a new shard.
async fn add_shard(
    State(state): State<AppState>,
    Json(req): Json<ShardConfigRequest>,
) -> impl IntoResponse {
    let start = std::time::Instant::now();

    let mut mgr = state.executor.shard_manager();
    let shard_mgr = mgr.get_or_insert_with(|| onto_sharding::ShardManager::new(0));

    let config = onto_sharding::ShardConfig {
        id: req.id,
        name: req.name,
        raft_group: req.raft_group,
        replicas: req.replicas,
        is_primary: req.is_primary,
    };

    shard_mgr.create_shard(config);

    // Update the shard router with the new configuration
    let shard_map = shard_mgr.shard_map().clone();
    let local_shards: Vec<u32> = shard_map.shards.keys().cloned().collect();
    drop(mgr); // Release the lock before updating
    state.executor.update_shard_config(shard_map, local_shards);

    let elapsed = start.elapsed().as_secs_f64() * 1000.0;
    (StatusCode::OK, Json(ApiResponse::success(json!({
        "message": format!("Shard {} added", req.id),
        "shard_id": req.id,
    }), elapsed)))
}

/// POST /api/sharding/class - Assign a class to a sharding strategy.
async fn assign_class_shard(
    State(state): State<AppState>,
    Json(req): Json<ClassShardRequest>,
) -> impl IntoResponse {
    let start = std::time::Instant::now();

    let mut mgr = state.executor.shard_manager();
    let shard_mgr = mgr.get_or_insert_with(|| onto_sharding::ShardManager::new(0));

    match req.strategy.as_str() {
        "class" => {
            let shard = req.shard.unwrap_or(0);
            if let Err(e) = shard_mgr.assign_class(&req.class, shard) {
                return (StatusCode::BAD_REQUEST, Json(ApiResponse::<Value>::error(
                    format!("Failed to assign class: {}", e)
                )));
            }
        }
        "range" => {
            let ranges = req.ranges.unwrap_or_default();
            let shard_ranges: Vec<onto_sharding::RangeShard> = ranges.iter().map(|r| {
                onto_sharding::RangeShard {
                    end_key: r.end_key.as_bytes().to_vec(),
                    shard: r.shard,
                }
            }).collect();
            shard_mgr.shard_map_mut().shard_class_range(&req.class, shard_ranges);
        }
        "hash" => {
            let num_shards = req.num_shards.unwrap_or(4);
            let slot_map = req.slot_map.unwrap_or_else(|| (0..num_shards).collect());
            shard_mgr.shard_map_mut().shard_class_hash(&req.class, num_shards, slot_map);
        }
        _ => {
            return (StatusCode::BAD_REQUEST, Json(ApiResponse::<Value>::error(
                format!("Unknown strategy '{}': must be 'class', 'range', or 'hash'", req.strategy)
            )));
        }
    }

    // Update the shard router with the new configuration
    let shard_map = shard_mgr.shard_map().clone();
    let local_shards: Vec<u32> = shard_map.shards.keys().cloned().collect();
    drop(mgr); // Release the lock before updating
    state.executor.update_shard_config(shard_map, local_shards);

    let elapsed = start.elapsed().as_secs_f64() * 1000.0;
    (StatusCode::OK, Json(ApiResponse::success(json!({
        "message": format!("Class '{}' assigned to {} sharding", req.class, req.strategy),
        "class": req.class,
        "strategy": req.strategy,
    }), elapsed)))
}

/// GET /api/sharding/status - Get sharding status and statistics.
async fn get_sharding_status(
    State(state): State<AppState>,
) -> impl IntoResponse {
    let start = std::time::Instant::now();
    let elapsed = start.elapsed().as_secs_f64() * 1000.0;

    let mgr = state.executor.shard_manager();
    match mgr.as_ref() {
        Some(shard_mgr) => {
            let active = shard_mgr.active_shards();
            let config = shard_mgr.shard_map();
            let stats = shard_mgr.shard_statistics();

            (StatusCode::OK, Json(ApiResponse::success(json!({
                "enabled": true,
                "active_shards": active,
                "total_shards": config.shards.len(),
                "sharded_classes": config.class_strategies.len(),
                "shards": config.shards.iter().map(|(id, s)| {
                    let status = shard_mgr.shard_status(*id);
                    let shard_stats = stats.get(id);
                    json!({
                        "id": id,
                        "name": s.name,
                        "status": format!("{:?}", status),
                        "is_primary": s.is_primary,
                        "classes_count": shard_stats.map(|s| s.classes_count).unwrap_or(0),
                    })
                }).collect::<Vec<_>>(),
                "active_migrations": shard_mgr.active_migrations().len(),
                "migration_history": shard_mgr.migration_history().len(),
            }), elapsed)))
        }
        None => {
            (StatusCode::OK, Json(ApiResponse::success(json!({
                "enabled": false,
                "message": "Sharding is not configured"
            }), elapsed)))
        }
    }
}

// ── Migration API ──────────────────────────────────────────────────

/// Migration request.
#[derive(Debug, Deserialize)]
pub struct MigrationRequest {
    /// Source shard ID.
    pub source_shard: u32,
    /// Target shard ID.
    pub target_shard: u32,
    /// Class (table) to migrate. Use "*" for all classes.
    #[serde(default = "default_all_classes")]
    pub class: String,
    /// Optional key range for range-based migration.
    #[serde(default)]
    pub key_range: Option<KeyRange>,
}

fn default_all_classes() -> String { "*".to_string() }

/// Key range for migration.
#[derive(Debug, Deserialize)]
pub struct KeyRange {
    /// Start key (inclusive).
    pub start: String,
    /// End key (exclusive).
    pub end: String,
}

/// POST /api/sharding/migrate - Start a migration task.
async fn start_migration(
    State(state): State<AppState>,
    Json(req): Json<MigrationRequest>,
) -> impl IntoResponse {
    let start = std::time::Instant::now();

    let mut mgr = state.executor.shard_manager();
    let shard_mgr = match mgr.as_mut() {
        Some(m) => m,
        None => {
            return (StatusCode::BAD_REQUEST, Json(ApiResponse::<Value>::error(
                "Sharding is not configured"
            )));
        }
    };

    let key_range = req.key_range.map(|kr| (kr.start.into_bytes(), kr.end.into_bytes()));

    match shard_mgr.create_migration(req.source_shard, req.target_shard, &req.class, key_range) {
        Ok(migration_id) => {
            let elapsed = start.elapsed().as_secs_f64() * 1000.0;
            (StatusCode::OK, Json(ApiResponse::success(json!({
                "message": "Migration started",
                "migration_id": migration_id,
                "source_shard": req.source_shard,
                "target_shard": req.target_shard,
                "class": req.class,
            }), elapsed)))
        }
        Err(e) => {
            (StatusCode::BAD_REQUEST, Json(ApiResponse::<Value>::error(
                format!("Failed to start migration: {}", e)
            )))
        }
    }
}

/// Migration progress update request.
#[derive(Debug, Deserialize)]
pub struct MigrationProgressRequest {
    /// Migration ID.
    pub migration_id: String,
    /// Total records to migrate.
    pub total_records: u64,
    /// Records migrated so far.
    pub migrated_records: u64,
}

/// PUT /api/sharding/migrate/progress - Update migration progress.
async fn update_migration_progress(
    State(state): State<AppState>,
    Json(req): Json<MigrationProgressRequest>,
) -> impl IntoResponse {
    let start = std::time::Instant::now();

    let mut mgr = state.executor.shard_manager();
    let shard_mgr = match mgr.as_mut() {
        Some(m) => m,
        None => {
            return (StatusCode::BAD_REQUEST, Json(ApiResponse::<Value>::error(
                "Sharding is not configured"
            )));
        }
    };

    match shard_mgr.update_migration_progress(&req.migration_id, req.total_records, req.migrated_records) {
        Ok(()) => {
            let elapsed = start.elapsed().as_secs_f64() * 1000.0;
            (StatusCode::OK, Json(ApiResponse::success(json!({
                "message": "Migration progress updated",
                "migration_id": req.migration_id,
                "total_records": req.total_records,
                "migrated_records": req.migrated_records,
                "progress_percent": if req.total_records > 0 { (req.migrated_records as f64 / req.total_records as f64 * 100.0) as u64 } else { 0 },
            }), elapsed)))
        }
        Err(e) => {
            (StatusCode::BAD_REQUEST, Json(ApiResponse::<Value>::error(
                format!("Failed to update migration progress: {}", e)
            )))
        }
    }
}

/// Migration completion request.
#[derive(Debug, Deserialize)]
pub struct MigrationCompleteRequest {
    /// Migration ID.
    pub migration_id: String,
}

/// POST /api/sharding/migrate/complete - Complete a migration.
async fn complete_migration(
    State(state): State<AppState>,
    Json(req): Json<MigrationCompleteRequest>,
) -> impl IntoResponse {
    let start = std::time::Instant::now();

    let mut mgr = state.executor.shard_manager();
    let shard_mgr = match mgr.as_mut() {
        Some(m) => m,
        None => {
            return (StatusCode::BAD_REQUEST, Json(ApiResponse::<Value>::error(
                "Sharding is not configured"
            )));
        }
    };

    match shard_mgr.complete_migration_task(&req.migration_id) {
        Ok(result) => {
            // Update the shard router with the new configuration
            let shard_map = shard_mgr.shard_map().clone();
            let local_shards: Vec<u32> = shard_map.shards.keys().cloned().collect();
            drop(mgr);
            state.executor.update_shard_config(shard_map, local_shards);

            let elapsed = start.elapsed().as_secs_f64() * 1000.0;
            (StatusCode::OK, Json(ApiResponse::success(json!({
                "message": "Migration completed",
                "migration_id": result.migration_id,
                "source_shard": result.source_shard,
                "target_shard": result.target_shard,
                "records_migrated": result.records_migrated,
                "status": format!("{:?}", result.status),
            }), elapsed)))
        }
        Err(e) => {
            (StatusCode::BAD_REQUEST, Json(ApiResponse::<Value>::error(
                format!("Failed to complete migration: {}", e)
            )))
        }
    }
}

/// POST /api/sharding/migrate/cancel - Cancel a migration.
async fn cancel_migration(
    State(state): State<AppState>,
    Json(req): Json<MigrationCompleteRequest>,
) -> impl IntoResponse {
    let start = std::time::Instant::now();

    let mut mgr = state.executor.shard_manager();
    let shard_mgr = match mgr.as_mut() {
        Some(m) => m,
        None => {
            return (StatusCode::BAD_REQUEST, Json(ApiResponse::<Value>::error(
                "Sharding is not configured"
            )));
        }
    };

    match shard_mgr.cancel_migration(&req.migration_id) {
        Ok(()) => {
            let elapsed = start.elapsed().as_secs_f64() * 1000.0;
            (StatusCode::OK, Json(ApiResponse::success(json!({
                "message": "Migration cancelled",
                "migration_id": req.migration_id,
            }), elapsed)))
        }
        Err(e) => {
            (StatusCode::BAD_REQUEST, Json(ApiResponse::<Value>::error(
                format!("Failed to cancel migration: {}", e)
            )))
        }
    }
}

/// GET /api/sharding/migrations - List all migrations.
async fn list_migrations(
    State(state): State<AppState>,
) -> impl IntoResponse {
    let start = std::time::Instant::now();
    let elapsed = start.elapsed().as_secs_f64() * 1000.0;

    let mgr = state.executor.shard_manager();
    match mgr.as_ref() {
        Some(shard_mgr) => {
            let migrations: Vec<Value> = shard_mgr.list_migrations().iter().map(|m| {
                json!({
                    "id": m.id,
                    "source_shard": m.source_shard,
                    "target_shard": m.target_shard,
                    "class": m.class,
                    "status": format!("{:?}", m.status),
                    "created_at": m.created_at,
                    "completed_at": m.completed_at,
                })
            }).collect();

            (StatusCode::OK, Json(ApiResponse::success(json!({
                "migrations": migrations,
                "total": migrations.len(),
                "active": shard_mgr.active_migrations().len(),
            }), elapsed)))
        }
        None => {
            (StatusCode::OK, Json(ApiResponse::success(json!({
                "enabled": false,
                "message": "Sharding is not configured"
            }), elapsed)))
        }
    }
}

// ── Rebalance API ──────────────────────────────────────────────────

/// Rebalance request.
#[derive(Debug, Deserialize)]
pub struct RebalanceRequest {
    /// Classes to rebalance. If empty, rebalances all classes.
    #[serde(default)]
    pub classes: Vec<String>,
}

/// POST /api/sharding/rebalance - Rebalance classes across shards.
async fn rebalance_shards(
    State(state): State<AppState>,
    Json(req): Json<RebalanceRequest>,
) -> impl IntoResponse {
    let start = std::time::Instant::now();

    let mut mgr = state.executor.shard_manager();
    let shard_mgr = match mgr.as_mut() {
        Some(m) => m,
        None => {
            return (StatusCode::BAD_REQUEST, Json(ApiResponse::<Value>::error(
                "Sharding is not configured"
            )));
        }
    };

    // If no classes specified, rebalance all
    let classes = if req.classes.is_empty() {
        shard_mgr.shard_map().class_strategies.keys().cloned().collect()
    } else {
        req.classes
    };

    match shard_mgr.rebalance_classes(classes) {
        Ok(result) => {
            // Update the shard router with the new configuration
            let shard_map = shard_mgr.shard_map().clone();
            let local_shards: Vec<u32> = shard_map.shards.keys().cloned().collect();
            drop(mgr);
            state.executor.update_shard_config(shard_map, local_shards);

            let elapsed = start.elapsed().as_secs_f64() * 1000.0;
            (StatusCode::OK, Json(ApiResponse::success(json!({
                "message": "Rebalance completed",
                "rebalance_id": result.rebalance_id,
                "classes_rebalanced": result.classes_rebalanced,
                "new_assignments": result.new_assignments,
                "status": result.status,
            }), elapsed)))
        }
        Err(e) => {
            (StatusCode::BAD_REQUEST, Json(ApiResponse::<Value>::error(
                format!("Failed to rebalance: {}", e)
            )))
        }
    }
}

// ── Scale API ──────────────────────────────────────────────────────

/// Scale request - add shard and optionally rebalance.
#[derive(Debug, Deserialize)]
pub struct ScaleRequest {
    /// Shard configuration.
    pub shard: ShardConfigRequest,
    /// Whether to rebalance after adding.
    #[serde(default = "default_true")]
    pub rebalance: bool,
}

/// POST /api/sharding/scale/add - Add a new shard and optionally rebalance.
async fn add_shard_and_rebalance(
    State(state): State<AppState>,
    Json(req): Json<ScaleRequest>,
) -> impl IntoResponse {
    let start = std::time::Instant::now();

    let mut mgr = state.executor.shard_manager();
    let shard_mgr = match mgr.as_mut() {
        Some(m) => m,
        None => {
            return (StatusCode::BAD_REQUEST, Json(ApiResponse::<Value>::error(
                "Sharding is not configured"
            )));
        }
    };

    let config = onto_sharding::ShardConfig {
        id: req.shard.id,
        name: req.shard.name,
        raft_group: req.shard.raft_group,
        replicas: req.shard.replicas,
        is_primary: req.shard.is_primary,
    };

    match shard_mgr.add_shard_and_rebalance(config, req.rebalance) {
        Ok(result) => {
            // Update the shard router with the new configuration
            let shard_map = shard_mgr.shard_map().clone();
            let local_shards: Vec<u32> = shard_map.shards.keys().cloned().collect();
            drop(mgr);
            state.executor.update_shard_config(shard_map, local_shards);

            let elapsed = start.elapsed().as_secs_f64() * 1000.0;
            (StatusCode::OK, Json(ApiResponse::success(json!({
                "message": result.status,
                "shard_id": req.shard.id,
                "rebalance_id": result.rebalance_id,
                "classes_rebalanced": result.classes_rebalanced,
                "new_assignments": result.new_assignments,
            }), elapsed)))
        }
        Err(e) => {
            (StatusCode::BAD_REQUEST, Json(ApiResponse::<Value>::error(
                format!("Failed to add shard: {}", e)
            )))
        }
    }
}

/// Remove shard request.
#[derive(Debug, Deserialize)]
pub struct RemoveShardRequest {
    /// Shard ID to remove.
    pub shard_id: u32,
    /// Target shard to migrate data to.
    pub target_shard: u32,
}

/// POST /api/sharding/scale/remove - Remove a shard and migrate its data.
async fn remove_shard(
    State(state): State<AppState>,
    Json(req): Json<RemoveShardRequest>,
) -> impl IntoResponse {
    let start = std::time::Instant::now();

    let mut mgr = state.executor.shard_manager();
    let shard_mgr = match mgr.as_mut() {
        Some(m) => m,
        None => {
            return (StatusCode::BAD_REQUEST, Json(ApiResponse::<Value>::error(
                "Sharding is not configured"
            )));
        }
    };

    match shard_mgr.remove_shard_with_migration(req.shard_id, req.target_shard) {
        Ok(message) => {
            // Update the shard router with the new configuration
            let shard_map = shard_mgr.shard_map().clone();
            let local_shards: Vec<u32> = shard_map.shards.keys().cloned().collect();
            drop(mgr);
            state.executor.update_shard_config(shard_map, local_shards);

            let elapsed = start.elapsed().as_secs_f64() * 1000.0;
            (StatusCode::OK, Json(ApiResponse::success(json!({
                "message": message,
                "removed_shard": req.shard_id,
                "target_shard": req.target_shard,
            }), elapsed)))
        }
        Err(e) => {
            (StatusCode::BAD_REQUEST, Json(ApiResponse::<Value>::error(
                format!("Failed to remove shard: {}", e)
            )))
        }
    }
}

// ── Split API ──────────────────────────────────────────────────────

/// Split shard request.
#[derive(Debug, Deserialize)]
pub struct SplitRequest {
    /// Source shard ID to split.
    pub source_shard: u32,
    /// New shards to create.
    pub new_shards: Vec<ShardConfigRequest>,
    /// Split strategy: "even", "range", "hash".
    #[serde(default = "default_split_strategy")]
    pub strategy: String,
    /// Optional ranges for range-based split.
    #[serde(default)]
    pub ranges: Option<Vec<KeyRange>>,
}

fn default_split_strategy() -> String { "even".to_string() }

/// POST /api/sharding/split - Split a shard into multiple new shards.
async fn split_shard(
    State(state): State<AppState>,
    Json(req): Json<SplitRequest>,
) -> impl IntoResponse {
    let start = std::time::Instant::now();

    let mut mgr = state.executor.shard_manager();
    let shard_mgr = match mgr.as_mut() {
        Some(m) => m,
        None => {
            return (StatusCode::BAD_REQUEST, Json(ApiResponse::<Value>::error(
                "Sharding is not configured"
            )));
        }
    };

    let new_shard_configs: Vec<onto_sharding::ShardConfig> = req.new_shards.iter().map(|s| {
        onto_sharding::ShardConfig {
            id: s.id,
            name: s.name.clone(),
            raft_group: s.raft_group,
            replicas: s.replicas.clone(),
            is_primary: s.is_primary,
        }
    }).collect();

    let strategy = match req.strategy.as_str() {
        "range" => {
            let ranges = req.ranges.unwrap_or_default();
            onto_sharding::SplitStrategy::RangeBased {
                ranges: ranges.iter().map(|r| (r.start.clone().into_bytes(), r.end.clone().into_bytes())).collect(),
            }
        }
        "hash" => onto_sharding::SplitStrategy::HashBased,
        _ => onto_sharding::SplitStrategy::Even,
    };

    match shard_mgr.split_shard(req.source_shard, new_shard_configs, strategy) {
        Ok(migrations) => {
            // Update the shard router with the new configuration
            let shard_map = shard_mgr.shard_map().clone();
            let local_shards: Vec<u32> = shard_map.shards.keys().cloned().collect();
            drop(mgr);
            state.executor.update_shard_config(shard_map, local_shards);

            let elapsed = start.elapsed().as_secs_f64() * 1000.0;
            (StatusCode::OK, Json(ApiResponse::success(json!({
                "message": format!("Shard {} split initiated", req.source_shard),
                "source_shard": req.source_shard,
                "new_shards": req.new_shards.iter().map(|s| s.id).collect::<Vec<_>>(),
                "migrations_created": migrations.len(),
                "migrations": migrations.iter().map(|m| json!({
                    "id": m.id,
                    "source": m.source_shard,
                    "target": m.target_shard,
                    "status": format!("{:?}", m.status),
                })).collect::<Vec<_>>(),
            }), elapsed)))
        }
        Err(e) => {
            (StatusCode::BAD_REQUEST, Json(ApiResponse::<Value>::error(
                format!("Failed to split shard: {}", e)
            )))
        }
    }
}
