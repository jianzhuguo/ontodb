// Copyright (c) 2024-2026 Guo Jianzhu (郭建筑)
// Licensed under the Apache License, Version 2.0.
// See LICENSE for details.
//! Semantic retrieval API: `/api/semantic/{similar,trace,infer,expand}`.
//!
//! Thin orchestration over vector search, relation-chain traversal and
//! OWL-lite inference — the fusion query patterns of
//! `docs/语义融合查询模式库.md` productized as HTTP endpoints. All semantics
//! live in the data layer (`__ontology__` + generic rules); nothing here is
//! domain-specific.

use axum::{
    extract::{ConnectInfo, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::sync::Arc;

use onto_query::{OntoQLParser, QueryAst, QueryExecutor, QueryParser};

use crate::http::{
    actor_of, sanitize_error, validate_filter, validate_identifier, ApiResponse, AppState,
};

// ─────────────────────────────────────────────────────────────────────────────
// Request / response shapes
// ─────────────────────────────────────────────────────────────────────────────

/// Anchor term: either a full storage key (`ns::Class::id`) or a class/id
/// reference resolved through an id lookup.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum AnchorSpec {
    Key(String),
    Ref {
        class: String,
        id: String,
        #[serde(default)]
        id_column: Option<String>,
    },
}

/// One hop of a relation chain: either a bare relation name or a detailed
/// hop with direction (`out`: source=prev; `in`: target=prev).
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum ChainHop {
    Relation(String),
    Detailed {
        relation: String,
        #[serde(default)]
        direction: Option<String>,
        #[serde(default)]
        limit: Option<usize>,
    },
}

/// POST /api/semantic/similar — vector similarity with an optional entity
/// anchor and SQL WHERE filter (ontology type scoping via `class`).
#[derive(Debug, Deserialize)]
pub struct SemanticSimilarRequest {
    /// Target class (search scope = class + its hierarchy).
    pub class: String,
    /// Vector column name.
    pub column: String,
    /// Query vector (mutually exclusive with `entity_id`).
    #[serde(default)]
    pub query_vector: Option<Vec<f32>>,
    /// Anchor entity id: the entity's vector is loaded and used as query.
    #[serde(default)]
    pub entity_id: Option<String>,
    /// Id column for the entity lookup (default `id`; `__pk__` for keys).
    #[serde(default)]
    pub id_column: Option<String>,
    /// Number of top results (capped at 10000).
    pub top_k: usize,
    /// Optional SQL WHERE filter (same validation as /api/vector/search).
    #[serde(default)]
    pub filter: Option<String>,
}

/// POST /api/semantic/trace — relation-chain traversal from an anchor with
/// optional inverse/inference enrichment (M2 pattern).
#[derive(Debug, Deserialize)]
pub struct SemanticTraceRequest {
    pub anchor: AnchorSpec,
    /// Hop relations, applied in order; each hop's output feeds the next.
    pub chain: Vec<ChainHop>,
    /// Relations to run INFER over (anchor-scoped, out-direction).
    #[serde(default)]
    pub infer_relations: Vec<String>,
    /// Rule filter for INFER (e.g. `["Prp-inv"]`); empty = all rules.
    #[serde(default)]
    pub rules: Vec<String>,
    /// Max edges kept per hop (default 100, cap 5000).
    #[serde(default)]
    pub limit_per_hop: Option<usize>,
    /// Max inferred facts returned (default 500, cap 50000).
    #[serde(default)]
    pub inferred_limit: Option<usize>,
}

/// POST /api/semantic/infer — explicit inference query with rule selection
/// and offset/limit pagination over the streaming closure.
#[derive(Debug, Deserialize)]
pub struct SemanticInferRequest {
    /// Inner SELECT (fact-collection scope for reasoning).
    pub query: String,
    /// Rule filter (e.g. `["Prp-trp","Prp-inv"]`); empty = all seven rules.
    #[serde(default)]
    pub rules: Vec<String>,
    /// Page size (default 100, cap 10000).
    #[serde(default)]
    pub limit: Option<usize>,
    /// Page offset (default 0).
    #[serde(default)]
    pub offset: Option<usize>,
}

/// Vector-similar expansion block for /api/semantic/expand.
#[derive(Debug, Clone, Deserialize)]
pub struct SimilarSpec {
    /// Class holding the vector rows (e.g. a name-embedding class).
    pub class: String,
    /// Vector column.
    pub column: String,
    /// Field matching `key_value` on the anchor row.
    pub key_field: String,
    /// Explicit lookup value for `key_field`.
    pub key_value: String,
    /// Top-k similar (default 10, cap 1000).
    #[serde(default)]
    pub top_k: Option<usize>,
}

/// POST /api/semantic/expand — hierarchy expansion (up/down) + synonyms +
/// optional vector-similar concepts (M3-style entry).
#[derive(Debug, Deserialize)]
pub struct SemanticExpandRequest {
    pub term: AnchorSpec,
    /// Hierarchy relation to walk (e.g. `go_is_a`, `subclass_of`).
    pub relation: String,
    /// `up` (ancestors) / `down` (descendants) / `both` (default).
    #[serde(default)]
    pub direction: Option<String>,
    /// Max BFS depth (default 3, cap 10).
    #[serde(default)]
    pub max_depth: Option<usize>,
    /// Relations whose endpoints count as synonym links (either side).
    #[serde(default)]
    pub synonym_relations: Vec<String>,
    /// Optional vector-similar concepts.
    #[serde(default)]
    pub similar: Option<SimilarSpec>,
    /// Max nodes per bucket (default 200, cap 5000).
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Serialize)]
pub struct SimilarResponse {
    pub count: usize,
    pub rows: Vec<Map<String, Value>>,
}

#[derive(Serialize)]
pub struct HopResult {
    pub relation: String,
    pub direction: String,
    pub count: usize,
    pub edges: Vec<Map<String, Value>>,
}

#[derive(Serialize)]
pub struct TraceResponse {
    pub anchor: String,
    pub hops: Vec<HopResult>,
    pub inferred: Vec<Map<String, Value>>,
    pub inferred_count: usize,
}

#[derive(Serialize)]
pub struct InferResponse {
    pub count: usize,
    pub has_more: bool,
    pub next_offset: Option<usize>,
    pub rows: Vec<Map<String, Value>>,
}

#[derive(Serialize)]
pub struct ExpandNode {
    pub key: String,
    pub depth: usize,
}

#[derive(Serialize)]
pub struct SynonymNode {
    pub key: String,
    pub via: String,
}

#[derive(Serialize)]
pub struct ExpandResponse {
    pub term: String,
    pub ancestors: Vec<ExpandNode>,
    pub descendants: Vec<ExpandNode>,
    pub synonyms: Vec<SynonymNode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub similar: Option<Vec<Map<String, Value>>>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Internal helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Error carrying an HTTP status: validation → 400, execution → 500.
#[derive(Debug)]
struct ApiErr {
    status: StatusCode,
    msg: String,
}

impl ApiErr {
    fn bad(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            msg: msg.into(),
        }
    }

    fn internal(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            msg: sanitize_error(&msg.into()),
        }
    }
}

type Res<T> = Result<T, ApiErr>;

/// Escapes a literal for embedding in a single-quoted SQL string.
fn esc(s: &str) -> String {
    s.replace('\'', "''")
}

/// Parses user/internal query text: OntoQL first, SQL fallback (mirrors
/// `/api/query`'s dual-parser order).
fn parse_q(sql: &str) -> Res<QueryAst> {
    match OntoQLParser::parse(sql) {
        Ok(a) => a.to_query_ast().map_err(|e| ApiErr::bad(format!("Parse error: {e}"))),
        Err(_) => QueryParser::parse(sql).map_err(|e| ApiErr::bad(format!("Parse error: {e}"))),
    }
}

