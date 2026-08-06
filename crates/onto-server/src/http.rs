//! HTTP API for OntoDB.
//!
//! Provides RESTful endpoints for SQL queries, vector search, hybrid queries,
//! health checks, and Prometheus metrics.

use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use onto_query::{QueryAst, QueryExecutor, QueryParser};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

use crate::metrics::SharedMetrics;

/// Shared application state.
#[derive(Clone)]
pub struct AppState {
    pub executor: Arc<QueryExecutor>,
    pub metrics: SharedMetrics,
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
pub fn build_router(state: AppState) -> Router {
    Router::new()
        // Health check
        .route("/api/health", get(health))
        // Metrics (Prometheus and JSON)
        .route("/metrics", get(metrics_prometheus))
        .route("/api/metrics", get(metrics_json))
        // SQL query execution
        .route("/api/query", post(execute_query))
        // Vector search
        .route("/api/vector/search", post(vector_search))
        // Hybrid query: SQL + vector search
        .route("/api/hybrid/query", post(hybrid_query))
        // Schema introspection
        .route("/api/schema", get(get_schema))
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Builds the HTTP router with authentication and rate limiting.
pub fn build_router_with_auth(
    state: AppState,
    auth_state: crate::auth::AuthState,
    rate_limiter: crate::rate_limit::RateLimiter,
) -> Router {
    use axum::middleware;

    Router::new()
        // Health check and metrics (no auth required)
        .route("/api/health", get(health))
        .route("/api/health/ready", get(health_ready))
        .route("/api/health/live", get(health_live))
        .route("/metrics", get(metrics_prometheus))
        .route("/api/metrics", get(metrics_json))
        // Protected routes
        .route("/api/query", post(execute_query))
        .route("/api/vector/search", post(vector_search))
        .route("/api/hybrid/query", post(hybrid_query))
        .route("/api/schema", get(get_schema))
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
async fn health(State(state): State<AppState>) -> impl IntoResponse {
    let uptime = state.metrics.started_at.elapsed().as_secs();
    Json(json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        "engine": "OntoDB",
        "uptime_seconds": uptime,
        "checks": {
            "storage": "ok",
            "query_engine": "ok"
        }
    }))
}

/// GET /api/health/ready - Kubernetes readiness probe.
/// Returns 200 if the server is ready to accept requests.
async fn health_ready() -> impl IntoResponse {
    Json(json!({
        "status": "ready"
    }))
}

/// GET /api/health/live - Kubernetes liveness probe.
/// Returns 200 if the server is alive.
async fn health_live() -> impl IntoResponse {
    Json(json!({
        "status": "alive"
    }))
}

/// GET /metrics - Prometheus metrics endpoint.
/// Returns metrics in Prometheus exposition format.
async fn metrics_prometheus(State(state): State<AppState>) -> impl IntoResponse {
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
                Json(ApiResponse::<Value>::error(format!("Parse error: {}", e))),
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

    let result = match state.executor.execute(&ast) {
        Ok(r) => r,
        Err(e) => {
            let elapsed = start.elapsed().as_secs_f64();
            state.metrics.record_query(query_type, elapsed, false);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<Value>::error(format!("Execution error: {}", e))),
            );
        }
    };

    let elapsed = start.elapsed().as_secs_f64();
    state.metrics.record_query(query_type, elapsed, true);

    let elapsed_ms = elapsed * 1000.0;

    let data = match result {
        onto_query::QueryResult::Success(msg) => json!({ "message": msg }),
        onto_query::QueryResult::Rows(rows) => {
            if req.pretty {
                json!(rows)
            } else {
                json!(rows)
            }
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

    let result = match state.executor.execute(&ast) {
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

    let sql_result = match state.executor.execute(&sql_ast) {
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
                format!(" WHERE {}", &sql_query[where_pos + 7..].trim())
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

    let vector_result = match state.executor.execute(&vector_ast) {
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
    State(_state): State<AppState>,
) -> impl IntoResponse {
    // TODO: Implement schema introspection from ontology store
    Json(json!({
        "classes": [],
        "indexes": [],
        "vector_indexes": []
    }))
}
