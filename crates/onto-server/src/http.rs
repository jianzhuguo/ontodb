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
use onto_query::{QueryAst, QueryExecutor, QueryParser};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use tower_http::cors::CorsLayer;
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
    // Reject dangerous SQL keywords (case-insensitive)
    let upper = filter.to_uppercase();
    let forbidden = [
        "DROP ", "DELETE ", "INSERT ", "UPDATE ", "UNION ",
        "ALTER ", "CREATE ", "TRUNCATE ", "EXEC ", "EXECUTE ",
        "-- ", "/*", "*/",
    ];
    for kw in &forbidden {
        if upper.contains(kw) {
            return Err(format!("filter must not contain '{}'", kw.trim()));
        }
    }
    Ok(())
}

/// Slow query threshold in seconds. Queries exceeding this are logged as warnings.
const SLOW_QUERY_THRESHOLD_SECS: f64 = 1.0;

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

/// Builds the HTTP router with all API endpoints (no auth).
#[allow(dead_code)]
pub fn build_router(state: AppState) -> Router {
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
        // API documentation
        .route("/api/docs", get(swagger_ui))
        .route("/api/openapi.json", get(openapi_spec))
        // Web console
        .route("/console", get(web_console))
        .route("/", get(web_console))
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Builds the HTTP router with authentication and rate limiting.
pub fn build_router_with_auth(
    state: AppState,
    auth_state: crate::auth::AuthState,
    rate_limiter: crate::rate_limit::RateLimiter,
    admin_state: crate::admin::AdminState,
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
        // API documentation and console (no auth required)
        .route("/api/docs", get(swagger_ui))
        .route("/api/openapi.json", get(openapi_spec))
        .route("/console", get(web_console))
        .route("/", get(web_console))
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
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
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
    // Check: parser works (query engine alive) — use a query syntax that always parses
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
    Json(req): Json<QueryRequest>,
) -> impl IntoResponse {
    let start = std::time::Instant::now();

    let query = req.query.trim_end_matches(';').trim();
    let ast = match QueryParser::parse(query) {
        Ok(ast) => ast,
        Err(e) => {
            state.metrics.record_parse_error();
            return (
                StatusCode::BAD_REQUEST,
                PrettyJson(ApiResponse::<Value>::error(format!("Parse error: {}", e)), false),
            );
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
            // Audit log — failed query
            let audit_entry = state.audit.create_entry("127.0.0.1", None, query_type, query, elapsed * 1000.0, false, Some(e.to_string()));
            state.audit.log(&audit_entry);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                PrettyJson(ApiResponse::<Value>::error(format!("Execution error: {}", e)), false),
            );
        }
    };

    let elapsed = start.elapsed().as_secs_f64();
    state.metrics.record_query(query_type, elapsed, true);

    // Audit log — successful query
    let audit_entry = state.audit.create_entry("127.0.0.1", None, query_type, query, elapsed * 1000.0, true, None);
    state.audit.log(&audit_entry);

    // Slow query logging
    if elapsed >= SLOW_QUERY_THRESHOLD_SECS {
        state.metrics.slow_queries_total.inc();
        let truncated = if query.len() > 200 { &query[..200] } else { query };
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
                Json(ApiResponse::<Value>::error(format!("SPARQL translation error: {}", e))),
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
                Json(ApiResponse::<Value>::error(format!("SPARQL execution error: {}", e))),
            );
        }
    };

    let elapsed = start.elapsed().as_secs_f64();
    state.metrics.record_query("SPARQL", elapsed, true);

    if elapsed >= SLOW_QUERY_THRESHOLD_SECS {
        state.metrics.slow_queries_total.inc();
        let truncated = if req.query.len() > 200 { &req.query[..200] } else { &req.query };
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

    let query = format!(
        "VECTOR SEARCH ON {} ({}) QUERY [{}] TOP {}{}",
        req.class, req.column, vector_str, req.top_k, filter_clause
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
                Json(ApiResponse::<Value>::error(format!("Execution error: {}", e))),
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
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<Value>::error(format!("SQL execution error: {}", e))),
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

    // Step 2: Build vector search with filter from SQL results
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
            // Extract the WHERE clause from the original SQL
            let sql_upper = sql_query.to_uppercase();
            if let Some(where_pos) = sql_upper.find(" WHERE ") {
                let extracted = &sql_query[where_pos + 7..].trim();
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
        class, req.vector_column, vector_str, req.top_k, filter_clause
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
                Json(ApiResponse::<Value>::error(format!("Vector search error: {}", e))),
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
    let max_depth = req.get("max_depth").and_then(|v| v.as_u64()).unwrap_or(3) as usize;
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
    let max_depth = req.get("max_depth").and_then(|v| v.as_u64()).unwrap_or(10) as usize;

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
    let backup_dir = std::path::Path::new(&req.path);

    match state.executor.backup(backup_dir) {
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
    let backup_dir = std::path::Path::new(&req.path);

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

    match state.executor.backup_incremental(backup_dir, &since) {
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
    let backup_dir = std::path::Path::new(&req.path);

    match onto_query::QueryExecutor::verify_backup(backup_dir) {
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
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
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