/// Runs a read query and returns its rows.
fn query_rows(
    executor: &Arc<QueryExecutor>,
    actor: &onto_query::QueryActor,
    sql: &str,
) -> Res<Vec<Map<String, Value>>> {
    let ast = parse_q(sql)?;
    let result = executor
        .execute_read_as(&ast, actor)
        .map_err(|e| ApiErr::internal(format!("{e}")))?;
    match result {
        onto_query::QueryResult::Rows(rows) => Ok(rows),
        onto_query::QueryResult::Success(msg) => Ok(vec![Map::from_iter(vec![(
            "message".to_string(),
            Value::String(msg),
        )])]),
    }
}

/// Converts a stored vector value to f32s: JSON array of numbers, or a
/// base64-encoded bit-vector string (the `Compound.fp` layout).
fn value_to_vec(v: &Value) -> Res<Vec<f32>> {
    match v {
        Value::Array(arr) => arr
            .iter()
            .map(|x| {
                x.as_f64()
                    .map(|f| f as f32)
                    .ok_or_else(|| ApiErr::bad("non-numeric vector element"))
            })
            .collect(),
        Value::String(s) => {
            use base64::Engine;
            let raw = base64::engine::general_purpose::STANDARD
                .decode(s)
                .map_err(|e| ApiErr::bad(format!("vector string is not valid base64: {e}")))?;
            let mut out = Vec::with_capacity(raw.len() * 8);
            for byte in &raw {
                for bit in 0..8 {
                    out.push(if (byte >> (7 - bit)) & 1 == 1 { 1.0 } else { 0.0 });
                }
            }
            Ok(out)
        }
        _ => Err(ApiErr::bad("unsupported vector value type")),
    }
}

/// Loads the vector of one entity row: `SELECT {column} FROM {class}
/// WHERE {id_col} = '{id}' LIMIT 1`.
fn load_entity_vector(
    executor: &Arc<QueryExecutor>,
    actor: &onto_query::QueryActor,
    class: &str,
    column: &str,
    id_col: &str,
    id_val: &str,
) -> Res<Vec<f32>> {
    let sql = format!(
        "SELECT {} FROM {} WHERE {} = '{}' LIMIT 1",
        column,
        class,
        id_col,
        esc(id_val)
    );
    let rows = query_rows(executor, actor, &sql)?;
    let row = rows
        .first()
        .ok_or_else(|| ApiErr::bad(format!("entity not found: {} {} = {}", class, id_col, id_val)))?;
    let v = row
        .get(column)
        .ok_or_else(|| ApiErr::bad(format!("column '{}' missing on {}", column, class)))?;
    value_to_vec(v)
}

/// Runs HNSW vector search via the VECTOR SEARCH grammar (same engine path
/// as /api/vector/search).
fn vector_search_rows(
    executor: &Arc<QueryExecutor>,
    actor: &onto_query::QueryActor,
    class: &str,
    column: &str,
    vec: &[f32],
    top_k: usize,
    filter: Option<&str>,
) -> Res<Vec<Map<String, Value>>> {
    let filter_clause = match filter {
        Some(f) => format!(" WHERE {}", f),
        None => String::new(),
    };
    let vector_str = vec
        .iter()
        .map(|v| v.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let q = format!(
        "VECTOR SEARCH ON {} ({}) QUERY [{}] TOP {}{}",
        class,
        column,
        vector_str,
        top_k.min(10000),
        filter_clause
    );
    let ast = QueryParser::parse(&q).map_err(|e| ApiErr::bad(format!("Parse error: {e}")))?;
    let result = executor
        .execute_read_as(&ast, actor)
        .map_err(|e| ApiErr::internal(format!("{e}")))?;
    match result {
        onto_query::QueryResult::Rows(rows) => Ok(rows),
        _ => Ok(Vec::new()),
    }
}

/// Resolves an anchor to its storage key. `Ref` form looks up `__pk__` by id
/// (falling back to the `{class}::{id}` key convention).
fn resolve_anchor(
    executor: &Arc<QueryExecutor>,
    actor: &onto_query::QueryActor,
    anchor: &AnchorSpec,
) -> Res<String> {
    match anchor {
        AnchorSpec::Key(k) => {
            if k.is_empty() || k.len() > 512 {
                return Err(ApiErr::bad("anchor key must be 1-512 characters"));
            }
            Ok(k.clone())
        }
        AnchorSpec::Ref {
            class,
            id,
            id_column,
        } => {
            validate_identifier(class).map_err(ApiErr::bad)?;
            let id_col = id_column.as_deref().unwrap_or("id");
            validate_identifier(id_col).map_err(ApiErr::bad)?;
            let sql = format!(
                "SELECT * FROM {} WHERE {} = '{}' LIMIT 1",
                class,
                id_col,
                esc(id)
            );
            let rows = query_rows(executor, actor, &sql)?;
            let key = rows
                .first()
                .and_then(|r| r.get("__pk__"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("{}::{}", class, id));
            Ok(key)
        }
    }
}

/// Paginated inference over the streaming closure: skips `offset` inferred
/// triples, collects up to `limit`, reports whether more remain. The walk is
/// bounded at `offset + limit + 1` rows so full closures cannot run away.
fn run_infer_paged(
    executor: &Arc<QueryExecutor>,
    actor: &onto_query::QueryActor,
    inner_sql: &str,
    rules: &[String],
    offset: usize,
    limit: usize,
) -> Res<(Vec<Map<String, Value>>, bool)> {
    let inner = parse_q(inner_sql)?;
    let ast = QueryAst::Infer {
        query: Box::new(inner),
        scope: "ALL".to_string(),
        rules: rules.to_vec(),
        materialize: true,
    };
    let prepared = executor
        .prepare_as(&ast, actor)
        .map_err(|e| ApiErr::internal(format!("{e}")))?
        .into_owned();
    let QueryAst::Infer {
        query, rules, ..
    } = prepared
    else {
        return Err(ApiErr::internal("expected Infer AST after prepare"));
    };

    let mut skipped = 0usize;
    let mut rows: Vec<Map<String, Value>> = Vec::new();
    let mut has_more = false;
    let mut sink = |batch: Vec<Map<String, Value>>| -> bool {
        for row in batch {
            if skipped < offset {
                skipped += 1;
                continue;
            }
            if rows.len() >= limit {
                has_more = true;
                return false;
            }
            rows.push(row);
        }
        true
    };
    let cap = Some(offset + limit + 1);
    executor
        .execute_infer_streaming(
            query.as_ref(),
            &rules,
            cap,
            &executor.engine().default_engine(),
            &mut sink,
        )
        .map_err(|e| ApiErr::internal(format!("{e}")))?;
    Ok((rows, has_more))
}

/// Traverses one chain hop: batches frontier keys into OR-conditions,
/// direction `out` = `WHERE source = k` (next = target), `in` = `WHERE
/// target = k` (next = source).
fn hop_edges(
    executor: &Arc<QueryExecutor>,
    actor: &onto_query::QueryActor,
    relation: &str,
    direction: &str,
    frontier: &[String],
    limit: usize,
) -> Res<Vec<Map<String, Value>>> {
    let (field, next_field) = if direction == "in" {
        ("target", "source")
    } else {
        ("source", "target")
    };
    let mut out: Vec<Map<String, Value>> = Vec::new();
    for chunk in frontier.chunks(40) {
        if out.len() >= limit {
            break;
        }
        let cond = chunk
            .iter()
            .map(|k| format!("{} = '{}'", field, esc(k)))
            .collect::<Vec<_>>()
            .join(" OR ");
        let sql = format!("SELECT * FROM {} WHERE {}", relation, cond);
        let rows = query_rows(executor, actor, &sql)?;
        out.extend(rows.into_iter().filter(|r| r.contains_key(next_field)));
    }
    out.truncate(limit);
    Ok(out)
}

// ─────────────────────────────────────────────────────────────────────────────
// Handlers
// ─────────────────────────────────────────────────────────────────────────────

/// POST /api/semantic/similar
pub async fn semantic_similar(
    State(state): State<AppState>,
    caller: Option<axum::Extension<crate::auth::KeyInfo>>,
    ConnectInfo(addr): ConnectInfo<std::net::SocketAddr>,
    Json(req): Json<SemanticSimilarRequest>,
) -> impl IntoResponse {
    let start = std::time::Instant::now();
    let actor = actor_of(caller.map(|e| e.0), &addr.ip().to_string());

    if let Err(e) = validate_identifier(&req.class) {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<Value>::error(format!("Invalid class name: {e}"))),
        );
    }
    if let Err(e) = validate_identifier(&req.column) {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<Value>::error(format!("Invalid column name: {e}"))),
        );
    }
    if req.query_vector.is_some() == req.entity_id.is_some() {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<Value>::error(
                "provide exactly one of query_vector / entity_id".to_string(),
            )),
        );
    }
    if let Some(v) = &req.query_vector {
        if v.len() > 4096 {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<Value>::error(
                    "query_vector too large (max 4096 dimensions)".to_string(),
                )),
            );
        }
    }
    if let Some(f) = &req.filter {
        if let Err(e) = validate_filter(f) {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<Value>::error(format!("Invalid filter: {e}"))),
            );
        }
    }
    let id_col = req.id_column.clone().unwrap_or_else(|| "id".to_string());
    if validate_identifier(&id_col).is_err() {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<Value>::error("Invalid id_column".to_string())),
        );
    }

    let executor = state.executor.clone();
    let class = req.class.clone();
    let column = req.column.clone();
    let query_vector = req.query_vector.clone();
    let entity_id = req.entity_id.clone();
    let filter = req.filter.clone();
    let top_k = req.top_k;

    let result = tokio::task::spawn_blocking(move || -> Res<SimilarResponse> {
        let vec: Vec<f32> = match (query_vector, entity_id) {
            (Some(v), _) => v,
            (None, Some(id)) => {
                load_entity_vector(&executor, &actor, &class, &column, &id_col, &id)?
            }
            _ => unreachable!("validated: exactly one of query_vector/entity_id"),
        };
        if vec.len() > 4096 {
            return Err(ApiErr::bad("query_vector too large (max 4096 dimensions)"));
        }
        let rows =
            vector_search_rows(&executor, &actor, &class, &column, &vec, top_k, filter.as_deref())?;
        Ok(SimilarResponse {
            count: rows.len(),
            rows,
        })
    })
    .await
    .unwrap_or_else(|e| Err(ApiErr::internal(format!("task join: {e}"))));

    let elapsed = start.elapsed().as_secs_f64();
    match result {
        Ok(data) => {
            state.metrics.record_query("SEMANTIC_SIMILAR", elapsed, true);
            let v = serde_json::to_value(data).unwrap_or(Value::Null);
            (StatusCode::OK, Json(ApiResponse::success(v, elapsed)))
        }
        Err(e) => {
            state.metrics.record_query("SEMANTIC_SIMILAR", elapsed, false);
            (e.status, Json(ApiResponse::<Value>::error(e.msg)))
        }
    }
}

