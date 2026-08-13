//! Query executor: runs parsed queries against the storage and ontology engines.

use crate::cache::{PlanCache, QueryCache};
use crate::optimizer::QueryPlanner;
use crate::parser::{AggregateFunc, ArithmeticOp, FilterExpr, LiteralValue, QueryAst, SelectColumns, SelectItem, ValueExpr, WindowExpr, WindowFunc};
use crate::optimizer::{ExecutionPlan, PlanNode};
use onto_core::{CoreError, Result};
use onto_core::binary_row::BinaryRow;
use onto_ontology::{DataType, OntologyStore, Reasoner};
use onto_sharding::{ShardRouter, ShardManager, ShardMap};
use onto_storage::LsmEngine;
use serde_json::{json, Map, Value};

/// Parse a storage row from bytes. Tries binary format first (fast), then falls back to JSON.
/// Returns None if parsing fails or result is not a JSON object.
#[inline]
fn simd_parse_row(bytes: &[u8]) -> Option<Map<String, Value>> {
    // Try binary format first (P2 optimization)
    if let Some(row) = BinaryRow::parse(bytes) {
        return row.to_map();
    }
    // Fall back to simd-json
    let mut buf = bytes.to_vec();
    match simd_json::to_owned_value(&mut buf) {
        Ok(val) => owned_value_to_serde(val),
        Err(_) => None,
    }
}

/// Convert simd_json OwnedValue to serde_json Value.
fn owned_value_to_serde(val: simd_json::OwnedValue) -> Option<Map<String, Value>> {
    use simd_json::OwnedValue as SVal;
    match val {
        SVal::Object(map) => {
            let mut out = Map::with_capacity(map.len());
            for (k, v) in map.into_iter() {
                out.insert(k, simd_val_to_serde(v));
            }
            Some(out)
        }
        _ => None,
    }
}

fn simd_val_to_serde(val: simd_json::OwnedValue) -> Value {
    use simd_json::OwnedValue as SVal;
    match val {
        SVal::Static(s) => match s {
            simd_json::StaticNode::Null => Value::Null,
            simd_json::StaticNode::Bool(b) => Value::Bool(b),
            simd_json::StaticNode::I64(n) => serde_json::json!(n),
            simd_json::StaticNode::U64(n) => serde_json::json!(n),
            simd_json::StaticNode::F64(n) => serde_json::json!(n),
        },
        SVal::String(s) => Value::String(s),
        SVal::Array(arr) => Value::Array(arr.into_iter().map(simd_val_to_serde).collect()),
        SVal::Object(map) => {
            let mut out = Map::with_capacity(map.len());
            for (k, v) in map.into_iter() {
                out.insert(k, simd_val_to_serde(v));
            }
            Value::Object(out)
        }
    }
}

/// Evaluate a FilterExpr directly on a BinaryRow without creating a Map.
/// Returns Some(true/false) if the filter could be fully evaluated,
/// or None if it requires full deserialization (e.g., Like, subqueries).
fn eval_binary_filter(row: &BinaryRow, expr: &FilterExpr) -> Option<bool> {
    match expr {
        FilterExpr::Eq(col, lit) => {
            let idx = row.find_field(col)?;
            let (tag, raw) = row.field_value_raw(idx);
            Some(binary_lit_eq(tag, raw, lit))
        }
        FilterExpr::Ne(col, lit) => {
            let idx = row.find_field(col)?;
            let (tag, raw) = row.field_value_raw(idx);
            Some(!binary_lit_eq(tag, raw, lit))
        }
        FilterExpr::Gt(col, lit) => {
            let idx = row.find_field(col)?;
            let (tag, raw) = row.field_value_raw(idx);
            binary_lit_ord(tag, raw, lit).map(|ord| ord == std::cmp::Ordering::Greater)
        }
        FilterExpr::Lt(col, lit) => {
            let idx = row.find_field(col)?;
            let (tag, raw) = row.field_value_raw(idx);
            binary_lit_ord(tag, raw, lit).map(|ord| ord == std::cmp::Ordering::Less)
        }
        FilterExpr::Gte(col, lit) => {
            let idx = row.find_field(col)?;
            let (tag, raw) = row.field_value_raw(idx);
            binary_lit_ord(tag, raw, lit).map(|ord| ord != std::cmp::Ordering::Less)
        }
        FilterExpr::Lte(col, lit) => {
            let idx = row.find_field(col)?;
            let (tag, raw) = row.field_value_raw(idx);
            binary_lit_ord(tag, raw, lit).map(|ord| ord != std::cmp::Ordering::Greater)
        }
        FilterExpr::IsNull(col) => {
            let idx = row.find_field(col)?;
            Some(row.field_type(idx) == onto_core::binary_row::TAG_NULL)
        }
        FilterExpr::IsNotNull(col) => {
            let idx = row.find_field(col)?;
            Some(row.field_type(idx) != onto_core::binary_row::TAG_NULL)
        }
        FilterExpr::In(col, values) => {
            let idx = row.find_field(col)?;
            let (tag, raw) = row.field_value_raw(idx);
            Some(values.iter().any(|lit| binary_lit_eq(tag, raw, lit)))
        }
        FilterExpr::Between(col, low, high) => {
            let idx = row.find_field(col)?;
            let (tag, raw) = row.field_value_raw(idx);
            let ge_low = binary_lit_ord(tag, raw, low)
                .map(|ord| ord != std::cmp::Ordering::Less)
                .unwrap_or(false);
            let le_high = binary_lit_ord(tag, raw, high)
                .map(|ord| ord != std::cmp::Ordering::Greater)
                .unwrap_or(false);
            Some(ge_low && le_high)
        }
        FilterExpr::And(l, r) => {
            let lv = eval_binary_filter(row, l)?;
            if !lv { return Some(false); }
            eval_binary_filter(row, r)
        }
        FilterExpr::Or(l, r) => {
            let lv = eval_binary_filter(row, l)?;
            if lv { return Some(true); }
            eval_binary_filter(row, r)
        }
        FilterExpr::Not(e) => {
            let v = eval_binary_filter(row, e)?;
            Some(!v)
        }
        // Like requires pattern matching on strings — extract and delegate
        FilterExpr::Like(col, pattern) => {
            let s = row.get_str(col)?;
            Some(QueryExecutor::like_match(s, pattern))
        }
        // Subqueries need engine access — can't evaluate here
        FilterExpr::InSubquery(..) | FilterExpr::Exists(..) | FilterExpr::NotExists(..) => None,
    }
}

/// Compare binary field value with a LiteralValue for equality.
fn binary_lit_eq(tag: u8, raw: &[u8], lit: &LiteralValue) -> bool {
    use onto_core::binary_row::{TAG_NULL, TAG_BOOL, TAG_INT, TAG_FLOAT, TAG_STRING, parse_string_value};
    match (tag, lit) {
        (TAG_NULL, LiteralValue::Null) => true,
        (TAG_NULL, _) => false,
        (_, LiteralValue::Null) => false,
        (TAG_BOOL, LiteralValue::Bool(b)) => raw.first().is_some_and(|v| (*v != 0) == *b),
        (TAG_INT, LiteralValue::Int(n)) => {
            if let Ok(arr) = <[u8; 8]>::try_from(raw) {
                i64::from_be_bytes(arr) == *n
            } else { false }
        }
        (TAG_INT, LiteralValue::Float(n)) => {
            if let Ok(arr) = <[u8; 8]>::try_from(raw) {
                (i64::from_be_bytes(arr) as f64) == *n
            } else { false }
        }
        (TAG_FLOAT, LiteralValue::Float(n)) => {
            if let Ok(arr) = <[u8; 8]>::try_from(raw) {
                f64::from_be_bytes(arr) == *n
            } else { false }
        }
        (TAG_FLOAT, LiteralValue::Int(n)) => {
            if let Ok(arr) = <[u8; 8]>::try_from(raw) {
                f64::from_be_bytes(arr) == (*n as f64)
            } else { false }
        }
        (TAG_STRING, LiteralValue::String(s)) => {
            parse_string_value(raw) == Some(s.as_str())
        }
        _ => false,
    }
}

/// Compare binary field value with a LiteralValue for ordering.
fn binary_lit_ord(tag: u8, raw: &[u8], lit: &LiteralValue) -> Option<std::cmp::Ordering> {
    use onto_core::binary_row::{TAG_INT, TAG_FLOAT, TAG_STRING, parse_string_value};
    match (tag, lit) {
        (TAG_INT, LiteralValue::Int(n)) => {
            let arr: [u8; 8] = raw.try_into().ok()?;
            Some(i64::from_be_bytes(arr).cmp(n))
        }
        (TAG_INT, LiteralValue::Float(n)) => {
            let arr: [u8; 8] = raw.try_into().ok()?;
            (i64::from_be_bytes(arr) as f64).partial_cmp(n)
        }
        (TAG_FLOAT, LiteralValue::Float(n)) => {
            let arr: [u8; 8] = raw.try_into().ok()?;
            f64::from_be_bytes(arr).partial_cmp(n)
        }
        (TAG_FLOAT, LiteralValue::Int(n)) => {
            let arr: [u8; 8] = raw.try_into().ok()?;
            f64::from_be_bytes(arr).partial_cmp(&(*n as f64))
        }
        (TAG_STRING, LiteralValue::String(s)) => {
            parse_string_value(raw).map(|v| v.cmp(s.as_str()))
        }
        _ => None,
    }
}

/// Convert a document to storage bytes. Uses binary format for fast scan/filter.
fn doc_to_storage_bytes(doc: &Map<String, Value>) -> Vec<u8> {
    onto_core::binary_row::map_to_binary(doc)
}

/// Parse storage bytes back to a Map. Handles both binary and legacy JSON formats.
fn storage_bytes_to_doc(bytes: &[u8]) -> Option<Map<String, Value>> {
    // Try binary format first
    if let Some(row) = BinaryRow::parse(bytes) {
        return row.to_map();
    }
    // Fall back to JSON
    match serde_json::from_slice::<Value>(bytes) {
        Ok(Value::Object(doc)) => Some(doc),
        _ => None,
    }
}

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Query execution configuration.
#[derive(Debug, Clone)]
pub struct QueryConfig {
    /// Maximum query execution time. Default: 30 seconds.
    pub query_timeout: Duration,
    /// Maximum memory per query in bytes. Default: 256 MB.
    pub memory_budget: usize,
}

impl Default for QueryConfig {
    fn default() -> Self {
        Self {
            query_timeout: Duration::from_secs(30),
            memory_budget: 256 * 1024 * 1024, // 256 MB
        }
    }
}

/// Process-wide monotonic clock for timeout tracking (nanoseconds since creation).
/// Avoids SystemTime overhead and clock-adjustment issues.
static PROCESS_START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

fn now_nanos() -> u64 {
    let start = PROCESS_START.get_or_init(std::time::Instant::now);
    start.elapsed().as_nanos() as u64
}

/// Executes parsed queries with caching support.
pub struct QueryExecutor {
    engine: Arc<LsmEngine>,
    ontology_store: OntologyStore,
    /// Graph store for unified entity anchor (relational ↔ graph sync).
    /// Optional for backward compatibility.
    graph: Option<Arc<onto_graph::GraphStore>>,
    /// Triple store for persistent RDF triples (SPO/POS/OSP indexes).
    /// Optional for backward compatibility.
    triple_store: Option<Arc<onto_ontology::TripleStore>>,
    /// Query planner for optimization (wrapped for interior mutability).
    planner: std::sync::RwLock<QueryPlanner>,
    /// Query result cache.
    query_cache: Arc<Mutex<QueryCache>>,
    /// Execution plan cache.
    plan_cache: Arc<Mutex<PlanCache>>,
    /// Monotonic counter for generating unique document keys.
    doc_counter: AtomicU64,
    /// Runtime execution statistics.
    runtime_stats: Arc<Mutex<RuntimeStats>>,
    /// Active multi-statement transaction ID (None = auto-commit mode).
    active_txn: Mutex<Option<onto_core::SeqNo>>,
    /// Query execution configuration.
    config: QueryConfig,
    /// Inference cache: class name → hierarchy (all super/sub classes).
    /// Cleared on ontology changes (CREATE ONTOLOGY).
    inference_cache: Mutex<InferenceCache>,
    /// Query deadline in nanoseconds (monotonic). Set before each query execution.
    /// Checked in hot loops to allow early cancellation of slow queries.
    query_deadline: AtomicU64,
    /// Shard router for data sharding. Optional for backward compatibility.
    /// Wrapped in RwLock for runtime updates.
    shard_router: std::sync::RwLock<Option<ShardRouter>>,
    /// Shard manager for shard lifecycle management. Optional for backward compatibility.
    shard_manager: Mutex<Option<ShardManager>>,
}

/// Column-level statistics collected by ANALYZE.
#[derive(Debug, Clone, Default)]
struct ColumnStats {
    non_null_count: u64,
    distinct_values: std::collections::HashSet<String>,
}

/// Runtime execution statistics for adaptive optimization.
#[derive(Debug, Clone, Default)]
pub struct RuntimeStats {
    /// Total queries executed.
    pub total_queries: u64,
    /// Total execution time in microseconds.
    pub total_time_us: u64,
    /// Per-table row counts observed during execution.
    pub table_row_counts: std::collections::HashMap<String, u64>,
    /// Per-table scan counts (how many times each table was scanned).
    pub table_scan_counts: std::collections::HashMap<String, u64>,
    /// Plan cache hit count.
    pub plan_cache_hits: u64,
    /// Plan cache miss count.
    pub plan_cache_misses: u64,
    /// Query cache hit count.
    pub query_cache_hits: u64,
    /// Query cache miss count.
    pub query_cache_misses: u64,
}

impl RuntimeStats {
    /// Average query execution time in microseconds.
    pub fn avg_query_time_us(&self) -> u64 {
        if self.total_queries == 0 {
            0
        } else {
            self.total_time_us / self.total_queries
        }
    }

    /// Plan cache hit rate as percentage.
    pub fn plan_cache_hit_rate(&self) -> f64 {
        let total = self.plan_cache_hits + self.plan_cache_misses;
        if total == 0 {
            0.0
        } else {
            (self.plan_cache_hits as f64 / total as f64) * 100.0
        }
    }
}

/// Cached inference results for ontology reasoning.
/// Avoids re-running the Reasoner on every query.
#[derive(Debug, Default)]
struct InferenceCache {
    /// Class name → set of all classes in hierarchy (superclasses + subclasses + equivalent).
    class_hierarchy: std::collections::HashMap<String, HashSet<String>>,
    /// Property name → set of all equivalent/sub-property names.
    property_aliases: std::collections::HashMap<String, HashSet<String>>,
    /// Property name → inverse property name.
    inverse_property: std::collections::HashMap<String, Option<String>>,
}

impl InferenceCache {
    fn clear(&mut self) {
        self.class_hierarchy.clear();
        self.property_aliases.clear();
        self.inverse_property.clear();
    }
}

impl QueryExecutor {
    pub fn new(engine: Arc<LsmEngine>, ontology_store: OntologyStore) -> Self {
        Self::with_config(engine, ontology_store, QueryConfig::default())
    }

    pub fn with_config(engine: Arc<LsmEngine>, ontology_store: OntologyStore, config: QueryConfig) -> Self {
        Self {
            engine,
            ontology_store,
            graph: None,
            triple_store: None,
            planner: std::sync::RwLock::new(QueryPlanner::new()),
            query_cache: Arc::new(Mutex::new(QueryCache::new(1000, Duration::from_secs(60)))),
            plan_cache: Arc::new(Mutex::new(PlanCache::new(500))),
            doc_counter: AtomicU64::new(0),
            runtime_stats: Arc::new(Mutex::new(RuntimeStats::default())),
            active_txn: Mutex::new(None),
            config,
            inference_cache: Mutex::new(InferenceCache::default()),
            query_deadline: AtomicU64::new(0),
            shard_router: std::sync::RwLock::new(None),
            shard_manager: Mutex::new(None),
        }
    }

    /// Sets the query deadline based on the configured timeout.
    fn arm_timeout(&self) {
        let deadline = now_nanos() + self.config.query_timeout.as_nanos() as u64;
        self.query_deadline.store(deadline, Ordering::Relaxed);
    }

    /// Returns an error if the current query has exceeded its timeout.
    /// Cheap: one atomic load + one subtraction. Safe to call in hot loops.
    fn check_timeout(&self) -> Result<()> {
        let deadline = self.query_deadline.load(Ordering::Relaxed);
        if deadline > 0 && now_nanos() > deadline {
            return Err(CoreError::Custom(format!(
                "query timeout: exceeded {} seconds",
                self.config.query_timeout.as_secs()
            )));
        }
        Ok(())
    }

    /// Set the graph store for unified entity anchor (relational ↔ graph sync).
    pub fn with_graph(mut self, graph: Arc<onto_graph::GraphStore>) -> Self {
        self.graph = Some(graph);
        self
    }

    /// Set the triple store for persistent RDF triples.
    pub fn with_triple_store(mut self, triple_store: Arc<onto_ontology::TripleStore>) -> Self {
        self.triple_store = Some(triple_store);
        self
    }

    /// Set the shard router for data sharding support.
    pub fn with_shard_router(self, shard_map: ShardMap, local_shards: Vec<onto_sharding::ShardId>) -> Self {
        {
            let mut router = self.shard_router.write().unwrap_or_else(|e| e.into_inner());
            *router = Some(ShardRouter::new(shard_map.clone(), local_shards));
        }
        {
            let mut mgr = self.shard_manager.lock().unwrap_or_else(|e| e.into_inner());
            *mgr = Some(ShardManager::from_json(
                &serde_json::to_string(&shard_map).unwrap_or_default()
            ).unwrap_or_else(|_| ShardManager::new(0)));
        }
        self
    }

    /// Get a snapshot of the shard router (cloned).
    pub fn shard_router_snapshot(&self) -> Option<ShardRouter> {
        let router = self.shard_router.read().unwrap_or_else(|e| e.into_inner());
        router.as_ref().map(|r| ShardRouter::new(
            r.shard_map().clone(),
            r.local_shards().to_vec(),
        ))
    }

    /// Get the shard manager reference (if configured).
    pub fn shard_manager(&self) -> std::sync::MutexGuard<'_, Option<ShardManager>> {
        self.shard_manager.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Update the shard configuration at runtime.
    pub fn update_shard_config(&self, shard_map: ShardMap, local_shards: Vec<onto_sharding::ShardId>) {
        {
            let mut router = self.shard_router.write().unwrap_or_else(|e| e.into_inner());
            *router = Some(ShardRouter::new(shard_map.clone(), local_shards));
        }
        {
            let mut mgr = self.shard_manager.lock().unwrap_or_else(|e| e.into_inner());
            *mgr = Some(ShardManager::from_json(
                &serde_json::to_string(&shard_map).unwrap_or_default()
            ).unwrap_or_else(|_| ShardManager::new(0)));
        }
    }

    /// Get the graph store reference (if configured).
    pub fn graph_store(&self) -> Option<&Arc<onto_graph::GraphStore>> {
        self.graph.as_ref()
    }

    /// Get the triple store reference (if configured).
    pub fn triple_store(&self) -> Option<&Arc<onto_ontology::TripleStore>> {
        self.triple_store.as_ref()
    }

    // ── Query Fusion: Vector + Graph + Relational ──

    /// Enrich vector search results with graph neighbors.
    ///
    /// For each vector search result, also returns the graph neighbors
    /// (related entities) with their relational attributes.
    pub fn vector_search_with_neighbors(
        &self,
        class: &str,
        column: &str,
        query_vector: &[f32],
        top_k: usize,
        edge_label: Option<&str>,
        _max_depth: usize,
    ) -> Result<Vec<Map<String, Value>>> {
        let results = self.plan_vector_search(&self.engine, class, column, query_vector, top_k, &None)?;

        let Some(ref graph) = self.graph else {
            return Ok(results); // No graph store, return vector results only
        };

        let mut enriched = Vec::new();
        for mut row in results {
            // Get entity ID from the row
            if let Some(pk) = row.get("__pk__").and_then(|v| v.as_str()) {
                let entity_id = onto_core::EntityId::new(class, pk);

                // Get graph neighbors
                let neighbors = graph.get_entity_neighbors(
                    &entity_id,
                    onto_graph::Direction::Out,
                    edge_label,
                );

                // Fetch relational attributes for each neighbor
                let mut neighbor_data = Vec::new();
                for neighbor_id in &neighbors {
                    if let Ok(Some(val_bytes)) = self.engine.get(&neighbor_id.to_lsm_key()) {
                        if let Some(doc) = storage_bytes_to_doc(&val_bytes) {
                            neighbor_data.push(serde_json::json!({
                                "entity": neighbor_id.to_string(),
                                "class": neighbor_id.class(),
                                "properties": doc,
                            }));
                        }
                    }
                }

                row.insert("_neighbors".to_string(), serde_json::json!(neighbor_data));
                row.insert("_neighbor_count".to_string(), serde_json::json!(neighbors.len()));
            }
            enriched.push(row);
        }

        Ok(enriched)
    }

    /// Enrich graph traversal results with relational attributes.
    ///
    /// Starting from an entity, traverses the graph and returns all visited
    /// entities with their full relational attributes.
    pub fn graph_traverse_with_attributes(
        &self,
        start_class: &str,
        start_pk: &str,
        direction: onto_graph::Direction,
        edge_label: Option<&str>,
        _max_depth: usize,
    ) -> Result<Vec<Map<String, Value>>> {
        let Some(ref graph) = self.graph else {
            return Ok(Vec::new());
        };

        let start_id = onto_core::EntityId::new(start_class, start_pk);
        let neighbors = graph.get_entity_neighbors(&start_id, direction, edge_label);

        let mut results = Vec::new();

        // Add the start entity itself
        if let Ok(Some(val_bytes)) = self.engine.get(&start_id.to_lsm_key()) {
            if let Some(mut doc) = storage_bytes_to_doc(&val_bytes) {
                doc.insert("_depth".to_string(), serde_json::json!(0));
                doc.insert("_entity".to_string(), serde_json::json!(start_id.to_string()));
                results.push(doc);
            }
        }

        // Add neighbors with their attributes
        for neighbor_id in &neighbors {
            if let Ok(Some(val_bytes)) = self.engine.get(&neighbor_id.to_lsm_key()) {
                if let Some(mut doc) = storage_bytes_to_doc(&val_bytes) {
                    doc.insert("_depth".to_string(), serde_json::json!(1));
                    doc.insert("_entity".to_string(), serde_json::json!(neighbor_id.to_string()));
                    results.push(doc);
                }
            }
        }

        Ok(results)
    }

    /// Hybrid query: vector search + graph traversal + relational filtering.
    ///
    /// 1. Vector search to find similar entities
    /// 2. For each result, traverse graph to find related entities
    /// 3. Apply relational filter to all entities
    /// 4. Return unified results
    pub fn hybrid_vector_graph_query(
        &self,
        class: &str,
        vector_column: &str,
        query_vector: &[f32],
        top_k: usize,
        graph_edge_label: Option<&str>,
        graph_depth: usize,
        relational_filter: Option<&FilterExpr>,
    ) -> Result<Vec<Map<String, Value>>> {
        // Step 1: Vector search
        let vector_results = self.plan_vector_search(
            &self.engine, class, vector_column, query_vector, top_k, &None,
        )?;

        let Some(ref graph) = self.graph else {
            return Ok(vector_results);
        };

        let mut all_entities: Vec<(String, Map<String, Value>)> = Vec::new();

        // Step 2: For each vector result, get graph neighbors
        for row in &vector_results {
            if let Some(pk) = row.get("__pk__").and_then(|v| v.as_str()) {
                let entity_id = onto_core::EntityId::new(class, pk);
                all_entities.push((entity_id.to_string(), row.clone()));

                if graph_depth > 0 {
                    let neighbors = graph.get_entity_neighbors(
                        &entity_id,
                        onto_graph::Direction::Out,
                        graph_edge_label,
                    );

                    for neighbor_id in &neighbors {
                        if let Ok(Some(val_bytes)) = self.engine.get(&neighbor_id.to_lsm_key()) {
                            if let Some(mut doc) = storage_bytes_to_doc(&val_bytes) {
                                doc.insert("_source_entity".to_string(), serde_json::json!(entity_id.to_string()));
                                doc.insert("_relation".to_string(), serde_json::json!(graph_edge_label.unwrap_or("related")));
                                all_entities.push((neighbor_id.to_string(), doc));
                            }
                        }
                    }
                }
            }
        }

        // Step 3: Apply relational filter if provided
        let filtered: Vec<Map<String, Value>> = if let Some(filter) = relational_filter {
            all_entities.into_iter()
                .filter(|(_, doc)| self.eval_filter(&self.engine, doc, filter))
                .map(|(_, doc)| doc)
                .collect()
        } else {
            all_entities.into_iter().map(|(_, doc)| doc).collect()
        };

        Ok(filtered)
    }

    /// Query triple store and return results as rows.
    ///
    /// Supports SPO, POS, OSP query patterns based on which components are provided.
    pub fn query_triples(
        &self,
        subject: Option<&str>,
        predicate: Option<&str>,
        object: Option<&str>,
    ) -> Result<Vec<Map<String, Value>>> {
        let Some(ref triple_store) = self.triple_store else {
            return Ok(Vec::new());
        };

        let triples = match (subject, predicate, object) {
            (Some(s), Some(p), Some(o)) => {
                // Check existence
                if triple_store.contains(s, p, o).unwrap_or(false) {
                    vec![onto_ontology::triple_store::Triple::new(s, p, o)]
                } else {
                    vec![]
                }
            }
            (Some(s), Some(p), None) => {
                // SPO query
                let objects = triple_store.lookup_spo(s, p).unwrap_or_default();
                objects.into_iter().map(|o| onto_ontology::triple_store::Triple::new(s, p, o)).collect()
            }
            (Some(s), None, None) => {
                // S query
                let pairs = triple_store.lookup_s(s).unwrap_or_default();
                pairs.into_iter().map(|(p, o)| onto_ontology::triple_store::Triple::new(s, p, o)).collect()
            }
            (None, Some(p), Some(o)) => {
                // POS query
                let subjects = triple_store.lookup_pos(p, o).unwrap_or_default();
                subjects.into_iter().map(|s| onto_ontology::triple_store::Triple::new(s, p, o)).collect()
            }
            (None, Some(p), None) => {
                // P query
                let pairs = triple_store.lookup_p(p).unwrap_or_default();
                pairs.into_iter().map(|(s, o)| onto_ontology::triple_store::Triple::new(s, p, o)).collect()
            }
            (None, None, Some(o)) => {
                // O query
                let pairs = triple_store.lookup_o(o).unwrap_or_default();
                pairs.into_iter().map(|(s, p)| onto_ontology::triple_store::Triple::new(s, p, o)).collect()
            }
            _ => {
                // Get all triples
                triple_store.get_all_triples().unwrap_or_default()
            }
        };

        // Convert to rows
        let rows: Vec<Map<String, Value>> = triples.into_iter().map(|t| {
            let mut row = Map::new();
            row.insert("subject".to_string(), serde_json::json!(t.subject));
            row.insert("predicate".to_string(), serde_json::json!(t.predicate));
            row.insert("object".to_string(), serde_json::json!(t.object));
            row
        }).collect();

        Ok(rows)
    }

    /// Get runtime execution statistics.
    pub fn runtime_stats(&self) -> RuntimeStats {
        self.runtime_stats.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Get engine storage statistics (SSTable count, total entries, etc.).
    pub fn engine_stats(&self) -> Option<onto_storage::engine::EngineStats> {
        Some(self.engine.stats())
    }

    /// Access the underlying storage engine.
    pub fn engine(&self) -> &onto_storage::LsmEngine {
        &self.engine
    }

    /// Creates a full snapshot backup to the given directory.
    pub fn backup(&self, backup_dir: &std::path::Path) -> Result<onto_storage::BackupManifest> {
        self.engine.backup(backup_dir)
    }

    /// Creates an incremental backup — only files modified since the given time.
    pub fn backup_incremental(
        &self,
        backup_dir: &std::path::Path,
        since: &std::time::SystemTime,
    ) -> Result<onto_storage::BackupManifest> {
        self.engine.backup_incremental(backup_dir, since)
    }

    /// Verifies a backup's integrity (file existence + checksums).
    pub fn verify_backup(backup_dir: &std::path::Path) -> Result<()> {
        onto_storage::LsmEngine::verify_backup(backup_dir)
    }

    /// Flushes the MemTable to SSTable on disk.
    pub fn flush(&self) -> Result<()> {
        self.engine.flush()
    }

    /// Get a reference to the query planner.
    pub fn planner(&self) -> std::sync::RwLockReadGuard<'_, QueryPlanner> {
        self.planner.read().unwrap_or_else(|e| e.into_inner())
    }

    /// Get a mutable reference to the query planner (for updating stats).
    pub fn planner_mut(&self) -> std::sync::RwLockWriteGuard<'_, QueryPlanner> {
        self.planner.write().unwrap_or_else(|e| e.into_inner())
    }

    /// Get query cache statistics.
    pub fn query_cache_stats(&self) -> crate::cache::CacheStats {
        self.query_cache.lock().unwrap_or_else(|e| e.into_inner()).stats().clone()
    }

    /// Get plan cache statistics.
    pub fn plan_cache_stats(&self) -> crate::cache::CacheStats {
        self.plan_cache.lock().unwrap_or_else(|e| e.into_inner()).stats().clone()
    }

    /// Clear all caches.
    pub fn clear_caches(&self) {
        self.query_cache.lock().unwrap_or_else(|e| e.into_inner()).clear();
        self.plan_cache.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }

    /// Returns schema introspection data: all ontologies, classes, properties, and indexes.
    pub fn schema_info(&self) -> Result<serde_json::Value> {
        let engine = &self.engine;

        let mut ontologies = Vec::new();
        let entries = engine.scan_prefix(b"__ontology__").unwrap_or_default();
        for (_key, val_bytes) in entries {
            if let Ok(ontology) = onto_ontology::Ontology::from_json_slice(&val_bytes) {
                let mut classes = serde_json::Map::new();
                for (name, class) in &ontology.classes {
                    let mut class_info = serde_json::Map::new();
                    class_info.insert("type".to_string(), serde_json::json!(format!("{:?}", class.class_type)));
                    class_info.insert("superclasses".to_string(), serde_json::json!(class.superclasses));
                    class_info.insert("equivalent_classes".to_string(), serde_json::json!(class.equivalent_classes));
                    class_info.insert("disjoint_with".to_string(), serde_json::json!(class.disjoint_with));
                    class_info.insert("properties".to_string(), serde_json::json!(class.properties));
                    classes.insert(name.clone(), serde_json::Value::Object(class_info));
                }

                let mut properties = serde_json::Map::new();
                for (name, prop) in &ontology.properties {
                    let mut prop_info = serde_json::Map::new();
                    prop_info.insert("domain".to_string(), serde_json::json!(prop.domain));
                    prop_info.insert("range".to_string(), serde_json::json!(prop.range.as_str()));
                    prop_info.insert("required".to_string(), serde_json::json!(prop.required));
                    prop_info.insert("multi_valued".to_string(), serde_json::json!(prop.multi_valued));
                    if let Some(ref inverse) = prop.inverse_of {
                        prop_info.insert("inverse_of".to_string(), serde_json::json!(inverse));
                    }
                    prop_info.insert("is_transitive".to_string(), serde_json::json!(prop.is_transitive));
                    prop_info.insert("is_symmetric".to_string(), serde_json::json!(prop.is_symmetric));
                    prop_info.insert("is_functional".to_string(), serde_json::json!(prop.is_functional));
                    if !prop.subproperty_of.is_empty() {
                        prop_info.insert("subproperty_of".to_string(), serde_json::json!(prop.subproperty_of));
                    }
                    if !prop.equivalent_properties.is_empty() {
                        prop_info.insert("equivalent_properties".to_string(), serde_json::json!(prop.equivalent_properties));
                    }
                    properties.insert(name.clone(), serde_json::Value::Object(prop_info));
                }

                let mut onto_info = serde_json::Map::new();
                onto_info.insert("name".to_string(), serde_json::json!(ontology.name));
                onto_info.insert("classes".to_string(), serde_json::Value::Object(classes));
                onto_info.insert("properties".to_string(), serde_json::Value::Object(properties));
                ontologies.push(serde_json::Value::Object(onto_info));
            }
        }

        // Collect index information
        let mut indexes = Vec::new();
        let index_entries = engine.scan_prefix(b"__idx_meta__").unwrap_or_default();
        for (key, val_bytes) in index_entries {
            let key_str = String::from_utf8_lossy(&key);
            if let Ok(meta) = serde_json::from_slice::<serde_json::Value>(&val_bytes) {
                indexes.push(serde_json::json!({
                    "key": key_str,
                    "metadata": meta
                }));
            }
        }

        // Collect vector index information
        let mut vector_indexes = Vec::new();
        let vec_entries = engine.scan_prefix(b"__vec_meta__").unwrap_or_default();
        for (key, val_bytes) in vec_entries {
            let key_str = String::from_utf8_lossy(&key);
            if let Ok(meta) = serde_json::from_slice::<serde_json::Value>(&val_bytes) {
                vector_indexes.push(serde_json::json!({
                    "key": key_str,
                    "metadata": meta
                }));
            }
        }

        Ok(serde_json::json!({
            "ontologies": ontologies,
            "indexes": indexes,
            "vector_indexes": vector_indexes
        }))
    }

    /// Returns the active transaction ID, if any.
    pub fn active_txn_id(&self) -> Option<onto_core::SeqNo> {
        *self.active_txn.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Returns true if a multi-statement transaction is active.
    pub fn in_transaction(&self) -> bool {
        self.active_txn.lock().unwrap_or_else(|e| e.into_inner()).is_some()
    }

    /// Classifies whether a query is read-only (can use a read lock).
    /// Returns true for SELECT, EXPLAIN, MATCH, VectorSearch, and graph queries.
    /// Note: Union and With are conservatively treated as writes because they
    /// can contain INSERT...SELECT or write CTEs.
    pub fn is_read_only_query(ast: &QueryAst) -> bool {
        ast.is_read_only()
    }

    /// Executes a query with a write lock (default path, full feature support).
    /// Supports all query types including subqueries, CTEs, materialized views,
    /// expressions, and transactions.
    pub fn execute(&self, ast: &QueryAst) -> Result<QueryResult> {
        self.arm_timeout();
        // For simple DML (INSERT/UPDATE/DELETE) without an active multi-statement txn,
        // use a short write lock that's only held during the commit phase.
        // This allows concurrent reads to proceed during query planning and execution.
        let active_txn = *self.active_txn.lock().unwrap_or_else(|e| e.into_inner());
        let is_simple_dml = active_txn.is_none() && matches!(
            ast,
            QueryAst::Insert { .. }
                | QueryAst::BatchInsert { .. }
                | QueryAst::BatchUpsert { .. }
                | QueryAst::Update { .. }
                | QueryAst::Delete { .. }
                | QueryAst::Upsert { .. }
        );

        if is_simple_dml {
            return self.execute_dml_short_lock(ast);
        }

        // For DDL, transactions, and complex queries, use a read lock
        // (all LsmEngine methods take &self via interior mutability)
        let engine = &self.engine;
        self.execute_write_with_engine(ast, engine)
    }

    /// Executes a simple DML statement (INSERT/UPDATE/DELETE) with minimal write lock scope.
    /// The write lock is only held during transaction begin + commit, not during query planning/execution.
    fn execute_dml_short_lock(&self, ast: &QueryAst) -> Result<QueryResult> {
        let start_time = std::time::Instant::now();
        let engine = &self.engine;

        // Phase 1: Begin transaction
        let txn_id = engine.begin_txn();

        // Phase 2: Execute query
        let result = self.execute_in_txn_with_plan(ast, engine, txn_id, None);

        // Phase 3: Commit or abort
        match &result {
            Ok(_) => { engine.commit_txn(txn_id)?; }
            Err(_) => { let _ = engine.abort_txn(txn_id); }
        }

        let elapsed = start_time.elapsed();
        if elapsed > self.config.query_timeout {
            return Err(CoreError::Custom(format!(
                "query timeout: exceeded {} seconds",
                self.config.query_timeout.as_secs()
            )));
        }

        let elapsed_us = elapsed.as_micros() as u64;
        {
            let mut stats = self.runtime_stats.lock().unwrap_or_else(|e| e.into_inner());
            stats.total_queries += 1;
            stats.total_time_us += elapsed_us;
        }

        result
    }

    /// Executes a read-only query with a read lock, allowing concurrent SELECT execution.
    /// Use this for pure SELECT queries that don't need subqueries in WHERE,
    /// materialized views, CTEs, or expression evaluation.
    /// Multiple `execute_read` calls can run concurrently.
    pub fn execute_read(&self, ast: &QueryAst) -> Result<QueryResult> {
        self.arm_timeout();
        let engine = &self.engine;
        self.execute_select_read(ast, engine)
    }

    /// Write-path execution: acquires write lock, handles timeout+stats+transactions.
    fn execute_write_with_engine(&self, ast: &QueryAst, engine: &LsmEngine) -> Result<QueryResult> {
        let start_time = std::time::Instant::now();

        let result = self.execute_with_engine_inner(ast, engine);

        let elapsed = start_time.elapsed();
        if elapsed > self.config.query_timeout {
            return Err(CoreError::Custom(format!(
                "query timeout: exceeded {} seconds",
                self.config.query_timeout.as_secs()
            )));
        }

        let elapsed_us = elapsed.as_micros() as u64;
        {
            let mut stats = self.runtime_stats.lock().unwrap_or_else(|e| e.into_inner());
            stats.total_queries += 1;
            stats.total_time_us += elapsed_us;
            if let QueryAst::Select { from, .. } = ast {
                *stats.table_scan_counts.entry(from.clone()).or_insert(0) += 1;
            }
        }

        result
    }

    /// Read-path execution for SELECT queries: uses read lock, no transaction overhead.
    /// Multiple SELECT queries can run concurrently.
    fn execute_select_read(&self, ast: &QueryAst, engine: &LsmEngine) -> Result<QueryResult> {
        let start_time = std::time::Instant::now();

        let result = match ast {
            QueryAst::Select { .. } => {
                // Check plan cache first
                let cached_plan = {
                    let ast_hash = Self::hash_ast(ast);
                    let cached = self.plan_cache.lock().unwrap_or_else(|e| e.into_inner()).get(ast_hash);
                    if cached.is_some() {
                        self.runtime_stats.lock().unwrap_or_else(|e| e.into_inner()).plan_cache_hits += 1;
                        cached
                    } else {
                        let plan = self.planner.read().unwrap_or_else(|e| e.into_inner()).plan(ast);
                        if let Ok(ref p) = plan {
                            self.plan_cache.lock().unwrap_or_else(|e| e.into_inner()).insert(ast_hash, p.clone());
                        }
                        self.runtime_stats.lock().unwrap_or_else(|e| e.into_inner()).plan_cache_misses += 1;
                        plan.ok()
                    }
                };

                if let Some(plan) = cached_plan {
                    let plan_result = self.execute_plan_read(&plan, engine)?;
                    let mut rows = match plan_result {
                        QueryResult::Rows(r) => r,
                        other => return Ok(other),
                    };

                    // Post-processing
                    if let QueryAst::Select {
                        distinct, columns, group_by, having, order_by, limit, offset, ..
                    } = ast {
                        let has_aggregates = Self::columns_have_aggregates(columns);

                        // Fast path: COUNT(*) was already computed by plan_seq_scan_count_only.
                        // Skip re-aggregation — just apply ORDER BY / LIMIT if present.
                        if has_aggregates && group_by.is_none()
                            && rows.len() == 1
                            && Self::is_pure_count_star(columns)
                        {
                            if let Some(lim) = limit {
                                rows.truncate(*lim);
                            }
                            return Ok(QueryResult::Rows(rows));
                        }

                        if group_by.is_some() || has_aggregates {
                            let result = self.execute_aggregation_read(engine, columns, &rows, group_by.as_ref(), having, order_by, *limit)?;
                            if let QueryResult::Rows(mut agg_rows) = result {
                                if *distinct { Self::dedup_rows(&mut agg_rows); }
                                return Ok(QueryResult::Rows(agg_rows));
                            }
                            return Ok(result);
                        }

                        if let SelectColumns::Columns(items) = columns {
                            let window_exprs: Vec<&WindowExpr> = items.iter().filter_map(|item| {
                                if let SelectItem::WindowFunction(w) = item { Some(w) } else { None }
                            }).collect();
                            if !window_exprs.is_empty() {
                                Self::execute_window_functions(&mut rows, &window_exprs);
                            }
                        }

                        if *distinct { Self::dedup_rows(&mut rows); }

                        if let Some(off) = offset {
                            if *off < rows.len() {
                                rows = rows.split_off(*off);
                            } else {
                                rows.clear();
                            }
                        }
                        if let Some(lim) = limit {
                            rows.truncate(*lim);
                        }
                    }

                    Ok(QueryResult::Rows(rows))
                } else {
                    // Plan generation failed, return error
                    Err(CoreError::Custom("failed to generate execution plan".to_string()))
                }
            }
            QueryAst::Explain { query } => {
                self.execute_explain_read(query, engine)
            }
            QueryAst::Analyze { table } => {
                self.execute_analyze_read(table, engine)
            }
            QueryAst::Match { variable, class, filter, returns } => {
                self.execute_match_read(engine, variable, class, filter, returns)
            }
            QueryAst::VectorSearch { class, column, query_vector, top_k, filter } => {
                self.execute_vector_search_read(engine, class, column, query_vector, *top_k, filter)
            }
            _ => Err(CoreError::Custom("unexpected query type in read path".to_string())),
        };

        let elapsed = start_time.elapsed();
        if elapsed > self.config.query_timeout {
            return Err(CoreError::Custom(format!(
                "query timeout: exceeded {} seconds",
                self.config.query_timeout.as_secs()
            )));
        }

        let elapsed_us = elapsed.as_micros() as u64;
        {
            let mut stats = self.runtime_stats.lock().unwrap_or_else(|e| e.into_inner());
            stats.total_queries += 1;
            stats.total_time_us += elapsed_us;
            if let QueryAst::Select { from, .. } = ast {
                *stats.table_scan_counts.entry(from.clone()).or_insert(0) += 1;
            }
        }

        result
    }

    /// Lock-free execution dispatch. Called by execute_write_with_engine and execute_select_read.
    /// Does NOT acquire any engine lock — the caller must hold one.
    /// Lock-free execution dispatch. Called by execute_write_with_engine and execute_select_read.
    /// Does NOT acquire any engine lock — the caller must hold one.
    /// Recursive callers (explain, CTE, union, filters) call this directly.
    fn execute_with_engine_inner(&self, ast: &QueryAst, engine: &LsmEngine) -> Result<QueryResult> {
        match ast {
            QueryAst::Explain { query } => {
                // EXPLAIN: generate and return the execution plan
                self.execute_explain(query, engine)
            }
            QueryAst::Analyze { table } => {
                // ANALYZE: collect table statistics
                self.execute_analyze(table, engine)
            }
            QueryAst::With { ctes, query, recursive } => {
                // WITH clause: execute CTEs and substitute into main query
                self.execute_with_ctes(ctes, query, engine, *recursive)
            }
            QueryAst::CreateOntology { sql } => {
                // DDL doesn't need MVCC transaction
                let ontology = onto_ontology::OntologyParser::parse(sql)?;
                self.ontology_store.save_with_engine(engine, &ontology)?;
                // Invalidate inference cache on ontology changes
                self.inference_cache.lock().unwrap_or_else(|e| e.into_inner()).clear();
                Ok(QueryResult::Success(format!(
                    "Ontology '{}' created with {} classes and {} properties",
                    ontology.name,
                    ontology.classes.len(),
                    ontology.properties.len()
                )))
            }
            QueryAst::Union { left, right, all } => {
                // UNION runs each sub-query in its own transaction
                self.execute_union(engine, left, right, *all)
            }
            QueryAst::CreateIndex { class, column } => {
                engine.create_index(class, column)?;
                self.refresh_index_stats(engine, class);
                Ok(QueryResult::Success(format!(
                    "Index created on {}.{}", class, column
                )))
            }
            QueryAst::CreateCompositeIndex { class, columns } => {
                // Create individual indexes for each column in the composite index
                // This enables index intersection for multi-column queries
                for col in columns {
                    engine.create_index(class, col)?;
                }
                self.refresh_index_stats(engine, class);
                Ok(QueryResult::Success(format!(
                    "Composite index created on {} ({})", class, columns.join(", ")
                )))
            }
            QueryAst::DropIndex { class, column } => {
                if engine.drop_index(class, column) {
                    Ok(QueryResult::Success(format!(
                        "Index dropped on {}.{}", class, column
                    )))
                } else {
                    Ok(QueryResult::Success(format!(
                        "No index found on {}.{}", class, column
                    )))
                }
            }
            QueryAst::CreateVectorIndex {
                class,
                column,
                metric,
                dimension,
                m,
                ef_construction,
                ef_search,
            } => {
                let distance_metric = match metric.to_lowercase().as_str() {
                    "l2" | "euclidean" => onto_storage::DistanceMetric::L2,
                    "cosine" => onto_storage::DistanceMetric::Cosine,
                    "innerproduct" | "inner_product" | "dot" => onto_storage::DistanceMetric::InnerProduct,
                    _ => onto_storage::DistanceMetric::Cosine,
                };
                engine.create_vector_index(
                    class, column, *dimension, distance_metric, *m, *ef_construction, *ef_search,
                )?;
                Ok(QueryResult::Success(format!(
                    "Vector index created on {}.{} (dim={}, metric={:?})",
                    class, column, dimension, distance_metric
                )))
            }
            QueryAst::DropVectorIndex { class, column } => {
                if engine.drop_vector_index(class, column) {
                    Ok(QueryResult::Success(format!(
                        "Vector index dropped on {}.{}", class, column
                    )))
                } else {
                    Ok(QueryResult::Success(format!(
                        "No vector index found on {}.{}", class, column
                    )))
                }
            }
            QueryAst::CreateMaterializedView { name, query } => {
                // Execute the query and store results as a materialized view
                let result = self.execute_with_engine_inner(query, engine)?;
                if let QueryResult::Rows(rows) = result {
                    let prefix = format!("__mv_{}::", name.to_lowercase());
                    let row_count = rows.len();
                    for (i, row) in rows.iter().enumerate() {
                        let key = format!("{}{:010}", prefix, i);
                        let value = doc_to_storage_bytes(row);
                        engine.put(key.as_bytes().to_vec(), value)?;
                    }
                    // Store metadata with original query for incremental refresh
                    let meta_key = format!("__mv_meta_{}::query", name.to_lowercase());
                    let query_json = serde_json::to_string(&query)
                        .map_err(|e| CoreError::Serialization(e.to_string()))?;
                    engine.put(meta_key.as_bytes().to_vec(), query_json.as_bytes().to_vec())?;
                    Ok(QueryResult::Success(format!(
                        "Materialized view '{}' created with {} rows", name, row_count
                    )))
                } else {
                    Ok(QueryResult::Success(format!(
                        "Materialized view '{}' created (no rows)", name
                    )))
                }
            }
            QueryAst::DropMaterializedView { name } => {
                let lower_name = name.to_lowercase();
                let prefix = format!("__mv_{}::", lower_name);
                let meta_key = format!("__mv_meta_{}::query", lower_name);
                let existing = engine.scan_prefix(prefix.as_bytes()).unwrap_or_default();
                let count = existing.len();
                for (key, _) in existing {
                    engine.delete(key)?;
                }
                engine.delete(meta_key.as_bytes().to_vec())?;
                if count > 0 {
                    Ok(QueryResult::Success(format!(
                        "Materialized view '{}' dropped ({} rows removed)", name, count
                    )))
                } else {
                    Ok(QueryResult::Success(format!(
                        "No materialized view '{}' found", name
                    )))
                }
            }
            QueryAst::RefreshMaterializedView { name } => {
                let lower_name = name.to_lowercase();
                let meta_key = format!("__mv_meta_{}::query", lower_name);
                let prefix = format!("__mv_{}::", lower_name);

                // Get the original query from metadata
                let query_bytes = engine.get(meta_key.as_bytes())?;
                let query_json = match query_bytes {
                    Some(bytes) => String::from_utf8(bytes)
                        .map_err(|_| CoreError::InvalidArgument("invalid query metadata".to_string()))?,
                    None => return Ok(QueryResult::Success(format!(
                        "No materialized view '{}' found (use CREATE MATERIALIZED VIEW first)", name
                    ))),
                };

                // Deserialize the query AST from JSON
                let query_ast: QueryAst = serde_json::from_str(&query_json)
                    .map_err(|e| CoreError::Serialization(format!("failed to deserialize query: {}", e)))?;
                let result = self.execute_with_engine_inner(&query_ast, engine)?;

                if let QueryResult::Rows(new_rows) = result {
                    // Get existing rows for comparison
                    let existing = engine.scan_prefix(prefix.as_bytes()).unwrap_or_default();
                    let new_count = new_rows.len();

                    // Build a set of old row JSON strings for comparison.
                    // Strip internal fields (__pk__, __class__) before comparison
                    // because __pk__ contains a sequence number that changes between queries.
                    let mut old_row_set: std::collections::HashMap<String, Vec<u8>> = std::collections::HashMap::new();
                    for (key, val_bytes) in existing {
                        if let Some(mut obj) = storage_bytes_to_doc(&val_bytes) {
                            Self::strip_internal_fields(&mut obj);
                            if let Ok(stripped_json) = serde_json::to_string(&serde_json::Value::Object(obj)) {
                                old_row_set.insert(stripped_json, key);
                            }
                        }
                    }

                    let mut added = 0;
                    let mut unchanged = 0;

                    // Track which old rows are still present
                    let mut seen_old_jsons: std::collections::HashSet<String> = std::collections::HashSet::new();

                    for (i, row) in new_rows.iter().enumerate() {
                        let key = format!("{}{:010}", prefix, i);
                        let value = doc_to_storage_bytes(row);
                        // Strip internal fields for comparison (same as old rows)
                        let mut stripped_row = row.clone();
                        Self::strip_internal_fields(&mut stripped_row);
                        let json_str = serde_json::to_string(&serde_json::Value::Object(stripped_row))
                            .map_err(|e| CoreError::Serialization(e.to_string()))?;

                        if old_row_set.contains_key(&json_str) {
                            seen_old_jsons.insert(json_str);
                            unchanged += 1;
                        } else {
                            added += 1;
                        }
                        engine.put(key.as_bytes().to_vec(), value)?;
                    }

                    // Remove rows that no longer exist in the new result
                    let mut removed = 0;
                    for (json_str, old_key) in old_row_set {
                        if !seen_old_jsons.contains(&json_str) {
                            engine.delete(old_key)?;
                            removed += 1;
                        }
                    }

                    Ok(QueryResult::Success(format!(
                        "Materialized view '{}' refreshed: {} added, {} removed, {} unchanged (total: {})",
                        name, added, removed, unchanged, new_count
                    )))
                } else {
                    Ok(QueryResult::Success(format!(
                        "Materialized view '{}' refreshed (query returned no rows)", name
                    )))
                }
            }
            QueryAst::Begin => {
                let mut txn = self.active_txn.lock().unwrap_or_else(|e| e.into_inner());
                if txn.is_some() {
                    return Err(CoreError::InvalidArgument(
                        "transaction already active (use COMMIT or ROLLBACK first)".to_string()
                    ));
                }
                let txn_id = engine.begin_txn();
                *txn = Some(txn_id);
                Ok(QueryResult::Success(format!("Transaction started (txn_id={})", txn_id)))
            }
            QueryAst::Commit => {
                let mut txn = self.active_txn.lock().unwrap_or_else(|e| e.into_inner());
                match txn.take() {
                    Some(txn_id) => {
                        engine.commit_txn(txn_id)?;
                        Ok(QueryResult::Success("Transaction committed".to_string()))
                    }
                    None => Err(CoreError::InvalidArgument(
                        "no active transaction to commit".to_string()
                    )),
                }
            }
            QueryAst::Rollback => {
                let mut txn = self.active_txn.lock().unwrap_or_else(|e| e.into_inner());
                match txn.take() {
                    Some(txn_id) => {
                        engine.abort_txn(txn_id)?;
                        Ok(QueryResult::Success("Transaction rolled back".to_string()))
                    }
                    None => Err(CoreError::InvalidArgument(
                        "no active transaction to roll back".to_string()
                    )),
                }
            }
            QueryAst::Backup { path } => {
                Self::validate_file_path(path)?;
                let backup_dir = std::path::Path::new(path);
                let manifest = engine.backup(backup_dir)?;
                Ok(QueryResult::Success(format!(
                    "Backup completed: {} files, {} bytes total",
                    manifest.files.len(),
                    manifest.files.iter().map(|f| f.size).sum::<u64>(),
                )))
            }
            QueryAst::Restore { path } => {
                Self::validate_file_path(path)?;
                let backup_dir = std::path::Path::new(path);
                let data_dir = engine.data_dir();
                let manifest = onto_storage::LsmEngine::restore(backup_dir, &data_dir)?;
                Ok(QueryResult::Success(format!(
                    "Restore completed: {} files restored",
                    manifest.files.len(),
                )))
            }
            QueryAst::Flush => {
                engine.flush()?;
                Ok(QueryResult::Success("MemTable flushed to SSTable".to_string()))
            }
            QueryAst::Copy { class, file_path, format } => {
                // COPY uses direct bulk load without transaction for maximum speed
                self.execute_copy(engine, class, file_path, *format)
            }
            _ => {
                // For SELECT queries, check plan cache first
                let cached_plan = if matches!(ast, QueryAst::Select { .. }) {
                    let ast_hash = Self::hash_ast(ast);
                    let cached = self.plan_cache.lock().unwrap_or_else(|e| e.into_inner()).get(ast_hash);
                    if cached.is_some() {
                        self.runtime_stats.lock().unwrap_or_else(|e| e.into_inner()).plan_cache_hits += 1;
                        cached
                    } else {
                        // Plan cache miss - generate and cache plan
                        let plan = self.planner.read().unwrap_or_else(|e| e.into_inner()).plan(ast);
                        if let Ok(ref p) = plan {
                            self.plan_cache.lock().unwrap_or_else(|e| e.into_inner()).insert(ast_hash, p.clone());
                        }
                        self.runtime_stats.lock().unwrap_or_else(|e| e.into_inner()).plan_cache_misses += 1;
                        plan.ok()
                    }
                } else {
                    None
                };

                // Check if there's an active multi-statement transaction
                let active_txn_id = *self.active_txn.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(txn_id) = active_txn_id {
                    // Use the active transaction (writes are buffered until COMMIT)
                    self.execute_in_txn_with_plan(ast, engine, txn_id, cached_plan.as_ref())
                } else {
                    // Auto-commit mode: each statement runs in its own transaction
                    let txn_id = engine.begin_txn();
                    let result = self.execute_in_txn_with_plan(ast, engine, txn_id, cached_plan.as_ref());
                    // Commit on success, abort on error
                    match &result {
                        Ok(_) => { engine.commit_txn(txn_id)?; }
                        Err(_) => { let _ = engine.abort_txn(txn_id); }
                    }
                    result
                }
            }
        }
    }

    // 鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺?
    //  Plan-driven execution engine (Phase 24)
    // 鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺?

    /// Builds a simple scan+filter plan for DELETE/UPDATE operations.
    fn build_scan_plan(&self, class: &str, filter: &Option<FilterExpr>) -> Result<ExecutionPlan> {
        let mut node = PlanNode::SeqScan {
            table: class.to_string(),
            alias: None,
            filter: None,
            estimated_rows: 1000,
        };
        if let Some(f) = filter {
            node = PlanNode::Filter {
                input: Box::new(node),
                predicate: f.clone(),
                estimated_rows: 500,
            };
        }
        Ok(ExecutionPlan::new(node, crate::optimizer::CostEstimate::zero()))
    }

    /// Executes a query using the plan-driven execution engine.
    /// This is the Phase 24 architecture: the executor walks the PlanNode tree
    /// instead of re-implementing optimization logic independently.
    pub fn execute_plan(&self, plan: &ExecutionPlan, engine: &LsmEngine) -> Result<QueryResult> {
        let rows = self.execute_plan_node(&plan.root, engine)?;
        self.check_memory_budget(&rows)?;
        Ok(QueryResult::Rows(rows))
    }

    /// Estimates memory usage of rows and checks against budget.
    fn check_memory_budget(&self, rows: &[Map<String, Value>]) -> Result<()> {
        let estimated_bytes = Self::estimate_rows_memory(rows);
        if estimated_bytes > self.config.memory_budget {
            return Err(CoreError::Custom(format!(
                "memory budget exceeded: estimated {} bytes, limit {} bytes",
                estimated_bytes, self.config.memory_budget
            )));
        }
        Ok(())
    }

    /// Estimates memory usage of a row set in bytes.
    fn estimate_rows_memory(rows: &[Map<String, Value>]) -> usize {
        let mut total = std::mem::size_of::<Vec<Map<String, Value>>>();
        for row in rows {
            total += std::mem::size_of::<Map<String, Value>>();
            for (key, val) in row {
                total += key.len() + std::mem::size_of::<String>();
                total += Self::estimate_value_memory(val);
            }
        }
        total
    }

    /// Estimates memory usage of a single JSON value.
    fn estimate_value_memory(val: &Value) -> usize {
        match val {
            Value::Null | Value::Bool(_) => 8,
            Value::Number(_) => 16,
            Value::String(s) => s.len() + 24, // String header + data
            Value::Array(arr) => {
                let mut total = 24; // Vec header
                for item in arr {
                    total += Self::estimate_value_memory(item);
                }
                total
            }
            Value::Object(map) => {
                let mut total = 24; // Map header
                for (k, v) in map {
                    total += k.len() + 24 + Self::estimate_value_memory(v);
                }
                total
            }
        }
    }

    /// Recursively executes a PlanNode and returns the result rows.
    fn execute_plan_node(&self, node: &PlanNode, engine: &LsmEngine) -> Result<Vec<Map<String, Value>>> {
        match node {
            PlanNode::SeqScan { table, alias, filter, .. } => {
                self.plan_seq_scan(engine, table, alias.as_deref(), filter, None)
            }
            PlanNode::IndexScan { table, alias, index_column, filter, .. } => {
                self.plan_index_scan(engine, table, alias.as_deref(), index_column, filter)
            }
            PlanNode::IndexLookup { table, alias, index_column, key, .. } => {
                self.plan_index_lookup(engine, table, alias.as_deref(), index_column, key)
            }
            PlanNode::VectorSearch { table, column, query_vector, top_k, filter, .. } => {
                self.plan_vector_search(engine, table, column, query_vector, *top_k, filter)
            }
            PlanNode::Filter { input, predicate, .. } => {
                let mut rows = self.execute_plan_node(input, engine)?;
                let filter_expr = Some(predicate.clone());
                rows.retain(|row| self.matches_filter(engine, row, &filter_expr));
                Ok(rows)
            }
            PlanNode::Projection { input, columns, .. } => {
                // Check if there are aggregates or expressions that need all columns
                let has_aggregates = Self::columns_have_aggregates(columns);
                let has_expr = match columns {
                    SelectColumns::Columns(items) => items.iter().any(|item| matches!(item, SelectItem::Expression(_))),
                    _ => false,
                };

                // Fast path: pure COUNT(*) — count rows without materializing Maps
                if has_aggregates && Self::is_pure_count_star(columns) {
                    if let PlanNode::SeqScan { table, filter, .. } = input.as_ref() {
                        return self.plan_seq_scan_count_only(engine, table, filter, columns);
                    }
                }

                // Only push column projection down when there are no aggregates/expressions
                // (aggregates and expressions may reference columns not in the SELECT list)
                let raw_rows = if !has_aggregates && !has_expr {
                    if let PlanNode::SeqScan { table, alias, filter, .. } = input.as_ref() {
                        self.plan_seq_scan(engine, table, alias.as_deref(), filter, Some(columns))?
                    } else {
                        self.execute_plan_node(input, engine)?
                    }
                } else {
                    self.execute_plan_node(input, engine)?
                };
                if has_aggregates {
                    // Aggregation handles projection in post-processing; pass through raw rows
                    Ok(raw_rows)
                } else if has_expr {
                    // Evaluate expressions using raw rows, then build projected result
                    let mut result = Vec::new();
                    for raw_row in &raw_rows {
                        let mut projected = Map::new();
                        if let SelectColumns::Columns(items) = columns {
                            for item in items {
                                match item {
                                    SelectItem::Column(col) => {
                                        let (real_col, alias_part) = if let Some(as_pos) = col.find(" as ") {
                                            (&col[..as_pos], Some(col[as_pos + 4..].trim()))
                                        } else {
                                            (col.as_str(), None)
                                        };
                                        if let Some(val) = raw_row.get(real_col) {
                                            let name = alias_part.unwrap_or(real_col);
                                            let name = name.split('.').next_back().unwrap_or(name);
                                            projected.insert(name.to_string(), val.clone());
                                        }
                                    }
                                    SelectItem::Expression(expr) => {
                                        let val = self.evaluate_value_expr(expr, raw_row, engine)?;
                                        let name = Self::value_expr_default_name(expr);
                                        projected.insert(name, val);
                                    }
                                    _ => {}
                                }
                            }
                        }
                        result.push(projected);
                    }
                    Ok(result)
                } else {
                    let projected: Vec<Map<String, Value>> = raw_rows.iter()
                        .map(|row| self.project_columns(row, columns))
                        .collect();
                    Ok(projected)
                }
            }
            PlanNode::NestedLoopJoin { left, right, join_clause, .. } => {
                let left_rows = self.execute_plan_node(left, engine)?;
                let right_rows = self.execute_plan_node(right, engine)?;
                Self::execute_nested_loop_join(left_rows, right_rows, join_clause)
            }
            PlanNode::HashJoin { left, right, join_clause, .. } => {
                let left_rows = self.execute_plan_node(left, engine)?;
                let right_rows = self.execute_plan_node(right, engine)?;
                Self::execute_hash_join_rows(left_rows, right_rows, join_clause)
            }
            PlanNode::SortMergeJoin { left, right, join_clause, .. } => {
                let left_rows = self.execute_plan_node(left, engine)?;
                let right_rows = self.execute_plan_node(right, engine)?;
                Self::execute_sort_merge_join_rows(left_rows, right_rows, join_clause)
            }
            PlanNode::Sort { input, order_by, .. } => {
                let mut rows = self.execute_plan_node(input, engine)?;
                // Apply each ORDER BY column in reverse (last key has highest priority)
                for ob in order_by.iter().rev() {
                    Self::sort_rows(&mut rows, &ob.column, ob.ascending);
                }
                Ok(rows)
            }
            PlanNode::Aggregation { input, .. } => {
                let rows = self.execute_plan_node(input, engine)?;
                // Basic aggregation: just return grouped rows
                // Full aggregation is handled by execute_aggregation
                Ok(rows)
            }
            PlanNode::Limit { input, .. } => {
                // LIMIT is handled in post-processing along with OFFSET
                // (OFFSET must be applied before LIMIT)
                self.execute_plan_node(input, engine)
            }
            PlanNode::Union { left, right, all, .. } => {
                let mut left_rows = self.execute_plan_node(left, engine)?;
                let right_rows = self.execute_plan_node(right, engine)?;
                left_rows.extend(right_rows);
                if !all {
                    Self::dedup_rows(&mut left_rows);
                }
                Ok(left_rows)
            }
            PlanNode::WindowFunction { input, windows, .. } => {
                let mut rows = self.execute_plan_node(input, engine)?;
                // Convert PlanWindowExpr to parser::WindowExpr for execution
                let parser_windows: Vec<crate::parser::WindowExpr> = windows.iter().map(|w| {
                    crate::parser::WindowExpr {
                        func: w.func.clone(),
                        arg: w.arg.clone(),
                        over: w.over.clone(),
                        alias: w.alias.clone(),
                    }
                }).collect();
                let window_refs: Vec<&crate::parser::WindowExpr> = parser_windows.iter().collect();
                Self::execute_window_functions(&mut rows, &window_refs);
                Ok(rows)
            }
        }
    }

    /// Plan-driven sequential scan with optional filter.
    fn plan_seq_scan(
        &self,
        engine: &LsmEngine,
        table: &str,
        alias: Option<&str>,
        filter: &Option<FilterExpr>,
        projected_columns: Option<&SelectColumns>,
    ) -> Result<Vec<Map<String, Value>>> {
        // Sharding: check if this table is sharded and route accordingly
        {
            let shard_router = self.shard_router.read().unwrap_or_else(|e| e.into_inner());
            if let Some(router) = shard_router.as_ref() {
                use onto_sharding::strategy::ShardTarget;
                let target = router.route_scan(table);
                match target {
                    ShardTarget::Single(shard) => {
                        if !router.is_local(shard) {
                            tracing::debug!(table = table, shard = shard, "shard not local, skipping scan");
                            return Ok(Vec::new());
                        }
                    }
                    ShardTarget::Multi(shards) => {
                        let local_shards: Vec<_> = shards.into_iter().filter(|s| router.is_local(*s)).collect();
                        if local_shards.is_empty() {
                            tracing::debug!(table = table, "no local shards for multi-shard scan");
                            return Ok(Vec::new());
                        }
                        tracing::debug!(table = table, shards = ?local_shards, "scanning local shards");
                    }
                    ShardTarget::All => {
                        // Scan all local shards
                    }
                }
            }
        }

        // Check CTE tables first
        let cte_prefix = format!("__cte_{}::", table.to_lowercase());
        let cte_entries = engine.scan_prefix(cte_prefix.as_bytes()).unwrap_or_default();
        if !cte_entries.is_empty() {
            let mut rows = Vec::new();
            for (_key, val_bytes) in &cte_entries {
                if val_bytes == b"__deleted__" { continue; }
                if let Some(doc) = storage_bytes_to_doc(val_bytes) {
                    rows.push(doc);
                }
            }
            if !rows.is_empty() {
                return Ok(rows);
            }
        }

        // Check materialized views
        let mv_prefix = format!("__mv_{}::", table.to_lowercase());
        let mv_entries = engine.scan_prefix(mv_prefix.as_bytes()).unwrap_or_default();
        if !mv_entries.is_empty() {
            let mut rows = Vec::new();
            for (_key, val_bytes) in &mv_entries {
                if val_bytes == b"__deleted__" { continue; }
                if let Some(doc) = storage_bytes_to_doc(val_bytes) {
                    rows.push(doc);
                }
            }
            if !rows.is_empty() {
                return Ok(rows);
            }
        }

        // Regular table scan — expand class hierarchy via ontology reasoning
        let class_hierarchy = self.get_class_hierarchy(engine, table);
        tracing::debug!("plan_seq_scan: table='{}', class_hierarchy={:?}", table, class_hierarchy);

        // Semantic optimization: narrow scan scope using __class__ filter and disjoint constraints
        let scan_classes = self.narrow_scan_scope(engine, table, filter, &class_hierarchy);
        tracing::debug!("plan_seq_scan: scan_classes={:?}", scan_classes);

        // Pre-extract simple filter column names for fast byte-level rejection.
        let fast_filter_cols = Self::extract_fast_filter_columns(filter);

        // Extract projected column names for selective BinaryRow conversion.
        let proj_cols: Vec<String> = match projected_columns {
            Some(SelectColumns::Columns(items)) => {
                let mut cols = Vec::with_capacity(items.len() + 4);
                for item in items {
                    match item {
                        SelectItem::Column(col) => {
                            let real = if let Some(pos) = col.find(" as ") { &col[..pos] } else { col.as_str() };
                            let name = real.split('.').next_back().unwrap_or(real).to_string();
                            if !cols.contains(&name) { cols.push(name); }
                        }
                        SelectItem::Aggregate(a) => {
                            if let Some(ref alias) = a.alias {
                                if !cols.contains(alias) { cols.push(alias.clone()); }
                            }
                        }
                        _ => {}
                    }
                }
                cols
            }
            _ => Vec::new(),
        };

        let mut rows = Vec::new();
        let mut scan_count: u64 = 0;
        for scan_class in &scan_classes {
            let prefix = format!("{}::", scan_class);
            let entries = engine.scan_prefix(prefix.as_bytes())?;
            for (key, val_bytes) in &entries {
                // Check query timeout every 1024 rows (cheap: one atomic load)
                scan_count += 1;
                if scan_count & 0x3FF == 0 {
                    self.check_timeout()?;
                }
                // Tier 1: fast byte-level rejection (definitely doesn't match → skip)
                if !fast_filter_cols.is_empty()
                    && Self::fast_filter_reject(val_bytes, filter, &fast_filter_cols) {
                        continue;
                    }
                // Tier 2: BinaryRow path (for binary-stored data) — no JSON parsing
                if let Some(brow) = BinaryRow::parse(val_bytes) {
                    if !brow.class_in_hierarchy(&scan_classes) {
                        continue;
                    }
                    if let Some(f) = filter {
                        match eval_binary_filter(&brow, f) {
                            Some(true) => {}
                            Some(false) => continue,
                            None => {
                                if let Some(mut doc) = brow.to_map() {
                                    if !self.eval_filter(engine, &doc, f) { continue; }
                                    doc.insert("__pk__".to_string(), Value::String(String::from_utf8_lossy(key).to_string()));
                                    // Add shard routing info for debugging
                                    if let Some(router) = self.shard_router.read().unwrap_or_else(|e| e.into_inner()).as_ref() {
                                        let pk = String::from_utf8_lossy(key);
                                        let target = router.route_key(table, pk.as_bytes());
                                        doc.insert("__shard__".to_string(), Value::String(format!("{:?}", target)));
                                    }
                                    rows.push(doc);
                                }
                                continue;
                            }
                        }
                    }
                    if let Some(mut doc) = brow.to_map_projected(&proj_cols) {
                        doc.insert("__pk__".to_string(), Value::String(String::from_utf8_lossy(key).to_string()));
                        // Add shard routing info for debugging
                        if let Some(router) = self.shard_router.read().unwrap_or_else(|e| e.into_inner()).as_ref() {
                            let pk = String::from_utf8_lossy(key);
                            let target = router.route_key(table, pk.as_bytes());
                            doc.insert("__shard__".to_string(), Value::String(format!("{:?}", target)));
                        }
                        rows.push(doc);
                    }
                    continue;
                }
                // Tier 3: JSON fallback (legacy data stored as JSON)
                if let Ok(serde_json::Value::Object(mut doc)) = serde_json::from_slice::<serde_json::Value>(val_bytes) {
                    if !scan_classes.contains(doc.get("__class__").and_then(|v| v.as_str()).unwrap_or("")) {
                        continue;
                    }
                    if let Some(f) = filter {
                        if !self.eval_filter(engine, &doc, f) { continue; }
                    }
                    doc.insert("__pk__".to_string(), Value::String(String::from_utf8_lossy(key).to_string()));
                    // Add shard routing info for debugging
                    if let Some(router) = self.shard_router.read().unwrap_or_else(|e| e.into_inner()).as_ref() {
                        let pk = String::from_utf8_lossy(key);
                        let target = router.route_key(table, pk.as_bytes());
                        doc.insert("__shard__".to_string(), Value::String(format!("{:?}", target)));
                    }
                    rows.push(doc);
                }
            }
        }

        // Apply alias to column names if specified
        if let Some(a) = alias {
            for row in &mut rows {
                let keys: Vec<String> = row.keys().cloned().collect();
                for key in keys {
                    if key != "__class__" {
                        if let Some(val) = row.remove(&key) {
                            row.insert(format!("{}.{}", a, key), val.clone());
                            row.insert(key, val);
                        }
                    }
                }
            }
        }

        Ok(rows)
    }

    /// Plan-driven index scan.
    fn plan_index_scan(
        &self,
        engine: &LsmEngine,
        table: &str,
        alias: Option<&str>,
        index_column: &str,
        filter: &Option<FilterExpr>,
    ) -> Result<Vec<Map<String, Value>>> {
        let class_hierarchy = self.get_class_hierarchy(engine, table);

        // Try to use the index for the primary lookup across the class hierarchy
        if let Some(f) = filter {
            let mut all_pkeys: Vec<Vec<u8>> = Vec::new();
            for scan_class in &class_hierarchy {
                if engine.has_index(scan_class, index_column) {
                    if let Some(pkeys) = Self::try_index_scan_single(engine, scan_class, f)? {
                        all_pkeys.extend(pkeys);
                    }
                }
            }
            if !all_pkeys.is_empty() {
                // Use BinaryRow for filtering, only convert to Map for passing rows
                let mut rows = Vec::new();
                for pk in &all_pkeys {
                    if let Ok(Some(val_bytes)) = engine.get(pk) {
                        // Fast path: BinaryRow class check without full Map conversion
                        if let Some(brow) = BinaryRow::parse(&val_bytes) {
                            if !brow.class_in_hierarchy(&class_hierarchy) {
                                continue;
                            }
                            // BinaryRow filter evaluation
                            if let Some(true) = eval_binary_filter(&brow, f) {
                                if let Some(mut doc) = brow.to_map() {
                                    doc.insert("__pk__".to_string(), Value::String(String::from_utf8_lossy(pk).to_string()));
                                    rows.push(doc);
                                }
                                continue;
                            }
                        }
                        // Fallback: full Map parsing
                        if let Some(mut doc) = simd_parse_row(&val_bytes) {
                            if doc.get("__class__")
                                .and_then(|v| v.as_str())
                                .map(|c| class_hierarchy.contains(c))
                                .unwrap_or(false)
                            {
                                doc.insert("__pk__".to_string(), Value::String(String::from_utf8_lossy(pk).to_string()));
                                rows.push(doc);
                            }
                        }
                    }
                }
                if let Some(a) = alias {
                    Self::apply_alias(&mut rows, a);
                }
                return Ok(rows);
            }
        }

        // Fallback to full scan
        self.plan_seq_scan(engine, table, alias, filter, None)
    }

    /// Plan-driven index lookup (point query).
    fn plan_index_lookup(
        &self,
        engine: &LsmEngine,
        table: &str,
        alias: Option<&str>,
        index_column: &str,
        key: &LiteralValue,
    ) -> Result<Vec<Map<String, Value>>> {
        if engine.has_index(table, index_column) {
            let json_val = Self::literal_to_json_static(key);
            let pkeys = {
                let index_mgr = engine.index_manager().read().unwrap_or_else(|e| e.into_inner());
                index_mgr.lookup_eq_read(table, index_column, &json_val).unwrap_or_default()
            };
            let mut rows = Self::fetch_rows_by_pks(engine, &pkeys)?;
            if let Some(a) = alias {
                Self::apply_alias(&mut rows, a);
            }
            return Ok(rows);
        }
        Ok(Vec::new())
    }

    /// Plan-driven vector search.
    fn plan_vector_search(
        &self,
        engine: &LsmEngine,
        table: &str,
        column: &str,
        query_vector: &[f32],
        top_k: usize,
        filter: &Option<FilterExpr>,
    ) -> Result<Vec<Map<String, Value>>> {
        if !engine.has_vector_index(table, column) {
            return Ok(Vec::new());
        }

        let search_results = if let Some(f) = filter {
            let class_hierarchy = self.get_class_hierarchy(engine, table);
            let mut allowed_ids = HashSet::new();
            for scan_class in &class_hierarchy {
                let prefix = format!("{}::", scan_class);
                let entries = engine.scan_prefix(prefix.as_bytes())?;
                for (key, val_bytes) in &entries {
                    if let Some(ref doc) = storage_bytes_to_doc(val_bytes) {
                        if class_hierarchy.contains(doc.get("__class__").and_then(|v| v.as_str()).unwrap_or(""))
                            && self.eval_filter(engine, doc, f) {
                                allowed_ids.insert(key.clone());
                            }
                    }
                }
            }
            engine.vector_index_manager().read().unwrap_or_else(|e| e.into_inner()).search_filtered(table, column, query_vector, top_k, &allowed_ids)?
        } else {
            engine.vector_index_manager().read().unwrap_or_else(|e| e.into_inner()).search(table, column, query_vector, top_k)?
        };

        let mut rows = Vec::new();
        for result in &search_results {
            if let Ok(Some(val_bytes)) = engine.get(&result.entry.id) {
                if let Some(mut doc) = storage_bytes_to_doc(&val_bytes) {
                    doc.insert("_distance".to_string(), serde_json::json!(result.distance));
                    rows.push(doc);
                }
            }
        }
        Ok(rows)
    }

    /// Nested loop join on pre-fetched rows.
    fn execute_nested_loop_join(
        left_rows: Vec<Map<String, Value>>,
        right_rows: Vec<Map<String, Value>>,
        join: &crate::parser::JoinClause,
    ) -> Result<Vec<Map<String, Value>>> {
        let _left_alias = None::<&str>;
        let right_alias = join.alias.as_deref().unwrap_or(&join.table);
        let (left_col, right_col) = Self::resolve_join_columns(&join.on)?;
        let join_type = join.join_type;

        let mut result = Vec::new();
        for left_row in &left_rows {
            let left_val = Self::resolve_column_value(left_row, &left_col);
            let mut matched = false;

            for right_row in &right_rows {
                let right_val = Self::resolve_column_value(right_row, &right_col);
                if left_val.is_some() && right_val.is_some() && left_val == right_val {
                    matched = true;
                    let mut merged = Map::new();
                    for (k, v) in left_row {
                        merged.insert(k.clone(), v.clone());
                    }
                    for (k, v) in right_row {
                        let key = format!("{}.{}", right_alias, k);
                        merged.insert(key, v.clone());
                        if !merged.contains_key(k) {
                            merged.insert(k.clone(), v.clone());
                        }
                    }
                    result.push(merged);
                }
            }

            // For LEFT JOIN: emit left row with NULLs for right columns if no match
            if !matched && join_type == crate::parser::JoinType::Left {
                let mut merged = Map::new();
                for (k, v) in left_row {
                    merged.insert(k.clone(), v.clone());
                }
                // Add NULL values for right side columns
                if let Some(right_sample) = right_rows.first() {
                    for k in right_sample.keys() {
                        let key = format!("{}.{}", right_alias, k);
                        merged.insert(key, Value::Null);
                        if !merged.contains_key(k) {
                            merged.insert(k.clone(), Value::Null);
                        }
                    }
                }
                result.push(merged);
            }
        }

        // For RIGHT JOIN: emit right rows with NULLs for left columns if no match
        if join_type == crate::parser::JoinType::Right {
            for right_row in &right_rows {
                let right_val = Self::resolve_column_value(right_row, &right_col);
                let mut matched = false;
                for left_row in &left_rows {
                    let left_val = Self::resolve_column_value(left_row, &left_col);
                    if left_val.is_some() && right_val.is_some() && left_val == right_val {
                        matched = true;
                        break;
                    }
                }
                if !matched {
                    let mut merged = Map::new();
                    // Add NULL values for left side columns
                    if let Some(left_sample) = left_rows.first() {
                        for k in left_sample.keys() {
                            merged.insert(k.clone(), Value::Null);
                        }
                    }
                    for (k, v) in right_row {
                        let key = format!("{}.{}", right_alias, k);
                        merged.insert(key, v.clone());
                        if !merged.contains_key(k) {
                            merged.insert(k.clone(), v.clone());
                        }
                    }
                    result.push(merged);
                }
            }
        }

        Ok(result)
    }

    /// Hash join on pre-fetched rows.
    fn execute_hash_join_rows(
        left_rows: Vec<Map<String, Value>>,
        right_rows: Vec<Map<String, Value>>,
        join: &crate::parser::JoinClause,
    ) -> Result<Vec<Map<String, Value>>> {
        let right_alias = join.alias.as_deref().unwrap_or(&join.table);
        let (left_col, right_col) = Self::resolve_join_columns(&join.on)?;
        let join_type = join.join_type;

        // Build hash table on right side
        let mut hash_table: std::collections::HashMap<String, Vec<&Map<String, Value>>> =
            std::collections::HashMap::new();
        for right_row in &right_rows {
            if let Some(val) = Self::resolve_column_value(right_row, &right_col) {
                hash_table.entry(val).or_default().push(right_row);
            }
        }

        // Track which right rows were matched (for RIGHT/FULL JOIN)
        let mut matched_right: std::collections::HashSet<usize> = std::collections::HashSet::new();

        // Probe with left rows
        let mut result = Vec::new();
        for left_row in &left_rows {
            let left_val = Self::resolve_column_value(left_row, &left_col);
            let mut matched = false;

            if let Some(left_val) = &left_val {
                if let Some(matching_rights) = hash_table.get(left_val) {
                    for right_row in matching_rights {
                        matched = true;
                        // Track matched right row index
                        if let Some(idx) = right_rows.iter().position(|r| std::ptr::eq(r, *right_row)) {
                            matched_right.insert(idx);
                        }
                        let mut merged = Map::new();
                        for (k, v) in left_row {
                            merged.insert(k.clone(), v.clone());
                        }
                        for (k, v) in *right_row {
                            let key = format!("{}.{}", right_alias, k);
                            merged.insert(key, v.clone());
                            if !merged.contains_key(k) {
                                merged.insert(k.clone(), v.clone());
                            }
                        }
                        result.push(merged);
                    }
                }
            }

            // For LEFT JOIN: emit left row with NULLs if no match
            if !matched && join_type == crate::parser::JoinType::Left {
                let mut merged = Map::new();
                for (k, v) in left_row {
                    merged.insert(k.clone(), v.clone());
                }
                if let Some(right_sample) = right_rows.first() {
                    for k in right_sample.keys() {
                        let key = format!("{}.{}", right_alias, k);
                        merged.insert(key, Value::Null);
                        if !merged.contains_key(k) {
                            merged.insert(k.clone(), Value::Null);
                        }
                    }
                }
                result.push(merged);
            }
        }

        // For RIGHT JOIN: emit unmatched right rows with NULLs for left
        if join_type == crate::parser::JoinType::Right {
            for (idx, right_row) in right_rows.iter().enumerate() {
                if !matched_right.contains(&idx) {
                    let mut merged = Map::new();
                    if let Some(left_sample) = left_rows.first() {
                        for k in left_sample.keys() {
                            merged.insert(k.clone(), Value::Null);
                        }
                    }
                    for (k, v) in right_row {
                        let key = format!("{}.{}", right_alias, k);
                        merged.insert(key, v.clone());
                        if !merged.contains_key(k) {
                            merged.insert(k.clone(), v.clone());
                        }
                    }
                    result.push(merged);
                }
            }
        }

        Ok(result)
    }

    /// Sort-merge join on pre-fetched rows.
    fn execute_sort_merge_join_rows(
        mut left_rows: Vec<Map<String, Value>>,
        mut right_rows: Vec<Map<String, Value>>,
        join: &crate::parser::JoinClause,
    ) -> Result<Vec<Map<String, Value>>> {
        let right_alias = join.alias.as_deref().unwrap_or(&join.table);
        let (left_col, right_col) = Self::resolve_join_columns(&join.on)?;
        let join_type = join.join_type;

        // Sort both sides
        left_rows.sort_by(|a, b| {
            let a_val = Self::resolve_column_value(a, &left_col).unwrap_or_default();
            let b_val = Self::resolve_column_value(b, &left_col).unwrap_or_default();
            Self::compare_values(&a_val, &b_val)
        });
        right_rows.sort_by(|a, b| {
            let a_val = Self::resolve_column_value(a, &right_col).unwrap_or_default();
            let b_val = Self::resolve_column_value(b, &right_col).unwrap_or_default();
            Self::compare_values(&a_val, &b_val)
        });

        // Track matched rows for outer joins
        let mut matched_left: std::collections::HashSet<usize> = std::collections::HashSet::new();
        let mut matched_right: std::collections::HashSet<usize> = std::collections::HashSet::new();

        // Merge
        let mut result = Vec::new();
        let mut li = 0;
        let mut ri = 0;
        while li < left_rows.len() && ri < right_rows.len() {
            let lv = Self::resolve_column_value(&left_rows[li], &left_col).unwrap_or_default();
            let rv = Self::resolve_column_value(&right_rows[ri], &right_col).unwrap_or_default();
            match Self::compare_values(&lv, &rv) {
                std::cmp::Ordering::Less => li += 1,
                std::cmp::Ordering::Greater => ri += 1,
                std::cmp::Ordering::Equal => {
                    // Handle duplicate keys
                    while ri < right_rows.len() {
                        let rv2 = Self::resolve_column_value(&right_rows[ri], &right_col).unwrap_or_default();
                        if Self::compare_values(&rv2, &rv) != std::cmp::Ordering::Equal { break; }
                        let mut li2 = li;
                        while li2 < left_rows.len() {
                            let lv2 = Self::resolve_column_value(&left_rows[li2], &left_col).unwrap_or_default();
                            if Self::compare_values(&lv2, &lv) != std::cmp::Ordering::Equal { break; }
                            matched_left.insert(li2);
                            matched_right.insert(ri);
                            let mut merged = Map::new();
                            for (k, v) in &left_rows[li2] { merged.insert(k.clone(), v.clone()); }
                            for (k, v) in &right_rows[ri] {
                                let key = format!("{}.{}", right_alias, k);
                                merged.insert(key, v.clone());
                                if !merged.contains_key(k) { merged.insert(k.clone(), v.clone()); }
                            }
                            result.push(merged);
                            li2 += 1;
                        }
                        ri += 1;
                    }
                    while li < left_rows.len() {
                        let lv2 = Self::resolve_column_value(&left_rows[li], &left_col).unwrap_or_default();
                        if Self::compare_values(&lv2, &lv) != std::cmp::Ordering::Equal { break; }
                        li += 1;
                    }
                }
            }
        }

        // For LEFT JOIN: emit unmatched left rows with NULLs
        if join_type == crate::parser::JoinType::Left {
            for (idx, left_row) in left_rows.iter().enumerate() {
                if !matched_left.contains(&idx) {
                    let mut merged = Map::new();
                    for (k, v) in left_row {
                        merged.insert(k.clone(), v.clone());
                    }
                    if let Some(right_sample) = right_rows.first() {
                        for k in right_sample.keys() {
                            let key = format!("{}.{}", right_alias, k);
                            merged.insert(key, Value::Null);
                            if !merged.contains_key(k) {
                                merged.insert(k.clone(), Value::Null);
                            }
                        }
                    }
                    result.push(merged);
                }
            }
        }

        // For RIGHT JOIN: emit unmatched right rows with NULLs
        if join_type == crate::parser::JoinType::Right {
            for (idx, right_row) in right_rows.iter().enumerate() {
                if !matched_right.contains(&idx) {
                    let mut merged = Map::new();
                    if let Some(left_sample) = left_rows.first() {
                        for k in left_sample.keys() {
                            merged.insert(k.clone(), Value::Null);
                        }
                    }
                    for (k, v) in right_row {
                        let key = format!("{}.{}", right_alias, k);
                        merged.insert(key, v.clone());
                        if !merged.contains_key(k) {
                            merged.insert(k.clone(), v.clone());
                        }
                    }
                    result.push(merged);
                }
            }
        }

        Ok(result)
    }

    /// Returns the set of classes to scan for a given table name,
    /// including all subclasses from the ontology hierarchy.
    ///
    /// Uses the inference engine's Cax-sco rule (subclass type propagation)
    /// to determine the complete class hierarchy: if x type A and A subClassOf B,
    /// then x type B. This means scanning for class A should include all subclasses.
    ///
    /// If no ontology is defined for the class, returns only the original class.
    fn get_class_hierarchy(&self, engine: &LsmEngine, table: &str) -> HashSet<String> {
        // Check cache first
        {
            let cache = self.inference_cache.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(cached) = cache.class_hierarchy.get(table) {
                tracing::debug!("get_class_hierarchy: cache hit for '{}': {:?}", table, cached);
                return cached.clone();
            }
        }

        let mut classes = HashSet::new();
        classes.insert(table.to_string());

        // Scan ALL ontologies to build complete class hierarchy
        // This handles the case where each class is stored in its own ontology
        let mut merged_ontology = onto_ontology::Ontology::new("__merged__");
        let entries = engine.scan_prefix(b"__ontology__").unwrap_or_default();
        tracing::debug!("get_class_hierarchy: found {} ontology entries", entries.len());
        for (_key, val_bytes) in entries {
            if let Ok(ontology) = onto_ontology::Ontology::from_json_slice(&val_bytes) {
                // Merge all classes into a single ontology
                for (name, class) in &ontology.classes {
                    if !merged_ontology.classes.contains_key(name) {
                        merged_ontology.classes.insert(name.clone(), class.clone());
                    }
                }
            }
        }

        tracing::debug!("get_class_hierarchy: merged ontology has {} classes", merged_ontology.classes.len());

        // Rebuild indexes on merged ontology
        merged_ontology.rebuild_indexes();

        // Now find subclasses using the merged ontology
        if merged_ontology.classes.contains_key(table) {
            tracing::debug!("get_class_hierarchy: found '{}' in merged ontology", table);
            let reasoner = Reasoner::new(merged_ontology.clone());
            let probe_triple = onto_ontology::Triple::type_of("__probe__", table);
            let result = reasoner.reason(&[probe_triple]);

            for triple in &result.all_facts {
                if triple.subject == "__probe__" && triple.predicate == "rdf:type" {
                    classes.insert(triple.object.clone());
                }
            }

            let subclasses = merged_ontology.get_all_subclasses(table);
            tracing::debug!("get_class_hierarchy: subclasses of '{}': {:?}", table, subclasses);
            classes.extend(subclasses);
        } else {
            tracing::debug!("get_class_hierarchy: '{}' NOT found in merged ontology", table);
        }

        tracing::debug!("get_class_hierarchy: final classes for '{}': {:?}", table, classes);

        // Store in cache
        {
            let mut cache = self.inference_cache.lock().unwrap_or_else(|e| e.into_inner());
            cache.class_hierarchy.insert(table.to_string(), classes.clone());
        }

        classes
    }

    /// Returns classes that are disjoint with the given class.
    fn get_disjoint_classes(&self, engine: &LsmEngine, class: &str) -> HashSet<String> {
        let mut disjoint = HashSet::new();
        let entries = engine.scan_prefix(b"__ontology__").unwrap_or_default();
        for (_key, val_bytes) in entries {
            if let Ok(ontology) = onto_ontology::Ontology::from_json_slice(&val_bytes) {
                if let Some(class_def) = ontology.classes.get(class) {
                    for d in &class_def.disjoint_with {
                        disjoint.insert(d.clone());
                    }
                }
                // Check reverse: if any class declares disjoint with our class
                for (name, class_def) in &ontology.classes {
                    if class_def.disjoint_with.contains(&class.to_string()) {
                        disjoint.insert(name.clone());
                    }
                }
            }
        }
        disjoint
    }

    /// Narrows the scan scope using __class__ filter and disjoint constraints.
    /// If the filter explicitly targets specific classes, only scan those.
    /// Exclude classes that are disjoint with the targeted classes.
    fn narrow_scan_scope(
        &self,
        engine: &LsmEngine,
        table: &str,
        filter: &Option<FilterExpr>,
        class_hierarchy: &HashSet<String>,
    ) -> HashSet<String> {
        let targeted = Self::extract_class_filter(filter);

        if targeted.is_empty() {
            // No __class__ constraint — scan full hierarchy but exclude disjoint classes
            let disjoint = self.get_disjoint_classes(engine, table);
            return class_hierarchy.difference(&disjoint).cloned().collect();
        }

        // __class__ constraint found — only scan targeted classes
        let mut scan_classes = HashSet::new();
        for target in &targeted {
            // Expand each targeted class to include its subclasses
            let hierarchy = self.get_class_hierarchy(engine, target);
            scan_classes.extend(hierarchy);
        }

        // Intersect with the original hierarchy (only scan classes that are in scope)
        let in_scope: HashSet<String> = scan_classes.intersection(class_hierarchy).cloned().collect();

        // Exclude disjoint classes for each targeted class
        let mut excluded = HashSet::new();
        for target in &targeted {
            let disjoint = self.get_disjoint_classes(engine, target);
            excluded.extend(disjoint);
        }

        in_scope.difference(&excluded).cloned().collect()
    }

    /// Extracts class names from __class__ = 'X' or __class__ IN ('X', 'Y') filter conditions.
    fn extract_class_filter(filter: &Option<FilterExpr>) -> HashSet<String> {
        let mut classes = HashSet::new();
        if let Some(f) = filter {
            Self::extract_class_from_expr(f, &mut classes);
        }
        classes
    }

    fn extract_class_from_expr(expr: &FilterExpr, classes: &mut HashSet<String>) {
        match expr {
            FilterExpr::Eq(col, LiteralValue::String(val)) if col == "__class__" => {
                classes.insert(val.clone());
            }
            FilterExpr::In(col, vals) if col == "__class__" => {
                for v in vals {
                    if let LiteralValue::String(s) = v {
                        classes.insert(s.clone());
                    }
                }
            }
            FilterExpr::And(left, right) => {
                Self::extract_class_from_expr(left, classes);
                Self::extract_class_from_expr(right, classes);
            }
            _ => {}
        }
    }

    /// Apply alias prefix to row column names.
    fn apply_alias(rows: &mut Vec<Map<String, Value>>, alias: &str) {
        for row in rows {
            let keys: Vec<String> = row.keys().cloned().collect();
            for key in keys {
                if key != "__class__" && !key.contains('.') {
                    if let Some(val) = row.remove(&key) {
                        row.insert(format!("{}.{}", alias, key), val.clone());
                        row.insert(key, val);
                    }
                }
            }
        }
    }

    /// Executes EXPLAIN: generates and returns the execution plan.
    /// Only executes the query for read-only queries (SELECT, etc.).
    /// DML queries (INSERT/UPDATE/DELETE) return the plan without execution.
    fn execute_explain(&self, query: &QueryAst, engine: &LsmEngine) -> Result<QueryResult> {
        let plan = self.planner.read().unwrap_or_else(|e| e.into_inner()).plan(query)?;
        let description = plan.describe();

        // Only execute for read-only queries; DML should not be executed in EXPLAIN
        let (actual_rows, elapsed_ms) = if Self::is_read_only_query(query) {
            let start = std::time::Instant::now();
            let actual_result = self.execute_with_engine_inner(query, engine);
            let elapsed = start.elapsed();
            let rows = match &actual_result {
                Ok(QueryResult::Rows(rows)) => rows.len(),
                _ => 0,
            };
            (rows, elapsed.as_secs_f64() * 1000.0)
        } else {
            (0, 0.0)
        };

        let plan_json = json!({
            "plan": format_plan_node(&plan.root),
            "cost": {
                "total": plan.cost.total_cost,
                "io": plan.cost.io_cost,
                "cpu": plan.cost.cpu_cost,
                "estimated_rows": plan.cost.rows,
                "actual_rows": actual_rows,
                "actual_time_ms": elapsed_ms,
            },
            "uses_index": plan.uses_index,
            "is_sorted": plan.is_sorted,
            "description": description,
        });

        Ok(QueryResult::Rows(vec![Map::from_iter(vec![
            ("plan".to_string(), plan_json),
        ])]))
    }

    /// Executes ANALYZE: collects table statistics for query optimization.
    /// Scans the table, counts rows, and collects column-level statistics.
    /// Uses class hierarchy expansion to include subclass documents.
    fn execute_analyze(&self, table: &str, engine: &LsmEngine) -> Result<QueryResult> {
        let class_hierarchy = self.get_class_hierarchy(engine, table);
        let mut row_count = 0u64;
        let mut column_stats: std::collections::HashMap<String, ColumnStats> = std::collections::HashMap::new();

        for scan_class in &class_hierarchy {
            let prefix = format!("{}::", scan_class);
            let entries = engine.scan_prefix(prefix.as_bytes()).unwrap_or_default();

            for (_key, val_bytes) in &entries {
                if let Some(doc) = simd_parse_row(val_bytes) {
                    if class_hierarchy.contains(doc.get("__class__").and_then(|v| v.as_str()).unwrap_or("")) {
                        row_count += 1;
                        for (col_name, col_value) in &doc {
                            if col_name == "__class__" { continue; }
                            let stats = column_stats.entry(col_name.clone()).or_default();
                            stats.non_null_count += 1;
                            // Track distinct values (sample up to 1000)
                            if stats.distinct_values.len() < 1000 {
                                let val_str = match col_value {
                                    serde_json::Value::String(s) => s.clone(),
                                    serde_json::Value::Number(n) => n.to_string(),
                                    serde_json::Value::Bool(b) => b.to_string(),
                                    _ => col_value.to_string(),
                                };
                                stats.distinct_values.insert(val_str);
                            }
                        }
                    }
                }
            }
        }

        // Update planner statistics
        let mut planner_stats = crate::optimizer::cost::TableStats {
            row_count,
            avg_row_size: 100,
            block_count: (row_count / 100).max(1),
            has_primary_index: false,
            secondary_indexes: Vec::new(),
            vector_indexes: Vec::new(),
        };

        // Build secondary index stats for columns with indexes
        for (col_name, col_stats) in &column_stats {
            if engine.has_index(table, col_name) {
                planner_stats.secondary_indexes.push(crate::optimizer::cost::IndexStats {
                    column: col_name.clone(),
                    cardinality: col_stats.distinct_values.len() as u64,
                    is_sorted: true,
                    tree_height: 3,
                });
            }
        }

        // Update runtime stats with table row counts
        {
            let mut stats = self.runtime_stats.lock().unwrap_or_else(|e| e.into_inner());
            stats.table_row_counts.insert(table.to_string(), row_count);
        }

        // Update planner statistics for future query optimization
        self.planner.write().unwrap_or_else(|e| e.into_inner()).update_stats(table.to_string(), planner_stats.clone());

        let mut result_rows = Vec::new();
        let mut summary = Map::new();
        summary.insert("table".to_string(), Value::String(table.to_string()));
        summary.insert("row_count".to_string(), Value::Number(serde_json::Number::from(row_count)));
        result_rows.push(summary);

        for (col_name, col_stats) in &column_stats {
            let mut col_row = Map::new();
            col_row.insert("column".to_string(), Value::String(col_name.clone()));
            col_row.insert("non_null_count".to_string(), Value::Number(serde_json::Number::from(col_stats.non_null_count)));
            col_row.insert("distinct_count".to_string(), Value::Number(serde_json::Number::from(col_stats.distinct_values.len() as u64)));
            let selectivity = if col_stats.non_null_count > 0 {
                col_stats.distinct_values.len() as f64 / col_stats.non_null_count as f64
            } else {
                0.0
            };
            col_row.insert("selectivity".to_string(), Value::Number(
                serde_json::Number::from_f64(selectivity).unwrap_or(serde_json::Number::from(0))
            ));
            result_rows.push(col_row);
        }

        Ok(QueryResult::Rows(result_rows))
    }

    // ── Read-only variants for concurrent SELECT path (&LsmEngine) ──

    /// Read-only EXPLAIN: generates execution plan, runs inner query via read path.
    fn execute_explain_read(&self, query: &QueryAst, engine: &LsmEngine) -> Result<QueryResult> {
        let plan = self.planner.read().unwrap_or_else(|e| e.into_inner()).plan(query)?;
        let description = plan.describe();

        let start = std::time::Instant::now();
        let actual_result = self.execute_select_read(query, engine);
        let elapsed = start.elapsed();
        let elapsed_ms = elapsed.as_secs_f64() * 1000.0;

        let actual_rows = match &actual_result {
            Ok(QueryResult::Rows(rows)) => rows.len(),
            _ => 0,
        };

        let plan_json = json!({
            "plan": format_plan_node(&plan.root),
            "cost": {
                "total": plan.cost.total_cost,
                "io": plan.cost.io_cost,
                "cpu": plan.cost.cpu_cost,
                "estimated_rows": plan.cost.rows,
                "actual_rows": actual_rows,
                "actual_time_ms": elapsed_ms,
            },
            "uses_index": plan.uses_index,
            "is_sorted": plan.is_sorted,
            "description": description,
        });

        Ok(QueryResult::Rows(vec![Map::from_iter(vec![
            ("plan".to_string(), plan_json),
        ])]))
    }

    /// Read-only ANALYZE: collects table statistics without engine mutation.
    fn execute_analyze_read(&self, table: &str, engine: &LsmEngine) -> Result<QueryResult> {
        let class_hierarchy = self.get_class_hierarchy_read(engine, table);
        let mut row_count = 0u64;
        let mut column_stats: std::collections::HashMap<String, ColumnStats> = std::collections::HashMap::new();

        for scan_class in &class_hierarchy {
            let prefix = format!("{}::", scan_class);
            let entries = engine.scan_prefix(prefix.as_bytes()).unwrap_or_default();
            for (_key, val_bytes) in &entries {
                if let Some(doc) = simd_parse_row(val_bytes) {
                    if class_hierarchy.contains(doc.get("__class__").and_then(|v| v.as_str()).unwrap_or("")) {
                        row_count += 1;
                        for (col_name, col_value) in &doc {
                            if col_name == "__class__" { continue; }
                            let stats = column_stats.entry(col_name.clone()).or_default();
                            stats.non_null_count += 1;
                            if stats.distinct_values.len() < 1000 {
                                let val_str = match col_value {
                                    serde_json::Value::String(s) => s.clone(),
                                    serde_json::Value::Number(n) => n.to_string(),
                                    serde_json::Value::Bool(b) => b.to_string(),
                                    _ => col_value.to_string(),
                                };
                                stats.distinct_values.insert(val_str);
                            }
                        }
                    }
                }
            }
        }

        let mut planner_stats = crate::optimizer::cost::TableStats {
            row_count,
            avg_row_size: 100,
            block_count: (row_count / 100).max(1),
            has_primary_index: false,
            secondary_indexes: Vec::new(),
            vector_indexes: Vec::new(),
        };

        for (col_name, col_stats) in &column_stats {
            if engine.has_index(table, col_name) {
                planner_stats.secondary_indexes.push(crate::optimizer::cost::IndexStats {
                    column: col_name.clone(),
                    cardinality: col_stats.distinct_values.len() as u64,
                    is_sorted: true,
                    tree_height: 3,
                });
            }
        }

        {
            let mut stats = self.runtime_stats.lock().unwrap_or_else(|e| e.into_inner());
            stats.table_row_counts.insert(table.to_string(), row_count);
        }
        self.planner.write().unwrap_or_else(|e| e.into_inner()).update_stats(table.to_string(), planner_stats.clone());

        let mut result_rows = Vec::new();
        let mut summary = Map::new();
        summary.insert("table".to_string(), Value::String(table.to_string()));
        summary.insert("row_count".to_string(), Value::Number(serde_json::Number::from(row_count)));
        result_rows.push(summary);

        for (col_name, col_stats) in &column_stats {
            let mut col_row = Map::new();
            col_row.insert("column".to_string(), Value::String(col_name.clone()));
            col_row.insert("non_null_count".to_string(), Value::Number(serde_json::Number::from(col_stats.non_null_count)));
            col_row.insert("distinct_count".to_string(), Value::Number(serde_json::Number::from(col_stats.distinct_values.len() as u64)));
            let selectivity = if col_stats.non_null_count > 0 {
                col_stats.distinct_values.len() as f64 / col_stats.non_null_count as f64
            } else {
                0.0
            };
            col_row.insert("selectivity".to_string(), Value::Number(
                serde_json::Number::from_f64(selectivity).unwrap_or(serde_json::Number::from(0))
            ));
            result_rows.push(col_row);
        }

        Ok(QueryResult::Rows(result_rows))
    }

    /// Refreshes planner stats for a table after index creation/deletion.
    /// Scans the table to collect row count and index info, then updates the planner.
    fn refresh_index_stats(&self, engine: &LsmEngine, table: &str) {
        let class_hierarchy = self.get_class_hierarchy(engine, table);
        let mut row_count: u64 = 0;
        let mut column_stats: HashMap<String, ColumnStats> = HashMap::new();

        for class_name in &class_hierarchy {
            let prefix = format!("{}::", class_name);
            if let Ok(entries) = engine.scan_prefix(prefix.as_bytes()) {
                for (_key, val_bytes) in &entries {
                    row_count += 1;
                    if let Some(doc) = simd_parse_row(val_bytes) {
                        for (col_name, col_val) in &doc {
                            if col_name.starts_with("__") { continue; }
                            let stats = column_stats.entry(col_name.clone()).or_insert_with(|| ColumnStats {
                                non_null_count: 0,
                                distinct_values: HashSet::new(),
                            });
                            stats.non_null_count += 1;
                            let val_str = match col_val {
                                Value::String(s) => s.clone(),
                                other => format!("{}", other),
                            };
                            stats.distinct_values.insert(val_str);
                        }
                    }
                }
            }
        }

        let mut planner_stats = crate::optimizer::cost::TableStats {
            row_count,
            avg_row_size: 100,
            block_count: (row_count / 100).max(1),
            has_primary_index: false,
            secondary_indexes: Vec::new(),
            vector_indexes: Vec::new(),
        };

        for (col_name, col_stats) in &column_stats {
            if engine.has_index(table, col_name) {
                planner_stats.secondary_indexes.push(crate::optimizer::cost::IndexStats {
                    column: col_name.clone(),
                    cardinality: col_stats.distinct_values.len() as u64,
                    is_sorted: true,
                    tree_height: 3,
                });
            }
        }

        self.planner.write().unwrap_or_else(|e| e.into_inner()).update_stats(table.to_string(), planner_stats);
    }

    /// Read-only plan execution with &LsmEngine.
    fn execute_plan_read(&self, plan: &ExecutionPlan, engine: &LsmEngine) -> Result<QueryResult> {
        let rows = self.execute_plan_node_read(&plan.root, engine)?;
        self.check_memory_budget(&rows)?;
        Ok(QueryResult::Rows(rows))
    }

    /// Read-only plan node execution with &LsmEngine.
    fn execute_plan_node_read(&self, node: &PlanNode, engine: &LsmEngine) -> Result<Vec<Map<String, Value>>> {
        match node {
            PlanNode::SeqScan { table, alias, filter, .. } => {
                self.plan_seq_scan_read(engine, table, alias.as_deref(), filter, None)
            }
            PlanNode::IndexScan { table, alias, index_column, filter, .. } => {
                self.plan_index_scan_read(engine, table, alias.as_deref(), index_column, filter)
            }
            PlanNode::IndexLookup { table, alias, index_column, key, .. } => {
                self.plan_index_lookup_read(engine, table, alias.as_deref(), index_column, key)
            }
            PlanNode::VectorSearch { table, column, query_vector, top_k, filter, .. } => {
                self.plan_vector_search_read(engine, table, column, query_vector, *top_k, filter)
            }
            PlanNode::Filter { input, predicate, .. } => {
                let mut rows = self.execute_plan_node_read(input, engine)?;
                rows.retain(|row| self.eval_filter_read(engine, row, predicate));
                Ok(rows)
            }
            PlanNode::Projection { input, columns, .. } => {
                let has_aggregates = Self::columns_have_aggregates(columns);
                let has_expr = match columns {
                    SelectColumns::Columns(items) => items.iter().any(|item| matches!(item, SelectItem::Expression(_))),
                    _ => false,
                };

                // Fast path: pure COUNT(*) — count rows without materializing Maps
                if has_aggregates && Self::is_pure_count_star(columns) {
                    if let PlanNode::SeqScan { table, filter, .. } = input.as_ref() {
                        return self.plan_seq_scan_count_only(engine, table, filter, columns);
                    }
                }

                // Only push column projection down when there are no aggregates/expressions
                let raw_rows = if !has_aggregates && !has_expr {
                    if let PlanNode::SeqScan { table, alias, filter, .. } = input.as_ref() {
                        self.plan_seq_scan_read(engine, table, alias.as_deref(), filter, Some(columns))?
                    } else {
                        self.execute_plan_node_read(input, engine)?
                    }
                } else {
                    self.execute_plan_node_read(input, engine)?
                };
                if has_aggregates {
                    Ok(raw_rows)
                } else if has_expr {
                    // Skip expression evaluation in read path (requires engine access for subqueries)
                    let mut result = Vec::new();
                    for raw_row in &raw_rows {
                        let mut projected = Map::new();
                        if let SelectColumns::Columns(items) = columns {
                            for item in items {
                                if let SelectItem::Column(col) = item {
                                    let (real_col, alias_part) = if let Some(as_pos) = col.find(" as ") {
                                        (&col[..as_pos], Some(col[as_pos + 4..].trim()))
                                    } else {
                                        (col.as_str(), None)
                                    };
                                    if let Some(val) = raw_row.get(real_col) {
                                        let name = alias_part.unwrap_or(real_col);
                                        let name = name.split('.').next_back().unwrap_or(name);
                                        projected.insert(name.to_string(), val.clone());
                                    }
                                }
                            }
                        }
                        result.push(projected);
                    }
                    Ok(result)
                } else {
                    let projected: Vec<Map<String, Value>> = raw_rows.iter()
                        .map(|row| self.project_columns(row, columns))
                        .collect();
                    Ok(projected)
                }
            }
            PlanNode::NestedLoopJoin { left, right, join_clause, .. } => {
                let left_rows = self.execute_plan_node_read(left, engine)?;
                let right_rows = self.execute_plan_node_read(right, engine)?;
                Self::execute_nested_loop_join(left_rows, right_rows, join_clause)
            }
            PlanNode::HashJoin { left, right, join_clause, .. } => {
                let left_rows = self.execute_plan_node_read(left, engine)?;
                let right_rows = self.execute_plan_node_read(right, engine)?;
                Self::execute_hash_join_rows(left_rows, right_rows, join_clause)
            }
            PlanNode::SortMergeJoin { left, right, join_clause, .. } => {
                let left_rows = self.execute_plan_node_read(left, engine)?;
                let right_rows = self.execute_plan_node_read(right, engine)?;
                Self::execute_sort_merge_join_rows(left_rows, right_rows, join_clause)
            }
            PlanNode::Sort { input, order_by, .. } => {
                let mut rows = self.execute_plan_node_read(input, engine)?;
                for ob in order_by.iter().rev() {
                    Self::sort_rows(&mut rows, &ob.column, ob.ascending);
                }
                Ok(rows)
            }
            PlanNode::Aggregation { input, .. } => {
                self.execute_plan_node_read(input, engine)
            }
            PlanNode::Limit { input, .. } => {
                self.execute_plan_node_read(input, engine)
            }
            PlanNode::Union { left, right, all, .. } => {
                let mut left_rows = self.execute_plan_node_read(left, engine)?;
                let right_rows = self.execute_plan_node_read(right, engine)?;
                left_rows.extend(right_rows);
                if !all {
                    Self::dedup_rows(&mut left_rows);
                }
                Ok(left_rows)
            }
            PlanNode::WindowFunction { input, windows, .. } => {
                let mut rows = self.execute_plan_node_read(input, engine)?;
                let parser_windows: Vec<crate::parser::WindowExpr> = windows.iter().map(|w| {
                    crate::parser::WindowExpr {
                        func: w.func.clone(),
                        arg: w.arg.clone(),
                        over: w.over.clone(),
                        alias: w.alias.clone(),
                    }
                }).collect();
                let window_refs: Vec<&crate::parser::WindowExpr> = parser_windows.iter().collect();
                Self::execute_window_functions(&mut rows, &window_refs);
                Ok(rows)
            }
        }
    }

    /// Read-only sequential scan with &LsmEngine.
    /// When `projected_columns` is Some, only those columns are converted from BinaryRow,
    /// avoiding the cost of deserializing all fields.
    fn plan_seq_scan_read(
        &self,
        engine: &LsmEngine,
        table: &str,
        alias: Option<&str>,
        filter: &Option<FilterExpr>,
        projected_columns: Option<&SelectColumns>,
    ) -> Result<Vec<Map<String, Value>>> {
        // Sharding: check if this table is sharded and route accordingly
        {
            let shard_router = self.shard_router.read().unwrap_or_else(|e| e.into_inner());
            if let Some(router) = shard_router.as_ref() {
                use onto_sharding::strategy::ShardTarget;
                let target = router.route_scan(table);
                match target {
                    ShardTarget::Single(shard) => {
                        if !router.is_local(shard) {
                            tracing::debug!(table = table, shard = shard, "shard not local, skipping scan");
                            return Ok(Vec::new());
                        }
                    }
                    ShardTarget::Multi(shards) => {
                        let local_shards: Vec<_> = shards.into_iter().filter(|s| router.is_local(*s)).collect();
                        if local_shards.is_empty() {
                            tracing::debug!(table = table, "no local shards for multi-shard scan");
                            return Ok(Vec::new());
                        }
                        tracing::debug!(table = table, shards = ?local_shards, "scanning local shards");
                    }
                    ShardTarget::All => {
                        // Scan all local shards
                    }
                }
            }
        }

        let class_hierarchy = self.get_class_hierarchy_read(engine, table);

        // Pre-extract simple filter column names for fast byte-level rejection.
        let fast_filter_cols = Self::extract_fast_filter_columns(filter);

        // Extract projected column names for selective BinaryRow conversion.
        // This avoids converting ALL fields when only a subset is needed.
        let proj_cols: Vec<String> = match projected_columns {
            Some(SelectColumns::Columns(items)) => {
                let mut cols = Vec::with_capacity(items.len() + 4);
                for item in items {
                    match item {
                        SelectItem::Column(col) => {
                            // Strip " as alias" suffix
                            let real = if let Some(pos) = col.find(" as ") { &col[..pos] } else { col.as_str() };
                            // Strip "table." prefix
                            let name = real.split('.').next_back().unwrap_or(real).to_string();
                            if !cols.contains(&name) { cols.push(name); }
                        }
                        SelectItem::Aggregate(a) => {
                            if let Some(ref alias) = a.alias {
                                if !cols.contains(alias) { cols.push(alias.clone()); }
                            }
                        }
                        _ => {}
                    }
                }
                cols
            }
            _ => Vec::new(), // SELECT * — use full to_map()
        };

        let mut rows = Vec::new();
        let mut scan_count: u64 = 0;
        for scan_class in &class_hierarchy {
            let prefix = format!("{}::", scan_class);
            let entries = engine.scan_prefix(prefix.as_bytes())?;
            for (key, val_bytes) in &entries {
                // Check query timeout every 1024 rows (cheap: one atomic load)
                scan_count += 1;
                if scan_count & 0x3FF == 0 {
                    self.check_timeout()?;
                }
                // Tier 1: fast byte-level rejection
                if !fast_filter_cols.is_empty()
                    && Self::fast_filter_reject(val_bytes, filter, &fast_filter_cols) {
                        tracing::debug!("fast_filter_reject: rejected row");
                        continue;
                    }
                // Tier 2: BinaryRow path (for binary-stored data) — no JSON parsing
                if let Some(brow) = BinaryRow::parse(val_bytes) {
                    if !brow.class_in_hierarchy(&class_hierarchy) {
                        tracing::debug!("class_in_hierarchy: rejected row, class={:?}", brow.class_value());
                        continue;
                    }
                    if let Some(f) = filter {
                        let filter_result = eval_binary_filter(&brow, f);
                        tracing::debug!("eval_binary_filter: result={:?}, filter={:?}", filter_result, f);
                        match filter_result {
                            Some(true) => {}
                            Some(false) => continue,
                            None => {
                                // Filter can't be evaluated from BinaryRow alone — need full map
                                if let Some(mut doc) = brow.to_map() {
                                    let filter_pass = self.eval_filter_read(engine, &doc, f);
                                    tracing::debug!("eval_filter_read: result={}", filter_pass);
                                    if !filter_pass { continue; }
                                    doc.insert("__pk__".to_string(), Value::String(String::from_utf8_lossy(key).to_string()));
                                    // Add shard routing info
                                    if let Some(router) = self.shard_router.read().unwrap_or_else(|e| e.into_inner()).as_ref() {
                                        let pk = String::from_utf8_lossy(key);
                                        let target = router.route_key(table, pk.as_bytes());
                                        doc.insert("__shard__".to_string(), Value::String(format!("{:?}", target)));
                                    }
                                    rows.push(doc);
                                }
                                continue;
                            }
                        }
                    }
                    // Use projected conversion when column list is available
                    if let Some(mut doc) = brow.to_map_projected(&proj_cols) {
                        doc.insert("__pk__".to_string(), Value::String(String::from_utf8_lossy(key).to_string()));
                        // Add shard routing info
                        if let Some(router) = self.shard_router.read().unwrap_or_else(|e| e.into_inner()).as_ref() {
                            let pk = String::from_utf8_lossy(key);
                            let target = router.route_key(table, pk.as_bytes());
                            doc.insert("__shard__".to_string(), Value::String(format!("{:?}", target)));
                        }
                        rows.push(doc);
                    }
                    continue;
                }
                // Tier 3: JSON fallback (legacy data stored as JSON)
                if let Ok(serde_json::Value::Object(mut doc)) = serde_json::from_slice::<serde_json::Value>(val_bytes) {
                    if !class_hierarchy.contains(doc.get("__class__").and_then(|v| v.as_str()).unwrap_or("")) {
                        continue;
                    }
                    if let Some(f) = filter {
                        if !self.eval_filter_read(engine, &doc, f) { continue; }
                    }
                    doc.insert("__pk__".to_string(), Value::String(String::from_utf8_lossy(key).to_string()));
                    // Add shard routing info
                    if let Some(router) = self.shard_router.read().unwrap_or_else(|e| e.into_inner()).as_ref() {
                        let pk = String::from_utf8_lossy(key);
                        let target = router.route_key(table, pk.as_bytes());
                        doc.insert("__shard__".to_string(), Value::String(format!("{:?}", target)));
                    }
                    rows.push(doc);
                }
            }
        }

        if let Some(a) = alias {
            for row in &mut rows {
                let keys: Vec<String> = row.keys().cloned().collect();
                for key in keys {
                    if key != "__class__" {
                        if let Some(val) = row.remove(&key) {
                            row.insert(format!("{}.{}", a, key), val.clone());
                            row.insert(key, val);
                        }
                    }
                }
            }
        }

        Ok(rows)
    }

    /// Returns true if the columns represent a pure `COUNT(*)` with no other items.
    /// Used to trigger the count-only fast path that skips row materialization.
    fn is_pure_count_star(columns: &SelectColumns) -> bool {
        match columns {
            SelectColumns::Columns(items) => {
                items.len() == 1
                    && matches!(
                        &items[0],
                        SelectItem::Aggregate(a) if a.func == AggregateFunc::Count && a.arg == "*"
                    )
            }
            _ => false,
        }
    }

    /// Count-only scan: counts matching rows without materializing Maps.
    /// For `SELECT COUNT(*) FROM T WHERE filter`, this avoids all deserialization
    /// after `eval_binary_filter` determines the row passes.
    fn plan_seq_scan_count_only(
        &self,
        engine: &LsmEngine,
        table: &str,
        filter: &Option<FilterExpr>,
        columns: &SelectColumns,
    ) -> Result<Vec<Map<String, Value>>> {
        let class_hierarchy = self.get_class_hierarchy_read(engine, table);
        let fast_filter_cols = Self::extract_fast_filter_columns(filter);
        let mut count: u64 = 0;

        for scan_class in &class_hierarchy {
            let prefix = format!("{}::", scan_class);
            let entries = engine.scan_prefix(prefix.as_bytes())?;
            for (_key, val_bytes) in &entries {
                // Tier 1: fast byte-level rejection
                if !fast_filter_cols.is_empty()
                    && Self::fast_filter_reject(val_bytes, filter, &fast_filter_cols) {
                        continue;
                    }
                // Tier 2: BinaryRow path — filter without full deserialization
                if let Some(brow) = BinaryRow::parse(val_bytes) {
                    if !brow.class_in_hierarchy(&class_hierarchy) {
                        continue;
                    }
                    if let Some(f) = filter {
                        match eval_binary_filter(&brow, f) {
                            Some(true) => { count += 1; }
                            Some(false) => continue,
                            None => {
                                // Filter needs full deserialization — rare case
                                if let Some(doc) = brow.to_map() {
                                    if self.eval_filter_read(engine, &doc, f) {
                                        count += 1;
                                    }
                                }
                            }
                        }
                    } else {
                        count += 1;
                    }
                    continue;
                }
                // Tier 3: JSON fallback
                if let Ok(serde_json::Value::Object(doc)) = serde_json::from_slice::<serde_json::Value>(val_bytes) {
                    if !class_hierarchy.contains(doc.get("__class__").and_then(|v| v.as_str()).unwrap_or("")) {
                        continue;
                    }
                    if let Some(f) = filter {
                        if !self.eval_filter_read(engine, &doc, f) { continue; }
                    }
                    count += 1;
                }
            }
        }

        // Return a single row with the count, using alias if provided
        let count_key = match columns {
            SelectColumns::Columns(items) => {
                if let Some(SelectItem::Aggregate(a)) = items.first() {
                    a.alias.clone().unwrap_or_else(|| "count(*)".to_string())
                } else {
                    "count(*)".to_string()
                }
            }
            _ => "count(*)".to_string(),
        };
        let mut result_row = Map::new();
        result_row.insert(count_key, Value::Number(serde_json::Number::from(count)));
        Ok(vec![result_row])
    }

    /// Extracts column names from simple comparison filters for fast byte-level rejection.
    /// Returns columns that can be checked without full JSON deserialization.
    fn extract_fast_filter_columns(filter: &Option<FilterExpr>) -> Vec<String> {
        let mut cols = Vec::new();
        if let Some(f) = filter {
            Self::collect_filter_columns(f, &mut cols);
        }
        // Only use fast filter for non-system columns
        cols.retain(|c| c != "__class__" && !c.starts_with("__"));
        cols.dedup();
        cols
    }

    fn collect_filter_columns(expr: &FilterExpr, cols: &mut Vec<String>) {
        match expr {
            FilterExpr::Eq(c, _)
            | FilterExpr::Ne(c, _)
            | FilterExpr::Gt(c, _)
            | FilterExpr::Lt(c, _)
            | FilterExpr::Gte(c, _)
            | FilterExpr::Lte(c, _)
            | FilterExpr::Like(c, _)
            | FilterExpr::IsNull(c)
            | FilterExpr::IsNotNull(c) => cols.push(c.clone()),
            FilterExpr::Between(c, _, _) => cols.push(c.clone()),
            FilterExpr::In(c, _) => cols.push(c.clone()),
            FilterExpr::And(l, r) | FilterExpr::Or(l, r) => {
                Self::collect_filter_columns(l, cols);
                Self::collect_filter_columns(r, cols);
            }
            FilterExpr::Not(e) => Self::collect_filter_columns(e, cols),
            _ => {}
        }
    }

    /// Fast byte-level filter rejection: extracts the filter column value from raw JSON
    /// bytes and checks the predicate without full deserialization.
    /// Returns true if the row should be REJECTED (definitely doesn't match).
    fn fast_filter_reject(
        val_bytes: &[u8],
        filter: &Option<FilterExpr>,
        _fast_cols: &[String],
    ) -> bool {
        let Some(f) = filter else { return false };
        Self::fast_filter_reject_expr(val_bytes, f)
    }

    fn fast_filter_reject_expr(val_bytes: &[u8], expr: &FilterExpr) -> bool {
        match expr {
            FilterExpr::Gt(col, lit)
            | FilterExpr::Gte(col, lit)
            | FilterExpr::Lt(col, lit)
            | FilterExpr::Lte(col, lit) => {
                if let Some(raw) = Self::extract_json_field(val_bytes, col) {
                    if let Some(field_num) = Self::parse_json_number(raw) {
                        if let Some(lit_num) = Self::literal_to_f64(lit) {
                            let reject = match expr {
                                FilterExpr::Gt(..) => field_num <= lit_num,
                                FilterExpr::Gte(..) => field_num < lit_num,
                                FilterExpr::Lt(..) => field_num >= lit_num,
                                FilterExpr::Lte(..) => field_num > lit_num,
                                _ => false,
                            };
                            return reject;
                        }
                    }
                }
                false
            }
            FilterExpr::Eq(col, lit) => {
                if let Some(raw) = Self::extract_json_field(val_bytes, col) {
                    match lit {
                        LiteralValue::Int(n) => {
                            if let Some(field_num) = Self::parse_json_number(raw) {
                                return field_num != (*n as f64);
                            }
                        }
                        LiteralValue::Float(n) => {
                            if let Some(field_num) = Self::parse_json_number(raw) {
                                return field_num != *n;
                            }
                        }
                        LiteralValue::String(s) => {
                            if let Some(field_str) = Self::parse_json_string(raw) {
                                return field_str != s.as_str();
                            }
                        }
                        _ => {}
                    }
                }
                false
            }
            FilterExpr::And(l, r) => {
                // For AND: reject if either side rejects
                Self::fast_filter_reject_expr(val_bytes, l)
                    || Self::fast_filter_reject_expr(val_bytes, r)
            }
            _ => false,
        }
    }

    /// Fast byte-level filter acceptance: checks if a row DEFINITELY matches the filter
    /// without creating a Map<String, Value>. Returns true if the filter is fully resolved
    /// and the row matches. Returns false if the filter can't be resolved at byte level
    /// (caller must fall back to full deserialization).
    #[allow(dead_code)]
    fn fast_filter_accept(val_bytes: &[u8], filter: &Option<FilterExpr>) -> bool {
        let Some(f) = filter else { return false };
        Self::fast_filter_accept_expr(val_bytes, f)
    }

    fn fast_filter_accept_expr(val_bytes: &[u8], expr: &FilterExpr) -> bool {
        match expr {
            FilterExpr::Gt(col, lit)
            | FilterExpr::Gte(col, lit)
            | FilterExpr::Lt(col, lit)
            | FilterExpr::Lte(col, lit) => {
                if let Some(raw) = Self::extract_json_field(val_bytes, col) {
                    if let Some(field_num) = Self::parse_json_number(raw) {
                        if let Some(lit_num) = Self::literal_to_f64(lit) {
                            return match expr {
                                FilterExpr::Gt(..) => field_num > lit_num,
                                FilterExpr::Gte(..) => field_num >= lit_num,
                                FilterExpr::Lt(..) => field_num < lit_num,
                                FilterExpr::Lte(..) => field_num <= lit_num,
                                _ => false,
                            };
                        }
                    }
                }
                false
            }
            FilterExpr::Eq(col, lit) => {
                if let Some(raw) = Self::extract_json_field(val_bytes, col) {
                    match lit {
                        LiteralValue::Int(n) => {
                            if let Some(field_num) = Self::parse_json_number(raw) {
                                return field_num == (*n as f64);
                            }
                        }
                        LiteralValue::Float(n) => {
                            if let Some(field_num) = Self::parse_json_number(raw) {
                                return field_num == *n;
                            }
                        }
                        LiteralValue::String(s) => {
                            if let Some(field_str) = Self::parse_json_string(raw) {
                                return field_str == *s;
                            }
                        }
                        LiteralValue::Bool(b) => {
                            let trimmed = raw.trim_ascii();
                            return if *b { trimmed == b"true" } else { trimmed == b"false" };
                        }
                        _ => {}
                    }
                }
                false
            }
            FilterExpr::And(l, r) => {
                Self::fast_filter_accept_expr(val_bytes, l)
                    && Self::fast_filter_accept_expr(val_bytes, r)
            }
            _ => false,
        }
    }

    /// Extracts a field's raw JSON value from serialized bytes.
    /// Looks for "field_name": <value> pattern.
    /// Returns the raw bytes of the value (number, string with quotes, etc.).
    fn extract_json_field<'a>(json_bytes: &'a [u8], field: &str) -> Option<&'a [u8]> {
        let json_str = std::str::from_utf8(json_bytes).ok()?;
        // Build the search pattern: "field_name":
        let pattern = format!("\"{}\":", field);
        let start = json_str.find(&pattern)?;
        let value_start = start + pattern.len();
        // Skip whitespace
        let value_start = value_start + json_str[value_start..].chars()
            .take_while(|c| c.is_whitespace())
            .map(|c| c.len_utf8())
            .sum::<usize>();
        if value_start >= json_str.len() {
            return None;
        }
        let rest = &json_str[value_start..];
        let value_end = if rest.starts_with('"') {
            // String value: find closing quote (handle escaped quotes)
            let mut end = 1;
            let bytes = rest.as_bytes();
            while end < bytes.len() {
                if bytes[end] == b'\\' {
                    end += 2;
                } else if bytes[end] == b'"' {
                    end += 1;
                    break;
                } else {
                    end += 1;
                }
            }
            end
        } else if rest.starts_with('[') {
            // Array: find matching bracket
            let mut depth = 0i32;
            let mut end = 0;
            for b in rest.bytes() {
                match b {
                    b'[' => depth += 1,
                    b']' => { depth -= 1; if depth == 0 { end += 1; break; } }
                    _ => {}
                }
                end += 1;
            }
            end
        } else if rest.starts_with('{') {
            // Object: find matching brace
            let mut depth = 0i32;
            let mut end = 0;
            for b in rest.bytes() {
                match b {
                    b'{' => depth += 1,
                    b'}' => { depth -= 1; if depth == 0 { end += 1; break; } }
                    _ => {}
                }
                end += 1;
            }
            end
        } else {
            // Number, bool, null: read until comma or closing brace
            rest.find([',', '}', ']']).unwrap_or(rest.len())
        };
        Some(&json_bytes[value_start..value_start + value_end])
    }

    /// Parses a raw JSON number from bytes (without quotes).
    fn parse_json_number(raw: &[u8]) -> Option<f64> {
        let s = std::str::from_utf8(raw).ok()?;
        s.trim().parse::<f64>().ok()
    }

    /// Parses a raw JSON string value (with quotes) and returns the inner content.
    fn parse_json_string(raw: &[u8]) -> Option<String> {
        let s = std::str::from_utf8(raw).ok()?;
        let s = s.trim();
        if !s.starts_with('"') || !s.ends_with('"') || s.len() < 2 {
            return None;
        }
        let inner = &s[1..s.len()-1];
        // Handle JSON escape sequences
        let mut result = String::with_capacity(inner.len());
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                match chars.next()? {
                    '"' => result.push('"'),
                    '\\' => result.push('\\'),
                    '/' => result.push('/'),
                    'n' => result.push('\n'),
                    'r' => result.push('\r'),
                    't' => result.push('\t'),
                    'b' => result.push('\u{0008}'),
                    'f' => result.push('\u{000C}'),
                    'u' => {
                        // Parse 4 hex digits
                        let hex: String = chars.by_ref().take(4).collect();
                        let code = u32::from_str_radix(&hex, 16).ok()?;
                        result.push(char::from_u32(code)?);
                    }
                    _ => return None,
                }
            } else {
                result.push(c);
            }
        }
        Some(result)
    }

    /// Converts a LiteralValue to f64 for numeric comparison.
    fn literal_to_f64(lit: &LiteralValue) -> Option<f64> {
        match lit {
            LiteralValue::Int(n) => Some(*n as f64),
            LiteralValue::Float(n) => Some(*n),
            _ => None,
        }
    }

    /// Read-only index scan with &LsmEngine.
    fn plan_index_scan_read(
        &self,
        engine: &LsmEngine,
        table: &str,
        alias: Option<&str>,
        index_column: &str,
        filter: &Option<FilterExpr>,
    ) -> Result<Vec<Map<String, Value>>> {
        let class_hierarchy = self.get_class_hierarchy_read(engine, table);

        if let Some(f) = filter {
            let mut all_pkeys: Vec<Vec<u8>> = Vec::new();
            for scan_class in &class_hierarchy {
                if engine.has_index(scan_class, index_column) {
                    if let Some(pkeys) = Self::try_index_scan_single_read(engine, scan_class, f) {
                        all_pkeys.extend(pkeys);
                    }
                }
            }
            if !all_pkeys.is_empty() {
                let mut rows = Self::fetch_rows_by_pks_read(engine, &all_pkeys)?;
                rows.retain(|row| {
                    row.get("__class__")
                        .and_then(|v| v.as_str())
                        .map(|c| class_hierarchy.contains(c))
                        .unwrap_or(false)
                });
                // Add shard routing info
                if let Some(router) = self.shard_router.read().unwrap_or_else(|e| e.into_inner()).as_ref() {
                    for row in &mut rows {
                        if let Some(pk) = row.get("__pk__").and_then(|v| v.as_str()) {
                            let target = router.route_key(table, pk.as_bytes());
                            row.insert("__shard__".to_string(), Value::String(format!("{:?}", target)));
                        }
                    }
                }
                if let Some(a) = alias {
                    Self::apply_alias(&mut rows, a);
                }
                return Ok(rows);
            }
        }

        self.plan_seq_scan_read(engine, table, alias, filter, None)
    }

    /// Read-only index lookup with &LsmEngine.
    fn plan_index_lookup_read(
        &self,
        engine: &LsmEngine,
        table: &str,
        alias: Option<&str>,
        index_column: &str,
        key: &LiteralValue,
    ) -> Result<Vec<Map<String, Value>>> {
        if engine.has_index(table, index_column) {
            let index_mgr = engine.index_manager().read().unwrap_or_else(|e| e.into_inner());
            let json_val = Self::literal_to_json_static(key);
            let pkeys = index_mgr.lookup_eq_read(table, index_column, &json_val).unwrap_or_default();
            let mut rows = Self::fetch_rows_by_pks_read(engine, &pkeys)?;
            if let Some(a) = alias {
                Self::apply_alias(&mut rows, a);
            }
            return Ok(rows);
        }
        Ok(Vec::new())
    }

    /// Read-only index scan attempt using in-memory index only.
    fn try_index_scan_single_read(
        engine: &LsmEngine,
        class: &str,
        filter: &FilterExpr,
    ) -> Option<Vec<Vec<u8>>> {
        let col = match filter {
            FilterExpr::Eq(c, _)
            | FilterExpr::Ne(c, _)
            | FilterExpr::Gt(c, _)
            | FilterExpr::Lt(c, _)
            | FilterExpr::Gte(c, _)
            | FilterExpr::Lte(c, _)
            | FilterExpr::Between(c, _, _)
            | FilterExpr::In(c, _)
            | FilterExpr::IsNull(c)
            | FilterExpr::IsNotNull(c) => c.clone(),
            _ => return None,
        };

        if !engine.has_index(class, &col) {
            return None;
        }

        let index_mgr = engine.index_manager().read().unwrap_or_else(|e| e.into_inner());

        let pkeys: Vec<Vec<u8>> = match filter {
            FilterExpr::Eq(_, val) => {
                let json_val = Self::literal_to_json_static(val);
                index_mgr.lookup_eq_read(class, &col, &json_val).unwrap_or_default()
            }
            FilterExpr::Gt(_, val) => {
                let json_val = Self::literal_to_json_static(val);
                index_mgr.lookup_gt_read(class, &col, &json_val).unwrap_or_default()
            }
            FilterExpr::Lt(_, val) => {
                let json_val = Self::literal_to_json_static(val);
                index_mgr.lookup_lt_read(class, &col, &json_val).unwrap_or_default()
            }
            FilterExpr::Gte(_, val) => {
                let json_val = Self::literal_to_json_static(val);
                let tree = index_mgr.get_index(class, &col)?;
                let encoded = onto_storage::IndexManager::encode_value(&json_val);
                tree.gte_scan(&encoded)
            }
            FilterExpr::Lte(_, val) => {
                let json_val = Self::literal_to_json_static(val);
                let tree = index_mgr.get_index(class, &col)?;
                let encoded = onto_storage::IndexManager::encode_value(&json_val);
                tree.lte_scan(&encoded)
            }
            FilterExpr::Between(_, low, high) => {
                let low_json = Self::literal_to_json_static(low);
                let high_json = Self::literal_to_json_static(high);
                index_mgr.lookup_range_read(class, &col, Some(&low_json), Some(&high_json)).unwrap_or_default()
            }
            FilterExpr::In(_, values) => {
                let mut all_pkeys = Vec::new();
                for val in values {
                    let json_val = Self::literal_to_json_static(val);
                    if let Some(pks) = index_mgr.lookup_eq_read(class, &col, &json_val) {
                        all_pkeys.extend(pks);
                    }
                }
                all_pkeys
            }
            _ => return None,
        };

        Some(pkeys)
    }

    /// Fetches rows by primary keys using &LsmEngine (read-only).
    fn fetch_rows_by_pks_read(
        engine: &LsmEngine,
        pkeys: &[Vec<u8>],
    ) -> Result<Vec<Map<String, Value>>> {
        let mut rows = Vec::new();
        for pk in pkeys {
            if let Ok(Some(val_bytes)) = engine.get(pk) {
                if let Some(doc) = simd_parse_row(&val_bytes) {
                    rows.push(doc);
                }
            }
        }
        Ok(rows)
    }

    /// Read-only vector search with &LsmEngine.
    fn plan_vector_search_read(
        &self,
        engine: &LsmEngine,
        table: &str,
        column: &str,
        query_vector: &[f32],
        top_k: usize,
        filter: &Option<FilterExpr>,
    ) -> Result<Vec<Map<String, Value>>> {
        if !engine.has_vector_index(table, column) {
            return Ok(Vec::new());
        }

        let search_results = if let Some(f) = filter {
            let class_hierarchy = self.get_class_hierarchy_read(engine, table);
            let mut allowed_ids = HashSet::new();
            for scan_class in &class_hierarchy {
                let prefix = format!("{}::", scan_class);
                let entries = engine.scan_prefix(prefix.as_bytes())?;
                for (key, val_bytes) in &entries {
                    if let Some(ref doc) = storage_bytes_to_doc(val_bytes) {
                        if class_hierarchy.contains(doc.get("__class__").and_then(|v| v.as_str()).unwrap_or(""))
                            && self.eval_filter_read(engine, doc, f) {
                                allowed_ids.insert(key.clone());
                            }
                    }
                }
            }
            engine.vector_index_manager().read().unwrap_or_else(|e| e.into_inner()).search_filtered(table, column, query_vector, top_k, &allowed_ids)?
        } else {
            engine.vector_index_manager().read().unwrap_or_else(|e| e.into_inner()).search(table, column, query_vector, top_k)?
        };

        let mut rows = Vec::new();
        for result in &search_results {
            if let Ok(Some(val_bytes)) = engine.get(&result.entry.id) {
                if let Some(mut doc) = storage_bytes_to_doc(&val_bytes) {
                    doc.insert("_distance".to_string(), serde_json::json!(result.distance));
                    rows.push(doc);
                }
            }
        }
        Ok(rows)
    }

    /// Read-only MATCH execution with &LsmEngine.
    fn execute_match_read(
        &self,
        engine: &LsmEngine,
        _variable: &str,
        class: &str,
        filter: &Option<FilterExpr>,
        returns: &[String],
    ) -> Result<QueryResult> {
        let class_hierarchy = self.get_class_hierarchy_read(engine, class);
        let mut rows = Vec::new();
        for scan_class in &class_hierarchy {
            let prefix = format!("{}::", scan_class);
            let entries = engine.scan_prefix(prefix.as_bytes())?;
            for (key, val_bytes) in &entries {
                // Tier 1: BinaryRow path (binary-stored data) — no JSON parsing
                if let Some(brow) = BinaryRow::parse(val_bytes) {
                    if !brow.class_in_hierarchy(&class_hierarchy) {
                        continue;
                    }
                    if let Some(mut doc) = brow.to_map() {
                        doc.insert("__pk__".to_string(), Value::String(String::from_utf8_lossy(key).to_string()));
                        rows.push(doc);
                    }
                    continue;
                }
                // Tier 2: JSON fallback (legacy data stored as JSON)
                if let Ok(serde_json::Value::Object(mut doc)) = serde_json::from_slice::<serde_json::Value>(val_bytes) {
                    if class_hierarchy.contains(doc.get("__class__").and_then(|v| v.as_str()).unwrap_or("")) {
                        doc.insert("__pk__".to_string(), Value::String(String::from_utf8_lossy(key).to_string()));
                        rows.push(doc);
                    }
                }
            }
        }

        if let Some(f) = filter {
            rows.retain(|row| Self::eval_filter_static_with_hierarchy(row, f, &class_hierarchy));
        }

        if returns.is_empty() {
            let cleaned: Vec<Map<String, Value>> = rows.into_iter().map(|mut r| {
                r.remove("__pk__");
                r
            }).collect();
            Ok(QueryResult::Rows(cleaned))
        } else {
            let projected: Vec<Map<String, Value>> = rows.iter().map(|row| {
                let mut result = Map::new();
                for col in returns {
                    if let Some(val) = Self::resolve_column_value(row, col) {
                        result.insert(col.clone(), Value::String(val));
                    }
                }
                result
            }).collect();
            Ok(QueryResult::Rows(projected))
        }
    }

    /// Read-only VectorSearch execution with &LsmEngine.
    fn execute_vector_search_read(
        &self,
        engine: &LsmEngine,
        class: &str,
        column: &str,
        query_vector: &[f32],
        top_k: usize,
        filter: &Option<FilterExpr>,
    ) -> Result<QueryResult> {
        if !engine.has_vector_index(class, column) {
            return Err(CoreError::InvalidArgument(format!(
                "no vector index on {}.{}", class, column
            )));
        }

        let search_results = if let Some(filter_expr) = filter {
            let class_hierarchy = self.get_class_hierarchy_read(engine, class);
            let mut allowed_ids = HashSet::new();
            for scan_class in &class_hierarchy {
                let prefix = format!("{}::", scan_class);
                let entries = engine.scan_prefix(prefix.as_bytes())?;
                for (key, val_bytes) in &entries {
                    if let Some(ref doc) = storage_bytes_to_doc(val_bytes) {
                        if class_hierarchy.contains(doc.get("__class__").and_then(|v| v.as_str()).unwrap_or(""))
                            && Self::eval_filter_static_with_hierarchy(doc, filter_expr, &class_hierarchy) {
                                allowed_ids.insert(key.clone());
                            }
                    }
                }
            }
            engine.vector_index_manager().read().unwrap_or_else(|e| e.into_inner()).search_filtered(class, column, query_vector, top_k, &allowed_ids)?
        } else {
            engine.vector_index_manager().read().unwrap_or_else(|e| e.into_inner()).search(class, column, query_vector, top_k)?
        };

        let mut rows = Vec::new();
        for result in &search_results {
            if let Ok(Some(val_bytes)) = engine.get(&result.entry.id) {
                if let Some(mut doc) = storage_bytes_to_doc(&val_bytes) {
                    doc.insert("_distance".to_string(), serde_json::json!(result.distance));
                    rows.push(doc);
                }
            }
        }
        Ok(QueryResult::Rows(rows))
    }

    /// Read-only aggregation with &LsmEngine (uses static filter for HAVING).
    fn execute_aggregation_read(
        &self,
        engine: &LsmEngine,
        columns: &SelectColumns,
        rows: &[Map<String, Value>],
        group_by: Option<&crate::parser::GroupByClause>,
        having: &Option<FilterExpr>,
        order_by: &[crate::parser::OrderBy],
        limit: Option<usize>,
    ) -> Result<QueryResult> {
        let groups: Vec<(String, Vec<&Map<String, Value>>)> = if let Some(gb) = group_by {
            let mut group_map: std::collections::BTreeMap<String, Vec<&Map<String, Value>>> =
                std::collections::BTreeMap::new();
            for row in rows {
                let key = gb.columns.iter()
                    .map(|col| Self::resolve_column_value(row, col).unwrap_or_else(|| "NULL".to_string()))
                    .collect::<Vec<_>>()
                    .join("\x00");
                group_map.entry(key).or_default().push(row);
            }
            group_map.into_iter().collect()
        } else {
            vec![("".to_string(), rows.iter().collect())]
        };

        let mut result_rows: Vec<Map<String, Value>> = Vec::new();

        for (_group_key, group_rows) in &groups {
            let mut result_row = Map::new();

            if let Some(gb) = group_by {
                for col in &gb.columns {
                    if let Some(val) = Self::resolve_column_value(group_rows[0], col) {
                        result_row.insert(col.clone(), Value::String(val));
                    }
                }
            }

            if let SelectColumns::Columns(items) = columns {
                for item in items {
                    match item {
                        SelectItem::Aggregate(agg) => {
                            let val = Self::compute_aggregate(agg, group_rows);
                            let name = agg.alias.clone().unwrap_or_else(|| Self::default_agg_name(agg));
                            result_row.insert(name, val);
                        }
                        SelectItem::Column(col) => {
                            let col_name = col.split(" as ").last().unwrap_or(col);
                            let col_name = col_name.split('.').next_back().unwrap_or(col_name);
                            if !result_row.contains_key(col_name) {
                                if let Some(val) = Self::resolve_column_value(group_rows[0], col) {
                                    result_row.insert(col_name.to_string(), Value::String(val));
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }

            // Use read-path filter for HAVING with ontology reasoning
            if having.as_ref().is_none_or(|h| self.eval_filter_read(engine, &result_row, h)) {
                result_rows.push(result_row);
            }
        }

        for ob in order_by.iter().rev() {
            Self::sort_rows(&mut result_rows, &ob.column, ob.ascending);
        }

        if let Some(limit) = limit {
            result_rows.truncate(limit);
        }

        Ok(QueryResult::Rows(result_rows))
    }

    /// Read-only class hierarchy lookup (no mutation needed).
    fn get_class_hierarchy_read(&self, engine: &LsmEngine, table: &str) -> HashSet<String> {
        {
            let cache = self.inference_cache.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(cached) = cache.class_hierarchy.get(table) {
                return cached.clone();
            }
        }

        let mut classes = HashSet::new();
        classes.insert(table.to_string());

        // Scan ALL ontologies and merge into one (handles per-class ontology storage)
        let mut merged_ontology = onto_ontology::Ontology::new("__merged__");
        let entries = engine.scan_prefix(b"__ontology__").unwrap_or_default();
        for (_key, val_bytes) in entries {
            if let Ok(ontology) = onto_ontology::Ontology::from_json_slice(&val_bytes) {
                for (name, class) in &ontology.classes {
                    if !merged_ontology.classes.contains_key(name) {
                        merged_ontology.classes.insert(name.clone(), class.clone());
                    }
                }
            }
        }

        // Rebuild indexes on merged ontology
        merged_ontology.rebuild_indexes();

        // Now find subclasses using the merged ontology
        if merged_ontology.classes.contains_key(table) {
            let reasoner = Reasoner::new(merged_ontology.clone());
            let probe_triple = onto_ontology::Triple::type_of("__probe__", table);
            let result = reasoner.reason(&[probe_triple]);
            for triple in &result.all_facts {
                if triple.subject == "__probe__" && triple.predicate == "rdf:type" {
                    classes.insert(triple.object.clone());
                }
            }
            let subclasses = merged_ontology.get_all_subclasses(table);
            classes.extend(subclasses);
        }

        {
            let mut cache = self.inference_cache.lock().unwrap_or_else(|e| e.into_inner());
            cache.class_hierarchy.insert(table.to_string(), classes.clone());
        }

        classes
    }

    /// Read-only filter evaluation with ontology reasoning (no subquery support).
    /// Supports: Eq, Ne, Gt, Lt, Gte, Lte, Like, Between, In, IsNull, IsNotNull, Not, And, Or.
    /// Class hierarchy and property inference are applied via read-only engine access.
    fn eval_filter_read(&self, engine: &LsmEngine, doc: &Map<String, Value>, expr: &FilterExpr) -> bool {
        match expr {
            FilterExpr::Eq(col, val) => {
                doc.get(col).is_some_and(|v| {
                    if col == "__class__" {
                        self.class_value_matches_read(engine, v, val)
                    } else {
                        self.property_value_matches_read(engine, doc, col, val)
                    }
                })
            }
            FilterExpr::Ne(col, val) => {
                !doc.get(col).is_some_and(|v| {
                    if col == "__class__" {
                        self.class_value_matches_read(engine, v, val)
                    } else {
                        self.property_value_matches_read(engine, doc, col, val)
                    }
                })
            }
            FilterExpr::Gt(col, val) => {
                doc.get(col).is_some_and(|v| self.value_gt(v, val))
            }
            FilterExpr::Lt(col, val) => {
                doc.get(col).is_some_and(|v| self.value_lt(v, val))
            }
            FilterExpr::Gte(col, val) => {
                doc.get(col).is_some_and(|v| self.value_gt(v, val) || self.value_matches(v, val))
            }
            FilterExpr::Lte(col, val) => {
                doc.get(col).is_some_and(|v| self.value_lt(v, val) || self.value_matches(v, val))
            }
            FilterExpr::Like(col, pattern) => {
                doc.get(col).is_some_and(|v| {
                    let s = match v {
                        Value::String(s) => s.clone(),
                        _ => v.to_string(),
                    };
                    Self::like_match(&s, pattern)
                })
            }
            FilterExpr::Between(col, low, high) => {
                doc.get(col).is_some_and(|v| {
                    self.value_gte(v, low) && self.value_lte(v, high)
                })
            }
            FilterExpr::In(col, values) => {
                doc.get(col).is_some_and(|v| {
                    if col == "__class__" {
                        values.iter().any(|val| self.class_value_matches_read(engine, v, val))
                    } else {
                        values.iter().any(|val| self.property_value_matches_read(engine, doc, col, val))
                    }
                })
            }
            FilterExpr::IsNull(col) => {
                doc.get(col).is_none_or(|v| matches!(v, Value::Null))
            }
            FilterExpr::IsNotNull(col) => {
                doc.get(col).is_some_and(|v| !matches!(v, Value::Null))
            }
            FilterExpr::Not(expr) => {
                !self.eval_filter_read(engine, doc, expr)
            }
            FilterExpr::And(left, right) => {
                self.eval_filter_read(engine, doc, left) && self.eval_filter_read(engine, doc, right)
            }
            FilterExpr::Or(left, right) => {
                self.eval_filter_read(engine, doc, left) || self.eval_filter_read(engine, doc, right)
            }
            _ => true, // Subquery filters (EXISTS, IN subquery) pass through in read path
        }
    }

    /// Read-only class value matching with ontology hierarchy.
    fn class_value_matches_read(&self, engine: &LsmEngine, v: &Value, lit: &LiteralValue) -> bool {
        match (v, lit) {
            (Value::String(doc_class), LiteralValue::String(target_class)) => {
                if doc_class == target_class {
                    return true;
                }
                let hierarchy = self.get_class_hierarchy_read(engine, target_class);
                hierarchy.contains(doc_class.as_str())
            }
            _ => self.value_matches(v, lit),
        }
    }

    /// Read-only property value matching with ontology inference (subproperty, inverse, symmetric).
    fn property_value_matches_read(
        &self,
        engine: &LsmEngine,
        doc: &Map<String, Value>,
        col: &str,
        val: &LiteralValue,
    ) -> bool {
        // 1. Direct match
        if let Some(v) = doc.get(col) {
            if self.value_matches(v, val) {
                return true;
            }
        }

        // 2. Equivalent/subproperty match
        let aliases = self.get_property_aliases_read(engine, col);
        for alias in &aliases {
            if alias != col {
                if let Some(v) = doc.get(alias) {
                    if self.value_matches(v, val) {
                        return true;
                    }
                }
            }
        }

        false
    }

    /// Read-only property aliases lookup (subproperty/equivalent property).
    fn get_property_aliases_read(&self, engine: &LsmEngine, property: &str) -> HashSet<String> {
        {
            let cache = self.inference_cache.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(cached) = cache.property_aliases.get(property) {
                return cached.clone();
            }
        }

        let mut aliases = HashSet::new();
        aliases.insert(property.to_string());

        let entries = engine.scan_prefix(b"__ontology__").unwrap_or_default();
        for (_key, val_bytes) in entries {
            if let Ok(ontology) = onto_ontology::Ontology::from_json_slice(&val_bytes) {
                if let Some(prop_def) = ontology.properties.get(property) {
                    for equiv in &prop_def.equivalent_properties {
                        aliases.insert(equiv.clone());
                    }
                    for (name, other_prop) in &ontology.properties {
                        if other_prop.subproperty_of.contains(&property.to_string()) {
                            aliases.insert(name.clone());
                        }
                    }
                }
                if let Some(prop_def) = ontology.properties.get(property) {
                    for parent in &prop_def.subproperty_of {
                        aliases.insert(parent.clone());
                        if let Some(parent_def) = ontology.properties.get(parent) {
                            for equiv in &parent_def.equivalent_properties {
                                aliases.insert(equiv.clone());
                            }
                        }
                    }
                }
            }
        }

        {
            let mut cache = self.inference_cache.lock().unwrap_or_else(|e| e.into_inner());
            cache.property_aliases.insert(property.to_string(), aliases.clone());
        }

        aliases
    }

    /// Executes a WITH clause (Common Table Expression).
    /// CTEs are materialized into temporary storage, then the main query runs.
    fn execute_with_ctes(
        &self,
        ctes: &[crate::parser::CteDefinition],
        query: &QueryAst,
        engine: &LsmEngine,
        _recursive: bool,
    ) -> Result<QueryResult> {
        // Materialize each CTE: execute the query and store results under a temp key
        for cte in ctes {
            let cte_result = self.execute_with_engine_inner(&cte.query, engine)?;
            if let QueryResult::Rows(rows) = cte_result {
                // Store CTE results as a temporary "table" using a special prefix
                let prefix = format!("__cte_{}::", cte.name.to_lowercase());
                // Clear any previous CTE with this name
                let existing = engine.scan_prefix(prefix.as_bytes()).unwrap_or_default();
                for (key, _) in existing {
                    engine.delete(key)?;
                }
                // Insert each row as a CTE entry
                for (i, row) in rows.iter().enumerate() {
                    let key = format!("{}{:010}", prefix, i);
                    let value = doc_to_storage_bytes(row);
                    engine.put(key.as_bytes().to_vec(), value)?;
                }
            }
        }

        // Execute the main query 鈥?it will scan CTE tables via the prefix scan path
        // We need to handle CTE name resolution in the main query
        let result = self.execute_with_engine_inner(query, engine);

        // Clean up CTE temporary data
        for cte in ctes {
            let prefix = format!("__cte_{}::", cte.name.to_lowercase());
            let existing = engine.scan_prefix(prefix.as_bytes()).unwrap_or_default();
            for (key, _) in existing {
                engine.delete(key)?;
            }
        }

        result
    }

    /// Executes a statement within an existing transaction, with an optional pre-computed plan.
    /// When a cached plan is provided for SELECT queries, skips re-planning.
    fn execute_in_txn_with_plan(
        &self,
        ast: &QueryAst,
        engine: &LsmEngine,
        txn_id: u64,
        cached_plan: Option<&ExecutionPlan>,
    ) -> Result<QueryResult> {
        // For SELECT queries with a cached plan, use it directly
        if let (QueryAst::Select { .. }, Some(plan)) = (ast, cached_plan) {
            let plan_result = self.execute_plan(plan, engine)?;
            let mut rows = match plan_result {
                QueryResult::Rows(r) => r,
                other => return Ok(other),
            };

            // Post-processing: aggregation, window functions, DISTINCT, OFFSET/LIMIT
            if let QueryAst::Select {
                distinct, columns, group_by, having, order_by, limit, offset, ..
            } = ast {
                let has_aggregates = Self::columns_have_aggregates(columns);

                // Fast path: COUNT(*) was already computed by plan_seq_scan_count_only.
                if has_aggregates && group_by.is_none()
                    && rows.len() == 1
                    && Self::is_pure_count_star(columns)
                {
                    if let Some(lim) = limit {
                        rows.truncate(*lim);
                    }
                    return Ok(QueryResult::Rows(rows));
                }

                if group_by.is_some() || has_aggregates {
                    let result = self.execute_aggregation(engine, columns, &rows, group_by.as_ref(), having, order_by, *limit)?;
                    if let QueryResult::Rows(mut agg_rows) = result {
                        if *distinct { Self::dedup_rows(&mut agg_rows); }
                        return Ok(QueryResult::Rows(agg_rows));
                    }
                    return Ok(result);
                }

                if let SelectColumns::Columns(items) = columns {
                    let window_exprs: Vec<&WindowExpr> = items.iter().filter_map(|item| {
                        if let SelectItem::WindowFunction(w) = item { Some(w) } else { None }
                    }).collect();
                    if !window_exprs.is_empty() {
                        Self::execute_window_functions(&mut rows, &window_exprs);
                    }
                }

                if *distinct { Self::dedup_rows(&mut rows); }

                if let Some(off) = offset {
                    if *off < rows.len() {
                        rows = rows.split_off(*off);
                    } else {
                        rows.clear();
                    }
                }
                if let Some(lim) = limit {
                    rows.truncate(*lim);
                }
            }

            return Ok(QueryResult::Rows(rows));
        }

        // Fallback to regular execution
        self.execute_in_txn(ast, engine, txn_id)
    }

    /// Executes a statement within an existing transaction.
    fn execute_in_txn(&self, ast: &QueryAst, engine: &LsmEngine, txn_id: u64) -> Result<QueryResult> {
        match ast {
            QueryAst::Insert {
                class,
                columns,
                values,
            } => self.execute_insert_txn(engine, txn_id, class, columns, values),
            QueryAst::BatchInsert {
                class,
                columns,
                rows,
            } => {
                let mut total = 0;
                for row in rows {
                    self.execute_insert_txn(engine, txn_id, class, columns, row)?;
                    total += 1;
                }
                Ok(QueryResult::Success(format!("{} row(s) inserted", total)))
            }
            QueryAst::InsertSelect {
                class,
                columns,
                query,
            } => {
                // Execute the SELECT query first
                let select_result = self.execute_with_engine_inner(query, engine)?;
                if let QueryResult::Rows(rows) = select_result {
                    let count = rows.len();
                    for row in &rows {
                        let values: Vec<crate::parser::LiteralValue> = columns.iter().map(|col| {
                            match row.get(col) {
                                Some(serde_json::Value::String(s)) => crate::parser::LiteralValue::String(s.clone()),
                                Some(serde_json::Value::Number(n)) => {
                                    if let Some(i) = n.as_i64() {
                                        crate::parser::LiteralValue::Int(i)
                                    } else {
                                        crate::parser::LiteralValue::Float(n.as_f64().unwrap_or(0.0))
                                    }
                                }
                                Some(serde_json::Value::Bool(b)) => crate::parser::LiteralValue::Bool(*b),
                                _ => crate::parser::LiteralValue::Null,
                            }
                        }).collect();
                        self.execute_insert_txn(engine, txn_id, class, columns, &values)?;
                    }
                    Ok(QueryResult::Success(format!("{} row(s) inserted from SELECT", count)))
                } else {
                    Ok(QueryResult::Success("0 rows inserted".to_string()))
                }
            }
            QueryAst::Upsert {
                class,
                columns,
                values,
                conflict_column,
                assignments,
            } => {
                // Check if a row with the conflict column value already exists
                let conflict_val = columns.iter().position(|c| c == conflict_column)
                    .and_then(|pos| values.get(pos));
                if let Some(val) = conflict_val {
                    // Search for existing row
                    let prefix = format!("{}::", class);
                    let entries = engine.txn_scan_prefix(txn_id, prefix.as_bytes())?;
                    let mut existing_key: Option<Vec<u8>> = None;
                    for (key, val_bytes) in &entries {
                        if let Ok(serde_json::Value::Object(doc)) = serde_json::from_slice::<serde_json::Value>(val_bytes) {
                            if doc.get("__class__").and_then(|v| v.as_str()) == Some(class) {
                                if let Some(existing_val) = doc.get(conflict_column) {
                                    let matches = match (existing_val, val) {
                                        (serde_json::Value::String(s), crate::parser::LiteralValue::String(l)) => s == l,
                                        (serde_json::Value::Number(n), crate::parser::LiteralValue::Int(l)) => n.as_i64() == Some(*l),
                                        _ => false,
                                    };
                                    if matches {
                                        existing_key = Some(key.clone());
                                        break;
                                    }
                                }
                            }
                        }
                    }
                    if let Some(key) = existing_key {
                        // Update existing row
                        if let Ok(Some(val_bytes)) = engine.txn_get(txn_id, &key) {
                            if let Ok(serde_json::Value::Object(mut doc)) = serde_json::from_slice::<serde_json::Value>(&val_bytes) {
                                for (col, assign_val) in assignments {
                                    doc.insert(col.clone(), self.literal_to_json(assign_val));
                                }
                                let new_value = doc_to_storage_bytes(&doc);
                                engine.txn_put(txn_id, key, new_value)?;
                                return Ok(QueryResult::Success("1 row updated (upsert)".to_string()));
                            }
                        }
                    }
                }
                // No conflict - insert normally
                self.execute_insert_txn(engine, txn_id, class, columns, values)
            }
            QueryAst::BatchUpsert {
                class,
                columns,
                rows,
                conflict_column,
                assignments,
            } => {
                let mut total = 0;
                for row in rows {
                    // Check if a row with the conflict column value already exists
                    let conflict_val = columns.iter().position(|c| c == conflict_column)
                        .and_then(|pos| row.get(pos));
                    if let Some(val) = conflict_val {
                        let prefix = format!("{}::", class);
                        let entries = engine.txn_scan_prefix(txn_id, prefix.as_bytes())?;
                        let mut existing_key: Option<Vec<u8>> = None;
                        for (key, val_bytes) in &entries {
                            if let Ok(serde_json::Value::Object(doc)) = serde_json::from_slice::<serde_json::Value>(val_bytes) {
                                if doc.get("__class__").and_then(|v| v.as_str()) == Some(class) {
                                    if let Some(existing_val) = doc.get(conflict_column) {
                                        let matches = match (existing_val, val) {
                                            (serde_json::Value::String(s), crate::parser::LiteralValue::String(l)) => s == l,
                                            (serde_json::Value::Number(n), crate::parser::LiteralValue::Int(l)) => n.as_i64() == Some(*l),
                                            _ => false,
                                        };
                                        if matches {
                                            existing_key = Some(key.clone());
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                        if let Some(key) = existing_key {
                            // Update existing row
                            if let Ok(Some(val_bytes)) = engine.txn_get(txn_id, &key) {
                                if let Ok(serde_json::Value::Object(mut doc)) = serde_json::from_slice::<serde_json::Value>(&val_bytes) {
                                    for (col, assign_val) in assignments {
                                        doc.insert(col.clone(), self.literal_to_json(assign_val));
                                    }
                                    let new_value = doc_to_storage_bytes(&doc);
                                    engine.txn_put(txn_id, key, new_value)?;
                                    total += 1;
                                    continue;
                                }
                            }
                        }
                    }
                    // No conflict - insert normally
                    self.execute_insert_txn(engine, txn_id, class, columns, row)?;
                    total += 1;
                }
                Ok(QueryResult::Success(format!("{} row(s) inserted/updated (batch upsert)", total)))
            }
            QueryAst::Import { class, file_path, format } => {
                self.execute_import(engine, txn_id, class, file_path, *format)
            }
            QueryAst::Copy { class, file_path, format } => {
                self.execute_copy(engine, class, file_path, *format)
            }
            QueryAst::Select {
                distinct,
                columns,
                from: _,
                from_alias: _,
                joins: _,
                filter: _,
                group_by,
                having,
                order_by,
                limit,
                offset,
                ..
            } => {
                // Phase 24: Use plan-driven execution
                // Generate execution plan, then execute it
                let plan = self.planner.read().unwrap_or_else(|e| e.into_inner()).plan(ast)?;

                // Execute the plan to get core result rows
                let plan_result = self.execute_plan(&plan, engine)?;
                let mut rows = match plan_result {
                    QueryResult::Rows(r) => r,
                    other => return Ok(other),
                };

                // Post-processing steps not yet in the plan:
                // 1. Aggregation (GROUP BY + HAVING)
                let has_aggregates = Self::columns_have_aggregates(columns);

                // Fast path: COUNT(*) was already computed by plan_seq_scan_count_only.
                if has_aggregates && group_by.is_none()
                    && rows.len() == 1
                    && Self::is_pure_count_star(columns)
                {
                    if let Some(lim) = limit {
                        rows.truncate(*lim);
                    }
                    return Ok(QueryResult::Rows(rows));
                }

                if group_by.is_some() || has_aggregates {
                    let result = self.execute_aggregation(engine, columns, &rows, group_by.as_ref(), having, order_by, *limit)?;
                    if let QueryResult::Rows(mut agg_rows) = result {
                        if *distinct { Self::dedup_rows(&mut agg_rows); }
                        return Ok(QueryResult::Rows(agg_rows));
                    }
                    return Ok(result);
                }

                // 2. Window functions (ValueExpr expressions are handled by Projection)
                if let SelectColumns::Columns(items) = columns {
                    let window_exprs: Vec<&WindowExpr> = items.iter().filter_map(|item| {
                        if let SelectItem::WindowFunction(w) = item { Some(w) } else { None }
                    }).collect();
                    if !window_exprs.is_empty() {
                        Self::execute_window_functions(&mut rows, &window_exprs);
                    }
                }

                // 4. DISTINCT
                if *distinct { Self::dedup_rows(&mut rows); }

                // 5. OFFSET then LIMIT (correct pagination order)
                if let Some(off) = offset {
                    if *off < rows.len() {
                        rows = rows.split_off(*off);
                    } else {
                        rows.clear();
                    }
                }
                if let Some(lim) = limit {
                    rows.truncate(*lim);
                }

                Ok(QueryResult::Rows(rows))
            }
            QueryAst::Delete { class, filter } => {
                // Plan-driven DELETE: scan + filter via plan, then delete matching rows
                let scan_plan = self.build_scan_plan(class, filter)?;
                let plan_result = self.execute_plan(&scan_plan, engine)?;
                let rows = match plan_result {
                    QueryResult::Rows(r) => r,
                    other => return Ok(other),
                };
                let mut deleted = 0usize;
                for row in &rows {
                    if let Some(Value::String(pk)) = row.get("__pk__") {
                        // Sync to graph store (unified entity anchor)
                        if let Some(ref graph) = self.graph {
                            let entity_id = onto_core::EntityId::new(class, pk);
                            let _ = graph.delete_vertex_by_entity(&entity_id);
                        }
                        engine.txn_delete(txn_id, pk.as_bytes().to_vec())?;
                        deleted += 1;
                    }
                }
                Ok(QueryResult::Success(format!("{} row(s) deleted", deleted)))
            }
            QueryAst::Update {
                class,
                assignments,
                filter,
            } => {
                // Plan-driven UPDATE: scan + filter via plan, then update matching rows
                let scan_plan = self.build_scan_plan(class, filter)?;
                let plan_result = self.execute_plan(&scan_plan, engine)?;
                let rows = match plan_result {
                    QueryResult::Rows(r) => r,
                    other => return Ok(other),
                };
                let mut updated = 0usize;
                for row in &rows {
                    if let Some(Value::String(pk)) = row.get("__pk__") {
                        let mut doc = row.clone();
                        doc.remove("__pk__"); // Don't persist the virtual pk column
                        for (col, val) in assignments {
                            let json_val = self.literal_to_json(val);
                            let json_val = Self::try_parse_vector(json_val);
                            doc.insert(col.clone(), json_val);
                        }
                        self.validate_document(engine, class, &doc)?;
                        let new_value = doc_to_storage_bytes(&doc);
                        engine.txn_put(txn_id, pk.as_bytes().to_vec(), new_value)?;
                        updated += 1;
                    }
                }
                Ok(QueryResult::Success(format!("{} row(s) updated", updated)))
            }
            QueryAst::Match {
                variable,
                class,
                filter,
                returns,
            } => self.execute_match_txn(engine, txn_id, variable, class, filter, returns),
            QueryAst::VectorSearch {
                class,
                column,
                query_vector,
                top_k,
                filter,
            } => self.execute_vector_search_txn(engine, txn_id, class, column, query_vector, *top_k, filter),
            _ => Err(onto_core::CoreError::InvalidArgument(
                "unsupported statement type in transaction".to_string(),
            )),
        }
    }

    /// Executes UNION [ALL] by running both queries and merging results.
    fn execute_union(&self, engine: &LsmEngine, left: &QueryAst, right: &QueryAst, all: bool) -> Result<QueryResult> {
        let left_result = self.execute_with_engine_inner(left, engine)?;
        let right_result = self.execute_with_engine_inner(right, engine)?;

        let mut rows = match left_result {
            QueryResult::Rows(r) => r,
            _ => vec![],
        };

        let right_rows = match right_result {
            QueryResult::Rows(r) => r,
            _ => vec![],
        };

        rows.extend(right_rows);

        if !all {
            let mut seen = std::collections::HashSet::new();
            rows.retain(|row| {
                let key: String = row
                    .iter()
                    .map(|(k, v)| format!("{}={}", k, v))
                    .collect::<Vec<_>>()
                    .join("\x00");
                seen.insert(key)
            });
        }

        Ok(QueryResult::Rows(rows))
    }

    /// Removes duplicate rows based on all column values.
    fn dedup_rows(rows: &mut Vec<Map<String, Value>>) {
        let mut seen = std::collections::HashSet::new();
        rows.retain(|row| {
            let key: Vec<String> = row
                .iter()
                .map(|(k, v)| format!("{}={}", k, v))
                .collect();
            seen.insert(key.join("\x00"))
        });
    }

    /// Sorts rows by a column. Tries numeric comparison first, falls back to string.
    fn sort_rows(rows: &mut Vec<Map<String, Value>>, col: &str, ascending: bool) {
        rows.sort_by(|a, b| {
            let a_val = a.get(col);
            let b_val = b.get(col);
            let ord = Self::compare_values_direct(a_val, b_val);
            if ascending { ord } else { ord.reverse() }
        });
    }

    /// Checks if the SELECT columns contain any aggregate functions.
    fn columns_have_aggregates(columns: &SelectColumns) -> bool {
        match columns {
            SelectColumns::All => false,
            SelectColumns::Columns(items) => items.iter().any(|item| matches!(item, SelectItem::Aggregate(_))),
        }
    }

    /// Executes aggregation: GROUP BY + aggregate functions + HAVING.
    fn execute_aggregation(
        &self,
        engine: &LsmEngine,
        columns: &SelectColumns,
        rows: &[Map<String, Value>],
        group_by: Option<&crate::parser::GroupByClause>,
        having: &Option<FilterExpr>,
        order_by: &[crate::parser::OrderBy],
        limit: Option<usize>,
    ) -> Result<QueryResult> {
        // Group rows by GROUP BY columns (or single group if no GROUP BY)
        // Optimized: use value_to_group_key directly instead of resolve_column_value
        let groups: Vec<(String, Vec<&Map<String, Value>>)> = if let Some(gb) = group_by {
            let mut group_map: std::collections::BTreeMap<String, Vec<&Map<String, Value>>> =
                std::collections::BTreeMap::new();
            for row in rows {
                let key = gb
                    .columns
                    .iter()
                    .map(|col| Self::value_to_group_key(row.get(col)))
                    .collect::<Vec<_>>()
                    .join("\x00");
                group_map.entry(key).or_default().push(row);
            }
            group_map.into_iter().collect()
        } else {
            // No GROUP BY: single group containing all rows
            vec![("".to_string(), rows.iter().collect())]
        };

        // Compute aggregates for each group
        let mut result_rows: Vec<Map<String, Value>> = Vec::new();

        for (_group_key, group_rows) in &groups {
            let mut result_row = Map::new();

            // Add GROUP BY columns to result
            if let Some(gb) = group_by {
                for col in &gb.columns {
                    if let Some(val) = Self::resolve_column_value(group_rows[0], col) {
                        result_row.insert(col.clone(), Value::String(val));
                    }
                }
            }

            // Compute aggregates
            if let SelectColumns::Columns(items) = columns {
                for item in items {
                    match item {
                        SelectItem::Aggregate(agg) => {
                            let val = Self::compute_aggregate(agg, group_rows);
                            let name = agg
                                .alias
                                .clone()
                                .unwrap_or_else(|| Self::default_agg_name(agg));
                            result_row.insert(name, val);
                        }
                        SelectItem::Column(col) => {
                            // For non-aggregate columns in GROUP BY result,
                            // the value comes from the first row (already added above)
                            // Only add if not already added by GROUP BY
                            let col_name = col.split(" as ").last().unwrap_or(col);
                            let col_name = col_name.split('.').next_back().unwrap_or(col_name);
                            if !result_row.contains_key(col_name) {
                                if let Some(val) = Self::resolve_column_value(group_rows[0], col) {
                                    result_row.insert(col_name.to_string(), Value::String(val));
                                }
                            }
                        }
                        SelectItem::WindowFunction(_window) => {
                            // Window functions are handled separately after aggregation
                            // Skip for now in GROUP BY context
                        }
                        SelectItem::Expression(_expr) => {
                            // Expressions are evaluated during projection
                        }
                    }
                }
            }

            // Apply HAVING filter
            if self.matches_filter(engine, &result_row, having) {
                result_rows.push(result_row);
            }
        }

        // Sort if ORDER BY specified (multi-column)
        for ob in order_by.iter().rev() {
            Self::sort_rows(&mut result_rows, &ob.column, ob.ascending);
        }

        // Apply LIMIT
        if let Some(limit) = limit {
            result_rows.truncate(limit);
        }

        Ok(QueryResult::Rows(result_rows))
    }

    /// Computes a single aggregate value for a group of rows.
    fn compute_aggregate(agg: &crate::parser::AggregateExpr, rows: &[&Map<String, Value>]) -> Value {
        match agg.func {
            AggregateFunc::Count => {
                if agg.arg == "*" {
                    Value::Number(serde_json::Number::from(rows.len()))
                } else {
                    let count = rows
                        .iter()
                        .filter(|row| Self::resolve_column_value(row, &agg.arg).is_some())
                        .count();
                    Value::Number(serde_json::Number::from(count))
                }
            }
            AggregateFunc::Sum => {
                let sum: f64 = rows
                    .iter()
                    .filter_map(|row| Self::resolve_column_value(row, &agg.arg))
                    .filter_map(|v| v.parse::<f64>().ok())
                    .sum();
                if sum.fract() == 0.0 {
                    Value::Number(serde_json::Number::from(sum.clamp(i64::MIN as f64, i64::MAX as f64) as i64))
                } else {
                    Value::Number(
                        serde_json::Number::from_f64(sum).unwrap_or(serde_json::Number::from(0)),
                    )
                }
            }
            AggregateFunc::Avg => {
                let values: Vec<f64> = rows
                    .iter()
                    .filter_map(|row| Self::resolve_column_value(row, &agg.arg))
                    .filter_map(|v| v.parse::<f64>().ok())
                    .collect();
                if values.is_empty() {
                    Value::Null
                } else {
                    let avg = values.iter().sum::<f64>() / values.len() as f64;
                    Value::Number(
                        serde_json::Number::from_f64(avg).unwrap_or(serde_json::Number::from(0)),
                    )
                }
            }
            AggregateFunc::Min => {
                let min = rows
                    .iter()
                    .filter_map(|row| Self::resolve_column_value(row, &agg.arg))
                    .min_by(|a, b| Self::compare_values(a, b));
                match min {
                    Some(v) => Value::String(v),
                    None => Value::Null,
                }
            }
            AggregateFunc::Max => {
                let max = rows
                    .iter()
                    .filter_map(|row| Self::resolve_column_value(row, &agg.arg))
                    .max_by(|a, b| Self::compare_values(a, b));
                match max {
                    Some(v) => Value::String(v),
                    None => Value::Null,
                }
            }
        }
    }

    /// Applies window functions to the result rows.
    /// For each window function:
    /// 1. Partition rows by PARTITION BY columns
    /// 2. Sort each partition by ORDER BY columns
    /// 3. Compute the window function value for each row
    fn execute_window_functions(rows: &mut Vec<Map<String, Value>>, windows: &[&WindowExpr]) {
        for window in windows {
            let alias = window.alias.clone().unwrap_or_else(|| {
                format!("{:?}()", window.func).to_lowercase()
            });

            // Partition the rows
            let partitions = Self::partition_rows(rows, &window.over.partition_by);

            // For each partition, sort and compute
            for partition_indices in &partitions {
                // Get the rows in this partition
                let mut partition_rows: Vec<(usize, Map<String, Value>)> = partition_indices
                    .iter()
                    .map(|&i| (i, rows[i].clone()))
                    .collect();

                // Sort partition by ORDER BY columns
                if !window.over.order_by.is_empty() {
                    for ob in window.over.order_by.iter().rev() {
                        partition_rows.sort_by(|a, b| {
                            let a_val = Self::resolve_column_value(&a.1, &ob.column).unwrap_or_default();
                            let b_val = Self::resolve_column_value(&b.1, &ob.column).unwrap_or_default();
                            let ord = Self::compare_values(&a_val, &b_val);
                            if ob.ascending { ord } else { ord.reverse() }
                        });
                    }
                }

                // Compute window function values
                let values = Self::compute_window_values(&window.func, window.arg.as_deref(), &partition_rows, &window.over.frame);

                // Write values back to rows
                for (i, val) in partition_indices.iter().zip(values.iter()) {
                    rows[*i].insert(alias.clone(), val.clone());
                }
            }
        }
    }

    /// Partitions rows by the given column names. Returns indices of rows in each partition.
    fn partition_rows(rows: &[Map<String, Value>], partition_by: &[String]) -> Vec<Vec<usize>> {
        if partition_by.is_empty() {
            return vec![(0..rows.len()).collect()];
        }

        let mut partitions: std::collections::HashMap<String, Vec<usize>> = std::collections::HashMap::new();
        for (i, row) in rows.iter().enumerate() {
            let key: String = partition_by
                .iter()
                .map(|col| Self::resolve_column_value(row, col).unwrap_or_else(|| "NULL".to_string()))
                .collect::<Vec<_>>()
                .join("\x00");
            partitions.entry(key).or_default().push(i);
        }
        partitions.into_values().collect()
    }

    /// Computes window function values for a partition.
    fn compute_window_values(
        func: &WindowFunc,
        arg: Option<&str>,
        partition: &[(usize, Map<String, Value>)],
        _frame: &Option<crate::parser::WindowFrame>,
    ) -> Vec<Value> {
        let n = partition.len();
        let mut values = Vec::with_capacity(n);

        for idx in 0..n {
            let val = match func {
                WindowFunc::RowNumber => Value::Number(serde_json::Number::from(idx + 1)),
                WindowFunc::Rank => {
                    // Rank: same value gets same rank, then skip
                    let _rank = 1;
                    if let Some(_ob) = partition.iter().find_map(|(_, row)| {
                        // Use the first ORDER BY column for ranking
                        Some(row)
                    }) {
                        // Simple rank: position + 1
                        Value::Number(serde_json::Number::from(idx + 1))
                    } else {
                        Value::Number(serde_json::Number::from(idx + 1))
                    }
                }
                WindowFunc::DenseRank => {
                    // Dense rank: same value gets same rank, no skip
                    Value::Number(serde_json::Number::from(idx + 1))
                }
                WindowFunc::Lag => {
                    // LAG(col, offset=1, default=NULL)
                    if idx == 0 {
                        Value::Null
                    } else {
                        let col = arg.unwrap_or("");
                        Self::resolve_column_value(&partition[idx - 1].1, col)
                            .map(Value::String)
                            .unwrap_or(Value::Null)
                    }
                }
                WindowFunc::Lead => {
                    // LEAD(col, offset=1, default=NULL)
                    if idx + 1 >= n {
                        Value::Null
                    } else {
                        let col = arg.unwrap_or("");
                        Self::resolve_column_value(&partition[idx + 1].1, col)
                            .map(Value::String)
                            .unwrap_or(Value::Null)
                    }
                }
                WindowFunc::FirstValue => {
                    let col = arg.unwrap_or("");
                    Self::resolve_column_value(&partition[0].1, col)
                        .map(Value::String)
                        .unwrap_or(Value::Null)
                }
                WindowFunc::LastValue => {
                    let col = arg.unwrap_or("");
                    Self::resolve_column_value(&partition[n - 1].1, col)
                        .map(Value::String)
                        .unwrap_or(Value::Null)
                }
                WindowFunc::NthValue => {
                    // NTH_VALUE(col, n) - n is in the arg after comma
                    let col = arg.unwrap_or("");
                    // For simplicity, return the value at the current row position
                    Self::resolve_column_value(&partition[idx].1, col)
                        .map(Value::String)
                        .unwrap_or(Value::Null)
                }
                WindowFunc::Sum => {
                    Self::compute_running_sum(arg.unwrap_or(""), partition, idx)
                }
                WindowFunc::Avg => {
                    Self::compute_running_avg(arg.unwrap_or(""), partition, idx)
                }
                WindowFunc::Min => {
                    Self::compute_running_min(arg.unwrap_or(""), partition, idx)
                }
                WindowFunc::Max => {
                    Self::compute_running_max(arg.unwrap_or(""), partition, idx)
                }
                WindowFunc::Count => {
                    Value::Number(serde_json::Number::from(idx + 1))
                }
            };
            values.push(val);
        }
        values
    }

    /// Computes running sum up to and including the current row.
    fn compute_running_sum(col: &str, partition: &[(usize, Map<String, Value>)], end: usize) -> Value {
        let sum: f64 = partition[..=end]
            .iter()
            .filter_map(|(_, row)| Self::resolve_column_value(row, col))
            .filter_map(|v| v.parse::<f64>().ok())
            .sum();
        if sum.fract() == 0.0 {
            Value::Number(serde_json::Number::from(sum.clamp(i64::MIN as f64, i64::MAX as f64) as i64))
        } else {
            Value::Number(serde_json::Number::from_f64(sum).unwrap_or(serde_json::Number::from(0)))
        }
    }

    /// Computes running average up to and including the current row.
    fn compute_running_avg(col: &str, partition: &[(usize, Map<String, Value>)], end: usize) -> Value {
        let values: Vec<f64> = partition[..=end]
            .iter()
            .filter_map(|(_, row)| Self::resolve_column_value(row, col))
            .filter_map(|v| v.parse::<f64>().ok())
            .collect();
        if values.is_empty() {
            Value::Null
        } else {
            let avg = values.iter().sum::<f64>() / values.len() as f64;
            Value::Number(serde_json::Number::from_f64(avg).unwrap_or(serde_json::Number::from(0)))
        }
    }

    /// Computes running minimum up to and including the current row.
    fn compute_running_min(col: &str, partition: &[(usize, Map<String, Value>)], end: usize) -> Value {
        let min = partition[..=end]
            .iter()
            .filter_map(|(_, row)| Self::resolve_column_value(row, col))
            .min_by(|a, b| Self::compare_values(a, b));
        match min {
            Some(v) => Value::String(v),
            None => Value::Null,
        }
    }

    /// Computes running maximum up to and including the current row.
    fn compute_running_max(col: &str, partition: &[(usize, Map<String, Value>)], end: usize) -> Value {
        let max = partition[..=end]
            .iter()
            .filter_map(|(_, row)| Self::resolve_column_value(row, col))
            .max_by(|a, b| Self::compare_values(a, b));
        match max {
            Some(v) => Value::String(v),
            None => Value::Null,
        }
    }

    /// Returns a default column name for a ValueExpr.
    fn value_expr_default_name(expr: &ValueExpr) -> String {
        match expr {
            ValueExpr::Column(col) => col.split('.').next_back().unwrap_or(col).to_string(),
            ValueExpr::Literal(lit) => format!("{:?}", lit),
            ValueExpr::CaseWhen { .. } => "case".to_string(),
            ValueExpr::ScalarSubquery(_) => "subquery".to_string(),
            ValueExpr::Arithmetic { .. } => "expr".to_string(),
            ValueExpr::Function { name, .. } => name.to_lowercase(),
        }
    }

    /// Returns a default column name for an aggregate without alias.
    fn default_agg_name(agg: &crate::parser::AggregateExpr) -> String {
        match agg.func {
            AggregateFunc::Count => format!("count({})", agg.arg),
            AggregateFunc::Sum => format!("sum({})", agg.arg),
            AggregateFunc::Avg => format!("avg({})", agg.arg),
            AggregateFunc::Min => format!("min({})", agg.arg),
            AggregateFunc::Max => format!("max({})", agg.arg),
        }
    }

    /// Resolves join column references from the ON condition.
    /// Returns (left_column_name, right_column_name) without alias prefixes.
    fn resolve_join_columns(on: &crate::parser::JoinOn) -> Result<(String, String)> {
        let left = on.left.split('.').next_back().unwrap_or(&on.left).to_string();
        let right = on.right.split('.').next_back().unwrap_or(&on.right).to_string();
        Ok((left, right))
    }

    /// Gets a value from a row, trying both aliased and unaliased column names.
    fn resolve_column_value(row: &Map<String, Value>, col: &str) -> Option<String> {
        // Try exact match first
        if let Some(v) = row.get(col) {
            return Some(Self::value_to_sort_key(v));
        }
        // Try matching by suffix (strip alias prefix)
        for (k, v) in row {
            if k.ends_with(&format!(".{}", col)) || k == col {
                return Some(Self::value_to_sort_key(v));
            }
        }
        None
    }

    /// Converts a JSON value to a string key for comparison.
    fn value_to_sort_key(v: &Value) -> String {
        match v {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Null => "NULL".to_string(),
            _ => v.to_string(),
        }
    }

    /// Compares two value strings, trying numeric comparison first.
    fn compare_values(a: &str, b: &str) -> std::cmp::Ordering {
        // Try numeric comparison
        if let (Ok(a_num), Ok(b_num)) = (a.parse::<f64>(), b.parse::<f64>()) {
            return a_num.partial_cmp(&b_num).unwrap_or(std::cmp::Ordering::Equal);
        }
        // Fall back to string comparison
        a.cmp(b)
    }

    /// Compares two Option<&Value> directly without string conversion.
    /// Avoids allocations for numeric comparisons.
    fn compare_values_direct(a: Option<&Value>, b: Option<&Value>) -> std::cmp::Ordering {
        match (a, b) {
            (None, None) => std::cmp::Ordering::Equal,
            (None, Some(_)) => std::cmp::Ordering::Less,
            (Some(_), None) => std::cmp::Ordering::Greater,
            (Some(a_val), Some(b_val)) => {
                match (a_val, b_val) {
                    (Value::Number(a_n), Value::Number(b_n)) => {
                        // Direct numeric comparison without string conversion
                        if let (Some(a_f), Some(b_f)) = (a_n.as_f64(), b_n.as_f64()) {
                            a_f.partial_cmp(&b_f).unwrap_or(std::cmp::Ordering::Equal)
                        } else {
                            a_n.to_string().cmp(&b_n.to_string())
                        }
                    }
                    (Value::String(a_s), Value::String(b_s)) => a_s.cmp(b_s),
                    (Value::Bool(a_b), Value::Bool(b_b)) => a_b.cmp(b_b),
                    (Value::Null, Value::Null) => std::cmp::Ordering::Equal,
                    (Value::Null, _) => std::cmp::Ordering::Less,
                    (_, Value::Null) => std::cmp::Ordering::Greater,
                    // Mixed types: compare type names for stability
                    _ => {
                        let a_str = Self::value_to_sort_key(a_val);
                        let b_str = Self::value_to_sort_key(b_val);
                        a_str.cmp(&b_str)
                    }
                }
            }
        }
    }

    /// Converts an Option<&Value> to a group key string.
    /// Avoids cloning for strings, uses compact representations for numbers.
    fn value_to_group_key(val: Option<&Value>) -> String {
        match val {
            None => "NULL".to_string(),
            Some(v) => match v {
                Value::String(s) => s.clone(),
                Value::Number(n) => n.to_string(),
                Value::Bool(b) => if *b { "true" } else { "false" }.to_string(),
                Value::Null => "NULL".to_string(),
                _ => v.to_string(),
            },
        }
    }

    // 鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺?
    //  Transactional versions (use txn_* API)
    // 鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺?

    fn execute_insert_txn(
        &self,
        engine: &LsmEngine,
        txn_id: u64,
        class: &str,
        columns: &[String],
        values: &[LiteralValue],
    ) -> Result<QueryResult> {
        let key = self.generate_doc_key(class);

        let mut doc = Map::new();
        doc.insert("__class__".to_string(), json!(class));
        for (col, val) in columns.iter().zip(values.iter()) {
            let json_val = self.literal_to_json(val);
            // Auto-parse vector strings like "[0.1, 0.2, 0.3]" into JSON arrays
            let json_val = Self::try_parse_vector(json_val);
            doc.insert(col.clone(), json_val);
        }

        // Validate against ontology schema
        self.validate_document(engine, class, &doc)?;

        let value = doc_to_storage_bytes(&doc);

        // Sync to graph store (unified entity anchor)
        if let Some(ref graph) = self.graph {
            if let Some(entity_id) = onto_core::EntityId::from_lsm_key(&key) {
                let _ = graph.upsert_vertex_from_entity(&entity_id, &[class.to_string()]);
            }
        }

        // Persist triples to triple store
        if let Some(ref triple_store) = self.triple_store {
            if let Ok(pk) = std::str::from_utf8(&key) {
                // (entity, rdf:type, class)
                let _ = triple_store.add_triple(pk, "rdf:type", class);
                // (entity, prop, value) for each column
                for (col, val) in columns.iter().zip(values.iter()) {
                    let json_val = self.literal_to_json(val);
                    if let Some(s) = json_val.as_str() {
                        let _ = triple_store.add_triple(pk, col, s);
                    } else if let Some(n) = json_val.as_number() {
                        let _ = triple_store.add_triple(pk, col, &n.to_string());
                    } else if let Some(b) = json_val.as_bool() {
                        let _ = triple_store.add_triple(pk, col, &b.to_string());
                    }
                }
            }
        }

        engine.txn_put(txn_id, key, value)?;
        Ok(QueryResult::Success("1 row inserted".to_string()))
    }

    /// Validates a file path to prevent path traversal attacks.
    /// Rejects paths containing ".." components or null bytes.
    fn validate_file_path(path: &str) -> Result<()> {
        if path.contains('\0') {
            return Err(CoreError::InvalidArgument("path contains null byte".into()));
        }
        let p = std::path::Path::new(path);
        // Reject paths with ".." components
        for component in p.components() {
            if let std::path::Component::ParentDir = component {
                return Err(CoreError::InvalidArgument(
                    "path traversal detected: '..' is not allowed".into(),
                ));
            }
        }
        Ok(())
    }

    /// Executes an IMPORT command: reads a CSV or JSON file and inserts rows into the target class.
    ///
    /// CSV format: first row is header (column names), subsequent rows are data.
    /// JSON format: array of objects, or one object per line (JSON Lines).
    ///
    /// All rows are inserted within the given transaction for atomicity.
    fn execute_import(
        &self,
        engine: &LsmEngine,
        txn_id: u64,
        class: &str,
        file_path: &str,
        format: crate::parser::ImportFormat,
    ) -> Result<QueryResult> {
        use crate::parser::ImportFormat;
        Self::validate_file_path(file_path)?;

        let content = std::fs::read_to_string(file_path).map_err(|_| {
            CoreError::InvalidArgument("file not found or unreadable".to_string())
        })?;

        match format {
            ImportFormat::Csv => self.execute_import_csv(engine, txn_id, class, &content),
            ImportFormat::Json => self.execute_import_json(engine, txn_id, class, &content),
        }
    }

    /// Executes a COPY command: direct bulk load without transaction.
    ///
    /// This is the fastest import path — uses `engine.put_batch()` directly
    /// with a single lock acquisition for all rows.
    ///
    /// Syntax: COPY <class> FROM '<file_path>' (FORMAT CSV|JSON)
    fn execute_copy(
        &self,
        engine: &LsmEngine,
        class: &str,
        file_path: &str,
        format: crate::parser::ImportFormat,
    ) -> Result<QueryResult> {
        use crate::parser::ImportFormat;
        Self::validate_file_path(file_path)?;

        let start = std::time::Instant::now();
        let content = std::fs::read_to_string(file_path).map_err(|_| {
            CoreError::InvalidArgument("file not found or unreadable".to_string())
        })?;

        let entries = match format {
            ImportFormat::Csv => self.parse_csv_to_entries(class, &content)?,
            ImportFormat::Json => self.parse_json_to_entries(class, &content)?,
        };

        // Sync to graph store before batch insert
        if let Some(ref graph) = self.graph {
            for (key, _) in &entries {
                if let Some(entity_id) = onto_core::EntityId::from_lsm_key(key) {
                    let _ = graph.upsert_vertex_from_entity(&entity_id, &[class.to_string()]);
                }
            }
        }

        let imported = engine.put_batch(entries)?;
        let elapsed = start.elapsed();

        let rate = imported as f64 / elapsed.as_secs_f64();
        Ok(QueryResult::Success(format!(
            "{} row(s) copied in {:.2}s ({:.0} rows/sec)",
            imported, elapsed.as_secs_f64(), rate
        )))
    }

    /// Parse CSV content into batch entries (key, value pairs).
    fn parse_csv_to_entries(&self, class: &str, content: &str) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let mut reader = csv::ReaderBuilder::new()
            .has_headers(true)
            .from_reader(content.as_bytes());

        let headers: Vec<String> = reader.headers()
            .map_err(|e| CoreError::InvalidArgument(format!("CSV header error: {}", e)))?
            .iter()
            .map(|h| h.trim().to_string())
            .collect();

        if headers.is_empty() {
            return Err(CoreError::InvalidArgument("CSV file has no headers".to_string()));
        }

        let mut entries = Vec::new();

        for result in reader.records() {
            let record = result.map_err(|e| {
                CoreError::InvalidArgument(format!("CSV parse error: {}", e))
            })?;

            let mut doc = serde_json::Map::new();
            doc.insert("__class__".to_string(), json!(class));

            for (i, field) in record.iter().enumerate() {
                if i < headers.len() && !headers[i].is_empty() {
                    let json_val = self.literal_to_json(&Self::parse_csv_value(field));
                    let json_val = Self::try_parse_vector(json_val);
                    doc.insert(headers[i].clone(), json_val);
                }
            }

            let key = self.generate_doc_key(class);
            let value = doc_to_storage_bytes(&doc);
            entries.push((key, value));
        }

        Ok(entries)
    }

    /// Parse JSON content into batch entries (key, value pairs).
    fn parse_json_to_entries(&self, class: &str, content: &str) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let trimmed = content.trim();
        let mut entries = Vec::new();

        if trimmed.starts_with('[') {
            // JSON array
            let arr: Vec<serde_json::Value> = serde_json::from_str(trimmed)
                .map_err(|e| CoreError::InvalidArgument(format!("JSON parse error: {}", e)))?;

            for item in arr {
                if let serde_json::Value::Object(map) = item {
                    let mut doc = serde_json::Map::new();
                    doc.insert("__class__".to_string(), json!(class));
                    for (key, val) in map {
                        doc.insert(key.clone(), val);
                    }
                    let key = self.generate_doc_key(class);
                    let value = doc_to_storage_bytes(&doc);
                    entries.push((key, value));
                }
            }
        } else {
            // JSON Lines
            for line in trimmed.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                if let serde_json::Value::Object(map) = serde_json::from_str::<serde_json::Value>(line)
                    .map_err(|e| CoreError::InvalidArgument(format!("JSON parse error: {}", e)))?
                {
                    let mut doc = serde_json::Map::new();
                    doc.insert("__class__".to_string(), json!(class));
                    for (key, val) in map {
                        doc.insert(key.clone(), val);
                    }
                    let key = self.generate_doc_key(class);
                    let value = doc_to_storage_bytes(&doc);
                    entries.push((key, value));
                }
            }
        }

        Ok(entries)
    }

    /// Imports data from CSV content.
    /// First row is treated as header (column names).
    /// Uses batch API for high-throughput insertion.
    fn execute_import_csv(
        &self,
        engine: &LsmEngine,
        txn_id: u64,
        class: &str,
        content: &str,
    ) -> Result<QueryResult> {
        let mut reader = csv::ReaderBuilder::new()
            .has_headers(true)
            .from_reader(content.as_bytes());

        let headers: Vec<String> = reader.headers()
            .map_err(|e| CoreError::InvalidArgument(format!("CSV header error: {}", e)))?
            .iter()
            .map(|h| h.trim().to_string())
            .collect();

        if headers.is_empty() {
            return Err(CoreError::InvalidArgument("CSV file has no headers".to_string()));
        }

        // Collect all valid rows first, then batch insert
        let mut batch: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
        let mut errors: Vec<String> = Vec::new();

        for (line_num, result) in reader.records().enumerate() {
            let record = match result {
                Ok(r) => r,
                Err(e) => {
                    errors.push(format!("line {}: parse error: {}", line_num + 2, e));
                    continue;
                }
            };

            let mut doc = serde_json::Map::new();
            doc.insert("__class__".to_string(), json!(class));

            for (i, field) in record.iter().enumerate() {
                if i < headers.len() {
                    let col = &headers[i];
                    if col.is_empty() {
                        continue;
                    }
                    let json_val = self.literal_to_json(&Self::parse_csv_value(field));
                    let json_val = Self::try_parse_vector(json_val);
                    doc.insert(col.clone(), json_val);
                }
            }

            let key = self.generate_doc_key(class);
            let value = doc_to_storage_bytes(&doc);
            batch.push((key, value));
        }

        // Batch insert into transaction buffer
        let imported = engine.txn_put_batch(txn_id, batch)
            .map_err(|e| CoreError::InvalidArgument(format!("batch insert error: {}", e)))? as u64;

        if errors.is_empty() {
            Ok(QueryResult::Success(format!("{} row(s) imported from CSV", imported)))
        } else {
            let msg = format!(
                "{} row(s) imported, {} error(s): {}",
                imported,
                errors.len(),
                errors.join("; ")
            );
            if imported == 0 {
                Err(CoreError::InvalidArgument(msg))
            } else {
                Ok(QueryResult::Success(msg))
            }
        }
    }

    /// Parses a CSV field string into a LiteralValue.
    /// Attempts to detect numeric and boolean types; defaults to String.
    fn parse_csv_value(field: &str) -> LiteralValue {
        let trimmed = field.trim();
        if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("null") {
            return LiteralValue::Null;
        }
        if trimmed.eq_ignore_ascii_case("true") {
            return LiteralValue::Bool(true);
        }
        if trimmed.eq_ignore_ascii_case("false") {
            return LiteralValue::Bool(false);
        }
        // Try integer
        if let Ok(i) = trimmed.parse::<i64>() {
            return LiteralValue::Int(i);
        }
        // Try float
        if let Ok(f) = trimmed.parse::<f64>() {
            return LiteralValue::Float(f);
        }
        LiteralValue::String(trimmed.to_string())
    }

    /// Imports data from JSON content.
    /// Supports: array of objects `[{...}, {...}]` or JSON Lines (one object per line).
    fn execute_import_json(
        &self,
        engine: &LsmEngine,
        txn_id: u64,
        class: &str,
        content: &str,
    ) -> Result<QueryResult> {
        let trimmed = content.trim();

        // Try parsing as a JSON array first
        if trimmed.starts_with('[') {
            return self.execute_import_json_array(engine, txn_id, class, trimmed);
        }

        // Fall back to JSON Lines (one object per line)
        self.execute_import_json_lines(engine, txn_id, class, trimmed)
    }

    /// Imports from a JSON array: `[{"col1": "val1"}, ...]`
    /// Uses batch API for high-throughput insertion.
    fn execute_import_json_array(
        &self,
        engine: &LsmEngine,
        txn_id: u64,
        class: &str,
        content: &str,
    ) -> Result<QueryResult> {
        let arr: Vec<serde_json::Value> = serde_json::from_str(content).map_err(|e| {
            CoreError::InvalidArgument(format!("JSON parse error: {}", e))
        })?;

        let mut batch: Vec<(Vec<u8>, Vec<u8>)> = Vec::with_capacity(arr.len());
        let mut errors = Vec::new();

        for (i, item) in arr.iter().enumerate() {
            match item {
                serde_json::Value::Object(map) => {
                    let mut doc = serde_json::Map::new();
                    doc.insert("__class__".to_string(), json!(class));
                    for (key, val) in map {
                        doc.insert(key.clone(), val.clone());
                    }
                    let key = self.generate_doc_key(class);
                    let value = doc_to_storage_bytes(&doc);
                    batch.push((key, value));
                }
                _ => {
                    errors.push(format!("item {}: expected JSON object", i + 1));
                }
            }
        }

        let imported = engine.txn_put_batch(txn_id, batch)
            .map_err(|e| CoreError::InvalidArgument(format!("batch insert error: {}", e)))? as u64;

        Self::build_import_result(imported, errors)
    }

    /// Imports from JSON Lines format (one JSON object per line).
    /// Uses batch API for high-throughput insertion.
    fn execute_import_json_lines(
        &self,
        engine: &LsmEngine,
        txn_id: u64,
        class: &str,
        content: &str,
    ) -> Result<QueryResult> {
        let mut batch: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
        let mut errors = Vec::new();

        for (line_num, line) in content.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            let obj: serde_json::Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(e) => {
                    errors.push(format!("line {}: JSON parse error: {}", line_num + 1, e));
                    continue;
                }
            };

            match obj {
                serde_json::Value::Object(map) => {
                    let mut doc = serde_json::Map::new();
                    doc.insert("__class__".to_string(), json!(class));
                    for (key, val) in map {
                        doc.insert(key.clone(), val.clone());
                    }
                    let key = self.generate_doc_key(class);
                    let value = doc_to_storage_bytes(&doc);
                    batch.push((key, value));
                }
                _ => {
                    errors.push(format!("line {}: expected JSON object", line_num + 1));
                }
            }
        }

        let imported = engine.txn_put_batch(txn_id, batch)
            .map_err(|e| CoreError::InvalidArgument(format!("batch insert error: {}", e)))? as u64;

        Self::build_import_result(imported, errors)
    }

    /// Imports a single JSON object as a row.
    #[allow(dead_code)]
    fn import_json_object(
        &self,
        engine: &LsmEngine,
        txn_id: u64,
        class: &str,
        obj: &serde_json::Value,
        row_num: usize,
    ) -> Result<()> {
        let map = match obj {
            serde_json::Value::Object(m) => m,
            _ => return Err(CoreError::InvalidArgument(
                format!("row {}: expected JSON object, got {}", row_num, obj_type_name(obj))
            )),
        };

        let mut columns = Vec::new();
        let mut values = Vec::new();

        for (key, val) in map {
            columns.push(key.clone());
            values.push(json_to_literal(val));
        }

        self.execute_insert_txn(engine, txn_id, class, &columns, &values)?;
        Ok(())
    }

    /// Builds the import result message, returning an error if all rows failed.
    fn build_import_result(imported: u64, errors: Vec<String>) -> Result<QueryResult> {
        if errors.is_empty() {
            Ok(QueryResult::Success(format!("{} row(s) imported from JSON", imported)))
        } else {
            let msg = format!(
                "{} row(s) imported, {} error(s): {}",
                imported,
                errors.len(),
                errors.join("; ")
            );
            if imported == 0 {
                Err(CoreError::InvalidArgument(msg))
            } else {
                Ok(QueryResult::Success(msg))
            }
        }
    }

    // Old execute_select_txn, execute_hash_join, execute_sort_merge_join,
    // execute_delete_txn, execute_update_txn removed in Phase 25.
    // All queries now use plan-driven execution via execute_plan().

    fn execute_match_txn(
        &self,
        engine: &LsmEngine,
        _txn_id: u64,
        _variable: &str,
        class: &str,
        filter: &Option<FilterExpr>,
        returns: &[String],
    ) -> Result<QueryResult> {
        // Build a scan+filter plan for the MATCH pattern
        let scan_plan = self.build_scan_plan(class, filter)?;
        let plan_result = self.execute_plan(&scan_plan, engine)?;
        let rows = match plan_result {
            QueryResult::Rows(r) => r,
            other => return Ok(other),
        };

        // Project requested columns
        if returns.is_empty() {
            // Remove __pk__ from results
            let cleaned: Vec<Map<String, Value>> = rows.into_iter().map(|mut r| {
                r.remove("__pk__");
                r
            }).collect();
            Ok(QueryResult::Rows(cleaned))
        } else {
            let projected: Vec<Map<String, Value>> = rows.iter().map(|row| {
                let mut result = Map::new();
                for col in returns {
                    if let Some(val) = Self::resolve_column_value(row, col) {
                        result.insert(col.clone(), Value::String(val));
                    }
                }
                result
            }).collect();
            Ok(QueryResult::Rows(projected))
        }
    }

    /// Executes a VECTOR SEARCH query.
    /// 1. If a WHERE filter is provided, get matching document keys first
    /// 2. Perform vector similarity search (with optional filter)
    /// 3. Fetch full documents for the results
    fn execute_vector_search_txn(
        &self,
        engine: &LsmEngine,
        txn_id: u64,
        class: &str,
        column: &str,
        query_vector: &[f32],
        top_k: usize,
        filter: &Option<FilterExpr>,
    ) -> Result<QueryResult> {
        // Check that a vector index exists
        if !engine.has_vector_index(class, column) {
            return Err(CoreError::InvalidArgument(format!(
                "no vector index on {}.{}",
                class, column
            )));
        }

        let search_results = if let Some(filter_expr) = filter {
            // Get document keys matching the filter, expanding class hierarchy
            let class_hierarchy = self.get_class_hierarchy(engine, class);
            let mut allowed_ids = HashSet::new();
            for scan_class in &class_hierarchy {
                let prefix = format!("{}::", scan_class);
                let entries = engine.txn_scan_prefix(txn_id, prefix.as_bytes())?;
                for (key, val_bytes) in &entries {
                    // BinaryRow path: evaluate filter without full JSON deserialization
                    if let Some(brow) = BinaryRow::parse(val_bytes) {
                        if !brow.class_in_hierarchy(&class_hierarchy) {
                            continue;
                        }
                        match eval_binary_filter(&brow, filter_expr) {
                            Some(true) => { allowed_ids.insert(key.clone()); }
                            Some(false) => {}
                            None => {
                                if let Some(ref doc) = brow.to_map() {
                                    if self.matches_filter(engine, doc, &Some(filter_expr.clone())) {
                                        allowed_ids.insert(key.clone());
                                    }
                                }
                            }
                        }
                        continue;
                    }
                    // JSON fallback (legacy data)
                    if let Ok(serde_json::Value::Object(ref doc)) =
                        serde_json::from_slice::<serde_json::Value>(val_bytes)
                    {
                        if class_hierarchy.contains(doc.get("__class__").and_then(|v| v.as_str()).unwrap_or(""))
                            && self.matches_filter(engine, doc, &Some(filter_expr.clone())) {
                                allowed_ids.insert(key.clone());
                            }
                    }
                }
            }

            engine.vector_index_manager().read().unwrap_or_else(|e| e.into_inner()).search_filtered(
                class, column, query_vector, top_k, &allowed_ids,
            )?
        } else {
            engine.vector_index_manager().read().unwrap_or_else(|e| e.into_inner()).search(
                class, column, query_vector, top_k,
            )?
        };

        // Fetch full documents for the search results
        let mut rows = Vec::new();
        for result in &search_results {
            if let Ok(Some(val_bytes)) = engine.txn_get(txn_id, &result.entry.id) {
                // Try BinaryRow first, then JSON fallback
                let doc = if let Some(brow) = BinaryRow::parse(&val_bytes) {
                    brow.to_map()
                } else {
                    match serde_json::from_slice::<serde_json::Value>(&val_bytes) {
                        Ok(serde_json::Value::Object(d)) => Some(d),
                        _ => None,
                    }
                };
                if let Some(mut d) = doc {
                    // Add the distance as a virtual column
                    d.insert(
                        "_distance".to_string(),
                        serde_json::json!(result.distance),
                    );
                    rows.push(d);
                }
            }
        }

        Ok(QueryResult::Rows(rows))
    }

    /// Computes a hash for a QueryAst for plan cache lookup.
    fn hash_ast(ast: &QueryAst) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        // Serialize AST to string and hash it
        let serialized = serde_json::to_string(ast).unwrap_or_default();
        serialized.hash(&mut hasher);
        hasher.finish()
    }

    /// Strips internal fields (__pk__, __class__) from a row for comparison.
    /// These fields are injected by the query executor and contain values
    /// (like primary keys with sequence numbers) that change between query executions,
    /// making direct JSON comparison unreliable.
    fn strip_internal_fields(row: &mut Map<String, Value>) {
        row.remove("__pk__");
        row.remove("__class__");
    }

    fn generate_doc_key(&self, class: &str) -> Vec<u8> {
        let seq = self.doc_counter.fetch_add(1, Ordering::Relaxed);
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        // Combine timestamp + counter for collision-free uniqueness
        let unique = ts << 64 | seq as u128;
        format!("{}::{:040}", class, unique).into_bytes()
    }

    /// Tries to use a secondary index for the given filter.
    /// Tries to use an index for a single (non-AND/OR) predicate.
    /// Returns Some(primary_keys) if index was used, None otherwise.
    fn try_index_scan_single(
        engine: &LsmEngine,
        class: &str,
        filter: &FilterExpr,
    ) -> Result<Option<Vec<Vec<u8>>>> {
        let col = match filter {
            FilterExpr::Eq(c, _)
            | FilterExpr::Ne(c, _)
            | FilterExpr::Gt(c, _)
            | FilterExpr::Lt(c, _)
            | FilterExpr::Gte(c, _)
            | FilterExpr::Lte(c, _)
            | FilterExpr::Between(c, _, _)
            | FilterExpr::In(c, _)
            | FilterExpr::IsNull(c)
            | FilterExpr::IsNotNull(c) => c.clone(),
            _ => return Ok(None),
        };

        if !engine.has_index(class, &col) {
            return Ok(None);
        }

        let index_mgr = engine.index_manager().read().unwrap_or_else(|e| e.into_inner());

        let pkeys: Vec<Vec<u8>> = match filter {
            FilterExpr::Eq(_, val) => {
                let json_val = Self::literal_to_json_static(val);
                index_mgr.lookup_eq_read(class, &col, &json_val).unwrap_or_default()
            }
            FilterExpr::Gt(_, val) => {
                let json_val = Self::literal_to_json_static(val);
                index_mgr.lookup_gt_read(class, &col, &json_val).unwrap_or_default()
            }
            FilterExpr::Lt(_, val) => {
                let json_val = Self::literal_to_json_static(val);
                index_mgr.lookup_lt_read(class, &col, &json_val).unwrap_or_default()
            }
            FilterExpr::Gte(_, val) => {
                let json_val = Self::literal_to_json_static(val);
                let tree = match index_mgr.get_index(class, &col) {
                    Some(t) => t,
                    None => return Ok(None),
                };
                let encoded = onto_storage::IndexManager::encode_value(&json_val);
                tree.gte_scan(&encoded)
            }
            FilterExpr::Lte(_, val) => {
                let json_val = Self::literal_to_json_static(val);
                let tree = match index_mgr.get_index(class, &col) {
                    Some(t) => t,
                    None => return Ok(None),
                };
                let encoded = onto_storage::IndexManager::encode_value(&json_val);
                tree.lte_scan(&encoded)
            }
            FilterExpr::Between(_, low, high) => {
                let low_json = Self::literal_to_json_static(low);
                let high_json = Self::literal_to_json_static(high);
                index_mgr
                    .lookup_range_read(class, &col, Some(&low_json), Some(&high_json))
                    .unwrap_or_default()
            }
            FilterExpr::In(_, values) => {
                let mut all_pkeys = Vec::new();
                for val in values {
                    let json_val = Self::literal_to_json_static(val);
                    if let Some(pks) = index_mgr.lookup_eq_read(class, &col, &json_val) {
                        all_pkeys.extend(pks);
                    }
                }
                all_pkeys
            }
            _ => return Ok(None),
        };

        Ok(Some(pkeys))
    }

    /// Static filter evaluation (no engine needed for simple predicates).
    #[allow(dead_code)]
    fn eval_filter_static(doc: &Map<String, Value>, filter: &FilterExpr) -> bool {
        Self::eval_filter_static_with_hierarchy(doc, filter, &HashSet::new())
    }

    /// Evaluates a static filter expression with optional class hierarchy for subclass-aware matching.
    ///
    /// When `class_hierarchy` is non-empty, `__class__` column comparisons (Eq/In) use
    /// subclass-aware matching: a document with `__class__ = "Employee"` matches a filter
    /// on `__class__ = "Person"` if Employee is a subclass of Person.
    #[allow(dead_code)]
    fn eval_filter_static_with_hierarchy(doc: &Map<String, Value>, filter: &FilterExpr, class_hierarchy: &HashSet<String>) -> bool {
        // Helper to resolve column value (preserving original type) with alias support
        let resolve_val = |col: &str| -> Option<&Value> {
            // Try exact match first
            if let Some(v) = doc.get(col) {
                return Some(v);
            }
            // Try alias-aware lookup (e.g., "o.quantity" matches "quantity")
            for (k, v) in doc {
                if k.ends_with(&format!(".{}", col)) || k == col {
                    return Some(v);
                }
            }
            None
        };
        match filter {
            FilterExpr::Eq(col, val) => {
                resolve_val(col).is_some_and(|v| {
                    if col == "__class__" && !class_hierarchy.is_empty() {
                        Self::class_value_matches_hierarchy(v, val, class_hierarchy)
                    } else {
                        Self::value_matches_static(v, val)
                    }
                })
            }
            FilterExpr::Ne(col, val) => {
                !resolve_val(col).is_some_and(|v| {
                    if col == "__class__" && !class_hierarchy.is_empty() {
                        Self::class_value_matches_hierarchy(v, val, class_hierarchy)
                    } else {
                        Self::value_matches_static(v, val)
                    }
                })
            }
            FilterExpr::Gt(col, val) => {
                resolve_val(col).is_some_and(|v| Self::value_gt_static(v, val))
            }
            FilterExpr::Lt(col, val) => {
                resolve_val(col).is_some_and(|v| Self::value_lt_static(v, val))
            }
            FilterExpr::Gte(col, val) => {
                resolve_val(col).is_some_and(|v| Self::value_gte_static(v, val))
            }
            FilterExpr::Lte(col, val) => {
                resolve_val(col).is_some_and(|v| Self::value_lte_static(v, val))
            }
            FilterExpr::Like(col, pattern) => {
                resolve_val(col).is_some_and(|v| {
                    let s = match v {
                        Value::String(s) => s.clone(),
                        _ => v.to_string(),
                    };
                    Self::like_match(&s, pattern)
                })
            }
            FilterExpr::Between(col, low, high) => {
                resolve_val(col).is_some_and(|v| {
                    Self::value_gte_static(v, low) && Self::value_lte_static(v, high)
                })
            }
            FilterExpr::In(col, values) => {
                resolve_val(col).is_some_and(|v| {
                    if col == "__class__" && !class_hierarchy.is_empty() {
                        values.iter().any(|lv| Self::class_value_matches_hierarchy(v, lv, class_hierarchy))
                    } else {
                        values.iter().any(|lv| Self::value_matches_static(v, lv))
                    }
                })
            }
            FilterExpr::And(left, right) => {
                Self::eval_filter_static_with_hierarchy(doc, left, class_hierarchy)
                    && Self::eval_filter_static_with_hierarchy(doc, right, class_hierarchy)
            }
            FilterExpr::Or(left, right) => {
                Self::eval_filter_static_with_hierarchy(doc, left, class_hierarchy)
                    || Self::eval_filter_static_with_hierarchy(doc, right, class_hierarchy)
            }
            _ => true, // Complex filters (EXISTS, subqueries) pass through
        }
    }

    /// Checks if a document's __class__ value matches a target class via the hierarchy.
    /// Returns true if the document's class equals the target or is a subclass of it.
    #[allow(dead_code)]
    fn class_value_matches_hierarchy(doc_val: &Value, target: &LiteralValue, class_hierarchy: &HashSet<String>) -> bool {
        match (doc_val, target) {
            (Value::String(doc_class), LiteralValue::String(target_class)) => {
                // Direct match or document's class is in the hierarchy (i.e. a subclass of target)
                doc_class == target_class || class_hierarchy.contains(doc_class.as_str())
            }
            _ => Self::value_matches_static(doc_val, target),
        }
    }

    #[allow(dead_code)]
    fn value_gte_static(v: &Value, lit: &LiteralValue) -> bool {
        Self::value_gt_static(v, lit) || Self::value_matches_static(v, lit)
    }

    #[allow(dead_code)]
    fn value_lte_static(v: &Value, lit: &LiteralValue) -> bool {
        Self::value_lt_static(v, lit) || Self::value_matches_static(v, lit)
    }

    #[allow(dead_code)]
    fn value_matches_static(v: &Value, lit: &LiteralValue) -> bool {
        match (v, lit) {
            (Value::String(s), LiteralValue::String(l)) => s == l,
            (Value::Number(n), LiteralValue::Int(l)) => n.as_i64() == Some(*l),
            (Value::Number(n), LiteralValue::Float(l)) => n.as_f64() == Some(*l),
            (Value::Bool(b), LiteralValue::Bool(l)) => b == l,
            (Value::Null, LiteralValue::Null) => true,
            _ => false,
        }
    }

    #[allow(dead_code)]
    fn value_gt_static(v: &Value, lit: &LiteralValue) -> bool {
        match (v, lit) {
            (Value::Number(n), LiteralValue::Int(l)) => n.as_i64().is_some_and(|n| n > *l),
            (Value::Number(n), LiteralValue::Float(l)) => n.as_f64().is_some_and(|n| n > *l),
            (Value::String(s), LiteralValue::String(l)) => s.as_str() > l.as_str(),
            _ => false,
        }
    }

    #[allow(dead_code)]
    fn value_lt_static(v: &Value, lit: &LiteralValue) -> bool {
        match (v, lit) {
            (Value::Number(n), LiteralValue::Int(l)) => n.as_i64().is_some_and(|n| n < *l),
            (Value::Number(n), LiteralValue::Float(l)) => n.as_f64().is_some_and(|n| n < *l),
            (Value::String(s), LiteralValue::String(l)) => s.as_str() < l.as_str(),
            _ => false,
        }
    }

    /// Fetches rows by their primary keys (plan-driven path, no txn).
    fn fetch_rows_by_pks(
        engine: &LsmEngine,
        pkeys: &[Vec<u8>],
    ) -> Result<Vec<Map<String, Value>>> {
        let mut rows = Vec::new();
        for pk in pkeys {
            if let Ok(Some(val_bytes)) = engine.get(pk) {
                if let Some(doc) = simd_parse_row(&val_bytes) {
                    rows.push(doc);
                }
            }
        }
        Ok(rows)
    }

    /// Converts a LiteralValue to a serde_json::Value (static helper).
    fn literal_to_json_static(lit: &LiteralValue) -> serde_json::Value {
        match lit {
            LiteralValue::Null => serde_json::Value::Null,
            LiteralValue::Bool(b) => serde_json::json!(b),
            LiteralValue::Int(i) => serde_json::json!(i),
            LiteralValue::Float(f) => serde_json::json!(f),
            LiteralValue::String(s) => serde_json::json!(s),
        }
    }

    fn matches_filter(&self, engine: &LsmEngine, doc: &Map<String, Value>, filter: &Option<FilterExpr>) -> bool {
        match filter {
            None => true,
            Some(expr) => self.eval_filter(engine, doc, expr),
        }
    }

    fn eval_filter(&self, engine: &LsmEngine, doc: &Map<String, Value>, expr: &FilterExpr) -> bool {
        match expr {
            FilterExpr::Eq(col, val) => {
                doc.get(col).is_some_and(|v| {
                    if col == "__class__" {
                        self.class_value_matches(engine, v, val)
                    } else {
                        self.property_value_matches(engine, doc, col, val)
                    }
                })
            }
            FilterExpr::Ne(col, val) => {
                !doc.get(col).is_some_and(|v| {
                    if col == "__class__" {
                        self.class_value_matches(engine, v, val)
                    } else {
                        self.property_value_matches(engine, doc, col, val)
                    }
                })
            }
            FilterExpr::Gt(col, val) => {
                doc.get(col)
                    .is_some_and(|v| self.value_gt(v, val))
            }
            FilterExpr::Lt(col, val) => {
                doc.get(col)
                    .is_some_and(|v| self.value_lt(v, val))
            }
            FilterExpr::Gte(col, val) => {
                doc.get(col)
                    .is_some_and(|v| self.value_gt(v, val) || self.value_matches(v, val))
            }
            FilterExpr::Lte(col, val) => {
                doc.get(col)
                    .is_some_and(|v| self.value_lt(v, val) || self.value_matches(v, val))
            }
            FilterExpr::Like(col, pattern) => {
                doc.get(col).is_some_and(|v| {
                    let s = match v {
                        Value::String(s) => s.clone(),
                        _ => v.to_string(),
                    };
                    Self::like_match(&s, pattern)
                })
            }
            FilterExpr::Between(col, low, high) => {
                doc.get(col).is_some_and(|v| {
                    self.value_gte(v, low) && self.value_lte(v, high)
                })
            }
            FilterExpr::In(col, values) => {
                doc.get(col).is_some_and(|v| {
                    if col == "__class__" {
                        values.iter().any(|val| self.class_value_matches(engine, v, val))
                    } else {
                        values.iter().any(|val| self.property_value_matches(engine, doc, col, val))
                    }
                })
            }
            FilterExpr::InSubquery(col, subquery) => {
                let sub_result = self.execute_with_engine_inner(subquery, engine);
                match sub_result {
                    Ok(QueryResult::Rows(rows)) => {
                        doc.get(col).is_some_and(|v| {
                            rows.iter().any(|row| {
                                row.values().any(|sv| {
                                    match (v, sv) {
                                        (Value::String(a), Value::String(b)) => a == b,
                                        (Value::Number(a), Value::Number(b)) => a == b,
                                        _ => *v == *sv,
                                    }
                                })
                            })
                        })
                    }
                    _ => false,
                }
            }
            FilterExpr::Exists(subquery) => {
                let sub_result = self.execute_with_engine_inner(subquery, engine);
                match sub_result {
                    Ok(QueryResult::Rows(rows)) => !rows.is_empty(),
                    _ => false,
                }
            }
            FilterExpr::NotExists(subquery) => {
                let sub_result = self.execute_with_engine_inner(subquery, engine);
                match sub_result {
                    Ok(QueryResult::Rows(rows)) => rows.is_empty(),
                    _ => false,
                }
            }
            FilterExpr::IsNull(col) => {
                doc.get(col).is_none_or(|v| matches!(v, Value::Null))
            }
            FilterExpr::IsNotNull(col) => {
                doc.get(col).is_some_and(|v| !matches!(v, Value::Null))
            }
            FilterExpr::Not(expr) => {
                !self.eval_filter(engine, doc, expr)
            }
            FilterExpr::And(left, right) => {
                self.eval_filter(engine, doc, left) && self.eval_filter(engine, doc, right)
            }
            FilterExpr::Or(left, right) => {
                self.eval_filter(engine, doc, left) || self.eval_filter(engine, doc, right)
            }
        }
    }

    fn value_matches(&self, v: &Value, lit: &LiteralValue) -> bool {
        match (v, lit) {
            (Value::String(s), LiteralValue::String(l)) => s == l,
            (Value::Number(n), LiteralValue::Int(l)) => n.as_i64() == Some(*l),
            (Value::Number(n), LiteralValue::Float(l)) => n.as_f64() == Some(*l),
            (Value::Bool(b), LiteralValue::Bool(l)) => b == l,
            (Value::Null, LiteralValue::Null) => true,
            _ => false,
        }
    }

    /// Checks if a document's __class__ value matches a target class using ontology reasoning.
    /// Returns true if the document's class equals the target or is in the class hierarchy
    /// (i.e., the document's class is a subclass of the target).
    fn class_value_matches(&self, engine: &LsmEngine, v: &Value, lit: &LiteralValue) -> bool {
        match (v, lit) {
            (Value::String(doc_class), LiteralValue::String(target_class)) => {
                if doc_class == target_class {
                    return true;
                }
                // Use get_class_hierarchy to check if doc_class is a subclass of target_class
                let hierarchy = self.get_class_hierarchy(engine, target_class);
                hierarchy.contains(doc_class.as_str())
            }
            _ => self.value_matches(v, lit),
        }
    }

    /// Returns all property names that are equivalent to or subproperties of the given property.
    /// This implements PrpSpo (subproperty propagation) and PrpEqp (equivalent property).
    ///
    /// For example, if `reportsTo` has subproperty `worksUnder` and equivalent property `supervisedBy`,
    /// then `get_property_aliases(engine, "reportsTo")` returns `{"reportsTo", "worksUnder", "supervisedBy"}`.
    fn get_property_aliases(&self, engine: &LsmEngine, property: &str) -> HashSet<String> {
        // Check cache first
        {
            let cache = self.inference_cache.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(cached) = cache.property_aliases.get(property) {
                return cached.clone();
            }
        }

        let mut aliases = HashSet::new();
        aliases.insert(property.to_string());

        let entries = engine.scan_prefix(b"__ontology__").unwrap_or_default();
        for (_key, val_bytes) in entries {
            if let Ok(ontology) = onto_ontology::Ontology::from_json_slice(&val_bytes) {
                if let Some(prop_def) = ontology.properties.get(property) {
                    for equiv in &prop_def.equivalent_properties {
                        aliases.insert(equiv.clone());
                    }
                    for (name, other_prop) in &ontology.properties {
                        if other_prop.subproperty_of.contains(&property.to_string()) {
                            aliases.insert(name.clone());
                        }
                    }
                }
                if let Some(prop_def) = ontology.properties.get(property) {
                    for parent in &prop_def.subproperty_of {
                        aliases.insert(parent.clone());
                        if let Some(parent_def) = ontology.properties.get(parent) {
                            for equiv in &parent_def.equivalent_properties {
                                aliases.insert(equiv.clone());
                            }
                        }
                    }
                }
            }
        }

        // Store in cache
        {
            let mut cache = self.inference_cache.lock().unwrap_or_else(|e| e.into_inner());
            cache.property_aliases.insert(property.to_string(), aliases.clone());
        }

        aliases
    }

    /// Returns the inverse property name for a given property, if defined.
    /// Implements PrpInv (inverse property inference).
    ///
    /// For example, if `reportsTo` has inverse `manages`, returns `Some("manages")`.
    fn get_inverse_property(&self, engine: &LsmEngine, property: &str) -> Option<String> {
        // Check cache first
        {
            let cache = self.inference_cache.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(cached) = cache.inverse_property.get(property) {
                return cached.clone();
            }
        }

        let entries = engine.scan_prefix(b"__ontology__").unwrap_or_default();
        let mut result = None;
        for (_key, val_bytes) in entries {
            if let Ok(ontology) = onto_ontology::Ontology::from_json_slice(&val_bytes) {
                if let Some(prop_def) = ontology.properties.get(property) {
                    if let Some(ref inverse) = prop_def.inverse_of {
                        result = Some(inverse.clone());
                        break;
                    }
                }
                for (name, prop_def) in &ontology.properties {
                    if prop_def.inverse_of.as_deref() == Some(property) {
                        result = Some(name.clone());
                        break;
                    }
                }
            }
        }

        // Store in cache
        {
            let mut cache = self.inference_cache.lock().unwrap_or_else(|e| e.into_inner());
            cache.inverse_property.insert(property.to_string(), result.clone());
        }

        result
    }

    /// Checks if a property is transitive.
    #[allow(dead_code)]
    fn is_transitive_property(&self, engine: &LsmEngine, property: &str) -> bool {
        let entries = engine.scan_prefix(b"__ontology__").unwrap_or_default();
        for (_key, val_bytes) in entries {
            if let Ok(ontology) = onto_ontology::Ontology::from_json_slice(&val_bytes) {
                if let Some(prop_def) = ontology.properties.get(property) {
                    return prop_def.is_transitive;
                }
            }
        }
        false
    }

    /// Checks if a property is symmetric.
    fn is_symmetric_property(&self, engine: &LsmEngine, property: &str) -> bool {
        let entries = engine.scan_prefix(b"__ontology__").unwrap_or_default();
        for (_key, val_bytes) in entries {
            if let Ok(ontology) = onto_ontology::Ontology::from_json_slice(&val_bytes) {
                if let Some(prop_def) = ontology.properties.get(property) {
                    return prop_def.is_symmetric;
                }
            }
        }
        false
    }

    /// Performs transitive closure lookup for a transitive property.
    #[allow(dead_code)]
    /// Returns all values reachable from `start_value` via the transitive property.
    ///
    /// For example, if `ancestor` is transitive and we have:
    ///   alice ancestor bob, bob ancestor charlie
    /// Then `transitive_closure(engine, "alice", "ancestor")` returns {"bob", "charlie"}.
    fn transitive_closure(
        &self,
        engine: &LsmEngine,
        start_id: &str,
        property: &str,
    ) -> HashSet<String> {
        let mut visited = HashSet::new();
        let mut to_visit = vec![start_id.to_string()];
        let property_aliases = self.get_property_aliases(engine, property);

        while let Some(current) = to_visit.pop() {
            if !visited.insert(current.clone()) {
                continue;
            }
            // Scan all classes to find documents with this ID
            let entries = engine.scan_prefix(b"__ontology__").unwrap_or_default();
            for (_key, val_bytes) in &entries {
                if let Ok(ontology) = serde_json::from_slice::<onto_ontology::Ontology>(val_bytes) {
                    for class_name in ontology.classes.keys() {
                        let doc_key = format!("{}::{}", class_name, current);
                        if let Ok(Some(doc_bytes)) = engine.get(doc_key.as_bytes()) {
                            if let Ok(serde_json::Value::Object(doc)) = serde_json::from_slice::<serde_json::Value>(&doc_bytes) {
                                for alias in &property_aliases {
                                    if let Some(serde_json::Value::String(val)) = doc.get(alias) {
                                        if !visited.contains(val) {
                                            to_visit.push(val.clone());
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        visited.remove(start_id);
        visited
    }

    /// Enhanced property value matching that considers ontology inference rules.
    ///
    /// For a property column, checks:
    /// 1. Direct match on the property
    /// 2. Match on equivalent/subproperty columns (PrpSpo, PrpEqp)
    /// 3. Inverse property match with swapped semantics (PrpInv)
    /// 4. Symmetric property match (PrpSymp)
    fn property_value_matches(
        &self,
        engine: &LsmEngine,
        doc: &Map<String, Value>,
        col: &str,
        val: &LiteralValue,
    ) -> bool {
        // 1. Direct match
        if let Some(v) = doc.get(col) {
            if self.value_matches(v, val) {
                return true;
            }
        }

        // 2. Equivalent/subproperty match (PrpSpo, PrpEqp)
        let aliases = self.get_property_aliases(engine, col);
        for alias in &aliases {
            if alias != col {
                if let Some(v) = doc.get(alias) {
                    if self.value_matches(v, val) {
                        return true;
                    }
                }
            }
        }

        // 3. Symmetric property match (PrpSymp)
        // If property is symmetric and doc has `col = X`, then `X col doc_id` also holds.
        // In document model: look for a document whose ID equals the filter value,
        // and check if that document has `col` pointing back to this document's ID.
        if self.is_symmetric_property(engine, col) {
            if let LiteralValue::String(target_id) = val {
                let doc_id = doc.get("__pk__")
                    .and_then(|v| v.as_str())
                    .map(|pk| {
                        // Extract the ID part from "Class::id"
                        pk.rsplit("::").next().unwrap_or(pk).to_string()
                    });
                if let Some(ref id) = doc_id {
                    if id == target_id {
                        // The filter is asking for documents where col = self, which always matches for symmetric
                        return true;
                    }
                }
            }
        }

        // 4. Inverse property match (PrpInv)
        // If `reportsTo` has inverse `manages`, and filter is `reportsTo = 'bob'`,
        // then check if doc has `manages = bob` (bob manages this doc's subject).
        // But in document model, we need to look up the target document.
        if let Some(inverse) = self.get_inverse_property(engine, col) {
            if let LiteralValue::String(target_id) = val {
                // Look up the target document to see if it has the inverse property pointing to us
                let doc_id = doc.get("__pk__")
                    .and_then(|v| v.as_str())
                    .map(|pk| pk.rsplit("::").next().unwrap_or(pk).to_string());
                if let Some(ref id) = doc_id {
                    // Check all classes for the target document
                    let entries = engine.scan_prefix(b"__ontology__").unwrap_or_default();
                    for (_key, val_bytes) in &entries {
                        if let Ok(ontology) = serde_json::from_slice::<onto_ontology::Ontology>(val_bytes) {
                            for class_name in ontology.classes.keys() {
                                let target_key = format!("{}::{}", class_name, target_id);
                                if let Ok(Some(target_bytes)) = engine.get(target_key.as_bytes()) {
                                    if let Ok(serde_json::Value::Object(target_doc)) = serde_json::from_slice::<serde_json::Value>(&target_bytes) {
                                        // Check if target has inverse property pointing to our doc
                                        if let Some(serde_json::Value::String(inverse_val)) = target_doc.get(&inverse) {
                                            if inverse_val == id {
                                                return true;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        false
    }

    fn value_gt(&self, v: &Value, lit: &LiteralValue) -> bool {
        match (v, lit) {
            (Value::Number(n), LiteralValue::Int(l)) => n.as_i64().is_some_and(|n| n > *l),
            (Value::Number(n), LiteralValue::Float(l)) => n.as_f64().is_some_and(|n| n > *l),
            (Value::String(s), LiteralValue::String(l)) => s.as_str() > l.as_str(),
            _ => false,
        }
    }

    fn value_lt(&self, v: &Value, lit: &LiteralValue) -> bool {
        match (v, lit) {
            (Value::Number(n), LiteralValue::Int(l)) => n.as_i64().is_some_and(|n| n < *l),
            (Value::Number(n), LiteralValue::Float(l)) => n.as_f64().is_some_and(|n| n < *l),
            (Value::String(s), LiteralValue::String(l)) => s.as_str() < l.as_str(),
            _ => false,
        }
    }

    fn value_gte(&self, v: &Value, lit: &LiteralValue) -> bool {
        self.value_gt(v, lit) || self.value_matches(v, lit)
    }

    fn value_lte(&self, v: &Value, lit: &LiteralValue) -> bool {
        self.value_lt(v, lit) || self.value_matches(v, lit)
    }

    /// SQL LIKE pattern matching. Supports % (zero or more chars) and _ (exactly one char).
    fn like_match(text: &str, pattern: &str) -> bool {
        let mut ti = 0;
        let mut pi = 0;
        let mut star_pi = usize::MAX;
        let mut star_ti = 0;
        let t_bytes = text.as_bytes();
        let p_bytes = pattern.as_bytes();

        while ti < t_bytes.len() {
            if pi < p_bytes.len() && (p_bytes[pi] == b'_' || p_bytes[pi] == t_bytes[ti]) {
                ti += 1;
                pi += 1;
            } else if pi < p_bytes.len() && p_bytes[pi] == b'%' {
                star_pi = pi;
                star_ti = ti;
                pi += 1;
            } else if star_pi != usize::MAX {
                pi = star_pi + 1;
                star_ti += 1;
                ti = star_ti;
            } else {
                return false;
            }
        }
        while pi < p_bytes.len() && p_bytes[pi] == b'%' {
            pi += 1;
        }
        pi == p_bytes.len()
    }

    /// Evaluates a ValueExpr against a row context, returning a JSON Value.
    fn evaluate_value_expr(
        &self,
        expr: &ValueExpr,
        row: &Map<String, Value>,
        engine: &LsmEngine,
    ) -> Result<Value> {
        match expr {
            ValueExpr::Column(col) => {
                let (real_col, _) = if let Some(as_pos) = col.find(" as ") {
                    (&col[..as_pos], Some(col[as_pos + 4..].trim()))
                } else {
                    (col.as_str(), None)
                };
                // Try direct lookup
                if let Some(val) = row.get(real_col) {
                    return Ok(val.clone());
                }
                // Try alias-aware lookup
                for (k, v) in row {
                    if k.ends_with(&format!(".{}", real_col)) || k == real_col {
                        return Ok(v.clone());
                    }
                }
                Ok(Value::Null)
            }
            ValueExpr::Literal(lit) => Ok(Self::literal_to_json_static(lit)),
            ValueExpr::CaseWhen {
                when_branches,
                else_expr,
            } => {
                for (cond, then_expr) in when_branches {
                    if self.eval_filter(engine, row, cond) {
                        return self.evaluate_value_expr(then_expr, row, engine);
                    }
                }
                if let Some(default) = else_expr {
                    return self.evaluate_value_expr(default, row, engine);
                }
                Ok(Value::Null)
            }
            ValueExpr::ScalarSubquery(subquery) => {
                let sub_result = self.execute_with_engine_inner(subquery, engine)?;
                match sub_result {
                    QueryResult::Rows(rows) => {
                        if let Some(first_row) = rows.first() {
                            // Return the first column of the first row
                            if let Some(val) = first_row.values().next() {
                                Ok(val.clone())
                            } else {
                                Ok(Value::Null)
                            }
                        } else {
                            Ok(Value::Null)
                        }
                    }
                    _ => Ok(Value::Null),
                }
            }
            ValueExpr::Arithmetic { op, left, right } => {
                let left_val = self.evaluate_value_expr(left, row, engine)?;
                let right_val = self.evaluate_value_expr(right, row, engine)?;
                Self::eval_arithmetic(op, &left_val, &right_val)
            }
            ValueExpr::Function { name, args } => {
                let arg_values: Vec<Value> = args.iter()
                    .map(|a| self.evaluate_value_expr(a, row, engine))
                    .collect::<Result<Vec<_>>>()?;
                Self::eval_builtin_function(name, &arg_values)
            }
        }
    }

    /// Evaluates a built-in function.
    fn eval_builtin_function(name: &str, args: &[Value]) -> Result<Value> {
        match name.to_uppercase().as_str() {
            "COALESCE" => {
                for arg in args {
                    if !arg.is_null() {
                        return Ok(arg.clone());
                    }
                }
                Ok(Value::Null)
            }
            "NULLIF" => {
                if args.len() >= 2 && args[0] == args[1] {
                    Ok(Value::Null)
                } else {
                    Ok(args.first().cloned().unwrap_or(Value::Null))
                }
            }
            "CONCAT" => {
                let mut result = String::new();
                for arg in args {
                    match arg {
                        Value::String(s) => result.push_str(s),
                        Value::Number(n) => result.push_str(&n.to_string()),
                        Value::Bool(b) => result.push_str(&b.to_string()),
                        Value::Null => {} // NULL concatenation produces NULL in strict mode
                        _ => result.push_str(&arg.to_string()),
                    }
                }
                Ok(Value::String(result))
            }
            "SUBSTRING" => {
                if let Some(Value::String(s)) = args.first() {
                    let start = args.get(1).and_then(|v| v.as_i64()).unwrap_or(1).max(1) as usize - 1;
                    let len = args.get(2).and_then(|v| v.as_i64()).map(|l| l.max(0) as usize);
                    let substr: String = s.chars().skip(start).take(len.unwrap_or(s.len())).collect();
                    Ok(Value::String(substr))
                } else {
                    Ok(Value::Null)
                }
            }
            "UPPER" => {
                if let Some(Value::String(s)) = args.first() {
                    Ok(Value::String(s.to_uppercase()))
                } else {
                    Ok(Value::Null)
                }
            }
            "LOWER" => {
                if let Some(Value::String(s)) = args.first() {
                    Ok(Value::String(s.to_lowercase()))
                } else {
                    Ok(Value::Null)
                }
            }
            "NOW" => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                Ok(Value::Number(serde_json::Number::from(now)))
            }
            "LENGTH" => {
                if let Some(Value::String(s)) = args.first() {
                    Ok(Value::Number(serde_json::Number::from(s.chars().count())))
                } else {
                    Ok(Value::Null)
                }
            }
            "TRIM" => {
                if let Some(Value::String(s)) = args.first() {
                    Ok(Value::String(s.trim().to_string()))
                } else {
                    Ok(Value::Null)
                }
            }
            "ABS" => {
                if let Some(val) = args.first() {
                    match val {
                        Value::Number(n) => {
                            if let Some(f) = n.as_f64() {
                                Ok(Value::Number(serde_json::Number::from_f64(f.abs()).unwrap_or(serde_json::Number::from(0))))
                            } else {
                                Ok(val.clone())
                            }
                        }
                        _ => Ok(Value::Null),
                    }
                } else {
                    Ok(Value::Null)
                }
            }
            "ROUND" => {
                if let Some(val) = args.first() {
                    let decimals = args.get(1).and_then(|v| v.as_i64()).unwrap_or(0) as u32;
                    match val {
                        Value::Number(n) => {
                            if let Some(f) = n.as_f64() {
                                let factor = 10f64.powi(decimals as i32);
                                let rounded = (f * factor).round() / factor;
                                Ok(Value::Number(serde_json::Number::from_f64(rounded).unwrap_or(serde_json::Number::from(0))))
                            } else {
                                Ok(val.clone())
                            }
                        }
                        _ => Ok(Value::Null),
                    }
                } else {
                    Ok(Value::Null)
                }
            }
            // ── GIS Functions ──
            "ST_POINT" => {
                // ST_POINT(lon, lat) → WKB hex string
                if args.len() >= 2 {
                    let lon = args[0].as_f64().unwrap_or(0.0);
                    let lat = args[1].as_f64().unwrap_or(0.0);
                    let point = onto_core::geo::Geometry::Point(onto_core::geo::Coord::new(lon, lat));
                    let wkb = point.to_wkb();
                    Ok(Value::String(hex::encode(&wkb)))
                } else {
                    Ok(Value::Null)
                }
            }
            "ST_DISTANCE" => {
                // ST_DISTANCE(geom1, geom2) → distance in meters
                if args.len() >= 2 {
                    let g1 = parse_geometry_from_value(&args[0]);
                    let g2 = parse_geometry_from_value(&args[1]);
                    match (g1, g2) {
                        (Some(a), Some(b)) => {
                            let d = onto_core::geo::distance(&a, &b);
                            Ok(json!(d))
                        }
                        _ => Ok(Value::Null),
                    }
                } else {
                    Ok(Value::Null)
                }
            }
            "ST_CONTAINS" => {
                // ST_CONTAINS(geom1, geom2) → boolean
                if args.len() >= 2 {
                    let g1 = parse_geometry_from_value(&args[0]);
                    let g2 = parse_geometry_from_value(&args[1]);
                    match (g1, g2) {
                        (Some(a), Some(b)) => Ok(Value::Bool(onto_core::geo::contains(&a, &b))),
                        _ => Ok(Value::Null),
                    }
                } else {
                    Ok(Value::Null)
                }
            }
            "ST_INTERSECTS" => {
                // ST_INTERSECTS(geom1, geom2) → boolean
                if args.len() >= 2 {
                    let g1 = parse_geometry_from_value(&args[0]);
                    let g2 = parse_geometry_from_value(&args[1]);
                    match (g1, g2) {
                        (Some(a), Some(b)) => Ok(Value::Bool(onto_core::geo::intersects(&a, &b))),
                        _ => Ok(Value::Null),
                    }
                } else {
                    Ok(Value::Null)
                }
            }
            "ST_WITHIN" => {
                // ST_WITHIN(geom1, geom2) → boolean
                if args.len() >= 2 {
                    let g1 = parse_geometry_from_value(&args[0]);
                    let g2 = parse_geometry_from_value(&args[1]);
                    match (g1, g2) {
                        (Some(a), Some(b)) => Ok(Value::Bool(onto_core::geo::within(&a, &b))),
                        _ => Ok(Value::Null),
                    }
                } else {
                    Ok(Value::Null)
                }
            }
            "ST_OVERLAPS" => {
                // ST_OVERLAPS(geom1, geom2) → boolean
                if args.len() >= 2 {
                    let g1 = parse_geometry_from_value(&args[0]);
                    let g2 = parse_geometry_from_value(&args[1]);
                    match (g1, g2) {
                        (Some(a), Some(b)) => Ok(Value::Bool(onto_core::geo::overlaps(&a, &b))),
                        _ => Ok(Value::Null),
                    }
                } else {
                    Ok(Value::Null)
                }
            }
            "ST_TOUCHES" => {
                // ST_TOUCHES(geom1, geom2) → boolean
                if args.len() >= 2 {
                    let g1 = parse_geometry_from_value(&args[0]);
                    let g2 = parse_geometry_from_value(&args[1]);
                    match (g1, g2) {
                        (Some(a), Some(b)) => Ok(Value::Bool(onto_core::geo::touches(&a, &b))),
                        _ => Ok(Value::Null),
                    }
                } else {
                    Ok(Value::Null)
                }
            }
            "ST_CROSSES" => {
                // ST_CROSSES(geom1, geom2) → boolean
                if args.len() >= 2 {
                    let g1 = parse_geometry_from_value(&args[0]);
                    let g2 = parse_geometry_from_value(&args[1]);
                    match (g1, g2) {
                        (Some(a), Some(b)) => Ok(Value::Bool(onto_core::geo::crosses(&a, &b))),
                        _ => Ok(Value::Null),
                    }
                } else {
                    Ok(Value::Null)
                }
            }
            "ST_DISJOINT" => {
                // ST_DISJOINT(geom1, geom2) → boolean
                if args.len() >= 2 {
                    let g1 = parse_geometry_from_value(&args[0]);
                    let g2 = parse_geometry_from_value(&args[1]);
                    match (g1, g2) {
                        (Some(a), Some(b)) => Ok(Value::Bool(onto_core::geo::disjoint(&a, &b))),
                        _ => Ok(Value::Null),
                    }
                } else {
                    Ok(Value::Null)
                }
            }
            "ST_EQUALS" => {
                // ST_EQUALS(geom1, geom2) → boolean
                if args.len() >= 2 {
                    let g1 = parse_geometry_from_value(&args[0]);
                    let g2 = parse_geometry_from_value(&args[1]);
                    match (g1, g2) {
                        (Some(a), Some(b)) => Ok(Value::Bool(onto_core::geo::equals(&a, &b))),
                        _ => Ok(Value::Null),
                    }
                } else {
                    Ok(Value::Null)
                }
            }
            "ST_RELATE" => {
                // ST_RELATE(geom1, geom2) → relationship string
                if args.len() >= 2 {
                    let g1 = parse_geometry_from_value(&args[0]);
                    let g2 = parse_geometry_from_value(&args[1]);
                    match (g1, g2) {
                        (Some(a), Some(b)) => {
                            let rel = onto_core::geo::relate(&a, &b);
                            Ok(Value::String(format!("{:?}", rel)))
                        }
                        _ => Ok(Value::Null),
                    }
                } else {
                    Ok(Value::Null)
                }
            }
            "ST_AS_TEXT" => {
                // ST_AS_TEXT(geom) → WKT string
                if let Some(geom) = parse_geometry_from_value(&args[0]) {
                    Ok(Value::String(geom.to_wkt()))
                } else {
                    Ok(Value::Null)
                }
            }
            "ST_FROM_TEXT" => {
                // ST_FROM_TEXT(wkt) → WKB hex string
                if let Some(Value::String(wkt)) = args.first() {
                    if let Some(geom) = onto_core::geo::Geometry::from_wkt(wkt) {
                        let wkb = geom.to_wkb();
                        Ok(Value::String(hex::encode(&wkb)))
                    } else {
                        Ok(Value::Null)
                    }
                } else {
                    Ok(Value::Null)
                }
            }
            "GEOHASH" => {
                // GEOHASH(lat, lon, precision) → geohash string
                if args.len() >= 2 {
                    let lat = args[0].as_f64().unwrap_or(0.0);
                    let lon = args[1].as_f64().unwrap_or(0.0);
                    let precision = args.get(2).and_then(|v| v.as_i64()).unwrap_or(8) as usize;
                    Ok(Value::String(onto_core::geo::geohash_encode(lat, lon, precision)))
                } else {
                    Ok(Value::Null)
                }
            }
            "ST_X" => {
                // ST_X(point) → x coordinate (longitude)
                if let Some(geom) = parse_geometry_from_value(&args[0]) {
                    if let onto_core::geo::Geometry::Point(c) = geom {
                        Ok(json!(c.x))
                    } else {
                        Ok(Value::Null)
                    }
                } else {
                    Ok(Value::Null)
                }
            }
            "ST_Y" => {
                // ST_Y(point) → y coordinate (latitude)
                if let Some(geom) = parse_geometry_from_value(&args[0]) {
                    if let onto_core::geo::Geometry::Point(c) = geom {
                        Ok(json!(c.y))
                    } else {
                        Ok(Value::Null)
                    }
                } else {
                    Ok(Value::Null)
                }
            }
            "ST_GEOMETRY_TYPE" => {
                // ST_GEOMETRY_TYPE(geom) → geometry type name
                if let Some(geom) = parse_geometry_from_value(&args[0]) {
                    Ok(Value::String(geom.geometry_type().to_string()))
                } else {
                    Ok(Value::Null)
                }
            }
            // ── Time Series Functions ──
            "TS_MEAN" => {
                // TS_MEAN(array) → mean of numeric array
                if let Some(Value::Array(arr)) = args.first() {
                    let values: Vec<f64> = arr.iter().filter_map(|v| v.as_f64()).collect();
                    if values.is_empty() {
                        Ok(Value::Null)
                    } else {
                        let mean = values.iter().sum::<f64>() / values.len() as f64;
                        Ok(json!(mean))
                    }
                } else {
                    Ok(Value::Null)
                }
            }
            "TS_STDDEV" => {
                // TS_STDDEV(array) → standard deviation of numeric array
                if let Some(Value::Array(arr)) = args.first() {
                    let values: Vec<f64> = arr.iter().filter_map(|v| v.as_f64()).collect();
                    if values.is_empty() {
                        Ok(Value::Null)
                    } else {
                        let mean = values.iter().sum::<f64>() / values.len() as f64;
                        let var = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / values.len() as f64;
                        Ok(json!(var.sqrt()))
                    }
                } else {
                    Ok(Value::Null)
                }
            }
            "TS_MIN" => {
                // TS_MIN(array) → minimum value
                if let Some(Value::Array(arr)) = args.first() {
                    let min = arr.iter().filter_map(|v| v.as_f64()).fold(f64::INFINITY, f64::min);
                    if min == f64::INFINITY { Ok(Value::Null) } else { Ok(json!(min)) }
                } else {
                    Ok(Value::Null)
                }
            }
            "TS_MAX" => {
                // TS_MAX(array) → maximum value
                if let Some(Value::Array(arr)) = args.first() {
                    let max = arr.iter().filter_map(|v| v.as_f64()).fold(f64::NEG_INFINITY, f64::max);
                    if max == f64::NEG_INFINITY { Ok(Value::Null) } else { Ok(json!(max)) }
                } else {
                    Ok(Value::Null)
                }
            }
            "TS_COUNT" => {
                // TS_COUNT(array) → count of non-null values
                if let Some(Value::Array(arr)) = args.first() {
                    let count = arr.iter().filter(|v| !v.is_null()).count();
                    Ok(json!(count))
                } else {
                    Ok(Value::Null)
                }
            }
            "TS_SUM" => {
                // TS_SUM(array) → sum of numeric values
                if let Some(Value::Array(arr)) = args.first() {
                    let sum: f64 = arr.iter().filter_map(|v| v.as_f64()).sum();
                    Ok(json!(sum))
                } else {
                    Ok(Value::Null)
                }
            }
            "TS_ANOMALY_DETECT" => {
                // TS_ANOMALY_DETECT(array, threshold) → array of anomalous indices
                if let Some(Value::Array(arr)) = args.first() {
                    let values: Vec<f64> = arr.iter().filter_map(|v| v.as_f64()).collect();
                    let threshold = args.get(1).and_then(|v| v.as_f64()).unwrap_or(2.0);
                    let anomalies = onto_core::time_series::detect_anomalies(&values, threshold);
                    Ok(Value::Array(anomalies.into_iter().map(|i| json!(i)).collect()))
                } else {
                    Ok(Value::Null)
                }
            }
            "TS_DTW" => {
                // TS_DTW(array1, array2) → DTW distance between two sequences
                if args.len() >= 2 {
                    match (&args[0], &args[1]) {
                        (Value::Array(a1), Value::Array(a2)) => {
                            let v1: Vec<f64> = a1.iter().filter_map(|v| v.as_f64()).collect();
                            let v2: Vec<f64> = a2.iter().filter_map(|v| v.as_f64()).collect();
                            let distance = onto_core::time_series::dtw_distance(&v1, &v2, None);
                            Ok(json!(distance))
                        }
                        _ => Ok(Value::Null),
                    }
                } else {
                    Ok(Value::Null)
                }
            }
            "TS_PERCENTILE" => {
                // TS_PERCENTILE(array, percentile) → percentile value
                if let Some(Value::Array(arr)) = args.first() {
                    let mut values: Vec<f64> = arr.iter().filter_map(|v| v.as_f64()).collect();
                    let percentile = args.get(1).and_then(|v| v.as_f64()).unwrap_or(50.0) / 100.0;
                    if values.is_empty() {
                        Ok(Value::Null)
                    } else {
                        values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                        let idx = (percentile * (values.len() - 1) as f64).round() as usize;
                        Ok(json!(values[idx.min(values.len() - 1)]))
                    }
                } else {
                    Ok(Value::Null)
                }
            }
            "TS_MEDIAN" => {
                // TS_MEDIAN(array) → median value
                if let Some(Value::Array(arr)) = args.first() {
                    let mut values: Vec<f64> = arr.iter().filter_map(|v| v.as_f64()).collect();
                    if values.is_empty() {
                        Ok(Value::Null)
                    } else {
                        values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                        let mid = values.len() / 2;
                        if values.len().is_multiple_of(2) {
                            Ok(json!((values[mid - 1] + values[mid]) / 2.0))
                        } else {
                            Ok(json!(values[mid]))
                        }
                    }
                } else {
                    Ok(Value::Null)
                }
            }
            _ => Err(CoreError::InvalidArgument(format!("unknown function: {}", name))),
        }
    }

    /// Evaluates arithmetic on two JSON values.
    fn eval_arithmetic(op: &ArithmeticOp, left: &Value, right: &Value) -> Result<Value> {
        let l = match left {
            Value::Number(n) => n.as_f64().unwrap_or(0.0),
            Value::String(s) => s.parse::<f64>().unwrap_or(0.0),
            _ => 0.0,
        };
        let r = match right {
            Value::Number(n) => n.as_f64().unwrap_or(0.0),
            Value::String(s) => s.parse::<f64>().unwrap_or(0.0),
            _ => 0.0,
        };
        let result = match op {
            ArithmeticOp::Add => l + r,
            ArithmeticOp::Sub => l - r,
            ArithmeticOp::Mul => l * r,
            ArithmeticOp::Div => {
                if r == 0.0 {
                    return Ok(Value::Null);
                }
                l / r
            }
        };
        Ok(json!(result))
    }

    fn project_columns(&self, doc: &Map<String, Value>, columns: &SelectColumns) -> Map<String, Value> {
        match columns {
            SelectColumns::All => doc.clone(),
            SelectColumns::Columns(items) => {
                let mut result = Map::new();
                for item in items {
                    match item {
                        SelectItem::Column(col) => {
                            // Handle "alias" in "col as alias"
                            let (real_col, alias) = if let Some(as_pos) = col.find(" as ") {
                                (&col[..as_pos], Some(col[as_pos + 4..].trim()))
                            } else {
                                (col.as_str(), None)
                            };

                            // Try direct lookup first (preserves Value type)
                            if let Some(val) = doc.get(real_col) {
                                let name = alias.unwrap_or(real_col);
                                let name = name.split('.').next_back().unwrap_or(name);
                                result.insert(name.to_string(), val.clone());
                            } else {
                                // Try alias-aware lookup
                                for (k, v) in doc {
                                    if k.ends_with(&format!(".{}", real_col)) || k == real_col {
                                        let name = alias.unwrap_or(real_col);
                                        let name = name.split('.').next_back().unwrap_or(name);
                                        result.insert(name.to_string(), v.clone());
                                        break;
                                    }
                                }
                            }
                        }
                        SelectItem::Aggregate(_) => {
                            // Aggregates are handled by execute_aggregation
                        }
                        SelectItem::WindowFunction(_) => {
                            // Window functions are handled separately
                        }
                        SelectItem::Expression(_expr) => {
                            // Evaluate the expression against the current row
                            // We need engine access, but project_columns doesn't have it
                            // This is handled in the Projection plan node before calling project_columns
                        }
                    }
                }
                result
            }
        }
    }

    // 鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺?
    //  Schema Validation
    // 鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺?

    /// Validates a document against the ontology schema.
    /// Checks: class exists, required fields present, type compatibility.
    /// Returns Ok(()) if valid, Err with a descriptive message if not.
    fn validate_document(
        &self,
        engine: &LsmEngine,
        class: &str,
        doc: &Map<String, Value>,
    ) -> Result<()> {
        // Find the ontology containing this class
        let ontology = match self.ontology_store.find_ontology_for_class(engine, class)? {
            Some(o) => o,
            None => return Ok(()), // No ontology defined — skip validation
        };

        let class_def = match ontology.classes.get(class) {
            Some(c) => c,
            None => return Ok(()), // Class not in ontology (shouldn't happen since find succeeded)
        };

        // Collect all properties for this class (including inherited)
        let props = ontology.get_class_properties(class);
        let prop_map: std::collections::HashMap<&str, &onto_ontology::Property> =
            props.iter().map(|p| (p.name.as_str(), *p)).collect();

        // Check required fields
        for prop in &props {
            if prop.required && !doc.contains_key(&prop.name) {
                return Err(CoreError::InvalidArgument(format!(
                    "required property '{}' is missing for class '{}'",
                    prop.name, class
                )));
            }
        }

        // Validate types of provided fields
        for (field_name, field_value) in doc {
            if field_name == "__class__" {
                continue;
            }
            if let Some(prop) = prop_map.get(field_name.as_str()) {
                Self::validate_value_type(field_name, field_value, prop.range)?;
            }
            // Fields not in the ontology are allowed (schema-on-read compatible)
        }

        // Validate OWL restrictions
        self.validate_restrictions(engine, &ontology, class_def, doc)?;

        // Validate class disjointness
        self.validate_disjointness(engine, &ontology, class, doc)?;

        Ok(())
    }

    /// Validates OWL restrictions on a document during INSERT.
    ///
    /// Checks: hasValue, MinCardinality, MaxCardinality, ExactCardinality,
    /// someValuesFrom, allValuesFrom.
    fn validate_restrictions(
        &self,
        engine: &LsmEngine,
        ontology: &onto_ontology::Ontology,
        class_def: &onto_ontology::Class,
        doc: &Map<String, Value>,
    ) -> Result<()> {
        // Collect restrictions from this class and all superclasses
        let mut all_restrictions = Vec::new();
        Self::collect_restrictions(ontology, &class_def.name, &mut all_restrictions, &mut std::collections::HashSet::new());

        for restriction in &all_restrictions {
            match restriction {
                onto_ontology::Restriction::HasValue { property, value } => {
                    // The property must have this specific value
                    if let Some(doc_val) = doc.get(property) {
                        let expected = Self::ontology_literal_to_json(value);
                        if doc_val != &expected {
                            return Err(CoreError::InvalidArgument(format!(
                                "restriction violation: property '{}' must have value {:?}, got {}",
                                property, value, doc_val
                            )));
                        }
                    }
                }
                onto_ontology::Restriction::MinCardinality { property, min } => {
                    // Check minimum number of values for the property
                    let count = Self::count_property_values(doc, property);
                    if count < *min {
                        return Err(CoreError::InvalidArgument(format!(
                            "restriction violation: property '{}' requires at least {} values, got {}",
                            property, min, count
                        )));
                    }
                }
                onto_ontology::Restriction::MaxCardinality { property, max } => {
                    let count = Self::count_property_values(doc, property);
                    if count > *max {
                        return Err(CoreError::InvalidArgument(format!(
                            "restriction violation: property '{}' allows at most {} values, got {}",
                            property, max, count
                        )));
                    }
                }
                onto_ontology::Restriction::ExactCardinality { property, count: expected } => {
                    let count = Self::count_property_values(doc, property);
                    if count != *expected {
                        return Err(CoreError::InvalidArgument(format!(
                            "restriction violation: property '{}' requires exactly {} values, got {}",
                            property, expected, count
                        )));
                    }
                }
                onto_ontology::Restriction::SomeValuesFrom { property, class: required_class } => {
                    // At least one value of the property must be an instance of the required class
                    if let Some(val) = doc.get(property) {
                        if let Value::String(ref_id) = val {
                            if !self.is_instance_of_class(engine, ref_id, required_class)? {
                                return Err(CoreError::InvalidArgument(format!(
                                    "restriction violation: property '{}' must reference an instance of class '{}'",
                                    property, required_class
                                )));
                            }
                        }
                    }
                }
                onto_ontology::Restriction::AllValuesFrom { property, class: required_class } => {
                    // All values of the property must be instances of the required class
                    if let Some(val) = doc.get(property) {
                        match val {
                            Value::String(ref_id) => {
                                if !self.is_instance_of_class(engine, ref_id, required_class)? {
                                    return Err(CoreError::InvalidArgument(format!(
                                        "restriction violation: all values of property '{}' must be instances of class '{}'",
                                        property, required_class
                                    )));
                                }
                            }
                            Value::Array(items) => {
                                for item in items {
                                    if let Value::String(ref_id) = item {
                                        if !self.is_instance_of_class(engine, ref_id, required_class)? {
                                            return Err(CoreError::InvalidArgument(format!(
                                                "restriction violation: all values of property '{}' must be instances of class '{}'",
                                                property, required_class
                                            )));
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Recursively collects restrictions from a class and its superclasses.
    fn collect_restrictions(
        ontology: &onto_ontology::Ontology,
        class_name: &str,
        restrictions: &mut Vec<onto_ontology::Restriction>,
        visited: &mut std::collections::HashSet<String>,
    ) {
        if !visited.insert(class_name.to_string()) {
            return;
        }
        if let Some(class) = ontology.classes.get(class_name) {
            restrictions.extend(class.restrictions.clone());
            for superclass in &class.superclasses {
                Self::collect_restrictions(ontology, superclass, restrictions, visited);
            }
        }
    }

    /// Counts the number of values for a property in a document.
    /// Returns 0 if the property is missing, 1 for scalar values, array length for arrays.
    fn count_property_values(doc: &Map<String, Value>, property: &str) -> usize {
        match doc.get(property) {
            None => 0,
            Some(Value::Null) => 0,
            Some(Value::Array(arr)) => arr.len(),
            Some(_) => 1,
        }
    }

    /// Checks if a document with the given ID is an instance of the specified class.
    fn is_instance_of_class(
        &self,
        engine: &LsmEngine,
        doc_id: &str,
        required_class: &str,
    ) -> Result<bool> {
        // Get the class hierarchy for the required class (includes subclasses)
        let hierarchy = self.get_class_hierarchy(engine, required_class);

        // Search all classes in the hierarchy for the document
        for class_name in &hierarchy {
            let key = format!("{}::{}", class_name, doc_id);
            if let Ok(Some(val_bytes)) = engine.get(key.as_bytes()) {
                if let Ok(serde_json::Value::Object(ref doc)) = serde_json::from_slice::<serde_json::Value>(&val_bytes) {
                    if let Some(Value::String(doc_class)) = doc.get("__class__") {
                        if hierarchy.contains(doc_class.as_str()) {
                            return Ok(true);
                        }
                    }
                }
            }
        }

        Ok(false)
    }

    /// Validates class disjointness constraints during INSERT.
    /// If class A is disjoint with class B, no instance can be of both types.
    fn validate_disjointness(
        &self,
        engine: &LsmEngine,
        ontology: &onto_ontology::Ontology,
        class: &str,
        doc: &Map<String, Value>,
    ) -> Result<()> {
        let class_def = match ontology.classes.get(class) {
            Some(c) => c,
            None => return Ok(()),
        };

        for disjoint_class in &class_def.disjoint_with {
            // Check if any existing instance with the same ID exists in the disjoint class
            if let Some(Value::String(pk)) = doc.get("__pk__") {
                let doc_id = pk.rsplit("::").next().unwrap_or(pk);
                let disjoint_key = format!("{}::{}", disjoint_class, doc_id);
                if let Ok(Some(_)) = engine.get(disjoint_key.as_bytes()) {
                    return Err(CoreError::InvalidArgument(format!(
                        "disjoint constraint violation: instance '{}' cannot be both '{}' and '{}'",
                        doc_id, class, disjoint_class
                    )));
                }
            }
        }

        Ok(())
    }

    /// Converts an ontology Literal to a JSON value for restriction validation.
    fn ontology_literal_to_json(lit: &onto_ontology::Literal) -> Value {
        match lit {
            onto_ontology::Literal::String(s) => json!(s),
            onto_ontology::Literal::Int(i) => json!(i),
            onto_ontology::Literal::Float(f) => json!(f),
            onto_ontology::Literal::Bool(b) => json!(b),
        }
    }

    /// Validates that a JSON value is compatible with the expected DataType.
    fn validate_value_type(field_name: &str, value: &Value, expected: DataType) -> Result<()> {
        let valid = match (expected, value) {
            (_, Value::Null) => true, // Null is always acceptable
            (DataType::String, Value::String(_)) => true,
            (DataType::Int64, Value::Number(n)) => n.is_i64(),
            (DataType::Float64, Value::Number(_)) => true, // int and float both ok
            (DataType::Bool, Value::Bool(_)) => true,
            (DataType::Array, Value::Array(_)) => true,
            (DataType::Object, Value::Object(_)) => true,
            (DataType::Bytes, Value::String(_)) => true, // bytes stored as base64 string
            _ => false,
        };
        if !valid {
            return Err(CoreError::InvalidArgument(format!(
                "type mismatch for property '{}': expected {:?}, got {}",
                field_name,
                expected,
                Self::json_type_name(value)
            )));
        }
        Ok(())
    }

    /// Returns a human-readable type name for a JSON value.
    fn json_type_name(value: &Value) -> &'static str {
        match value {
            Value::Null => "null",
            Value::Bool(_) => "bool",
            Value::Number(n) if n.is_i64() => "int64",
            Value::Number(_) => "float64",
            Value::String(_) => "string",
            Value::Array(_) => "array",
            Value::Object(_) => "object",
        }
    }

    fn literal_to_json(&self, lit: &LiteralValue) -> Value {
        match lit {
            LiteralValue::Null => Value::Null,
            LiteralValue::Bool(b) => json!(b),
            LiteralValue::Int(i) => json!(i),
            LiteralValue::Float(f) => json!(f),
            LiteralValue::String(s) => json!(s),
        }
    }

    /// Attempts to parse a JSON string that looks like a vector array "[0.1, 0.2, ...]"
    /// into an actual JSON array of numbers. Returns the original value if not a vector string.
    fn try_parse_vector(val: Value) -> Value {
        if let Value::String(ref s) = val {
            let trimmed = s.trim();
            if trimmed.starts_with('[') && trimmed.ends_with(']') {
                if let Ok(arr) = serde_json::from_str::<Vec<f64>>(trimmed) {
                    return Value::Array(arr.into_iter().map(|f| json!(f)).collect());
                }
            }
        }
        val
    }
}

/// Converts a JSON value to a LiteralValue for import.
#[allow(dead_code)]
fn json_to_literal(val: &serde_json::Value) -> LiteralValue {
    match val {
        serde_json::Value::Null => LiteralValue::Null,
        serde_json::Value::Bool(b) => LiteralValue::Bool(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                LiteralValue::Int(i)
            } else {
                LiteralValue::Float(n.as_f64().unwrap_or(0.0))
            }
        }
        serde_json::Value::String(s) => LiteralValue::String(s.clone()),
        // Arrays and objects are serialized as JSON strings for storage
        other => LiteralValue::String(other.to_string()),
    }
}

/// Returns a human-readable type name for a JSON value.
#[allow(dead_code)]
fn obj_type_name(val: &serde_json::Value) -> &'static str {
    match val {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

/// Query execution result.
#[derive(Debug)]
pub enum QueryResult {
    /// Success message (for INSERT, UPDATE, DELETE, CREATE).
    Success(String),
    /// Row results (for SELECT, MATCH).
    Rows(Vec<Map<String, Value>>),
}

impl QueryResult {
    /// Formats the result as a string.
    pub fn format(&self) -> String {
        match self {
            QueryResult::Success(msg) => msg.clone(),
            QueryResult::Rows(rows) => {
                if rows.is_empty() {
                    return "(0 rows)".to_string();
                }

                let mut output = String::new();

                // Header
                if let Some(first) = rows.first() {
                    let cols: Vec<&String> = first.keys().collect();
                    output.push_str(&cols.iter().map(|c| c.as_str()).collect::<Vec<_>>().join(" | "));
                    output.push('\n');
                    output.push_str(&"-".repeat(cols.iter().map(|c| c.len()).sum::<usize>() + cols.len() * 3));
                    output.push('\n');
                }

                // Rows
                for row in rows {
                    let vals: Vec<String> = row
                        .values()
                        .map(|v| match v {
                            Value::String(s) => s.clone(),
                            Value::Number(n) => n.to_string(),
                            Value::Bool(b) => b.to_string(),
                            Value::Null => "NULL".to_string(),
                            _ => v.to_string(),
                        })
                        .collect();
                    output.push_str(&vals.join(" | "));
                    output.push('\n');
                }

                output.push_str(&format!("({} rows)", rows.len()));
                output
            }
        }
    }
}

/// Format a PlanNode as a JSON value for EXPLAIN output.
fn format_plan_node(node: &crate::optimizer::PlanNode) -> Value {
    use crate::optimizer::PlanNode;

    match node {
        PlanNode::SeqScan { table, alias, estimated_rows, .. } => {
            json!({
                "type": "SeqScan",
                "table": table,
                "alias": alias,
                "rows": estimated_rows,
            })
        }
        PlanNode::IndexScan { table, index_column, estimated_rows, .. } => {
            json!({
                "type": "IndexScan",
                "table": table,
                "index": index_column,
                "rows": estimated_rows,
            })
        }
        PlanNode::IndexLookup { table, index_column, estimated_rows, .. } => {
            json!({
                "type": "IndexLookup",
                "table": table,
                "index": index_column,
                "rows": estimated_rows,
            })
        }
        PlanNode::VectorSearch { table, column, top_k, estimated_rows, .. } => {
            json!({
                "type": "VectorSearch",
                "table": table,
                "column": column,
                "top_k": top_k,
                "rows": estimated_rows,
            })
        }
        PlanNode::Filter { input, estimated_rows, .. } => {
            json!({
                "type": "Filter",
                "input": format_plan_node(input),
                "rows": estimated_rows,
            })
        }
        PlanNode::Projection { input, estimated_rows, .. } => {
            json!({
                "type": "Projection",
                "input": format_plan_node(input),
                "rows": estimated_rows,
            })
        }
        PlanNode::NestedLoopJoin { left, right, estimated_rows, .. } => {
            json!({
                "type": "NestedLoopJoin",
                "left": format_plan_node(left),
                "right": format_plan_node(right),
                "rows": estimated_rows,
            })
        }
        PlanNode::HashJoin { left, right, estimated_rows, .. } => {
            json!({
                "type": "HashJoin",
                "left": format_plan_node(left),
                "right": format_plan_node(right),
                "rows": estimated_rows,
            })
        }
        PlanNode::SortMergeJoin { left, right, estimated_rows, .. } => {
            json!({
                "type": "SortMergeJoin",
                "left": format_plan_node(left),
                "right": format_plan_node(right),
                "rows": estimated_rows,
            })
        }
        PlanNode::Sort { input, order_by, estimated_rows, .. } => {
            let ob_json: Vec<Value> = order_by.iter().map(|ob| {
                json!({"column": ob.column, "ascending": ob.ascending})
            }).collect();
            json!({
                "type": "Sort",
                "input": format_plan_node(input),
                "order_by": ob_json,
                "rows": estimated_rows,
            })
        }
        PlanNode::Aggregation { input, group_by, estimated_rows, .. } => {
            json!({
                "type": "Aggregation",
                "input": format_plan_node(input),
                "group_by": group_by,
                "rows": estimated_rows,
            })
        }
        PlanNode::Limit { input, count, estimated_rows, .. } => {
            json!({
                "type": "Limit",
                "input": format_plan_node(input),
                "count": count,
                "rows": estimated_rows,
            })
        }
        PlanNode::Union { left, right, all, estimated_rows, .. } => {
            json!({
                "type": "Union",
                "all": all,
                "left": format_plan_node(left),
                "right": format_plan_node(right),
                "rows": estimated_rows,
            })
        }
        PlanNode::WindowFunction { input, windows, estimated_rows, .. } => {
            let win_json: Vec<Value> = windows.iter().map(|w| {
                json!({
                    "function": format!("{:?}", w.func),
                    "argument": w.arg,
                    "alias": w.alias,
                    "partition_by": w.over.partition_by,
                })
            }).collect();
            json!({
                "type": "WindowFunction",
                "input": format_plan_node(input),
                "windows": win_json,
                "rows": estimated_rows,
            })
        }
    }
}

/// Parse a geometry from a JSON value.
///
/// Supports:
/// - WKB hex string (from ST_POINT, ST_FROM_TEXT)
/// - WKT string (e.g., "POINT(116.4 39.9)")
/// - GeoJSON object
fn parse_geometry_from_value(val: &Value) -> Option<onto_core::geo::Geometry> {
    match val {
        Value::String(s) => {
            // Try WKB hex first
            if let Ok(wkb) = hex::decode(s) {
                if let Some(geom) = onto_core::geo::Geometry::from_wkb(&wkb) {
                    return Some(geom);
                }
            }
            // Try WKT
            onto_core::geo::Geometry::from_wkt(s)
        }
        Value::Object(map) => {
            // Try GeoJSON
            parse_geojson(map)
        }
        _ => None,
    }
}

/// Parse a GeoJSON object into a Geometry.
fn parse_geojson(map: &serde_json::Map<String, Value>) -> Option<onto_core::geo::Geometry> {
    let geom_type = map.get("type")?.as_str()?;
    let coords = map.get("coordinates")?;

    match geom_type {
        "Point" => {
            let arr = coords.as_array()?;
            if arr.len() >= 2 {
                let lon = arr[0].as_f64()?;
                let lat = arr[1].as_f64()?;
                Some(onto_core::geo::Geometry::Point(onto_core::geo::Coord::new(lon, lat)))
            } else {
                None
            }
        }
        "LineString" => {
            let arr = coords.as_array()?;
            let mut points = Vec::new();
            for coord in arr {
                let c = coord.as_array()?;
                if c.len() >= 2 {
                    points.push(onto_core::geo::Coord::new(c[0].as_f64()?, c[1].as_f64()?));
                }
            }
            Some(onto_core::geo::Geometry::LineString(points))
        }
        "Polygon" => {
            let arr = coords.as_array()?;
            let mut rings = Vec::new();
            for ring in arr {
                let ring_arr = ring.as_array()?;
                let mut points = Vec::new();
                for coord in ring_arr {
                    let c = coord.as_array()?;
                    if c.len() >= 2 {
                        points.push(onto_core::geo::Coord::new(c[0].as_f64()?, c[1].as_f64()?));
                    }
                }
                rings.push(points);
            }
            Some(onto_core::geo::Geometry::Polygon(rings))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::QueryParser;
    use onto_storage::StorageOptions;
    use tempfile::tempdir;

    fn setup() -> (QueryExecutor, tempfile::TempDir) {
        setup_with_config(QueryConfig::default())
    }

    fn setup_with_config(config: QueryConfig) -> (QueryExecutor, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            memtable_size_limit: 1024 * 1024,
            ..Default::default()
        };
        let engine = Arc::new(LsmEngine::open(options).unwrap());
        let ontology_store = OntologyStore::new(engine.clone());
        let executor = QueryExecutor::with_config(engine, ontology_store, config);
        (executor, dir)
    }

    fn insert_row(executor: &QueryExecutor, class: &str, name: &str, price: i64) {
        let ast = QueryAst::Insert {
            class: class.to_string(),
            columns: vec!["name".to_string(), "price".to_string()],
            values: vec![
                LiteralValue::String(name.to_string()),
                LiteralValue::Int(price),
            ],
        };
        executor.execute(&ast).unwrap();
    }

    #[test]
    fn test_select_all() {
        let (executor, _dir) = setup();

        insert_row(&executor, "Product", "iPhone", 999);
        insert_row(&executor, "Product", "iPad", 799);
        insert_row(&executor, "Product", "MacBook", 1999);

        // Flush to ensure data is in SSTables
        executor.engine().flush().unwrap();

        let ast = QueryParser::parse("SELECT * FROM Product").unwrap();
        let result = executor.execute(&ast).unwrap();

        match &result {
            QueryResult::Rows(rows) => assert_eq!(rows.len(), 3),
            _ => panic!("expected Rows"),
        }
    }

    #[test]
    fn test_select_with_filter() {
        let (executor, _dir) = setup();

        insert_row(&executor, "Product", "iPhone", 999);
        insert_row(&executor, "Product", "iPad", 799);
        insert_row(&executor, "Product", "MacBook", 1999);

        executor.engine().flush().unwrap();

        let ast = QueryParser::parse("SELECT name, price FROM Product WHERE price > 900").unwrap();
        let result = executor.execute(&ast).unwrap();

        match &result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 2); // iPhone and MacBook
            }
            _ => panic!("expected Rows"),
        }
    }

    #[test]
    fn test_select_with_limit() {
        let (executor, _dir) = setup();

        insert_row(&executor, "Product", "iPhone", 999);
        insert_row(&executor, "Product", "iPad", 799);
        insert_row(&executor, "Product", "MacBook", 1999);

        executor.engine().flush().unwrap();

        let ast = QueryParser::parse("SELECT * FROM Product LIMIT 2").unwrap();
        let result = executor.execute(&ast).unwrap();

        match &result {
            QueryResult::Rows(rows) => assert_eq!(rows.len(), 2),
            _ => panic!("expected Rows"),
        }
    }

    #[test]
    fn test_select_different_classes() {
        let (executor, _dir) = setup();

        insert_row(&executor, "Product", "iPhone", 999);
        insert_row(&executor, "Customer", "Alice", 0);
        insert_row(&executor, "Product", "iPad", 799);

        executor.engine().flush().unwrap();

        // Should only return Products
        let ast = QueryParser::parse("SELECT * FROM Product").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => assert_eq!(rows.len(), 2),
            _ => panic!("expected Rows"),
        }

        // Should only return Customers
        let ast = QueryParser::parse("SELECT * FROM Customer").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => assert_eq!(rows.len(), 1),
            _ => panic!("expected Rows"),
        }
    }

    #[test]
    fn test_update() {
        let (executor, _dir) = setup();

        insert_row(&executor, "Product", "iPhone", 999);
        insert_row(&executor, "Product", "iPad", 799);

        executor.engine().flush().unwrap();

        let ast = QueryParser::parse("UPDATE Product SET price = 899 WHERE name = 'iPhone'").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Success(msg) => assert!(msg.contains("1 row(s) updated")),
            _ => panic!("expected Success"),
        }

        // Verify the update
        let ast = QueryParser::parse("SELECT * FROM Product WHERE name = 'iPhone'").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].get("price").unwrap().as_i64().unwrap(), 899);
            }
            _ => panic!("expected Rows"),
        }
    }

    #[test]
    fn test_delete() {
        let (executor, _dir) = setup();

        insert_row(&executor, "Product", "iPhone", 999);
        insert_row(&executor, "Product", "iPad", 799);

        executor.engine().flush().unwrap();

        let ast = QueryParser::parse("DELETE FROM Product WHERE name = 'iPhone'").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Success(msg) => assert!(msg.contains("1 row(s) deleted")),
            _ => panic!("expected Success"),
        }

        // Verify only iPad remains
        let ast = QueryParser::parse("SELECT * FROM Product").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].get("name").unwrap().as_str().unwrap(), "iPad");
            }
            _ => panic!("expected Rows"),
        }
    }

    #[test]
    fn test_match_semantic_query() {
        let (executor, _dir) = setup();

        insert_row(&executor, "Product", "iPhone", 999);
        insert_row(&executor, "Product", "iPad", 799);

        executor.engine().flush().unwrap();

        let ast = QueryParser::parse("MATCH (p: Product) WHERE price > 900 RETURN name, price").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].get("name").unwrap().as_str().unwrap(), "iPhone");
            }
            _ => panic!("expected Rows"),
        }
    }

    // 鈹€鈹€ UPDATE edge cases 鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€

    #[test]
    fn test_update_no_match() {
        let (executor, _dir) = setup();
        insert_row(&executor, "Product", "iPhone", 999);
        executor.engine().flush().unwrap();

        let ast = QueryParser::parse("UPDATE Product SET price = 899 WHERE name = 'Galaxy'").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Success(msg) => assert!(msg.contains("0 row(s) updated")),
            _ => panic!("expected Success with 0 updated"),
        }
    }

    #[test]
    fn test_update_multiple_rows() {
        let (executor, _dir) = setup();
        insert_row(&executor, "Product", "iPhone", 999);
        insert_row(&executor, "Product", "iPad", 999);
        insert_row(&executor, "Product", "MacBook", 1999);
        executor.engine().flush().unwrap();

        // Update all rows with price 999
        let ast = QueryParser::parse("UPDATE Product SET price = 899 WHERE price = 999").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Success(msg) => assert!(msg.contains("2 row(s) updated")),
            _ => panic!("expected 2 updated"),
        }

        // Verify both are updated
        let ast = QueryParser::parse("SELECT * FROM Product WHERE price = 899").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => assert_eq!(rows.len(), 2),
            _ => panic!("expected 2 rows"),
        }
    }

    #[test]
    fn test_update_multiple_fields() {
        let (executor, _dir) = setup();
        insert_row(&executor, "Product", "iPhone", 999);
        executor.engine().flush().unwrap();

        let ast = QueryParser::parse("UPDATE Product SET name = 'iPhone 15', price = 1099 WHERE name = 'iPhone'").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Success(msg) => assert!(msg.contains("1 row(s) updated")),
            _ => panic!("expected 1 updated"),
        }

        let ast = QueryParser::parse("SELECT * FROM Product WHERE name = 'iPhone 15'").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].get("price").unwrap().as_i64().unwrap(), 1099);
            }
            _ => panic!("expected 1 row"),
        }
    }

    // 鈹€鈹€ DELETE edge cases 鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€

    #[test]
    fn test_delete_no_match() {
        let (executor, _dir) = setup();
        insert_row(&executor, "Product", "iPhone", 999);
        executor.engine().flush().unwrap();

        let ast = QueryParser::parse("DELETE FROM Product WHERE name = 'Galaxy'").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Success(msg) => assert!(msg.contains("0 row(s) deleted")),
            _ => panic!("expected 0 deleted"),
        }

        // Original row still exists
        let ast = QueryParser::parse("SELECT * FROM Product").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => assert_eq!(rows.len(), 1),
            _ => panic!("expected 1 row"),
        }
    }

    #[test]
    fn test_delete_multiple_rows() {
        let (executor, _dir) = setup();
        insert_row(&executor, "Product", "iPhone", 999);
        insert_row(&executor, "Product", "iPad", 799);
        insert_row(&executor, "Product", "MacBook", 1999);
        executor.engine().flush().unwrap();

        // Delete all with price < 1000
        let ast = QueryParser::parse("DELETE FROM Product WHERE price < 1000").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Success(msg) => assert!(msg.contains("2 row(s) deleted")),
            _ => panic!("expected 2 deleted"),
        }

        // Only MacBook remains
        let ast = QueryParser::parse("SELECT * FROM Product").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].get("name").unwrap().as_str().unwrap(), "MacBook");
            }
            _ => panic!("expected 1 row"),
        }
    }

    #[test]
    fn test_delete_all() {
        let (executor, _dir) = setup();
        insert_row(&executor, "Product", "iPhone", 999);
        insert_row(&executor, "Product", "iPad", 799);
        executor.engine().flush().unwrap();

        // Delete all (no WHERE)
        let ast = QueryParser::parse("DELETE FROM Product").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Success(msg) => assert!(msg.contains("2 row(s) deleted")),
            _ => panic!("expected 2 deleted"),
        }

        let ast = QueryParser::parse("SELECT * FROM Product").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => assert_eq!(rows.len(), 0),
            _ => panic!("expected 0 rows"),
        }
    }

    // 鈹€鈹€ Full lifecycle 鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€

    #[test]
    fn test_full_lifecycle() {
        let (executor, _dir) = setup();

        // 1. CREATE ONTOLOGY
        let ast = QueryParser::parse(
            "CREATE ONTOLOGY shop (CLASS Product, PROPERTY name DOMAIN Product RANGE STRING, PROPERTY price DOMAIN Product RANGE INT64)"
        ).unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Success(msg) => assert!(msg.contains("created")),
            _ => panic!("expected Success"),
        }

        // 2. INSERT
        for (name, price) in [("iPhone", 999), ("iPad", 799), ("MacBook", 1999), ("AirPods", 249)] {
            let ast = QueryParser::parse(&format!(
                "INSERT INTO Product (name, price) VALUES ('{}', {})", name, price
            )).unwrap();
            executor.execute(&ast).unwrap();
        }

        // 3. SELECT all
        let ast = QueryParser::parse("SELECT * FROM Product").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => assert_eq!(rows.len(), 4),
            _ => panic!("expected 4 rows"),
        }

        // 4. SELECT with filter
        let ast = QueryParser::parse("SELECT name FROM Product WHERE price > 500").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => assert_eq!(rows.len(), 3),
            _ => panic!("expected 3 rows"),
        }

        // 5. UPDATE
        let ast = QueryParser::parse("UPDATE Product SET price = 1099 WHERE name = 'iPhone'").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Success(msg) => assert!(msg.contains("1 row(s) updated")),
            _ => panic!("expected 1 updated"),
        }

        // 6. Verify update
        let ast = QueryParser::parse("SELECT price FROM Product WHERE name = 'iPhone'").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].get("price").unwrap().as_i64().unwrap(), 1099);
            }
            _ => panic!("expected updated price"),
        }

        // 7. DELETE
        let ast = QueryParser::parse("DELETE FROM Product WHERE price < 300").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Success(msg) => assert!(msg.contains("1 row(s) deleted")),
            _ => panic!("expected 1 deleted"),
        }

        // 8. Verify delete
        let ast = QueryParser::parse("SELECT * FROM Product").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => assert_eq!(rows.len(), 3),
            _ => panic!("expected 3 rows"),
        }

        // 9. MATCH
        let ast = QueryParser::parse("MATCH (p: Product) WHERE price > 1000 RETURN name").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => assert_eq!(rows.len(), 2), // iPhone 1099, MacBook 1999
            _ => panic!("expected 2 rows"),
        }
    }

    // 鈹€鈹€ Data persistence across flush 鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€

    #[test]
    fn test_data_persists_after_flush() {
        let (executor, _dir) = setup();

        insert_row(&executor, "Product", "iPhone", 999);
        executor.engine().flush().unwrap();

        // Insert more, flush again
        insert_row(&executor, "Product", "iPad", 799);
        executor.engine().flush().unwrap();

        // Both should be visible
        let ast = QueryParser::parse("SELECT * FROM Product").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => assert_eq!(rows.len(), 2),
            _ => panic!("expected 2 rows"),
        }
    }

    #[test]
    fn test_update_then_flush_then_select() {
        let (executor, _dir) = setup();

        insert_row(&executor, "Product", "iPhone", 999);
        executor.engine().flush().unwrap();

        // Update
        let ast = QueryParser::parse("UPDATE Product SET price = 899 WHERE name = 'iPhone'").unwrap();
        executor.execute(&ast).unwrap();
        executor.engine().flush().unwrap();

        // Verify after flush
        let ast = QueryParser::parse("SELECT price FROM Product WHERE name = 'iPhone'").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].get("price").unwrap().as_i64().unwrap(), 899);
            }
            _ => panic!("expected updated price after flush"),
        }
    }

    // 鈹€鈹€ JOIN tests 鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€

    fn insert_order(executor: &QueryExecutor, product_id: &str, quantity: i64) {
        let ast = QueryAst::Insert {
            class: "Order".to_string(),
            columns: vec!["product_id".to_string(), "quantity".to_string()],
            values: vec![
                LiteralValue::String(product_id.to_string()),
                LiteralValue::Int(quantity),
            ],
        };
        executor.execute(&ast).unwrap();
    }

    #[test]
    fn test_join_basic() {
        let (executor, _dir) = setup();

        // Insert products
        insert_row(&executor, "Product", "iPhone", 999);
        insert_row(&executor, "Product", "iPad", 799);

        // Insert orders referencing products
        insert_order(&executor, "iPhone", 3);
        insert_order(&executor, "iPad", 5);
        insert_order(&executor, "iPhone", 1);

        executor.engine().flush().unwrap();

        // JOIN: Product p JOIN Order o ON p.name = o.product_id
        let ast = QueryParser::parse(
            "SELECT p.name, o.quantity FROM Product p JOIN Order o ON p.name = o.product_id"
        ).unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 3); // 2 iPhone orders + 1 iPad order
            }
            _ => panic!("expected 3 rows from JOIN"),
        }
    }

    #[test]
    fn test_join_with_where() {
        let (executor, _dir) = setup();

        insert_row(&executor, "Product", "iPhone", 999);
        insert_row(&executor, "Product", "iPad", 799);

        insert_order(&executor, "iPhone", 3);
        insert_order(&executor, "iPad", 5);
        insert_order(&executor, "iPhone", 1);

        executor.engine().flush().unwrap();

        // JOIN with WHERE filter on right table
        let ast = QueryParser::parse(
            "SELECT p.name, o.quantity FROM Product p JOIN Order o ON p.name = o.product_id WHERE o.quantity > 2"
        ).unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 2); // iPhone qty=3, iPad qty=5
            }
            _ => panic!("expected 2 rows from JOIN with WHERE"),
        }
    }

    #[test]
    fn test_join_with_limit() {
        let (executor, _dir) = setup();

        insert_row(&executor, "Product", "iPhone", 999);
        insert_row(&executor, "Product", "iPad", 799);

        insert_order(&executor, "iPhone", 3);
        insert_order(&executor, "iPad", 5);
        insert_order(&executor, "iPhone", 1);

        executor.engine().flush().unwrap();

        let ast = QueryParser::parse(
            "SELECT p.name, o.quantity FROM Product p JOIN Order o ON p.name = o.product_id LIMIT 2"
        ).unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 2);
            }
            _ => panic!("expected 2 rows from JOIN with LIMIT"),
        }
    }

    #[test]
    fn test_join_no_match() {
        let (executor, _dir) = setup();

        insert_row(&executor, "Product", "iPhone", 999);

        // Order references non-existent product
        insert_order(&executor, "Galaxy", 1);

        executor.engine().flush().unwrap();

        let ast = QueryParser::parse(
            "SELECT p.name, o.quantity FROM Product p JOIN Order o ON p.name = o.product_id"
        ).unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 0); // No match
            }
            _ => panic!("expected 0 rows from JOIN with no match"),
        }
    }

    #[test]
    fn test_join_select_star() {
        let (executor, _dir) = setup();

        insert_row(&executor, "Product", "iPhone", 999);
        insert_order(&executor, "iPhone", 3);

        executor.engine().flush().unwrap();

        // SELECT * with JOIN should return all columns from both tables
        let ast = QueryParser::parse(
            "SELECT * FROM Product p JOIN Order o ON p.name = o.product_id"
        ).unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 1);
                // Should have columns from both tables
                assert!(rows[0].contains_key("p.name") || rows[0].contains_key("name"));
                assert!(rows[0].contains_key("o.quantity") || rows[0].contains_key("quantity"));
            }
            _ => panic!("expected 1 row from SELECT * JOIN"),
        }
    }

    // 鈹€鈹€ GROUP BY and aggregate tests 鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€

    #[test]
    fn test_count_star() {
        let (executor, _dir) = setup();
        insert_row(&executor, "Product", "iPhone", 999);
        insert_row(&executor, "Product", "iPad", 799);
        insert_row(&executor, "Product", "MacBook", 1999);
        executor.engine().flush().unwrap();

        let ast = QueryParser::parse("SELECT COUNT(*) FROM Product").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].get("count(*)").unwrap().as_i64().unwrap(), 3);
            }
            _ => panic!("expected 1 row with count"),
        }
    }

    #[test]
    fn test_sum_avg() {
        let (executor, _dir) = setup();
        insert_row(&executor, "Product", "iPhone", 999);
        insert_row(&executor, "Product", "iPad", 799);
        insert_row(&executor, "Product", "MacBook", 1999);
        executor.engine().flush().unwrap();

        let ast = QueryParser::parse("SELECT SUM(price) as total, AVG(price) as avg_price FROM Product").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].get("total").unwrap().as_i64().unwrap(), 3797);
                let avg = rows[0].get("avg_price").unwrap().as_f64().unwrap();
                assert!((avg - 1265.666).abs() < 1.0, "avg should be ~1265.67, got {}", avg);
            }
            _ => panic!("expected 1 row with sum and avg"),
        }
    }

    #[test]
    fn test_min_max() {
        let (executor, _dir) = setup();
        insert_row(&executor, "Product", "iPhone", 999);
        insert_row(&executor, "Product", "iPad", 799);
        insert_row(&executor, "Product", "MacBook", 1999);
        executor.engine().flush().unwrap();

        let ast = QueryParser::parse("SELECT MIN(price) as min_p, MAX(price) as max_p FROM Product").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].get("min_p").unwrap().as_str().unwrap(), "799");
                assert_eq!(rows[0].get("max_p").unwrap().as_str().unwrap(), "1999");
            }
            _ => panic!("expected 1 row with min and max"),
        }
    }

    #[test]
    fn test_group_by_basic() {
        let (executor, _dir) = setup();

        // Use a simple schema with a "category" field
        let ast = QueryAst::Insert {
            class: "Item".to_string(),
            columns: vec!["name".to_string(), "category".to_string(), "price".to_string()],
            values: vec![
                LiteralValue::String("iPhone".to_string()),
                LiteralValue::String("phone".to_string()),
                LiteralValue::Int(999),
            ],
        };
        executor.execute(&ast).unwrap();

        let ast = QueryAst::Insert {
            class: "Item".to_string(),
            columns: vec!["name".to_string(), "category".to_string(), "price".to_string()],
            values: vec![
                LiteralValue::String("iPad".to_string()),
                LiteralValue::String("tablet".to_string()),
                LiteralValue::Int(799),
            ],
        };
        executor.execute(&ast).unwrap();

        let ast = QueryAst::Insert {
            class: "Item".to_string(),
            columns: vec!["name".to_string(), "category".to_string(), "price".to_string()],
            values: vec![
                LiteralValue::String("Galaxy".to_string()),
                LiteralValue::String("phone".to_string()),
                LiteralValue::Int(899),
            ],
        };
        executor.execute(&ast).unwrap();

        let ast = QueryAst::Insert {
            class: "Item".to_string(),
            columns: vec!["name".to_string(), "category".to_string(), "price".to_string()],
            values: vec![
                LiteralValue::String("Pixel".to_string()),
                LiteralValue::String("phone".to_string()),
                LiteralValue::Int(699),
            ],
        };
        executor.execute(&ast).unwrap();

        executor.engine().flush().unwrap();

        // GROUP BY category with COUNT and SUM
        let ast = QueryParser::parse(
            "SELECT category, COUNT(*) as cnt, SUM(price) as total FROM Item GROUP BY category"
        ).unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 2); // phone, tablet

                // Find phone group
                let phone_row = rows.iter().find(|r| {
                    r.get("category").and_then(|v| v.as_str()) == Some("phone")
                }).unwrap();
                assert_eq!(phone_row.get("cnt").unwrap().as_i64().unwrap(), 3);
                assert_eq!(phone_row.get("total").unwrap().as_i64().unwrap(), 2597);

                // Find tablet group
                let tablet_row = rows.iter().find(|r| {
                    r.get("category").and_then(|v| v.as_str()) == Some("tablet")
                }).unwrap();
                assert_eq!(tablet_row.get("cnt").unwrap().as_i64().unwrap(), 1);
                assert_eq!(tablet_row.get("total").unwrap().as_i64().unwrap(), 799);
            }
            _ => panic!("expected 2 grouped rows"),
        }
    }

    #[test]
    fn test_group_by_with_having() {
        let (executor, _dir) = setup();

        for (name, cat, price) in [
            ("iPhone", "phone", 999),
            ("Galaxy", "phone", 899),
            ("Pixel", "phone", 699),
            ("iPad", "tablet", 799),
        ] {
            let ast = QueryAst::Insert {
                class: "Item".to_string(),
                columns: vec!["name".to_string(), "category".to_string(), "price".to_string()],
                values: vec![
                    LiteralValue::String(name.to_string()),
                    LiteralValue::String(cat.to_string()),
                    LiteralValue::Int(price),
                ],
            };
            executor.execute(&ast).unwrap();
        }
        executor.engine().flush().unwrap();

        // GROUP BY category HAVING COUNT(*) > 1
        let ast = QueryParser::parse(
            "SELECT category, COUNT(*) as cnt FROM Item GROUP BY category HAVING cnt > 1"
        ).unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 1); // only "phone" has count > 1
                assert_eq!(rows[0].get("category").unwrap().as_str().unwrap(), "phone");
                assert_eq!(rows[0].get("cnt").unwrap().as_i64().unwrap(), 3);
            }
            _ => panic!("expected 1 row after HAVING filter"),
        }
    }

    #[test]
    fn test_group_by_with_limit() {
        let (executor, _dir) = setup();

        for (name, cat, price) in [
            ("iPhone", "phone", 999),
            ("Galaxy", "phone", 899),
            ("iPad", "tablet", 799),
            ("MacBook", "laptop", 1999),
        ] {
            let ast = QueryAst::Insert {
                class: "Item".to_string(),
                columns: vec!["name".to_string(), "category".to_string(), "price".to_string()],
                values: vec![
                    LiteralValue::String(name.to_string()),
                    LiteralValue::String(cat.to_string()),
                    LiteralValue::Int(price),
                ],
            };
            executor.execute(&ast).unwrap();
        }
        executor.engine().flush().unwrap();

        let ast = QueryParser::parse(
            "SELECT category, COUNT(*) as cnt FROM Item GROUP BY category LIMIT 2"
        ).unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 2);
            }
            _ => panic!("expected 2 rows with LIMIT"),
        }
    }

    // 鈹€鈹€ ORDER BY tests 鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€

    #[test]
    fn test_order_by_asc() {
        let (executor, _dir) = setup();
        insert_row(&executor, "Product", "MacBook", 1999);
        insert_row(&executor, "Product", "iPhone", 999);
        insert_row(&executor, "Product", "iPad", 799);
        executor.engine().flush().unwrap();

        let ast = QueryParser::parse("SELECT name, price FROM Product ORDER BY price").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 3);
                assert_eq!(rows[0].get("name").unwrap().as_str().unwrap(), "iPad");
                assert_eq!(rows[1].get("name").unwrap().as_str().unwrap(), "iPhone");
                assert_eq!(rows[2].get("name").unwrap().as_str().unwrap(), "MacBook");
            }
            _ => panic!("expected sorted rows"),
        }
    }

    #[test]
    fn test_order_by_desc() {
        let (executor, _dir) = setup();
        insert_row(&executor, "Product", "MacBook", 1999);
        insert_row(&executor, "Product", "iPhone", 999);
        insert_row(&executor, "Product", "iPad", 799);
        executor.engine().flush().unwrap();

        let ast = QueryParser::parse("SELECT name, price FROM Product ORDER BY price DESC").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 3);
                assert_eq!(rows[0].get("name").unwrap().as_str().unwrap(), "MacBook");
                assert_eq!(rows[1].get("name").unwrap().as_str().unwrap(), "iPhone");
                assert_eq!(rows[2].get("name").unwrap().as_str().unwrap(), "iPad");
            }
            _ => panic!("expected reverse sorted rows"),
        }
    }

    #[test]
    fn test_order_by_with_limit() {
        let (executor, _dir) = setup();
        insert_row(&executor, "Product", "MacBook", 1999);
        insert_row(&executor, "Product", "iPhone", 999);
        insert_row(&executor, "Product", "iPad", 799);
        executor.engine().flush().unwrap();

        let ast = QueryParser::parse("SELECT name FROM Product ORDER BY price DESC LIMIT 2").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 2);
                assert_eq!(rows[0].get("name").unwrap().as_str().unwrap(), "MacBook");
                assert_eq!(rows[1].get("name").unwrap().as_str().unwrap(), "iPhone");
            }
            _ => panic!("expected top 2 by price desc"),
        }
    }

    #[test]
    fn test_order_by_group_by() {
        let (executor, _dir) = setup();

        for (name, cat, price) in [
            ("iPhone", "phone", 999),
            ("Galaxy", "phone", 899),
            ("iPad", "tablet", 799),
            ("MacBook", "laptop", 1999),
        ] {
            let ast = QueryAst::Insert {
                class: "Item".to_string(),
                columns: vec!["name".to_string(), "category".to_string(), "price".to_string()],
                values: vec![
                    LiteralValue::String(name.to_string()),
                    LiteralValue::String(cat.to_string()),
                    LiteralValue::Int(price),
                ],
            };
            executor.execute(&ast).unwrap();
        }
        executor.engine().flush().unwrap();

        // GROUP BY + ORDER BY total DESC
        let ast = QueryParser::parse(
            "SELECT category, SUM(price) as total FROM Item GROUP BY category ORDER BY total DESC"
        ).unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 3);
                // DESC: laptop(1999) > phone(1898) > tablet(799)
                assert_eq!(rows[0].get("category").unwrap().as_str().unwrap(), "laptop");
                assert_eq!(rows[1].get("category").unwrap().as_str().unwrap(), "phone");
                assert_eq!(rows[2].get("category").unwrap().as_str().unwrap(), "tablet");
            }
            _ => panic!("expected sorted grouped rows"),
        }
    }

    // 鈹€鈹€ DISTINCT tests 鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€

    #[test]
    fn test_distinct() {
        let (executor, _dir) = setup();
        insert_row(&executor, "Product", "iPhone", 999);
        insert_row(&executor, "Product", "iPhone", 999); // duplicate
        insert_row(&executor, "Product", "iPad", 799);
        executor.engine().flush().unwrap();

        let ast = QueryParser::parse("SELECT DISTINCT name, price FROM Product").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 2); // iPhone and iPad only
            }
            _ => panic!("expected 2 distinct rows"),
        }
    }

    // 鈹€鈹€ LIKE tests 鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€

    #[test]
    fn test_like_prefix() {
        let (executor, _dir) = setup();
        insert_row(&executor, "Product", "iPhone", 999);
        insert_row(&executor, "Product", "iPad", 799);
        insert_row(&executor, "Product", "iMac", 1299);
        insert_row(&executor, "Product", "MacBook", 1999);
        executor.engine().flush().unwrap();

        let ast = QueryParser::parse("SELECT name FROM Product WHERE name LIKE 'i%'").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 3); // iPhone, iPad, iMac
            }
            _ => panic!("expected 3 rows with 'i%' prefix"),
        }
    }

    #[test]
    fn test_like_suffix() {
        let (executor, _dir) = setup();
        insert_row(&executor, "Product", "iPhone", 999);
        insert_row(&executor, "Product", "iPad", 799);
        insert_row(&executor, "Product", "MacBook Pro", 1999);
        executor.engine().flush().unwrap();

        let ast = QueryParser::parse("SELECT name FROM Product WHERE name LIKE '%Pro'").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].get("name").unwrap().as_str().unwrap(), "MacBook Pro");
            }
            _ => panic!("expected 1 row with '%Pro' suffix"),
        }
    }

    #[test]
    fn test_like_single_char() {
        let (executor, _dir) = setup();
        insert_row(&executor, "Product", "iPhone", 999);
        insert_row(&executor, "Product", "iPad", 799);
        insert_row(&executor, "Product", "iMac", 1299);
        executor.engine().flush().unwrap();

        // i_ade should NOT match (underscore is exactly one char)
        let ast = QueryParser::parse("SELECT name FROM Product WHERE name LIKE 'iP_d'").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 1); // iPad
                assert_eq!(rows[0].get("name").unwrap().as_str().unwrap(), "iPad");
            }
            _ => panic!("expected 1 row for 'iP_d'"),
        }
    }

    // 鈹€鈹€ BETWEEN tests 鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€

    #[test]
    fn test_between() {
        let (executor, _dir) = setup();
        insert_row(&executor, "Product", "iPhone", 999);
        insert_row(&executor, "Product", "iPad", 799);
        insert_row(&executor, "Product", "MacBook", 1999);
        insert_row(&executor, "Product", "AirPods", 249);
        executor.engine().flush().unwrap();

        let ast = QueryParser::parse("SELECT name, price FROM Product WHERE price BETWEEN 500 AND 1500").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 2); // iPad(799), iPhone(999)
            }
            _ => panic!("expected 2 rows between 500 and 1500"),
        }
    }

    // 鈹€鈹€ IN tests 鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€

    #[test]
    fn test_in() {
        let (executor, _dir) = setup();
        insert_row(&executor, "Product", "iPhone", 999);
        insert_row(&executor, "Product", "iPad", 799);
        insert_row(&executor, "Product", "MacBook", 1999);
        insert_row(&executor, "Product", "AirPods", 249);
        executor.engine().flush().unwrap();

        let ast = QueryParser::parse("SELECT name FROM Product WHERE name IN ('iPhone', 'MacBook', 'AirPods')").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 3);
            }
            _ => panic!("expected 3 rows with IN"),
        }
    }

    #[test]
    fn test_in_with_numbers() {
        let (executor, _dir) = setup();
        insert_row(&executor, "Product", "iPhone", 999);
        insert_row(&executor, "Product", "iPad", 799);
        insert_row(&executor, "Product", "MacBook", 1999);
        executor.engine().flush().unwrap();

        let ast = QueryParser::parse("SELECT name FROM Product WHERE price IN (799, 1999)").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 2); // iPad, MacBook
            }
            _ => panic!("expected 2 rows with IN on numbers"),
        }
    }

    // 鈹€鈹€ UNION tests 鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€

    #[test]
    fn test_union_basic() {
        let (executor, _dir) = setup();
        insert_row(&executor, "Product", "iPhone", 999);
        insert_row(&executor, "Product", "iPad", 799);

        // Insert into a different class
        let ast = QueryAst::Insert {
            class: "Item".to_string(),
            columns: vec!["name".to_string(), "price".to_string()],
            values: vec![LiteralValue::String("Widget".to_string()), LiteralValue::Int(49)],
        };
        executor.execute(&ast).unwrap();
        executor.engine().flush().unwrap();

        let ast = QueryParser::parse(
            "SELECT name FROM Product UNION SELECT name FROM Item"
        ).unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 3); // iPhone, iPad, Widget
            }
            _ => panic!("expected 3 rows from UNION"),
        }
    }

    #[test]
    fn test_union_all() {
        let (executor, _dir) = setup();
        insert_row(&executor, "Product", "iPhone", 999);

        let ast = QueryAst::Insert {
            class: "Item".to_string(),
            columns: vec!["name".to_string(), "price".to_string()],
            values: vec![LiteralValue::String("iPhone".to_string()), LiteralValue::Int(999)],
        };
        executor.execute(&ast).unwrap();
        executor.engine().flush().unwrap();

        // UNION ALL keeps duplicates
        let ast = QueryParser::parse(
            "SELECT name FROM Product UNION ALL SELECT name FROM Item"
        ).unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 2); // iPhone from Product + iPhone from Item
            }
            _ => panic!("expected 2 rows from UNION ALL"),
        }

        // UNION (without ALL) removes duplicates
        let ast = QueryParser::parse(
            "SELECT name FROM Product UNION SELECT name FROM Item"
        ).unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 1); // deduplicated
            }
            _ => panic!("expected 1 row from UNION (dedup)"),
        }
    }

    // 鈹€鈹€ Subquery tests 鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€

    #[test]
    fn test_subquery_in_where() {
        let (executor, _dir) = setup();
        insert_row(&executor, "Product", "iPhone", 999);
        insert_row(&executor, "Product", "iPad", 799);
        insert_row(&executor, "Product", "MacBook", 1999);
        executor.engine().flush().unwrap();

        // Subquery: select names where price > 900
        let ast = QueryParser::parse(
            "SELECT name FROM Product WHERE name IN (SELECT name FROM Product WHERE price > 900)"
        ).unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 2); // iPhone, MacBook
            }
            _ => panic!("expected 2 rows from subquery IN"),
        }
    }

    #[test]
    fn test_subquery_no_match() {
        let (executor, _dir) = setup();
        insert_row(&executor, "Product", "iPhone", 999);
        executor.engine().flush().unwrap();

        // Subquery returns empty set
        let ast = QueryParser::parse(
            "SELECT name FROM Product WHERE name IN (SELECT name FROM Product WHERE price > 9999)"
        ).unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 0);
            }
            _ => panic!("expected 0 rows from empty subquery"),
        }
    }

    // 鈹€鈹€ Index-accelerated range query tests 鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€

    #[test]
    fn test_index_range_gt() {
        let (executor, _dir) = setup();

        // Create index on price
        let ast = QueryParser::parse("CREATE INDEX ON Product (price)").unwrap();
        executor.execute(&ast).unwrap();

        insert_row(&executor, "Product", "iPhone", 999);
        insert_row(&executor, "Product", "iPad", 799);
        insert_row(&executor, "Product", "MacBook", 1999);
        insert_row(&executor, "Product", "AirPods", 249);
        executor.engine().flush().unwrap();

        // Range query: price > 500 鈥?should use index
        let ast = QueryParser::parse("SELECT name, price FROM Product WHERE price > 500").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 3); // iPad(799), iPhone(999), MacBook(1999)
            }
            _ => panic!("expected 3 rows"),
        }
    }

    #[test]
    fn test_index_range_lt() {
        let (executor, _dir) = setup();

        let ast = QueryParser::parse("CREATE INDEX ON Product (price)").unwrap();
        executor.execute(&ast).unwrap();

        insert_row(&executor, "Product", "iPhone", 999);
        insert_row(&executor, "Product", "iPad", 799);
        insert_row(&executor, "Product", "AirPods", 249);
        executor.engine().flush().unwrap();

        let ast = QueryParser::parse("SELECT name FROM Product WHERE price < 500").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 1); // AirPods(249)
                assert_eq!(rows[0].get("name").unwrap().as_str().unwrap(), "AirPods");
            }
            _ => panic!("expected 1 row"),
        }
    }

    #[test]
    fn test_index_range_between() {
        let (executor, _dir) = setup();

        let ast = QueryParser::parse("CREATE INDEX ON Product (price)").unwrap();
        executor.execute(&ast).unwrap();

        insert_row(&executor, "Product", "iPhone", 999);
        insert_row(&executor, "Product", "iPad", 799);
        insert_row(&executor, "Product", "MacBook", 1999);
        insert_row(&executor, "Product", "AirPods", 249);
        executor.engine().flush().unwrap();

        let ast = QueryParser::parse("SELECT name, price FROM Product WHERE price BETWEEN 700 AND 1000").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 2); // iPad(799), iPhone(999)
            }
            _ => panic!("expected 2 rows"),
        }
    }

    #[test]
    fn test_index_range_gte_lte() {
        let (executor, _dir) = setup();

        let ast = QueryParser::parse("CREATE INDEX ON Product (price)").unwrap();
        executor.execute(&ast).unwrap();

        insert_row(&executor, "Product", "iPhone", 999);
        insert_row(&executor, "Product", "iPad", 799);
        insert_row(&executor, "Product", "MacBook", 1999);
        executor.engine().flush().unwrap();

        // >=
        let ast = QueryParser::parse("SELECT name FROM Product WHERE price >= 999").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 2); // iPhone(999), MacBook(1999)
            }
            _ => panic!("expected 2 rows for gte"),
        }

        // <=
        let ast = QueryParser::parse("SELECT name FROM Product WHERE price <= 999").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 2); // iPad(799), iPhone(999)
            }
            _ => panic!("expected 2 rows for lte"),
        }
    }

    #[test]
    fn test_index_in_clause() {
        let (executor, _dir) = setup();

        let ast = QueryParser::parse("CREATE INDEX ON Product (price)").unwrap();
        executor.execute(&ast).unwrap();

        insert_row(&executor, "Product", "iPhone", 999);
        insert_row(&executor, "Product", "iPad", 799);
        insert_row(&executor, "Product", "MacBook", 1999);
        insert_row(&executor, "Product", "AirPods", 249);
        executor.engine().flush().unwrap();

        let ast = QueryParser::parse("SELECT name FROM Product WHERE price IN (249, 1999)").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 2); // AirPods, MacBook
            }
            _ => panic!("expected 2 rows for IN"),
        }
    }

    // 鈹€鈹€ Index consistency on UPDATE/DELETE 鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€

    #[test]
    fn test_index_update_consistency() {
        let (executor, _dir) = setup();

        let ast = QueryParser::parse("CREATE INDEX ON Product (price)").unwrap();
        executor.execute(&ast).unwrap();

        insert_row(&executor, "Product", "iPhone", 999);
        executor.engine().flush().unwrap();

        // Verify index works for original price
        let ast = QueryParser::parse("SELECT name FROM Product WHERE price = 999").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => assert_eq!(rows.len(), 1),
            _ => panic!("expected 1 row at price 999"),
        }

        // Update the price
        let ast = QueryParser::parse("UPDATE Product SET price = 1099 WHERE name = 'iPhone'").unwrap();
        executor.execute(&ast).unwrap();

        // Old price should no longer be found via index
        let ast = QueryParser::parse("SELECT name FROM Product WHERE price = 999").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => assert_eq!(rows.len(), 0, "old price 999 should not be in index"),
            _ => panic!("expected 0 rows"),
        }

        // New price should be found via index
        let ast = QueryParser::parse("SELECT name FROM Product WHERE price = 1099").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 1, "new price 1099 should be in index");
                assert_eq!(rows[0].get("name").unwrap().as_str().unwrap(), "iPhone");
            }
            _ => panic!("expected 1 row at price 1099"),
        }
    }

    #[test]
    fn test_index_delete_consistency() {
        let (executor, _dir) = setup();

        let ast = QueryParser::parse("CREATE INDEX ON Product (price)").unwrap();
        executor.execute(&ast).unwrap();

        insert_row(&executor, "Product", "iPhone", 999);
        insert_row(&executor, "Product", "iPad", 799);
        executor.engine().flush().unwrap();

        // Delete iPhone
        let ast = QueryParser::parse("DELETE FROM Product WHERE name = 'iPhone'").unwrap();
        executor.execute(&ast).unwrap();

        // Price 999 should no longer be in index
        let ast = QueryParser::parse("SELECT name FROM Product WHERE price = 999").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => assert_eq!(rows.len(), 0, "deleted row should not be in index"),
            _ => panic!("expected 0 rows"),
        }

        // iPad should still be indexed
        let ast = QueryParser::parse("SELECT name FROM Product WHERE price = 799").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => assert_eq!(rows.len(), 1),
            _ => panic!("expected 1 row"),
        }
    }

    #[test]
    fn test_index_range_after_update() {
        let (executor, _dir) = setup();

        let ast = QueryParser::parse("CREATE INDEX ON Product (price)").unwrap();
        executor.execute(&ast).unwrap();

        insert_row(&executor, "Product", "iPhone", 999);
        insert_row(&executor, "Product", "iPad", 799);
        executor.engine().flush().unwrap();

        // Move iPhone from 999 to 599
        let ast = QueryParser::parse("UPDATE Product SET price = 599 WHERE name = 'iPhone'").unwrap();
        executor.execute(&ast).unwrap();

        // Range scan: price < 700 should now find iPhone(599) but not iPad(799)
        let ast = QueryParser::parse("SELECT name, price FROM Product WHERE price < 700").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].get("name").unwrap().as_str().unwrap(), "iPhone");
                assert_eq!(rows[0].get("price").unwrap().as_i64().unwrap(), 599);
            }
            _ => panic!("expected 1 row after update"),
        }

        // Range scan: price > 600 should find iPad(799) but not iPhone(599)
        let ast = QueryParser::parse("SELECT name FROM Product WHERE price > 600").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].get("name").unwrap().as_str().unwrap(), "iPad");
            }
            _ => panic!("expected 1 row in range > 600"),
        }
    }

    // 鈹€鈹€ Vector search tests 鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€

    #[test]
    fn test_vector_search_basic() {
        let (executor, _dir) = setup();

        // Create vector index
        let ast = QueryParser::parse(
            "CREATE VECTOR INDEX ON Product (embedding) METRIC cosine DIMENSION 3"
        ).unwrap();
        executor.execute(&ast).unwrap();

        // Insert documents with vectors
        let ast = QueryAst::Insert {
            class: "Product".to_string(),
            columns: vec!["name".to_string(), "embedding".to_string()],
            values: vec![
                LiteralValue::String("cat".to_string()),
                LiteralValue::String("[0.9, 0.1, 0.0]".to_string()),
            ],
        };
        executor.execute(&ast).unwrap();

        let ast = QueryAst::Insert {
            class: "Product".to_string(),
            columns: vec!["name".to_string(), "embedding".to_string()],
            values: vec![
                LiteralValue::String("dog".to_string()),
                LiteralValue::String("[0.8, 0.2, 0.0]".to_string()),
            ],
        };
        executor.execute(&ast).unwrap();

        let ast = QueryAst::Insert {
            class: "Product".to_string(),
            columns: vec!["name".to_string(), "embedding".to_string()],
            values: vec![
                LiteralValue::String("car".to_string()),
                LiteralValue::String("[0.0, 0.1, 0.9]".to_string()),
            ],
        };
        executor.execute(&ast).unwrap();

        executor.engine().flush().unwrap();

        // Vector search: find 2 nearest to [1.0, 0.0, 0.0]
        let ast = QueryParser::parse(
            "VECTOR SEARCH ON Product (embedding) QUERY [1.0, 0.0, 0.0] TOP 2"
        ).unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 2);
                // First result should be "cat" (closest to [1,0,0])
                assert_eq!(rows[0].get("name").unwrap().as_str().unwrap(), "cat");
                // Should have _distance column
                assert!(rows[0].get("_distance").is_some());
            }
            _ => panic!("expected 2 rows from vector search"),
        }
    }

    #[test]
    fn test_vector_search_with_filter() {
        let (executor, _dir) = setup();

        let ast = QueryParser::parse(
            "CREATE VECTOR INDEX ON Product (embedding) METRIC l2 DIMENSION 3"
        ).unwrap();
        executor.execute(&ast).unwrap();

        // Insert products with category
        for (name, cat, vec_str) in [
            ("cat", "animal", "[0.9, 0.1, 0.0]"),
            ("dog", "animal", "[0.8, 0.2, 0.0]"),
            ("car", "vehicle", "[0.0, 0.1, 0.9]"),
            ("truck", "vehicle", "[0.1, 0.0, 0.8]"),
        ] {
            let ast = QueryAst::Insert {
                class: "Product".to_string(),
                columns: vec![
                    "name".to_string(),
                    "category".to_string(),
                    "embedding".to_string(),
                ],
                values: vec![
                    LiteralValue::String(name.to_string()),
                    LiteralValue::String(cat.to_string()),
                    LiteralValue::String(vec_str.to_string()),
                ],
            };
            executor.execute(&ast).unwrap();
        }
        executor.engine().flush().unwrap();

        // Vector search with filter: only "animal" category
        let ast = QueryParser::parse(
            "VECTOR SEARCH ON Product (embedding) QUERY [1.0, 0.0, 0.0] TOP 3 WHERE category = 'animal'"
        ).unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 2); // only cat and dog
                assert_eq!(rows[0].get("name").unwrap().as_str().unwrap(), "cat");
                assert_eq!(rows[1].get("name").unwrap().as_str().unwrap(), "dog");
            }
            _ => panic!("expected 2 rows from filtered vector search"),
        }
    }

    #[test]
    fn test_vector_index_create_drop() {
        let (executor, _dir) = setup();

        // Create
        let ast = QueryParser::parse(
            "CREATE VECTOR INDEX ON Product (embedding) METRIC cosine DIMENSION 128 M 32 EF_CONSTRUCTION 400 EF_SEARCH 200"
        ).unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Success(msg) => assert!(msg.contains("Vector index created")),
            _ => panic!("expected Success"),
        }

        // Verify it exists
        assert!(executor.engine().has_vector_index("Product", "embedding"));

        // Drop
        let ast = QueryParser::parse(
            "DROP VECTOR INDEX ON Product (embedding)"
        ).unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Success(msg) => assert!(msg.contains("Vector index dropped")),
            _ => panic!("expected Success"),
        }

        // Verify it's gone
        assert!(!executor.engine().has_vector_index("Product", "embedding"));
    }

    #[test]
    fn test_vector_index_persistence_across_restart() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path().to_path_buf();

        // Create vector index and insert data
        {
            let options = StorageOptions {
                data_dir: data_dir.clone(),
                memtable_size_limit: 1024 * 1024,
                ..Default::default()
            };
            let engine = Arc::new(LsmEngine::open(options).unwrap());
            let ontology_store = OntologyStore::new(engine.clone());
            let executor = QueryExecutor::new(engine.clone(), ontology_store);

            let ast = QueryParser::parse(
                "CREATE VECTOR INDEX ON Product (embedding) METRIC cosine DIMENSION 3"
            ).unwrap();
            executor.execute(&ast).unwrap();

            let ast = QueryAst::Insert {
                class: "Product".to_string(),
                columns: vec!["name".to_string(), "embedding".to_string()],
                values: vec![
                    LiteralValue::String("item1".to_string()),
                    LiteralValue::String("[1.0, 0.0, 0.0]".to_string()),
                ],
            };
            executor.execute(&ast).unwrap();
            engine.flush().unwrap();
        }

        // Reopen and verify vector index is rebuilt
        {
            let options = StorageOptions {
                data_dir,
                memtable_size_limit: 1024 * 1024,
                ..Default::default()
            };
            let engine = LsmEngine::open(options).unwrap();

            // Vector index should be rebuilt from persisted metadata
            assert!(engine.has_vector_index("Product", "embedding"));

            // Search should work with rebuilt index
            let results = engine.vector_index_manager().read().unwrap_or_else(|e| e.into_inner()).search(
                "Product", "embedding", &[1.0, 0.0, 0.0], 1,
            ).unwrap();
            assert_eq!(results.len(), 1);
        }
    }

    // 鈹€鈹€ Phase 21: CASE WHEN tests 鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€

    #[test]
    fn test_case_when_basic() {
        let (executor, _dir) = setup();
        insert_row(&executor, "Product", "iPhone", 999);
        insert_row(&executor, "Product", "iPad", 799);
        insert_row(&executor, "Product", "MacBook", 1999);
        executor.engine().flush().unwrap();

        let ast = QueryParser::parse(
            "SELECT name, CASE WHEN price > 1000 THEN 'expensive' ELSE 'affordable' END FROM Product"
        ).unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 3);
                // MacBook should be 'expensive'
                let macbook = rows.iter().find(|r| {
                    r.get("name").and_then(|v| v.as_str()) == Some("MacBook")
                }).unwrap();
                assert_eq!(macbook.get("case").unwrap().as_str().unwrap(), "expensive");
            }
            _ => panic!("expected Rows"),
        }
    }

    // 鈹€鈹€ Phase 21: CTE tests 鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€

    #[test]
    fn test_cte_basic() {
        let (executor, _dir) = setup();
        insert_row(&executor, "Product", "iPhone", 999);
        insert_row(&executor, "Product", "iPad", 799);
        insert_row(&executor, "Product", "MacBook", 1999);
        executor.engine().flush().unwrap();

        let ast = QueryParser::parse(
            "WITH expensive AS (SELECT * FROM Product WHERE price > 900) SELECT * FROM expensive"
        ).unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 2); // iPhone and MacBook
            }
            _ => panic!("expected Rows"),
        }
    }

    // 鈹€鈹€ Phase 21: Window Function tests 鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€

    #[test]
    fn test_window_row_number() {
        let (executor, _dir) = setup();
        insert_row(&executor, "Product", "iPhone", 999);
        insert_row(&executor, "Product", "iPad", 799);
        insert_row(&executor, "Product", "MacBook", 1999);
        executor.engine().flush().unwrap();

        let ast = QueryParser::parse(
            "SELECT name, ROW_NUMBER() OVER (ORDER BY price DESC) FROM Product"
        ).unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 3);
                // All rows should have the window function column
                for row in rows {
                    assert!(row.contains_key("rownumber()"), "missing rownumber() key, got: {:?}", row.keys().collect::<Vec<_>>());
                }
            }
            _ => panic!("expected Rows"),
        }
    }

    #[test]
    fn test_window_rank() {
        let (executor, _dir) = setup();
        insert_row(&executor, "Product", "iPhone", 999);
        insert_row(&executor, "Product", "iPad", 799);
        insert_row(&executor, "Product", "MacBook", 1999);
        executor.engine().flush().unwrap();

        let ast = QueryParser::parse(
            "SELECT name, RANK() OVER (ORDER BY price DESC) FROM Product"
        ).unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 3);
                for row in rows {
                    assert!(row.contains_key("rank()"));
                }
            }
            _ => panic!("expected Rows"),
        }
    }

    #[test]
    fn test_window_running_aggregates() {
        let (executor, _dir) = setup();
        insert_row(&executor, "Product", "iPhone", 999);
        insert_row(&executor, "Product", "iPad", 799);
        insert_row(&executor, "Product", "MacBook", 1999);
        executor.engine().flush().unwrap();

        let ast = QueryParser::parse(
            "SELECT name, SUM(price) OVER (ORDER BY price), AVG(price) OVER (ORDER BY price) FROM Product"
        ).unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 3);
                for row in rows {
                    // Check that at least some aggregate keys exist
                    let keys: Vec<&str> = row.keys().map(|s| s.as_str()).collect();
                    assert!(keys.len() >= 2, "expected at least 2 columns, got: {:?}", keys);
                }
            }
            _ => panic!("expected Rows"),
        }
    }

    // 鈹€鈹€ Phase 21: EXPLAIN ANALYZE tests 鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€

    #[test]
    fn test_explain_analyze() {
        let (executor, _dir) = setup();
        insert_row(&executor, "Product", "iPhone", 999);
        executor.engine().flush().unwrap();

        let ast = QueryParser::parse("EXPLAIN SELECT * FROM Product").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 1);
                let plan = rows[0].get("plan").unwrap();
                assert!(plan.get("cost").is_some());
                assert!(plan.get("cost").unwrap().get("actual_time_ms").is_some());
            }
            _ => panic!("expected Rows"),
        }
    }

    // 鈹€鈹€ Phase 21: Materialized View tests 鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€

    #[test]
    fn test_materialized_view_create_and_query() {
        let (executor, _dir) = setup();
        insert_row(&executor, "Product", "iPhone", 999);
        insert_row(&executor, "Product", "iPad", 799);
        insert_row(&executor, "Product", "MacBook", 1999);
        executor.engine().flush().unwrap();

        // Create materialized view
        let ast = QueryParser::parse(
            "CREATE MATERIALIZED VIEW expensive_products AS SELECT * FROM Product WHERE price > 900"
        ).unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Success(msg) => assert!(msg.contains("2 rows")),
            _ => panic!("expected Success"),
        }

        // Query the materialized view
        let ast = QueryParser::parse("SELECT * FROM expensive_products").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 2);
            }
            _ => panic!("expected Rows from materialized view"),
        }
    }

    #[test]
    fn test_materialized_view_drop() {
        let (executor, _dir) = setup();
        insert_row(&executor, "Product", "iPhone", 999);
        executor.engine().flush().unwrap();

        // Create
        let ast = QueryParser::parse(
            "CREATE MATERIALIZED VIEW mv_test AS SELECT * FROM Product"
        ).unwrap();
        executor.execute(&ast).unwrap();

        // Drop
        let ast = QueryParser::parse("DROP MATERIALIZED VIEW mv_test").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Success(msg) => assert!(msg.contains("dropped")),
            _ => panic!("expected Success"),
        }

        // Query should return empty after drop
        let ast = QueryParser::parse("SELECT * FROM mv_test").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => {
                assert!(rows.is_empty(), "expected 0 rows after DROP MATERIALIZED VIEW, got {}", rows.len());
            }
            _ => panic!("expected Rows"),
        }
    }

    #[test]
    fn test_materialized_view_incremental_refresh() {
        let (executor, _dir) = setup();
        insert_row(&executor, "Product", "iPhone", 999);
        insert_row(&executor, "Product", "iPad", 799);
        executor.engine().flush().unwrap();

        // Create materialized view
        let ast = QueryParser::parse(
            "CREATE MATERIALIZED VIEW expensive_mv AS SELECT * FROM Product WHERE price > 900"
        ).unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Success(msg) => {
                println!("Create result: {}", msg);
                assert!(msg.contains("1 rows"));
            }
            _ => panic!("expected Success with 1 row"),
        }

        // Add a new product
        insert_row(&executor, "Product", "MacBook", 1999);
        executor.engine().flush().unwrap();

        // Refresh the materialized view
        let ast = QueryParser::parse("REFRESH MATERIALIZED VIEW expensive_mv").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Success(msg) => {
                println!("Refresh result: {}", msg);
                assert!(msg.contains("refreshed"));
                // Should show 1 added (MacBook), 0 removed, 1 unchanged (iPhone)
                assert!(msg.contains("1 added"));
                assert!(msg.contains("0 removed"));
                assert!(msg.contains("1 unchanged"));
            }
            _ => panic!("expected Success, got: {:?}", result),
        }

        // Query the refreshed materialized view
        let ast = QueryParser::parse("SELECT * FROM expensive_mv").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 2); // iPhone and MacBook
            }
            _ => panic!("expected Rows"),
        }
    }

    // 鈹€鈹€ Phase 22: ANALYZE tests 鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€

    #[test]
    fn test_analyze_command() {
        let (executor, _dir) = setup();
        insert_row(&executor, "Product", "iPhone", 999);
        insert_row(&executor, "Product", "iPad", 799);
        insert_row(&executor, "Product", "MacBook", 1999);
        executor.engine().flush().unwrap();

        let ast = QueryParser::parse("ANALYZE Product").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => {
                // Should have at least 1 row (table summary)
                assert!(rows.len() >= 1);
                // First row should be table stats
                assert_eq!(rows[0].get("table").unwrap().as_str().unwrap(), "Product");
                assert_eq!(rows[0].get("row_count").unwrap().as_i64().unwrap(), 3);
            }
            _ => panic!("expected Rows from ANALYZE"),
        }
    }

    // 鈹€鈹€ Phase 22: Plan Cache tests 鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€

    #[test]
    fn test_plan_cache_integration() {
        let (executor, _dir) = setup();
        insert_row(&executor, "Product", "iPhone", 999);
        executor.engine().flush().unwrap();

        // First query - plan cache miss
        let ast = QueryParser::parse("SELECT * FROM Product WHERE price > 500").unwrap();
        let _ = executor.execute(&ast).unwrap();

        // Second query - plan cache hit (same AST)
        let _ = executor.execute(&ast).unwrap();

        let stats = executor.runtime_stats();
        assert_eq!(stats.plan_cache_misses, 1, "should have 1 plan cache miss");
        assert_eq!(stats.plan_cache_hits, 1, "should have 1 plan cache hit");
    }

    // 鈹€鈹€ Phase 22: Composite Index tests 鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€

    #[test]
    fn test_composite_index_create() {
        let (executor, _dir) = setup();
        insert_row(&executor, "Product", "iPhone", 999);
        executor.engine().flush().unwrap();

        // Create composite index
        let ast = QueryParser::parse("CREATE INDEX ON Product (name, price)").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Success(msg) => {
                assert!(msg.contains("Composite index"));
                assert!(msg.contains("name"));
                assert!(msg.contains("price"));
            }
            _ => panic!("expected Success"),
        }
    }

    // 鈹€鈹€ Phase 22: Index Condition Pushdown tests 鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€

    #[test]
    fn test_index_condition_pushdown_and() {
        let (executor, _dir) = setup();

        // Create index on price
        let ast = QueryParser::parse("CREATE INDEX ON Product (price)").unwrap();
        executor.execute(&ast).unwrap();

        insert_row(&executor, "Product", "iPhone", 999);
        insert_row(&executor, "Product", "iPad", 799);
        insert_row(&executor, "Product", "MacBook", 1999);
        executor.engine().flush().unwrap();

        // AND condition: price > 800 AND price < 1500
        // Should use index for price > 800, then filter price < 1500
        let ast = QueryParser::parse(
            "SELECT name, price FROM Product WHERE price > 800 AND price < 1500"
        ).unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 1); // Only iPhone (999)
                assert_eq!(rows[0].get("name").unwrap().as_str().unwrap(), "iPhone");
            }
            _ => panic!("expected Rows"),
        }
    }

    // 鈹€鈹€ Phase 22: Runtime stats tests 鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€

    #[test]
    fn test_runtime_stats_tracking() {
        let (executor, _dir) = setup();
        insert_row(&executor, "Product", "iPhone", 999);
        executor.engine().flush().unwrap();

        let before = executor.runtime_stats().total_queries;

        // Execute a few queries
        for _ in 0..3 {
            let ast = QueryParser::parse("SELECT * FROM Product").unwrap();
            let _ = executor.execute(&ast).unwrap();
        }

        let stats = executor.runtime_stats();
        assert_eq!(stats.total_queries - before, 3, "should have 3 more queries");
        assert!(stats.total_time_us > 0);
        assert!(stats.table_scan_counts.contains_key("Product"));
    }

    // 鈹€鈹€ Phase 23: LIMIT OFFSET tests 鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€

    #[test]
    fn test_limit_offset() {
        let (executor, _dir) = setup();
        for i in 0..5 {
            insert_row(&executor, "Product", &format!("item{}", i), i * 100);
        }
        executor.engine().flush().unwrap();

        // LIMIT 2 OFFSET 2 should skip first 2, return next 2
        let ast = QueryParser::parse("SELECT name FROM Product ORDER BY price LIMIT 2 OFFSET 2").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 2);
            }
            _ => panic!("expected Rows"),
        }
    }

    #[test]
    fn test_offset_beyond_data() {
        let (executor, _dir) = setup();
        insert_row(&executor, "Product", "iPhone", 999);
        executor.engine().flush().unwrap();

        let ast = QueryParser::parse("SELECT * FROM Product LIMIT 10 OFFSET 100").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 0);
            }
            _ => panic!("expected empty Rows"),
        }
    }

    // 鈹€鈹€ Phase 23: Batch INSERT tests 鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€

    #[test]
    fn test_batch_insert() {
        let (executor, _dir) = setup();

        let ast = QueryParser::parse(
            "INSERT INTO Product (name, price) VALUES ('iPhone', 999), ('iPad', 799), ('MacBook', 1999)"
        ).unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Success(msg) => assert!(msg.contains("3 row(s) inserted")),
            _ => panic!("expected Success"),
        }

        // Verify all rows were inserted
        let ast = QueryParser::parse("SELECT * FROM Product").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => assert_eq!(rows.len(), 3),
            _ => panic!("expected 3 rows"),
        }
    }

    // 鈹€鈹€ Phase 23: Built-in function tests 鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€

    #[test]
    fn test_coalesce_function() {
        let (executor, _dir) = setup();
        insert_row(&executor, "Product", "iPhone", 999);
        executor.engine().flush().unwrap();

        let ast = QueryParser::parse(
            "SELECT COALESCE(name, 'unknown') FROM Product"
        ).unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].values().next().unwrap().as_str().unwrap(), "iPhone");
            }
            _ => panic!("expected Rows"),
        }
    }

    #[test]
    fn test_concat_function() {
        let (executor, _dir) = setup();
        insert_row(&executor, "Product", "iPhone", 999);
        executor.engine().flush().unwrap();

        let ast = QueryParser::parse(
            "SELECT CONCAT(name, ' - ', 'Premium') FROM Product"
        ).unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].values().next().unwrap().as_str().unwrap(), "iPhone - Premium");
            }
            _ => panic!("expected Rows"),
        }
    }

    #[test]
    fn test_upper_lower_functions() {
        let (executor, _dir) = setup();
        insert_row(&executor, "Product", "iPhone", 999);
        executor.engine().flush().unwrap();

        let ast = QueryParser::parse("SELECT UPPER(name) FROM Product").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows[0].values().next().unwrap().as_str().unwrap(), "IPHONE");
            }
            _ => panic!("expected Rows"),
        }
    }

    // 鈹€鈹€ Phase 23: Transaction command tests 鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€

    #[test]
    fn test_transaction_commands() {
        let (executor, _dir) = setup();

        // BEGIN starts a transaction
        let ast = QueryParser::parse("BEGIN").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Success(msg) => assert!(msg.contains("started")),
            _ => panic!("expected Success"),
        }
        assert!(executor.in_transaction());

        // ROLLBACK aborts the transaction
        let ast = QueryParser::parse("ROLLBACK").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Success(msg) => assert!(msg.contains("rolled back")),
            _ => panic!("expected Success"),
        }
        assert!(!executor.in_transaction());

        // BEGIN + COMMIT works
        let ast = QueryParser::parse("BEGIN").unwrap();
        executor.execute(&ast).unwrap();
        assert!(executor.in_transaction());

        let ast = QueryParser::parse("COMMIT").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Success(msg) => assert!(msg.contains("committed")),
            _ => panic!("expected Success"),
        }
        assert!(!executor.in_transaction());
    }

    // 鈹€鈹€ Phase 23: Recursive CTE parser test 鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€

    #[test]
    fn test_recursive_cte_parse() {
        let ast = QueryParser::parse(
            "WITH RECURSIVE cte AS (SELECT 1 UNION ALL SELECT n+1 FROM cte WHERE n < 10) SELECT * FROM cte"
        ).unwrap();
        match ast {
            QueryAst::With { recursive, .. } => assert!(recursive),
            _ => panic!("expected With"),
        }
    }

    // Multi-statement transaction tests (Phase 26)

    #[test]
    fn test_multi_stmt_txn_commit() {
        let (executor, _dir) = setup();

        // Create a class with an ontology
        executor.execute(&QueryParser::parse(
            "CREATE ONTOLOGY shop (CLASS Product, PROPERTY name DOMAIN Product RANGE STRING, PROPERTY price DOMAIN Product RANGE INT64)"
        ).unwrap()).unwrap();

        // BEGIN, INSERT, COMMIT - data should persist
        executor.execute(&QueryParser::parse("BEGIN").unwrap()).unwrap();
        assert!(executor.in_transaction());

        executor.execute(&QueryParser::parse(
            "INSERT INTO Product (name, price) VALUES ('Widget', 100)"
        ).unwrap()).unwrap();

        executor.execute(&QueryParser::parse("COMMIT").unwrap()).unwrap();
        assert!(!executor.in_transaction());

        // Data should be visible after commit
        let result = executor.execute(&QueryParser::parse(
            "SELECT name, price FROM Product"
        ).unwrap()).unwrap();
        match result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].get("name").unwrap(), &Value::String("Widget".to_string()));
            }
            _ => panic!("expected Rows"),
        }
    }

    #[test]
    fn test_multi_stmt_txn_rollback() {
        let (executor, _dir) = setup();

        // Create a class
        executor.execute(&QueryParser::parse(
            "CREATE ONTOLOGY shop (CLASS Product, PROPERTY name DOMAIN Product RANGE STRING, PROPERTY price DOMAIN Product RANGE INT64)"
        ).unwrap()).unwrap();

        // BEGIN, INSERT, ROLLBACK - data should be discarded
        executor.execute(&QueryParser::parse("BEGIN").unwrap()).unwrap();
        executor.execute(&QueryParser::parse(
            "INSERT INTO Product (name, price) VALUES ('Widget', 100)"
        ).unwrap()).unwrap();
        executor.execute(&QueryParser::parse("ROLLBACK").unwrap()).unwrap();
        assert!(!executor.in_transaction());

        // Data should NOT be visible after rollback
        let result = executor.execute(&QueryParser::parse(
            "SELECT name, price FROM Product"
        ).unwrap()).unwrap();
        match result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 0);
            }
            _ => panic!("expected Rows"),
        }
    }

    #[test]
    fn test_multi_stmt_txn_multiple_inserts() {
        let (executor, _dir) = setup();

        executor.execute(&QueryParser::parse(
            "CREATE ONTOLOGY shop (CLASS Product, PROPERTY name DOMAIN Product RANGE STRING, PROPERTY price DOMAIN Product RANGE INT64)"
        ).unwrap()).unwrap();

        // BEGIN, multiple INSERTs, COMMIT
        executor.execute(&QueryParser::parse("BEGIN").unwrap()).unwrap();
        executor.execute(&QueryParser::parse(
            "INSERT INTO Product (name, price) VALUES ('A', 10)"
        ).unwrap()).unwrap();
        executor.execute(&QueryParser::parse(
            "INSERT INTO Product (name, price) VALUES ('B', 20)"
        ).unwrap()).unwrap();
        executor.execute(&QueryParser::parse(
            "INSERT INTO Product (name, price) VALUES ('C', 30)"
        ).unwrap()).unwrap();
        executor.execute(&QueryParser::parse("COMMIT").unwrap()).unwrap();

        // All three rows should be visible
        let result = executor.execute(&QueryParser::parse(
            "SELECT name FROM Product ORDER BY name"
        ).unwrap()).unwrap();
        match result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 3);
            }
            _ => panic!("expected Rows"),
        }
    }

    #[test]
    fn test_multi_stmt_txn_update_rollback() {
        let (executor, _dir) = setup();

        executor.execute(&QueryParser::parse(
            "CREATE ONTOLOGY shop (CLASS Product, PROPERTY name DOMAIN Product RANGE STRING, PROPERTY price DOMAIN Product RANGE INT64)"
        ).unwrap()).unwrap();

        // Insert initial data (auto-commit)
        executor.execute(&QueryParser::parse(
            "INSERT INTO Product (name, price) VALUES ('Widget', 100)"
        ).unwrap()).unwrap();

        // BEGIN, UPDATE, ROLLBACK - update should be discarded
        executor.execute(&QueryParser::parse("BEGIN").unwrap()).unwrap();
        executor.execute(&QueryParser::parse(
            "UPDATE Product SET price = 999 WHERE name = 'Widget'"
        ).unwrap()).unwrap();
        executor.execute(&QueryParser::parse("ROLLBACK").unwrap()).unwrap();

        // Original price should be visible
        let result = executor.execute(&QueryParser::parse(
            "SELECT price FROM Product WHERE name = 'Widget'"
        ).unwrap()).unwrap();
        match result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].get("price").unwrap(), &Value::Number(100.into()));
            }
            _ => panic!("expected Rows"),
        }
    }

    #[test]
    fn test_begin_while_in_txn_errors() {
        let (executor, _dir) = setup();

        executor.execute(&QueryParser::parse("BEGIN").unwrap()).unwrap();
        let result = executor.execute(&QueryParser::parse("BEGIN").unwrap());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("transaction already active"));

        // Clean up
        executor.execute(&QueryParser::parse("ROLLBACK").unwrap()).unwrap();
    }

    #[test]
    fn test_commit_without_txn_errors() {
        let (executor, _dir) = setup();

        let result = executor.execute(&QueryParser::parse("COMMIT").unwrap());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("no active transaction"));
    }

    #[test]
    fn test_rollback_without_txn_errors() {
        let (executor, _dir) = setup();

        let result = executor.execute(&QueryParser::parse("ROLLBACK").unwrap());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("no active transaction"));
    }

    #[test]
    fn test_query_config_defaults() {
        let config = QueryConfig::default();
        assert_eq!(config.query_timeout, Duration::from_secs(30));
        assert_eq!(config.memory_budget, 256 * 1024 * 1024);
    }

    #[test]
    fn test_memory_estimation() {
        let rows = vec![
            {
                let mut row = Map::new();
                row.insert("name".to_string(), Value::String("test".to_string()));
                row.insert("price".to_string(), Value::Number(100.into()));
                row
            }
        ];
        let estimated = QueryExecutor::estimate_rows_memory(&rows);
        assert!(estimated > 0);
        assert!(estimated < 1000); // Should be small for one row
    }

    #[test]
    fn test_memory_budget_exceeded() {
        let (_executor, _dir) = setup();

        // Set a very small memory budget
        let config = QueryConfig {
            query_timeout: Duration::from_secs(30),
            memory_budget: 10, // 10 bytes - will be exceeded
        };
        let (small_exec, _dir2) = setup_with_config(config);

        // Create data
        small_exec.execute(&QueryParser::parse(
            "CREATE ONTOLOGY shop (CLASS Product, PROPERTY name DOMAIN Product RANGE STRING)"
        ).unwrap()).unwrap();

        // Insert should work (writes are buffered)
        small_exec.execute(&QueryParser::parse(
            "INSERT INTO Product (name) VALUES ('A long product name that exceeds budget')"
        ).unwrap()).unwrap();

        // SELECT should fail due to memory budget
        let result = small_exec.execute(&QueryParser::parse(
            "SELECT name FROM Product"
        ).unwrap());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("memory budget exceeded"));
    }

    // P26-3: IS NULL, NOT, LEFT JOIN tests

    #[test]
    fn test_is_null_filter() {
        let (executor, _dir) = setup();

        executor.execute(&QueryParser::parse(
            "CREATE ONTOLOGY shop (CLASS Product, PROPERTY name DOMAIN Product RANGE STRING, PROPERTY price DOMAIN Product RANGE INT64)"
        ).unwrap()).unwrap();

        // Insert with NULL price
        executor.execute(&QueryParser::parse(
            "INSERT INTO Product (name) VALUES ('NoPrice')"
        ).unwrap()).unwrap();
        // Insert with price
        executor.execute(&QueryParser::parse(
            "INSERT INTO Product (name, price) VALUES ('WithPrice', 100)"
        ).unwrap()).unwrap();

        // IS NULL should find the row without price
        let result = executor.execute(&QueryParser::parse(
            "SELECT name FROM Product WHERE price IS NULL"
        ).unwrap()).unwrap();
        match result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].get("name").unwrap(), &Value::String("NoPrice".to_string()));
            }
            _ => panic!("expected Rows"),
        }
    }

    #[test]
    fn test_is_not_null_filter() {
        let (executor, _dir) = setup();

        executor.execute(&QueryParser::parse(
            "CREATE ONTOLOGY shop (CLASS Product, PROPERTY name DOMAIN Product RANGE STRING, PROPERTY price DOMAIN Product RANGE INT64)"
        ).unwrap()).unwrap();

        executor.execute(&QueryParser::parse(
            "INSERT INTO Product (name) VALUES ('NoPrice')"
        ).unwrap()).unwrap();
        executor.execute(&QueryParser::parse(
            "INSERT INTO Product (name, price) VALUES ('WithPrice', 100)"
        ).unwrap()).unwrap();

        // IS NOT NULL should find the row with price
        let result = executor.execute(&QueryParser::parse(
            "SELECT name FROM Product WHERE price IS NOT NULL"
        ).unwrap()).unwrap();
        match result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].get("name").unwrap(), &Value::String("WithPrice".to_string()));
            }
            _ => panic!("expected Rows"),
        }
    }

    #[test]
    fn test_not_filter() {
        let (executor, _dir) = setup();

        executor.execute(&QueryParser::parse(
            "CREATE ONTOLOGY shop (CLASS Product, PROPERTY name DOMAIN Product RANGE STRING, PROPERTY price DOMAIN Product RANGE INT64)"
        ).unwrap()).unwrap();

        executor.execute(&QueryParser::parse(
            "INSERT INTO Product (name, price) VALUES ('A', 100)"
        ).unwrap()).unwrap();
        executor.execute(&QueryParser::parse(
            "INSERT INTO Product (name, price) VALUES ('B', 200)"
        ).unwrap()).unwrap();

        // NOT (price = 100) should find B
        let result = executor.execute(&QueryParser::parse(
            "SELECT name FROM Product WHERE NOT (price = 100)"
        ).unwrap()).unwrap();
        match result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].get("name").unwrap(), &Value::String("B".to_string()));
            }
            _ => panic!("expected Rows"),
        }
    }

    #[test]
    fn test_left_join_parse() {
        // Test that LEFT JOIN parses correctly
        let result = QueryParser::parse(
            "SELECT name, value FROM TableA LEFT JOIN TableB ON TableA.id = TableB.a_id"
        );
        // Just verify it parses without error
        match &result {
            Ok(_) => {},
            Err(e) => panic!("Parse error: {:?}", e),
        }
    }

    // ═══════════════════════════════════════════════════════════════════
    //  Import tests
    // ═══════════════════════════════════════════════════════════════════

    #[test]
    fn test_import_csv_basic() {
        let (executor, dir) = setup();
        let csv_path = dir.path().join("products.csv");

        executor.execute(&QueryParser::parse(
            "CREATE ONTOLOGY shop (CLASS Product, PROPERTY name DOMAIN Product RANGE STRING, PROPERTY price DOMAIN Product RANGE INT64)"
        ).unwrap()).unwrap();

        // Write CSV file
        std::fs::write(&csv_path, "name,price\nWidget,100\nGadget,200\nDoohickey,300\n").unwrap();

        let result = executor.execute(&QueryParser::parse(
            &format!("IMPORT INTO Product FROM CSV '{}'", csv_path.display())
        ).unwrap()).unwrap();

        match result {
            QueryResult::Success(msg) => {
                assert!(msg.contains("3 row(s) imported"), "unexpected msg: {}", msg);
            }
            _ => panic!("expected Success"),
        }

        // Verify data was inserted
        let select_result = executor.execute(&QueryParser::parse(
            "SELECT name, price FROM Product"
        ).unwrap()).unwrap();
        match select_result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 3);
            }
            _ => panic!("expected Rows"),
        }
    }

    #[test]
    fn test_import_csv_type_detection() {
        let (executor, dir) = setup();
        let csv_path = dir.path().join("typed.csv");

        executor.execute(&QueryParser::parse(
            "CREATE ONTOLOGY test (CLASS Item, PROPERTY name DOMAIN Item RANGE STRING, PROPERTY qty DOMAIN Item RANGE INT64, PROPERTY active DOMAIN Item RANGE BOOL)"
        ).unwrap()).unwrap();

        std::fs::write(&csv_path, "name,qty,active\nA,42,true\nB,0,false\n").unwrap();

        let result = executor.execute(&QueryParser::parse(
            &format!("IMPORT INTO Item FROM CSV '{}'", csv_path.display())
        ).unwrap()).unwrap();

        match result {
            QueryResult::Success(msg) => assert!(msg.contains("2 row(s) imported")),
            _ => panic!("expected Success"),
        }
    }

    #[test]
    fn test_import_json_array() {
        let (executor, dir) = setup();
        let json_path = dir.path().join("products.json");

        executor.execute(&QueryParser::parse(
            "CREATE ONTOLOGY shop (CLASS Product, PROPERTY name DOMAIN Product RANGE STRING, PROPERTY price DOMAIN Product RANGE INT64)"
        ).unwrap()).unwrap();

        std::fs::write(&json_path, r#"[
            {"name": "Widget", "price": 100},
            {"name": "Gadget", "price": 200}
        ]"#).unwrap();

        let result = executor.execute(&QueryParser::parse(
            &format!("IMPORT INTO Product FROM JSON '{}'", json_path.display())
        ).unwrap()).unwrap();

        match result {
            QueryResult::Success(msg) => {
                assert!(msg.contains("2 row(s) imported"), "unexpected msg: {}", msg);
            }
            _ => panic!("expected Success"),
        }
    }

    #[test]
    fn test_import_json_lines() {
        let (executor, dir) = setup();
        let json_path = dir.path().join("products.jsonl");

        executor.execute(&QueryParser::parse(
            "CREATE ONTOLOGY shop (CLASS Product, PROPERTY name DOMAIN Product RANGE STRING, PROPERTY price DOMAIN Product RANGE INT64)"
        ).unwrap()).unwrap();

        std::fs::write(&json_path, "{\"name\": \"Widget\", \"price\": 100}\n{\"name\": \"Gadget\", \"price\": 200}\n").unwrap();

        let result = executor.execute(&QueryParser::parse(
            &format!("IMPORT INTO Product FROM JSON '{}'", json_path.display())
        ).unwrap()).unwrap();

        match result {
            QueryResult::Success(msg) => {
                assert!(msg.contains("2 row(s) imported"), "unexpected msg: {}", msg);
            }
            _ => panic!("expected Success"),
        }
    }

    #[test]
    fn test_import_csv_file_not_found() {
        let (executor, _dir) = setup();

        executor.execute(&QueryParser::parse(
            "CREATE ONTOLOGY shop (CLASS Product, PROPERTY name DOMAIN Product RANGE STRING)"
        ).unwrap()).unwrap();

        let result = executor.execute(&QueryParser::parse(
            "IMPORT INTO Product FROM CSV '/nonexistent/file.csv'"
        ).unwrap());

        assert!(result.is_err(), "should fail for nonexistent file");
    }

    #[test]
    fn test_import_csv_empty_file() {
        let (executor, dir) = setup();
        let csv_path = dir.path().join("empty.csv");

        executor.execute(&QueryParser::parse(
            "CREATE ONTOLOGY shop (CLASS Product, PROPERTY name DOMAIN Product RANGE STRING)"
        ).unwrap()).unwrap();

        std::fs::write(&csv_path, "name\n").unwrap();

        let result = executor.execute(&QueryParser::parse(
            &format!("IMPORT INTO Product FROM CSV '{}'", csv_path.display())
        ).unwrap()).unwrap();

        match result {
            QueryResult::Success(msg) => {
                assert!(msg.contains("0 row(s) imported"), "unexpected msg: {}", msg);
            }
            _ => panic!("expected Success"),
        }
    }

    #[test]
    fn test_import_json_array_with_nulls() {
        let (executor, dir) = setup();
        let json_path = dir.path().join("nullable.json");

        executor.execute(&QueryParser::parse(
            "CREATE ONTOLOGY test (CLASS Item, PROPERTY name DOMAIN Item RANGE STRING, PROPERTY value DOMAIN Item RANGE INT64)"
        ).unwrap()).unwrap();

        std::fs::write(&json_path, r#"[
            {"name": "A", "value": 42},
            {"name": "B", "value": null},
            {"name": "C"}
        ]"#).unwrap();

        let result = executor.execute(&QueryParser::parse(
            &format!("IMPORT INTO Item FROM JSON '{}'", json_path.display())
        ).unwrap()).unwrap();

        match result {
            QueryResult::Success(msg) => {
                assert!(msg.contains("3 row(s) imported"), "unexpected msg: {}", msg);
            }
            _ => panic!("expected Success"),
        }
    }

    #[test]
    fn test_import_csv_with_special_values() {
        let (executor, dir) = setup();
        let csv_path = dir.path().join("special.csv");

        executor.execute(&QueryParser::parse(
            "CREATE ONTOLOGY test (CLASS Item, PROPERTY name DOMAIN Item RANGE STRING, PROPERTY score DOMAIN Item RANGE FLOAT64)"
        ).unwrap()).unwrap();

        std::fs::write(&csv_path, "name,score\n\"quoted name\",3.14\nnormal,NaN\n").unwrap();

        let result = executor.execute(&QueryParser::parse(
            &format!("IMPORT INTO Item FROM CSV '{}'", csv_path.display())
        ).unwrap()).unwrap();

        match result {
            QueryResult::Success(msg) => {
                // "NaN" won't parse as f64 in Rust's parse (it does!), but let's check
                assert!(msg.contains("imported"), "unexpected msg: {}", msg);
            }
            _ => panic!("expected Success"),
        }
    }

    #[test]
    fn test_parse_csv_value_types() {
        // Test null/empty detection
        match QueryExecutor::parse_csv_value("null") {
            LiteralValue::Null => {}
            other => panic!("expected Null, got {:?}", other),
        }
        match QueryExecutor::parse_csv_value("NULL") {
            LiteralValue::Null => {}
            other => panic!("expected Null, got {:?}", other),
        }
        match QueryExecutor::parse_csv_value("") {
            LiteralValue::Null => {}
            other => panic!("expected Null, got {:?}", other),
        }
        match QueryExecutor::parse_csv_value("  ") {
            LiteralValue::Null => {}
            other => panic!("expected Null, got {:?}", other),
        }
        // Test boolean detection
        match QueryExecutor::parse_csv_value("true") {
            LiteralValue::Bool(true) => {}
            other => panic!("expected Bool(true), got {:?}", other),
        }
        match QueryExecutor::parse_csv_value("false") {
            LiteralValue::Bool(false) => {}
            other => panic!("expected Bool(false), got {:?}", other),
        }
        // Test integer detection
        match QueryExecutor::parse_csv_value("42") {
            LiteralValue::Int(42) => {}
            other => panic!("expected Int(42), got {:?}", other),
        }
        match QueryExecutor::parse_csv_value("-7") {
            LiteralValue::Int(-7) => {}
            other => panic!("expected Int(-7), got {:?}", other),
        }
        // Test float detection
        match QueryExecutor::parse_csv_value("3.14") {
            LiteralValue::Float(f) => assert!((f - 3.14).abs() < 1e-10),
            other => panic!("expected Float, got {:?}", other),
        }
        // Test string fallback
        match QueryExecutor::parse_csv_value("hello") {
            LiteralValue::String(ref s) if s == "hello" => {}
            other => panic!("expected String(\"hello\"), got {:?}", other),
        }
    }
}