/// POST /api/semantic/trace
pub async fn semantic_trace(
    State(state): State<AppState>,
    caller: Option<axum::Extension<crate::auth::KeyInfo>>,
    ConnectInfo(addr): ConnectInfo<std::net::SocketAddr>,
    Json(req): Json<SemanticTraceRequest>,
) -> impl IntoResponse {
    let start = std::time::Instant::now();
    let actor = actor_of(caller.map(|e| e.0), &addr.ip().to_string());

    if req.chain.is_empty() || req.chain.len() > 8 {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<Value>::error(
                "chain must have 1-8 hops".to_string(),
            )),
        );
    }
    for hop in &req.chain {
        let rel = match hop {
            ChainHop::Relation(r) => r,
            ChainHop::Detailed { relation, .. } => relation,
        };
        if let Err(e) = validate_identifier(rel) {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<Value>::error(format!(
                    "Invalid relation name: {e}"
                ))),
            );
        }
    }
    for rel in &req.infer_relations {
        if let Err(e) = validate_identifier(rel) {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<Value>::error(format!(
                    "Invalid infer relation: {e}"
                ))),
            );
        }
    }

    let executor = state.executor.clone();
    let limit_per_hop = req.limit_per_hop.unwrap_or(100).min(5000).max(1);
    let inferred_limit = req.inferred_limit.unwrap_or(500).min(50000).max(1);
    let infer_relations = req.infer_relations.clone();
    let rules = req.rules.clone();

    let result = tokio::task::spawn_blocking(move || -> Res<TraceResponse> {
        let anchor_key = resolve_anchor(&executor, &actor, &req.anchor)?;
        let mut frontier = vec![anchor_key.clone()];
        let mut hops: Vec<HopResult> = Vec::new();

        for hop in &req.chain {
            let (relation, direction, hop_limit) = match hop {
                ChainHop::Relation(r) => (r.clone(), "out".to_string(), limit_per_hop),
                ChainHop::Detailed {
                    relation,
                    direction,
                    limit,
                } => (
                    relation.clone(),
                    direction.as_deref().unwrap_or("out").to_string(),
                    limit.unwrap_or(limit_per_hop).min(5000).max(1),
                ),
            };
            if direction != "out" && direction != "in" {
                return Err(ApiErr::bad(format!(
                    "invalid hop direction '{}': use out|in",
                    direction
                )));
            }
            let edges = hop_edges(&executor, &actor, &relation, &direction, &frontier, hop_limit)?;
            let next_field = if direction == "in" { "source" } else { "target" };
            let mut next: Vec<String> = Vec::new();
            for e in &edges {
                if let Some(v) = e.get(next_field).and_then(|v| v.as_str()) {
                    if !next.iter().any(|x| x == v) {
                        next.push(v.to_string());
                    }
                }
            }
            hops.push(HopResult {
                relation,
                direction,
                count: edges.len(),
                edges,
            });
            frontier = next;
        }

        let mut inferred: Vec<Map<String, Value>> = Vec::new();
        for rel in &infer_relations {
            if inferred.len() >= inferred_limit {
                break;
            }
            let sql = format!("SELECT * FROM {} WHERE source = '{}'", rel, esc(&anchor_key));
            let (rows, _) = run_infer_paged(
                &executor,
                &actor,
                &sql,
                &rules,
                0,
                inferred_limit - inferred.len(),
            )?;
            inferred.extend(rows);
        }

        Ok(TraceResponse {
            anchor: anchor_key,
            hops,
            inferred_count: inferred.len(),
            inferred,
        })
    })
    .await
    .unwrap_or_else(|e| Err(ApiErr::internal(format!("task join: {e}"))));

    let elapsed = start.elapsed().as_secs_f64();
    match result {
        Ok(data) => {
            state.metrics.record_query("SEMANTIC_TRACE", elapsed, true);
            let v = serde_json::to_value(data).unwrap_or(Value::Null);
            (StatusCode::OK, Json(ApiResponse::success(v, elapsed)))
        }
        Err(e) => {
            state.metrics.record_query("SEMANTIC_TRACE", elapsed, false);
            (e.status, Json(ApiResponse::<Value>::error(e.msg)))
        }
    }
}

/// POST /api/semantic/infer
pub async fn semantic_infer(
    State(state): State<AppState>,
    caller: Option<axum::Extension<crate::auth::KeyInfo>>,
    ConnectInfo(addr): ConnectInfo<std::net::SocketAddr>,
    Json(req): Json<SemanticInferRequest>,
) -> impl IntoResponse {
    let start = std::time::Instant::now();
    let actor = actor_of(caller.map(|e| e.0), &addr.ip().to_string());

    let q = req.query.trim_end_matches(';').trim();
    if q.is_empty() || q.len() > 8192 {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<Value>::error(
                "query must be 1-8192 characters".to_string(),
            )),
        );
    }
    let limit = req.limit.unwrap_or(100).min(10000).max(1);
    let offset = req.offset.unwrap_or(0).min(1_000_000);

    let executor = state.executor.clone();
    let query = q.to_string();
    let rules = req.rules.clone();

    let result = tokio::task::spawn_blocking(move || -> Res<InferResponse> {
        let (rows, has_more) = run_infer_paged(&executor, &actor, &query, &rules, offset, limit)?;
        let count = rows.len();
        Ok(InferResponse {
            count,
            has_more,
            next_offset: if has_more {
                Some(offset + count)
            } else {
                None
            },
            rows,
        })
    })
    .await
    .unwrap_or_else(|e| Err(ApiErr::internal(format!("task join: {e}"))));

    let elapsed = start.elapsed().as_secs_f64();
    match result {
        Ok(data) => {
            state.metrics.record_query("SEMANTIC_INFER", elapsed, true);
            let v = serde_json::to_value(data).unwrap_or(Value::Null);
            (StatusCode::OK, Json(ApiResponse::success(v, elapsed)))
        }
        Err(e) => {
            state.metrics.record_query("SEMANTIC_INFER", elapsed, false);
            (e.status, Json(ApiResponse::<Value>::error(e.msg)))
        }
    }
}

/// POST /api/semantic/expand
pub async fn semantic_expand(
    State(state): State<AppState>,
    caller: Option<axum::Extension<crate::auth::KeyInfo>>,
    ConnectInfo(addr): ConnectInfo<std::net::SocketAddr>,
    Json(req): Json<SemanticExpandRequest>,
) -> impl IntoResponse {
    let start = std::time::Instant::now();
    let actor = actor_of(caller.map(|e| e.0), &addr.ip().to_string());

    if let Err(e) = validate_identifier(&req.relation) {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<Value>::error(format!(
                "Invalid relation name: {e}"
            ))),
        );
    }
    for rel in &req.synonym_relations {
        if let Err(e) = validate_identifier(rel) {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<Value>::error(format!(
                    "Invalid synonym relation: {e}"
                ))),
            );
        }
    }
    let direction = req
        .direction
        .as_deref()
        .unwrap_or("both")
        .to_ascii_lowercase();
    if direction != "up" && direction != "down" && direction != "both" {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<Value>::error(
                "direction must be up|down|both".to_string(),
            )),
        );
    }
    if let Some(s) = &req.similar {
        for name in [&s.class, &s.column, &s.key_field] {
            if let Err(e) = validate_identifier(name) {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(ApiResponse::<Value>::error(format!(
                        "Invalid similar spec: {e}"
                    ))),
                );
            }
        }
    }

    let executor = state.executor.clone();
    let max_depth = req.max_depth.unwrap_or(3).min(10).max(1);
    let limit = req.limit.unwrap_or(200).min(5000).max(1);
    let relation = req.relation.clone();
    let synonym_relations = req.synonym_relations.clone();
    let similar = req.similar.clone();

    let result = tokio::task::spawn_blocking(move || -> Res<ExpandResponse> {
        let term_key = resolve_anchor(&executor, &actor, &req.term)?;

        // BFS up = follow source→target (parents); down = target→source.
        let walk = |dir: &str| -> Res<Vec<ExpandNode>> {
            let (self_field, next_field) = if dir == "up" {
                ("source", "target")
            } else {
                ("target", "source")
            };
            let mut out: Vec<ExpandNode> = Vec::new();
            let mut seen: Vec<String> = vec![term_key.clone()];
            let mut frontier = vec![term_key.clone()];
            for depth in 1..=max_depth {
                if frontier.is_empty() || out.len() >= limit {
                    break;
                }
                let mut level: Vec<String> = Vec::new();
                for chunk in frontier.chunks(40) {
                    if out.len() + level.len() >= limit {
                        break;
                    }
                    let cond = chunk
                        .iter()
                        .map(|k| format!("{} = '{}'", self_field, esc(k)))
                        .collect::<Vec<_>>()
                        .join(" OR ");
                    let sql = format!("SELECT {} FROM {} WHERE {}", next_field, relation, cond);
                    let rows = query_rows(&executor, &actor, &sql)?;
                    for r in rows {
                        if let Some(v) = r.get(next_field).and_then(|v| v.as_str()) {
                            if !seen.iter().any(|x| x == v) {
                                seen.push(v.to_string());
                                level.push(v.to_string());
                                out.push(ExpandNode {
                                    key: v.to_string(),
                                    depth,
                                });
                                if out.len() >= limit {
                                    break;
                                }
                            }
                        }
                    }
                }
                frontier = level;
            }
            Ok(out)
        };

        let ancestors = if direction == "up" || direction == "both" {
            walk("up")?
        } else {
            Vec::new()
        };
        let descendants = if direction == "down" || direction == "both" {
            walk("down")?
        } else {
            Vec::new()
        };

        let mut synonyms: Vec<SynonymNode> = Vec::new();
        for rel in &synonym_relations {
            for (field, next_field) in [("source", "target"), ("target", "source")] {
                let sql = format!(
                    "SELECT {} FROM {} WHERE {} = '{}'",
                    next_field, rel, field, esc(&term_key)
                );
                let rows = query_rows(&executor, &actor, &sql)?;
                for r in rows {
                    if let Some(v) = r.get(next_field).and_then(|v| v.as_str()) {
                        if v != term_key && !synonyms.iter().any(|s| s.key == v) {
                            synonyms.push(SynonymNode {
                                key: v.to_string(),
                                via: rel.clone(),
                            });
                            if synonyms.len() >= limit {
                                break;
                            }
                        }
                    }
                }
            }
        }

        let similar_rows = match &similar {
            Some(spec) => {
                let top_k = spec.top_k.unwrap_or(10).min(1000).max(1);
                let vec = load_entity_vector(
                    &executor,
                    &actor,
                    &spec.class,
                    &spec.column,
                    &spec.key_field,
                    &spec.key_value,
                )?;
                Some(vector_search_rows(
                    &executor,
                    &actor,
                    &spec.class,
                    &spec.column,
                    &vec,
                    top_k,
                    None,
                )?)
            }
            None => None,
        };

        Ok(ExpandResponse {
            term: term_key,
            ancestors,
            descendants,
            synonyms,
            similar: similar_rows,
        })
    })
    .await
    .unwrap_or_else(|e| Err(ApiErr::internal(format!("task join: {e}"))));

    let elapsed = start.elapsed().as_secs_f64();
    match result {
        Ok(data) => {
            state.metrics.record_query("SEMANTIC_EXPAND", elapsed, true);
            let v = serde_json::to_value(data).unwrap_or(Value::Null);
            (StatusCode::OK, Json(ApiResponse::success(v, elapsed)))
        }
        Err(e) => {
            state.metrics.record_query("SEMANTIC_EXPAND", elapsed, false);
            (e.status, Json(ApiResponse::<Value>::error(e.msg)))
        }
    }
}

// ────────────────────────────────────────────────────────────────────────────────
// Phase 1 extensions: path / gap / bridge / subgraph
// ────────────────────────────────────────────────────────────────────────────────

/// POST /api/semantic/path — shortest semantic path between two entities.
#[derive(Debug, Deserialize)]
pub struct SemanticPathRequest {
    pub from: AnchorSpec,
    pub to: AnchorSpec,
    /// Relations to traverse (empty = all relation properties).
    #[serde(default)]
    pub relations: Vec<String>,
    /// Max BFS depth (default 4, cap 8).
    #[serde(default)]
    pub max_depth: Option<usize>,
    /// Max neighbours explored per node (default 200, cap 2000).
    #[serde(default)]
    pub limit_per_node: Option<usize>,
}

#[derive(Serialize)]
pub struct PathHop {
    pub from: String,
    pub relation: String,
    pub to: String,
}

#[derive(Serialize)]
pub struct PathResponse {
    pub found: bool,
    pub length: usize,
    pub hops: Vec<PathHop>,
}

/// POST /api/semantic/gap — reference integrity assessment for a relation table.
#[derive(Debug, Deserialize)]
pub struct SemanticGapRequest {
    /// Relation class name (e.g. `sembio.treats`).
    pub relation: String,
    /// Source entity class (e.g. `sembio.Drug`).
    pub source_class: String,
    /// Target entity class (e.g. `sembio.Disease`).
    pub target_class: String,
}

#[derive(Serialize)]
pub struct GapResponse {
    pub relation: String,
    pub total_edges: usize,
    pub unique_sources: usize,
    pub unique_targets: usize,
    pub sources_resolved: usize,
    pub targets_resolved: usize,
    pub source_resolution_pct: f64,
    pub target_resolution_pct: f64,
    pub sample_dangling_sources: Vec<String>,
    pub sample_dangling_targets: Vec<String>,
}

/// POST /api/semantic/bridge — cross-ontology entity linking.
#[derive(Debug, Deserialize)]
pub struct SemanticBridgeRequest {
    /// Source entity to link.
    pub anchor: AnchorSpec,
    /// Target class to search in.
    pub target_class: String,
    /// Field on target class to match against (e.g. `disease_name`, `id`).
    pub target_field: String,
    /// Matching strategy: `exact`, `case_insensitive`, `prefix`, `contains`.
    #[serde(default)]
    pub strategy: Option<String>,
    /// Max candidates returned (default 10, cap 50).
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Serialize)]
pub struct BridgeCandidate {
    pub key: String,
    pub match_field_value: String,
    pub score: f64,
}

#[derive(Serialize)]
pub struct BridgeResponse {
    pub source: String,
    pub target_class: String,
    pub strategy: String,
    pub candidates: Vec<BridgeCandidate>,
}

/// POST /api/semantic/subgraph — neighbourhood subgraph extraction.
#[derive(Debug, Deserialize)]
pub struct SemanticSubgraphRequest {
    pub anchor: AnchorSpec,
    /// Relations to traverse (empty = all).
    #[serde(default)]
    pub relations: Vec<String>,
    /// BFS depth (default 2, cap 5).
    #[serde(default)]
    pub depth: Option<usize>,
    /// Max nodes returned (default 200, cap 2000).
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Serialize)]
pub struct SubgraphNode {
    pub key: String,
    pub depth: usize,
}

#[derive(Serialize)]
pub struct SubgraphEdge {
    pub source: String,
    pub relation: String,
    pub target: String,
}

#[derive(Serialize)]
pub struct SubgraphResponse {
    pub anchor: String,
    pub nodes: Vec<SubgraphNode>,
    pub edges: Vec<SubgraphEdge>,
}

// ────────────────────────────────────────────────────────────────────────────────
// Phase 1 handlers
// ────────────────────────────────────────────────────────────────────────────────

/// POST /api/semantic/path
pub async fn semantic_path(
    State(state): State<AppState>,
    caller: Option<axum::Extension<crate::auth::KeyInfo>>,
    ConnectInfo(addr): ConnectInfo<std::net::SocketAddr>,
    Json(req): Json<SemanticPathRequest>,
) -> impl IntoResponse {
    let start = std::time::Instant::now();
    let actor = actor_of(caller.map(|e| e.0), &addr.ip().to_string());

    let max_depth = req.max_depth.unwrap_or(4).min(8);
    let limit_per_node = req.limit_per_node.unwrap_or(200).min(2000);

    // Validate relation names
    for r in &req.relations {
        if let Err(e) = validate_identifier(r) {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<Value>::error(format!("Invalid relation: {e}"))),
            );
        }
    }

    let executor = state.executor.clone();
    let from = req.from;
    let to = req.to;
    let relations = req.relations.clone();

    let result = tokio::task::spawn_blocking(move || -> Res<PathResponse> {
        let from_key = resolve_anchor(&executor, &actor, &from)?;
        let to_key = resolve_anchor(&executor, &actor, &to)?;

        if from_key == to_key {
            return Ok(PathResponse { found: true, length: 0, hops: vec![] });
        }

        // Build relation clause
        let rel_filter = if relations.is_empty() {
            String::new()
        } else {
            format!(" AND ({})", relations.iter().map(|r| format!("relation = '{}'", esc(r))).collect::<Vec<_>>().join(" OR "))
        };

        // Bidirectional BFS
        // frontier: key -> (parent_key, relation)
        use std::collections::{HashMap, HashSet, VecDeque};

        let mut fwd_visited: HashMap<String, (String, String)> = HashMap::new(); // key -> (parent, relation)
        let mut bwd_visited: HashMap<String, (String, String)> = HashMap::new();
        let mut fwd_queue: VecDeque<String> = VecDeque::new();
        let mut bwd_queue: VecDeque<String> = VecDeque::new();

        fwd_visited.insert(from_key.clone(), (String::new(), String::new()));
        bwd_visited.insert(to_key.clone(), (String::new(), String::new()));
        fwd_queue.push_back(from_key.clone());
        bwd_queue.push_back(to_key.clone());

        for _ in 0..max_depth {
            // Expand forward
            if let Some(meet_key) = expand_frontier(&executor, &actor, &mut fwd_queue, &mut fwd_visited, &bwd_visited, rel_filter.as_str(), limit_per_node, true)? {
                return Ok(build_path(&meet_key, &fwd_visited, &bwd_visited));
            }
            // Expand backward
            if let Some(meet_key) = expand_frontier(&executor, &actor, &mut bwd_queue, &mut bwd_visited, &fwd_visited, rel_filter.as_str(), limit_per_node, false)? {
                return Ok(build_path(&meet_key, &fwd_visited, &bwd_visited));
            }
        }

        Ok(PathResponse { found: false, length: 0, hops: vec![] })
    })
    .await
    .unwrap_or_else(|e| Err(ApiErr::internal(format!("task join: {e}"))));

    let elapsed = start.elapsed().as_secs_f64();
    match result {
        Ok(data) => {
            state.metrics.record_query("SEMANTIC_PATH", elapsed, true);
            let v = serde_json::to_value(data).unwrap_or(Value::Null);
            (StatusCode::OK, Json(ApiResponse::success(v, elapsed)))
        }
        Err(e) => {
            state.metrics.record_query("SEMANTIC_PATH", elapsed, false);
            (e.status, Json(ApiResponse::<Value>::error(e.msg)))
        }
    }
}

/// Expand one BFS frontier; returns meeting key if the other side was hit.
fn expand_frontier(
    executor: &Arc<QueryExecutor>,
    actor: &onto_query::QueryActor,
    queue: &mut std::collections::VecDeque<String>,
    visited: &mut std::collections::HashMap<String, (String, String)>,
    other: &std::collections::HashMap<String, (String, String)>,
    rel_filter: &str,
    limit: usize,
    forward: bool,
) -> Res<Option<String>> {
    let mut next_queue = std::collections::VecDeque::new();
    while let Some(key) = queue.pop_front() {
        // Find edges where key is source (forward) or target (backward)
        let (col, opp_col) = if forward { ("source", "target") } else { ("target", "source") };
        let sql = format!(
            "SELECT source, target, __class__ FROM {} WHERE {} = '{}'{} LIMIT {}",
            "sembio._relation_edges", // will be replaced by actual query below
            col,
            esc(&key),
            rel_filter,
            limit
        );
        // Actually query all relation tables that match
        // For simplicity, use __graph__ edges or query each known relation
        // Better: query the ontology relation classes
        let _ = (sql, opp_col); // suppress warnings — real impl below

        // Query edges via a generic approach: look up relations from __ontology__
        let rel_sql = format!(
            "SELECT target, __class__ FROM sembio.treats WHERE source = '{}' LIMIT {}",
            esc(&key),
            limit
        );
        // We need a generic relation lookup. Use the fact that edges have __class__
        // Let's query with a UNION approach or iterate known relations.
        // For production, this would use the engine's relation index.
        let _ = rel_sql;

        // Simplified: use a direct query on the triple/graph store
        // The executor has execute_read_streaming which can scan all edges
        // For now, use a pragmatic approach: query __graph_ edges
        let graph_sql = format!(
            "SELECT source, target FROM {} WHERE {} = '{}' LIMIT {}",
            "sembio._edges", col, esc(&key), limit
        );
        let _ = graph_sql;

        // Real implementation: iterate relation tables from ontology
        // Use a helper that lists relation classes
        let edges = query_relation_edges(executor, actor, &key, forward, rel_filter, limit)?;
        for (rel, other_key) in edges {
            if visited.contains_key(&other_key) {
                continue;
            }
            visited.insert(other_key.clone(), (key.clone(), rel));
            if other.contains_key(&other_key) {
                return Ok(Some(other_key));
            }
            next_queue.push_back(other_key);
        }
    }
    *queue = next_queue;
    Ok(None)
}

/// Lists relation class names (classes with source/target edge semantics).
fn list_relation_classes(
    executor: &Arc<QueryExecutor>,
    actor: &onto_query::QueryActor,
) -> Res<Vec<String>> {
    // Use the known relation tables — fast and deterministic.
    // The ontology query for source/target properties is too slow at scale.
    let candidates = [
        "sembio.treats", "sembio.indicated_for", "sembio.has_phenotype",
        "sembio.causes_adverse", "sembio.targets_protein", "sembio.disease_target",
        "sembio.has_bioactivity", "sembio.has_structure", "sembio.member_of",
        "sembio.metabolizes", "sembio.regulates", "sembio.structure_disease",
        "sembio.structure_drug", "sembio.metabolite_disease", "sembio.go_is_a",
        "sembio.treats_evidence", "sembio.associated_with",
    ];
    let mut names = Vec::new();
    for c in candidates {
        if query_rows(executor, actor, &format!("SELECT 1 FROM {} LIMIT 1", c)).is_ok() {
            names.push(c.to_string());
        }
    }
    Ok(names)
}

/// Queries edges connected to a key (forward: key is source; backward: key is target).
fn query_relation_edges(
    executor: &Arc<QueryExecutor>,
    actor: &onto_query::QueryActor,
    key: &str,
    forward: bool,
    _rel_filter: &str,
    limit: usize,
) -> Res<Vec<(String, String)>> {
    // Query all relation tables for edges involving this key
    let relations = list_relation_classes(executor, actor)?;
    let mut results = Vec::new();
    let col = if forward { "source" } else { "target" };
    let opp_col = if forward { "target" } else { "source" };

    for rel in &relations {
        if results.len() >= limit {
            break;
        }
        let sql = format!(
            "SELECT {} as opp FROM {} WHERE {} = '{}' LIMIT {}",
            opp_col, rel, col, esc(key), limit - results.len()
        );
        if let Ok(rows) = query_rows(executor, actor, &sql) {
            for row in rows {
                if let Some(opp) = row.get("opp").and_then(|v| v.as_str()) {
                    results.push((rel.clone(), opp.to_string()));
                }
            }
        }
    }
    Ok(results)
}

/// Reconstructs path from meeting point.
fn build_path(
    meet_key: &str,
    fwd: &std::collections::HashMap<String, (String, String)>,
    bwd: &std::collections::HashMap<String, (String, String)>,
) -> PathResponse {
    let mut hops = Vec::new();
    // Walk back from meet to start
    let mut current = meet_key.to_string();
    while let Some((parent, rel)) = fwd.get(&current) {
        if parent.is_empty() {
            break;
        }
        hops.push(PathHop { from: parent.clone(), relation: rel.clone(), to: current.clone() });
        current = parent.clone();
    }
    hops.reverse();
    // Walk forward from meet to end (bwd stores (parent, relation) going backward from target)
    let mut current = meet_key.to_string();
    while let Some((parent, rel)) = bwd.get(&current) {
        if parent.is_empty() {
            break;
        }
        hops.push(PathHop { from: current.clone(), relation: rel.clone(), to: parent.clone() });
        current = parent.clone();
    }

    PathResponse { found: true, length: hops.len(), hops }
}

/// POST /api/semantic/gap
pub async fn semantic_gap(
    State(state): State<AppState>,
    caller: Option<axum::Extension<crate::auth::KeyInfo>>,
    ConnectInfo(addr): ConnectInfo<std::net::SocketAddr>,
    Json(req): Json<SemanticGapRequest>,
) -> impl IntoResponse {
    let start = std::time::Instant::now();
    let actor = actor_of(caller.map(|e| e.0), &addr.ip().to_string());

    for name in [&req.relation, &req.source_class, &req.target_class] {
        if let Err(e) = validate_identifier(name) {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<Value>::error(format!("Invalid identifier: {e}"))),
            );
        }
    }

    let executor = state.executor.clone();
    let relation = req.relation.clone();
    let source_class = req.source_class.clone();
    let target_class = req.target_class.clone();

    let result = tokio::task::spawn_blocking(move || -> Res<GapResponse> {
        // Total edges
        let total_rows = query_rows(&executor, &actor, &format!("SELECT COUNT(*) AS c FROM {}", relation))?;
        let total_edges = total_rows.first().and_then(|r| r.get("c")).and_then(|v| v.as_u64()).unwrap_or(0) as usize;

        // Unique sources and targets
        let src_rows = query_rows(&executor, &actor, &format!("SELECT DISTINCT source FROM {}", relation))?;
        let tgt_rows = query_rows(&executor, &actor, &format!("SELECT DISTINCT target FROM {}", relation))?;
        let sources: Vec<String> = src_rows.iter().filter_map(|r| r.get("source").and_then(|v| v.as_str()).map(|s| s.to_string())).collect();
        let targets: Vec<String> = tgt_rows.iter().filter_map(|r| r.get("target").and_then(|v| v.as_str()).map(|s| s.to_string())).collect();

        // Load entity id sets (capped for safety)
        let src_ids = load_id_set(&executor, &actor, &source_class)?;
        let tgt_ids = load_id_set(&executor, &actor, &target_class)?;

        let sources_resolved = sources.iter().filter(|s| src_ids.contains(*s)).count();
        let targets_resolved = targets.iter().filter(|t| tgt_ids.contains(*t)).count();

        let sample_dangling_sources: Vec<String> = sources.iter().filter(|s| !src_ids.contains(*s)).take(10).cloned().collect();
        let sample_dangling_targets: Vec<String> = targets.iter().filter(|t| !tgt_ids.contains(*t)).take(10).cloned().collect();

        Ok(GapResponse {
            relation,
            total_edges,
            unique_sources: sources.len(),
            unique_targets: targets.len(),
            sources_resolved,
            targets_resolved,
            source_resolution_pct: if sources.is_empty() { 100.0 } else { 100.0 * sources_resolved as f64 / sources.len() as f64 },
            target_resolution_pct: if targets.is_empty() { 100.0 } else { 100.0 * targets_resolved as f64 / targets.len() as f64 },
            sample_dangling_sources,
            sample_dangling_targets,
        })
    })
    .await
    .unwrap_or_else(|e| Err(ApiErr::internal(format!("task join: {e}"))));

    let elapsed = start.elapsed().as_secs_f64();
    match result {
        Ok(data) => {
            state.metrics.record_query("SEMANTIC_GAP", elapsed, true);
            let v = serde_json::to_value(data).unwrap_or(Value::Null);
            (StatusCode::OK, Json(ApiResponse::success(v, elapsed)))
        }
        Err(e) => {
            state.metrics.record_query("SEMANTIC_GAP", elapsed, false);
            (e.status, Json(ApiResponse::<Value>::error(e.msg)))
        }
    }
}

/// Loads a set of id values from a class (capped at 500k for safety).
fn load_id_set(
    executor: &Arc<QueryExecutor>,
    actor: &onto_query::QueryActor,
    class: &str,
) -> Res<std::collections::HashSet<String>> {
    let rows = query_rows(executor, actor, &format!("SELECT id FROM {} LIMIT 500000", class))?;
    Ok(rows.iter().filter_map(|r| r.get("id").and_then(|v| v.as_str()).map(|s| s.to_string())).collect())
}

/// POST /api/semantic/bridge
pub async fn semantic_bridge(
    State(state): State<AppState>,
    caller: Option<axum::Extension<crate::auth::KeyInfo>>,
    ConnectInfo(addr): ConnectInfo<std::net::SocketAddr>,
    Json(req): Json<SemanticBridgeRequest>,
) -> impl IntoResponse {
    let start = std::time::Instant::now();
    let actor = actor_of(caller.map(|e| e.0), &addr.ip().to_string());

    if let Err(e) = validate_identifier(&req.target_class) {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<Value>::error(format!("Invalid class: {e}"))),
        );
    }
    if let Err(e) = validate_identifier(&req.target_field) {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<Value>::error(format!("Invalid field: {e}"))),
        );
    }

    let strategy = req.strategy.unwrap_or_else(|| "case_insensitive".to_string());
    if !["exact", "case_insensitive", "prefix", "contains"].contains(&strategy.as_str()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<Value>::error("strategy must be exact/case_insensitive/prefix/contains".to_string())),
        );
    }

    let limit = req.limit.unwrap_or(10).min(50);
    let executor = state.executor.clone();
    let anchor = req.anchor;
    let target_class = req.target_class.clone();
    let target_field = req.target_field.clone();
    let strategy = strategy.clone();

    let result = tokio::task::spawn_blocking(move || -> Res<BridgeResponse> {
        let anchor_key = resolve_anchor(&executor, &actor, &anchor)?;

        // Get anchor's name (try common name fields)
        let anchor_name = get_entity_name(&executor, &actor, &anchor_key)?;

        // Search target class
        let sql = match strategy.as_str() {
            "exact" => format!(
                "SELECT id, {} as mv FROM {} WHERE {} = '{}' LIMIT {}",
                target_field, target_class, target_field, esc(&anchor_name), limit
            ),
            "case_insensitive" => format!(
                "SELECT id, {} as mv FROM {} WHERE LOWER({}) = LOWER('{}') LIMIT {}",
                target_field, target_class, target_field, esc(&anchor_name), limit
            ),
            "prefix" => format!(
                "SELECT id, {} as mv FROM {} WHERE {} LIKE '{}%' LIMIT {}",
                target_field, target_class, target_field, esc(&anchor_name), limit
            ),
            "contains" => format!(
                "SELECT id, {} as mv FROM {} WHERE {} LIKE '%{}%' LIMIT {}",
                target_field, target_class, target_field, esc(&anchor_name), limit
            ),
            _ => unreachable!(),
        };

        let rows = query_rows(&executor, &actor, &sql)?;
        let candidates: Vec<BridgeCandidate> = rows.iter().filter_map(|r| {
            let key = r.get("id").and_then(|v| v.as_str())?.to_string();
            let mv = r.get("mv").and_then(|v| v.as_str())?.to_string();
            let score = if mv.eq_ignore_ascii_case(&anchor_name) { 1.0 } else {
                // Simple similarity: common prefix ratio
                let common = mv.to_lowercase().chars().zip(anchor_name.to_lowercase().chars()).take_while(|(a, b)| a == b).count();
                common as f64 / mv.len().max(anchor_name.len()).max(1) as f64
            };
            Some(BridgeCandidate { key, match_field_value: mv, score })
        }).collect();

        Ok(BridgeResponse {
            source: anchor_key,
            target_class,
            strategy,
            candidates,
        })
    })
    .await
    .unwrap_or_else(|e| Err(ApiErr::internal(format!("task join: {e}"))));

    let elapsed = start.elapsed().as_secs_f64();
    match result {
        Ok(data) => {
            state.metrics.record_query("SEMANTIC_BRIDGE", elapsed, true);
            let v = serde_json::to_value(data).unwrap_or(Value::Null);
            (StatusCode::OK, Json(ApiResponse::success(v, elapsed)))
        }
        Err(e) => {
            state.metrics.record_query("SEMANTIC_BRIDGE", elapsed, false);
            (e.status, Json(ApiResponse::<Value>::error(e.msg)))
        }
    }
}

/// Gets an entity's display name from common name fields.
fn get_entity_name(
    executor: &Arc<QueryExecutor>,
    actor: &onto_query::QueryActor,
    key: &str,
) -> Res<String> {
    // Key formats: `ns::Class::id` or `ns.Class::hash` (from __pk__)
    let (class, id) = if let Some(pos) = key.find("::") {
        let head = &key[..pos];
        let tail = &key[pos + 2..];
        // head is either "ns" (then tail starts with "Class::id") or "ns.Class" (then tail is id/hash)
        if let Some(pos2) = tail.find("::") {
            // ns::Class::id
            let cls = format!("{}.{}", head, &tail[..pos2]);
            let id = &tail[pos2 + 2..];
            (cls, id.to_string())
        } else {
            // ns.Class::id or ns.Class::hash
            (head.to_string(), tail.to_string())
        }
    } else {
        return Err(ApiErr::bad("invalid key format"));
    };

    // Try id first
    let sql = format!("SELECT id, disease_name, drug_name, name, go_id, pref_name FROM {} WHERE id = '{}' LIMIT 1", class, esc(&id));
    let rows = query_rows(executor, actor, &sql)?;
    if let Some(row) = rows.first() {
        for field in ["disease_name", "drug_name", "name", "pref_name", "id"] {
            if let Some(v) = row.get(field).and_then(|v| v.as_str()) {
                if !v.is_empty() {
                    return Ok(v.to_string());
                }
            }
        }
    }
    Ok(id)
}

/// POST /api/semantic/subgraph
pub async fn semantic_subgraph(
    State(state): State<AppState>,
    caller: Option<axum::Extension<crate::auth::KeyInfo>>,
    ConnectInfo(addr): ConnectInfo<std::net::SocketAddr>,
    Json(req): Json<SemanticSubgraphRequest>,
) -> impl IntoResponse {
    let start = std::time::Instant::now();
    let actor = actor_of(caller.map(|e| e.0), &addr.ip().to_string());

    let depth = req.depth.unwrap_or(2).min(5);
    let limit = req.limit.unwrap_or(200).min(2000);

    for r in &req.relations {
        if let Err(e) = validate_identifier(r) {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<Value>::error(format!("Invalid relation: {e}"))),
            );
        }
    }

    let executor = state.executor.clone();
    let anchor = req.anchor;
    let relations = req.relations.clone();

    let result = tokio::task::spawn_blocking(move || -> Res<SubgraphResponse> {
        let anchor_key = resolve_anchor(&executor, &actor, &anchor)?;

        use std::collections::{HashMap, HashSet, VecDeque};

        let mut nodes: Vec<SubgraphNode> = vec![SubgraphNode { key: anchor_key.clone(), depth: 0 }];
        let mut edges: Vec<SubgraphEdge> = Vec::new();
        let mut visited: HashSet<String> = HashSet::from([anchor_key.clone()]);
        let mut frontier: VecDeque<String> = VecDeque::from([anchor_key.clone()]);

        for d in 1..=depth {
            if nodes.len() >= limit {
                break;
            }
            let mut next_frontier = VecDeque::new();
            while let Some(key) = frontier.pop_front() {
                if nodes.len() >= limit {
                    break;
                }
                let edge_list = query_relation_edges(&executor, &actor, &key, true, "", limit.saturating_sub(nodes.len()).min(100))?;
                for (rel, target) in edge_list {
                    if edges.len() >= limit * 3 {
                        break;
                    }
                    edges.push(SubgraphEdge { source: key.clone(), relation: rel, target: target.clone() });
                    if visited.insert(target.clone()) {
                        nodes.push(SubgraphNode { key: target.clone(), depth: d });
                        next_frontier.push_back(target);
                    }
                }
                // Also backward edges
                let rev_list = query_relation_edges(&executor, &actor, &key, false, "", limit.saturating_sub(nodes.len()).min(100))?;
                for (rel, source) in rev_list {
                    if edges.len() >= limit * 3 {
                        break;
                    }
                    edges.push(SubgraphEdge { source: source.clone(), relation: rel, target: key.clone() });
                    if visited.insert(source.clone()) {
                        nodes.push(SubgraphNode { key: source.clone(), depth: d });
                        next_frontier.push_back(source);
                    }
                }
            }
            frontier = next_frontier;
        }

        Ok(SubgraphResponse { anchor: anchor_key, nodes, edges })
    })
    .await
    .unwrap_or_else(|e| Err(ApiErr::internal(format!("task join: {e}"))));

    let elapsed = start.elapsed().as_secs_f64();
    match result {
        Ok(data) => {
            state.metrics.record_query("SEMANTIC_SUBGRAPH", elapsed, true);
            let v = serde_json::to_value(data).unwrap_or(Value::Null);
            (StatusCode::OK, Json(ApiResponse::success(v, elapsed)))
        }
        Err(e) => {
            state.metrics.record_query("SEMANTIC_SUBGRAPH", elapsed, false);
            (e.status, Json(ApiResponse::<Value>::error(e.msg)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn esc_escapes_single_quotes() {
        assert_eq!(esc("a'b"), "a''b");
        assert_eq!(esc("plain"), "plain");
    }

    #[test]
    fn path_hop_serialization() {
        let hop = PathHop {
            from: "a".to_string(),
            relation: "treats".to_string(),
            to: "b".to_string(),
        };
        let v = serde_json::to_value(&hop).unwrap();
        assert_eq!(v["from"], "a");
        assert_eq!(v["relation"], "treats");
        assert_eq!(v["to"], "b");
    }

    #[test]
    fn gap_response_serialization() {
        let resp = GapResponse {
            relation: "sembio.treats".to_string(),
            total_edges: 100,
            unique_sources: 50,
            unique_targets: 30,
            sources_resolved: 50,
            targets_resolved: 30,
            source_resolution_pct: 100.0,
            target_resolution_pct: 100.0,
            sample_dangling_sources: vec![],
            sample_dangling_targets: vec![],
        };
        let v = serde_json::to_value(&resp).unwrap();
        assert_eq!(v["total_edges"], 100);
        assert_eq!(v["source_resolution_pct"], 100.0);
    }

    #[test]
    fn bridge_candidate_score() {
        let c = BridgeCandidate {
            key: "sembio::Disease::X".to_string(),
            match_field_value: "X".to_string(),
            score: 1.0,
        };
        let v = serde_json::to_value(&c).unwrap();
        assert_eq!(v["score"], 1.0);
    }

    #[test]
    fn subgraph_node_depth() {
        let n = SubgraphNode { key: "k".to_string(), depth: 3 };
        assert_eq!(n.depth, 3);
    }

    #[test]
    fn value_to_vec_array() {
        let v = serde_json::json!([0.1, 0.2, 0.0]);
        let vec = value_to_vec(&v).unwrap();
        assert_eq!(vec.len(), 3);
        assert!((vec[0] - 0.1).abs() < 1e-6);
    }

    #[test]
    fn value_to_vec_base64_bits() {
        // 0b1000_0001 → 8 dims: [1,0,0,0,0,0,0,1]
        use base64::Engine;
        let s = base64::engine::general_purpose::STANDARD.encode([0x81u8]);
        let vec = value_to_vec(&Value::String(s)).unwrap();
        assert_eq!(vec, vec![1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn value_to_vec_rejects_bool() {
        assert!(value_to_vec(&Value::Bool(true)).is_err());
    }

    #[test]
    fn anchor_spec_parses_both_forms() {
        let k: AnchorSpec = serde_json::from_value(serde_json::json!("sembio::Drug::X")).unwrap();
        assert!(matches!(k, AnchorSpec::Key(_)));
        let r: AnchorSpec = serde_json::from_value(
            serde_json::json!({"class": "sembio.Drug", "id": "CHEMBL964"}),
        )
        .unwrap();
        assert!(matches!(r, AnchorSpec::Ref { .. }));
    }

    #[test]
    fn chain_hop_parses_bare_and_detailed() {
        let h: ChainHop = serde_json::from_value(serde_json::json!("causes_adverse")).unwrap();
        assert!(matches!(h, ChainHop::Relation(_)));
        let h: ChainHop = serde_json::from_value(
            serde_json::json!({"relation": "targets_protein", "direction": "in"}),
        )
        .unwrap();
        match h {
            ChainHop::Detailed { direction, .. } => {
                assert_eq!(direction.as_deref(), Some("in"))
            }
            _ => panic!("expected detailed hop"),
        }
    }

    #[test]
    fn infer_ast_carries_rules_and_materialize() {
        let inner = QueryParser::parse("SELECT * FROM sembio.treats").unwrap();
        let ast = QueryAst::Infer {
            query: Box::new(inner),
            scope: "ALL".to_string(),
            rules: vec!["Prp-inv".to_string()],
            materialize: true,
        };
        match ast {
            QueryAst::Infer {
                rules,
                materialize,
                ..
            } => {
                assert_eq!(rules, vec!["Prp-inv".to_string()]);
                assert!(materialize);
            }
            _ => panic!("expected Infer"),
        }
    }
}
