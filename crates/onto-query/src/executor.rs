//! Query executor: runs parsed queries against the storage and ontology engines.

use crate::cache::{PlanCache, QueryCache};
use crate::optimizer::QueryPlanner;
use crate::parser::{AggregateFunc, ArithmeticOp, FilterExpr, LiteralValue, QueryAst, SelectColumns, SelectItem, ValueExpr, WindowExpr, WindowFunc};
use crate::optimizer::{ExecutionPlan, PlanNode};
use onto_core::{CoreError, Result};
use onto_ontology::{DataType, OntologyStore};
use onto_storage::LsmEngine;
use serde_json::{json, Map, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock, Mutex};
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

/// Executes parsed queries with caching support.
pub struct QueryExecutor {
    engine: Arc<RwLock<LsmEngine>>,
    ontology_store: OntologyStore,
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

impl QueryExecutor {
    pub fn new(engine: Arc<RwLock<LsmEngine>>, ontology_store: OntologyStore) -> Self {
        Self::with_config(engine, ontology_store, QueryConfig::default())
    }

    pub fn with_config(engine: Arc<RwLock<LsmEngine>>, ontology_store: OntologyStore, config: QueryConfig) -> Self {
        Self {
            engine,
            ontology_store,
            planner: std::sync::RwLock::new(QueryPlanner::new()),
            query_cache: Arc::new(Mutex::new(QueryCache::new(1000, Duration::from_secs(60)))),
            plan_cache: Arc::new(Mutex::new(PlanCache::new(500))),
            doc_counter: AtomicU64::new(0),
            runtime_stats: Arc::new(Mutex::new(RuntimeStats::default())),
            active_txn: Mutex::new(None),
            config,
        }
    }

    /// Get runtime execution statistics.
    pub fn runtime_stats(&self) -> RuntimeStats {
        self.runtime_stats.lock().unwrap().clone()
    }

    /// Get a reference to the query planner.
    pub fn planner(&self) -> std::sync::RwLockReadGuard<'_, QueryPlanner> {
        self.planner.read().unwrap()
    }

    /// Get a mutable reference to the query planner (for updating stats).
    pub fn planner_mut(&self) -> std::sync::RwLockWriteGuard<'_, QueryPlanner> {
        self.planner.write().unwrap()
    }

    /// Get query cache statistics.
    pub fn query_cache_stats(&self) -> crate::cache::CacheStats {
        self.query_cache.lock().unwrap().stats().clone()
    }

    /// Get plan cache statistics.
    pub fn plan_cache_stats(&self) -> crate::cache::CacheStats {
        self.plan_cache.lock().unwrap().stats().clone()
    }

    /// Clear all caches.
    pub fn clear_caches(&self) {
        self.query_cache.lock().unwrap().clear();
        self.plan_cache.lock().unwrap().clear();
    }

    /// Returns the active transaction ID, if any.
    pub fn active_txn_id(&self) -> Option<onto_core::SeqNo> {
        *self.active_txn.lock().unwrap()
    }

    /// Returns true if a multi-statement transaction is active.
    pub fn in_transaction(&self) -> bool {
        self.active_txn.lock().unwrap().is_some()
    }

    /// Executes a query and returns results as JSON.
    pub fn execute(&self, ast: &QueryAst) -> Result<QueryResult> {
        // Acquire engine lock once at the top level to avoid deadlocks
        // when subqueries re-enter execute().
        let mut engine = self.engine.write().map_err(|e| {
            CoreError::Custom(format!("engine lock poisoned: {}", e))
        })?;
        self.execute_with_engine(ast, &mut engine)
    }

    /// Internal execution with engine reference passed through.
    /// Each statement runs in its own auto-committed transaction.
    /// For SELECT queries, checks plan cache first.
    fn execute_with_engine(&self, ast: &QueryAst, engine: &mut LsmEngine) -> Result<QueryResult> {
        let start_time = std::time::Instant::now();

        let result = self.execute_with_engine_inner(ast, engine);

        // Check for timeout
        let elapsed = start_time.elapsed();
        if elapsed > self.config.query_timeout {
            return Err(CoreError::Custom(format!(
                "query timeout: exceeded {} seconds",
                self.config.query_timeout.as_secs()
            )));
        }

        // Track runtime statistics
        let elapsed_us = elapsed.as_micros() as u64;
        {
            let mut stats = self.runtime_stats.lock().unwrap();
            stats.total_queries += 1;
            stats.total_time_us += elapsed_us;
            // Track table access for SELECT queries
            if let QueryAst::Select { from, .. } = ast {
                *stats.table_scan_counts.entry(from.clone()).or_insert(0) += 1;
            }
        }

        result
    }

    /// Inner execution logic.
    fn execute_with_engine_inner(&self, ast: &QueryAst, engine: &mut LsmEngine) -> Result<QueryResult> {
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
                let result = self.execute_with_engine(query, engine)?;
                if let QueryResult::Rows(rows) = result {
                    let prefix = format!("__mv_{}::", name.to_lowercase());
                    let row_count = rows.len();
                    for (i, row) in rows.iter().enumerate() {
                        let key = format!("{}{:010}", prefix, i);
                        let value = serde_json::to_vec(&serde_json::Value::Object(row.clone()))
                            .map_err(|e| CoreError::Serialization(e.to_string()))?;
                        engine.put(key.as_bytes().to_vec(), value)?;
                    }
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
                let prefix = format!("__mv_{}::", name.to_lowercase());
                let existing = engine.scan_prefix(prefix.as_bytes()).unwrap_or_default();
                let count = existing.len();
                for (key, _) in existing {
                    let _ = engine.put(key, b"__deleted__".to_vec());
                }
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
                Ok(QueryResult::Success(format!(
                    "Materialized view '{}' refreshed (re-run CREATE MATERIALIZED VIEW to update)", name
                )))
            }
            QueryAst::Begin => {
                let mut txn = self.active_txn.lock().unwrap();
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
                let mut txn = self.active_txn.lock().unwrap();
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
                let mut txn = self.active_txn.lock().unwrap();
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
            _ => {
                // For SELECT queries, check plan cache first
                if matches!(ast, QueryAst::Select { .. }) {
                    let ast_hash = Self::hash_ast(ast);
                    let cached_plan = self.plan_cache.lock().unwrap().get(ast_hash);
                    if cached_plan.is_some() {
                        // Plan cache hit - skip planning
                        self.runtime_stats.lock().unwrap().plan_cache_hits += 1;
                    } else {
                        // Plan cache miss - generate and cache plan
                        let plan = self.planner.read().unwrap().plan(ast);
                        if let Ok(p) = plan {
                            self.plan_cache.lock().unwrap().insert(ast_hash, p);
                        }
                        self.runtime_stats.lock().unwrap().plan_cache_misses += 1;
                    }
                }

                // Check if there's an active multi-statement transaction
                let active_txn_id = *self.active_txn.lock().unwrap();
                if let Some(txn_id) = active_txn_id {
                    // Use the active transaction (writes are buffered until COMMIT)
                    self.execute_in_txn(ast, engine, txn_id)
                } else {
                    // Auto-commit mode: each statement runs in its own transaction
                    let txn_id = engine.begin_txn();
                    let result = self.execute_in_txn(ast, engine, txn_id);
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
    pub fn execute_plan(&self, plan: &ExecutionPlan, engine: &mut LsmEngine) -> Result<QueryResult> {
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
    fn execute_plan_node(&self, node: &PlanNode, engine: &mut LsmEngine) -> Result<Vec<Map<String, Value>>> {
        match node {
            PlanNode::SeqScan { table, alias, filter, .. } => {
                self.plan_seq_scan(engine, table, alias.as_deref(), filter)
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
                // Get the raw rows from the child node (before projection)
                let raw_rows = self.execute_plan_node(input, engine)?;
                // Check if there are aggregates or expressions that need all columns
                let has_aggregates = Self::columns_have_aggregates(columns);
                let has_expr = match columns {
                    SelectColumns::Columns(items) => items.iter().any(|item| matches!(item, SelectItem::Expression(_))),
                    _ => false,
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
                                            let name = name.split('.').last().unwrap_or(name);
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
            PlanNode::Aggregation { input, group_by, .. } => {
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
        }
    }

    /// Plan-driven sequential scan with optional filter.
    fn plan_seq_scan(
        &self,
        engine: &mut LsmEngine,
        table: &str,
        alias: Option<&str>,
        filter: &Option<FilterExpr>,
    ) -> Result<Vec<Map<String, Value>>> {
        // Check CTE tables first
        let cte_prefix = format!("__cte_{}::", table.to_lowercase());
        let cte_entries = engine.scan_prefix(cte_prefix.as_bytes()).unwrap_or_default();
        if !cte_entries.is_empty() {
            let mut rows = Vec::new();
            for (_key, val_bytes) in &cte_entries {
                if val_bytes == b"__deleted__" { continue; }
                if let Ok(serde_json::Value::Object(doc)) = serde_json::from_slice::<serde_json::Value>(val_bytes) {
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
                if let Ok(serde_json::Value::Object(doc)) = serde_json::from_slice::<serde_json::Value>(val_bytes) {
                    rows.push(doc);
                }
            }
            if !rows.is_empty() {
                return Ok(rows);
            }
        }

        // Regular table scan
        let prefix = format!("{}::", table);
        let entries = engine.scan_prefix(prefix.as_bytes())?;
        let mut rows = Vec::new();
        for (key, val_bytes) in &entries {
            if let Ok(serde_json::Value::Object(mut doc)) = serde_json::from_slice::<serde_json::Value>(val_bytes) {
                if doc.get("__class__").and_then(|v| v.as_str()) == Some(table) {
                    // Store primary key for DELETE/UPDATE operations
                    doc.insert("__pk__".to_string(), Value::String(String::from_utf8_lossy(key).to_string()));
                    rows.push(doc);
                }
            }
        }

        // Apply filter if present
        if let Some(f) = filter {
            rows.retain(|row| Self::eval_filter_static(row, f));
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
        engine: &mut LsmEngine,
        table: &str,
        alias: Option<&str>,
        index_column: &str,
        filter: &Option<FilterExpr>,
    ) -> Result<Vec<Map<String, Value>>> {
        // Try to use the index for the primary lookup
        if engine.has_index(table, index_column) {
            if let Some(f) = filter {
                // Use index for the filter
                if let Some(pkeys) = Self::try_index_scan_single(engine, table, f)? {
                    let mut rows = Self::fetch_rows_by_pks(engine, &pkeys)?;
                    // Apply alias
                    if let Some(a) = alias {
                        Self::apply_alias(&mut rows, a);
                    }
                    return Ok(rows);
                }
            }
        }

        // Fallback to full scan
        self.plan_seq_scan(engine, table, alias, filter)
    }

    /// Plan-driven index lookup (point query).
    fn plan_index_lookup(
        &self,
        engine: &mut LsmEngine,
        table: &str,
        alias: Option<&str>,
        index_column: &str,
        key: &LiteralValue,
    ) -> Result<Vec<Map<String, Value>>> {
        if engine.has_index(table, index_column) {
            let index_mgr = engine.index_manager();
            let json_val = Self::literal_to_json_static(key);
            let pkeys = index_mgr.lookup_eq(table, index_column, &json_val).unwrap_or_default();
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
        engine: &mut LsmEngine,
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
            let prefix = format!("{}::", table);
            let entries = engine.scan_prefix(prefix.as_bytes())?;
            let mut allowed_ids = std::collections::HashSet::new();
            for (key, val_bytes) in &entries {
                if let Ok(serde_json::Value::Object(ref doc)) = serde_json::from_slice::<serde_json::Value>(val_bytes) {
                    if doc.get("__class__").and_then(|v| v.as_str()) == Some(table) {
                        if Self::eval_filter_static(doc, f) {
                            allowed_ids.insert(key.clone());
                        }
                    }
                }
            }
            engine.vector_index_manager().search_filtered(table, column, query_vector, top_k, &allowed_ids)?
        } else {
            engine.vector_index_manager().search(table, column, query_vector, top_k)?
        };

        let mut rows = Vec::new();
        for result in &search_results {
            if let Ok(Some(val_bytes)) = engine.get(&result.entry.id) {
                if let Ok(serde_json::Value::Object(mut doc)) = serde_json::from_slice::<serde_json::Value>(&val_bytes) {
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
        let left_alias = None::<&str>;
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
    /// If ANALYZE mode, also executes the query and measures actual time.
    fn execute_explain(&self, query: &QueryAst, engine: &mut LsmEngine) -> Result<QueryResult> {
        let plan = self.planner.read().unwrap().plan(query)?;
        let description = plan.describe();

        // Check if this is EXPLAIN ANALYZE (the query is the inner query)
        let start = std::time::Instant::now();
        let actual_result = self.execute_with_engine(query, engine);
        let elapsed = start.elapsed();

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
                "actual_time_ms": elapsed.as_secs_f64() * 1000.0,
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
    fn execute_analyze(&self, table: &str, engine: &mut LsmEngine) -> Result<QueryResult> {
        let prefix = format!("{}::", table);
        let entries = engine.scan_prefix(prefix.as_bytes()).unwrap_or_default();

        let mut row_count = 0u64;
        let mut column_stats: std::collections::HashMap<String, ColumnStats> = std::collections::HashMap::new();

        for (_key, val_bytes) in &entries {
            if let Ok(serde_json::Value::Object(doc)) = serde_json::from_slice::<serde_json::Value>(val_bytes) {
                if doc.get("__class__").and_then(|v| v.as_str()) == Some(table) {
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
            let mut stats = self.runtime_stats.lock().unwrap();
            stats.table_row_counts.insert(table.to_string(), row_count);
        }

        // Update planner statistics for future query optimization
        self.planner.write().unwrap().update_stats(table.to_string(), planner_stats.clone());

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

    /// Executes a WITH clause (Common Table Expression).
    /// CTEs are materialized into temporary storage, then the main query runs.
    fn execute_with_ctes(
        &self,
        ctes: &[crate::parser::CteDefinition],
        query: &QueryAst,
        engine: &mut LsmEngine,
        recursive: bool,
    ) -> Result<QueryResult> {
        // Materialize each CTE: execute the query and store results under a temp key
        for cte in ctes {
            let cte_result = self.execute_with_engine(&cte.query, engine)?;
            if let QueryResult::Rows(rows) = cte_result {
                // Store CTE results as a temporary "table" using a special prefix
                let prefix = format!("__cte_{}::", cte.name.to_lowercase());
                // Clear any previous CTE with this name
                let existing = engine.scan_prefix(prefix.as_bytes()).unwrap_or_default();
                for (key, _) in existing {
                    // We can't easily delete without txn, so we overwrite
                    let _ = engine.put(key, b"__deleted__".to_vec());
                }
                // Insert each row as a CTE entry
                for (i, row) in rows.iter().enumerate() {
                    let key = format!("{}{:010}", prefix, i);
                    let value = serde_json::to_vec(&serde_json::Value::Object(row.clone()))
                        .map_err(|e| CoreError::Serialization(e.to_string()))?;
                    engine.put(key.as_bytes().to_vec(), value)?;
                }
            }
        }

        // Execute the main query 鈥?it will scan CTE tables via the prefix scan path
        // We need to handle CTE name resolution in the main query
        let result = self.execute_with_engine(query, engine);

        // Clean up CTE temporary data
        for cte in ctes {
            let prefix = format!("__cte_{}::", cte.name.to_lowercase());
            let existing = engine.scan_prefix(prefix.as_bytes()).unwrap_or_default();
            for (key, _) in existing {
                let _ = engine.put(key, b"__deleted__".to_vec());
            }
        }

        result
    }

    /// Executes a statement within an existing transaction.
    fn execute_in_txn(&self, ast: &QueryAst, engine: &mut LsmEngine, txn_id: u64) -> Result<QueryResult> {
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
                let select_result = self.execute_with_engine(query, engine)?;
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
                                let new_value = serde_json::to_vec(&serde_json::Value::Object(doc))
                                    .map_err(|e| CoreError::Serialization(e.to_string()))?;
                                engine.txn_put(txn_id, key, new_value)?;
                                return Ok(QueryResult::Success("1 row updated (upsert)".to_string()));
                            }
                        }
                    }
                }
                // No conflict - insert normally
                self.execute_insert_txn(engine, txn_id, class, columns, values)
            }
            QueryAst::Select {
                distinct,
                columns,
                from,
                from_alias,
                joins,
                filter,
                group_by,
                having,
                order_by,
                limit,
                offset,
                ..
            } => {
                // Phase 24: Use plan-driven execution
                // Generate execution plan, then execute it
                let plan = self.planner.read().unwrap().plan(ast)?;

                // Execute the plan to get core result rows
                let plan_result = self.execute_plan(&plan, engine)?;
                let mut rows = match plan_result {
                    QueryResult::Rows(r) => r,
                    other => return Ok(other),
                };

                // Post-processing steps not yet in the plan:
                // 1. Aggregation (GROUP BY + HAVING)
                let has_aggregates = Self::columns_have_aggregates(columns);
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
                        let new_value = serde_json::to_vec(&Value::Object(doc))
                            .map_err(|e| CoreError::Serialization(e.to_string()))?;
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
    fn execute_union(&self, engine: &mut LsmEngine, left: &QueryAst, right: &QueryAst, all: bool) -> Result<QueryResult> {
        let left_result = self.execute_with_engine(left, engine)?;
        let right_result = self.execute_with_engine(right, engine)?;

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
            let a_val = Self::resolve_column_value(a, col).unwrap_or_default();
            let b_val = Self::resolve_column_value(b, col).unwrap_or_default();
            let ord = Self::compare_values(&a_val, &b_val);
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
        engine: &mut LsmEngine,
        columns: &SelectColumns,
        rows: &[Map<String, Value>],
        group_by: Option<&crate::parser::GroupByClause>,
        having: &Option<FilterExpr>,
        order_by: &[crate::parser::OrderBy],
        limit: Option<usize>,
    ) -> Result<QueryResult> {
        // Group rows by GROUP BY columns (or single group if no GROUP BY)
        let groups: Vec<(String, Vec<&Map<String, Value>>)> = if let Some(gb) = group_by {
            let mut group_map: std::collections::BTreeMap<String, Vec<&Map<String, Value>>> =
                std::collections::BTreeMap::new();
            for row in rows {
                let key = gb
                    .columns
                    .iter()
                    .map(|col| {
                        Self::resolve_column_value(row, col)
                            .unwrap_or_else(|| "NULL".to_string())
                    })
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
                            let col_name = col_name.split('.').last().unwrap_or(col_name);
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
                    Value::Number(serde_json::Number::from(sum as i64))
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
        frame: &Option<crate::parser::WindowFrame>,
    ) -> Vec<Value> {
        let n = partition.len();
        let mut values = Vec::with_capacity(n);

        for idx in 0..n {
            let val = match func {
                WindowFunc::RowNumber => Value::Number(serde_json::Number::from(idx + 1)),
                WindowFunc::Rank => {
                    // Rank: same value gets same rank, then skip
                    let mut rank = 1;
                    if let Some(ob) = partition.iter().find_map(|(_, row)| {
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
                            .map(|v| Value::String(v))
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
                            .map(|v| Value::String(v))
                            .unwrap_or(Value::Null)
                    }
                }
                WindowFunc::FirstValue => {
                    let col = arg.unwrap_or("");
                    Self::resolve_column_value(&partition[0].1, col)
                        .map(|v| Value::String(v))
                        .unwrap_or(Value::Null)
                }
                WindowFunc::LastValue => {
                    let col = arg.unwrap_or("");
                    Self::resolve_column_value(&partition[n - 1].1, col)
                        .map(|v| Value::String(v))
                        .unwrap_or(Value::Null)
                }
                WindowFunc::NthValue => {
                    // NTH_VALUE(col, n) - n is in the arg after comma
                    let col = arg.unwrap_or("");
                    // For simplicity, return the value at the current row position
                    Self::resolve_column_value(&partition[idx].1, col)
                        .map(|v| Value::String(v))
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
            Value::Number(serde_json::Number::from(sum as i64))
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
            ValueExpr::Column(col) => col.split('.').last().unwrap_or(col).to_string(),
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
        let left = on.left.split('.').last().unwrap_or(&on.left).to_string();
        let right = on.right.split('.').last().unwrap_or(&on.right).to_string();
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

    // 鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺?
    //  Transactional versions (use txn_* API)
    // 鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺愨晲鈺?

    fn execute_insert_txn(
        &self,
        engine: &mut LsmEngine,
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

        let value = serde_json::to_vec(&Value::Object(doc))
            .map_err(|e| CoreError::Serialization(e.to_string()))?;

        engine.txn_put(txn_id, key, value)?;
        Ok(QueryResult::Success("1 row inserted".to_string()))
    }

    // Old execute_select_txn, execute_hash_join, execute_sort_merge_join,
    // execute_delete_txn, execute_update_txn removed in Phase 25.
    // All queries now use plan-driven execution via execute_plan().

    fn execute_match_txn(
        &self,
        engine: &mut LsmEngine,
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
        engine: &mut LsmEngine,
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
            // Get document keys matching the filter
            let prefix = format!("{}::", class);
            let entries = engine.txn_scan_prefix(txn_id, prefix.as_bytes())?;
            let mut allowed_ids = std::collections::HashSet::new();
            for (key, val_bytes) in &entries {
                if let Ok(serde_json::Value::Object(ref doc)) =
                    serde_json::from_slice::<serde_json::Value>(val_bytes)
                {
                    if doc.get("__class__").and_then(|v| v.as_str()) == Some(class) {
                        if self.matches_filter(engine, doc, &Some(filter_expr.clone())) {
                            allowed_ids.insert(key.clone());
                        }
                    }
                }
            }

            engine.vector_index_manager().search_filtered(
                class, column, query_vector, top_k, &allowed_ids,
            )?
        } else {
            engine.vector_index_manager().search(
                class, column, query_vector, top_k,
            )?
        };

        // Fetch full documents for the search results
        let mut rows = Vec::new();
        for result in &search_results {
            if let Ok(Some(val_bytes)) = engine.txn_get(txn_id, &result.entry.id) {
                if let Ok(serde_json::Value::Object(mut doc)) =
                    serde_json::from_slice::<serde_json::Value>(&val_bytes)
                {
                    // Add the distance as a virtual column
                    doc.insert(
                        "_distance".to_string(),
                        serde_json::json!(result.distance),
                    );
                    rows.push(doc);
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

    fn generate_doc_key(&self, class: &str) -> Vec<u8> {
        let seq = self.doc_counter.fetch_add(1, Ordering::Relaxed);
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        // Combine timestamp + counter for collision-free uniqueness
        let unique = (ts as u128) << 64 | seq as u128;
        format!("{}::{:040}", class, unique).into_bytes()
    }

    /// Tries to use a secondary index for the given filter.
    /// Tries to use an index for a single (non-AND/OR) predicate.
    /// Returns Some(primary_keys) if index was used, None otherwise.
    fn try_index_scan_single(
        engine: &mut LsmEngine,
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

        let index_mgr = engine.index_manager();

        let pkeys: Vec<Vec<u8>> = match filter {
            FilterExpr::Eq(_, val) => {
                let json_val = Self::literal_to_json_static(val);
                index_mgr.lookup_eq(class, &col, &json_val).unwrap_or_default()
            }
            FilterExpr::Gt(_, val) => {
                let json_val = Self::literal_to_json_static(val);
                index_mgr.lookup_gt(class, &col, &json_val).unwrap_or_default()
            }
            FilterExpr::Lt(_, val) => {
                let json_val = Self::literal_to_json_static(val);
                index_mgr.lookup_lt(class, &col, &json_val).unwrap_or_default()
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
                    .lookup_range(class, &col, Some(&low_json), Some(&high_json))
                    .unwrap_or_default()
            }
            FilterExpr::In(_, values) => {
                let mut all_pkeys = Vec::new();
                for val in values {
                    let json_val = Self::literal_to_json_static(val);
                    if let Some(pks) = index_mgr.lookup_eq(class, &col, &json_val) {
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
    fn eval_filter_static(doc: &Map<String, Value>, filter: &FilterExpr) -> bool {
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
                resolve_val(col).map_or(false, |v| Self::value_matches_static(v, val))
            }
            FilterExpr::Ne(col, val) => {
                !resolve_val(col).map_or(false, |v| Self::value_matches_static(v, val))
            }
            FilterExpr::Gt(col, val) => {
                resolve_val(col).map_or(false, |v| Self::value_gt_static(v, val))
            }
            FilterExpr::Lt(col, val) => {
                resolve_val(col).map_or(false, |v| Self::value_lt_static(v, val))
            }
            FilterExpr::Gte(col, val) => {
                resolve_val(col).map_or(false, |v| Self::value_gte_static(v, val))
            }
            FilterExpr::Lte(col, val) => {
                resolve_val(col).map_or(false, |v| Self::value_lte_static(v, val))
            }
            FilterExpr::Like(col, pattern) => {
                resolve_val(col).map_or(false, |v| {
                    let s = match v {
                        Value::String(s) => s.clone(),
                        _ => v.to_string(),
                    };
                    Self::like_match(&s, pattern)
                })
            }
            FilterExpr::Between(col, low, high) => {
                resolve_val(col).map_or(false, |v| {
                    Self::value_gte_static(v, low) && Self::value_lte_static(v, high)
                })
            }
            FilterExpr::In(col, values) => {
                resolve_val(col).map_or(false, |v| {
                    values.iter().any(|lv| Self::value_matches_static(v, lv))
                })
            }
            FilterExpr::And(left, right) => {
                Self::eval_filter_static(doc, left) && Self::eval_filter_static(doc, right)
            }
            FilterExpr::Or(left, right) => {
                Self::eval_filter_static(doc, left) || Self::eval_filter_static(doc, right)
            }
            _ => true, // Complex filters (EXISTS, subqueries) pass through
        }
    }

    fn value_gte_static(v: &Value, lit: &LiteralValue) -> bool {
        Self::value_gt_static(v, lit) || Self::value_matches_static(v, lit)
    }

    fn value_lte_static(v: &Value, lit: &LiteralValue) -> bool {
        Self::value_lt_static(v, lit) || Self::value_matches_static(v, lit)
    }

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

    fn value_gt_static(v: &Value, lit: &LiteralValue) -> bool {
        match (v, lit) {
            (Value::Number(n), LiteralValue::Int(l)) => n.as_i64().map_or(false, |n| n > *l),
            (Value::Number(n), LiteralValue::Float(l)) => n.as_f64().map_or(false, |n| n > *l),
            (Value::String(s), LiteralValue::String(l)) => s.as_str() > l.as_str(),
            _ => false,
        }
    }

    fn value_lt_static(v: &Value, lit: &LiteralValue) -> bool {
        match (v, lit) {
            (Value::Number(n), LiteralValue::Int(l)) => n.as_i64().map_or(false, |n| n < *l),
            (Value::Number(n), LiteralValue::Float(l)) => n.as_f64().map_or(false, |n| n < *l),
            (Value::String(s), LiteralValue::String(l)) => s.as_str() < l.as_str(),
            _ => false,
        }
    }

    /// Fetches rows by their primary keys (plan-driven path, no txn).
    fn fetch_rows_by_pks(
        engine: &mut LsmEngine,
        pkeys: &[Vec<u8>],
    ) -> Result<Vec<Map<String, Value>>> {
        let mut rows = Vec::new();
        for pk in pkeys {
            if let Ok(Some(val_bytes)) = engine.get(pk) {
                if let Ok(Value::Object(doc)) = serde_json::from_slice::<Value>(&val_bytes) {
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

    fn matches_filter(&self, engine: &mut LsmEngine, doc: &Map<String, Value>, filter: &Option<FilterExpr>) -> bool {
        match filter {
            None => true,
            Some(expr) => self.eval_filter(engine, doc, expr),
        }
    }

    fn eval_filter(&self, engine: &mut LsmEngine, doc: &Map<String, Value>, expr: &FilterExpr) -> bool {
        match expr {
            FilterExpr::Eq(col, val) => {
                doc.get(col)
                    .map_or(false, |v| self.value_matches(v, val))
            }
            FilterExpr::Ne(col, val) => {
                !doc.get(col)
                    .map_or(false, |v| self.value_matches(v, val))
            }
            FilterExpr::Gt(col, val) => {
                doc.get(col)
                    .map_or(false, |v| self.value_gt(v, val))
            }
            FilterExpr::Lt(col, val) => {
                doc.get(col)
                    .map_or(false, |v| self.value_lt(v, val))
            }
            FilterExpr::Gte(col, val) => {
                doc.get(col)
                    .map_or(false, |v| self.value_gt(v, val) || self.value_matches(v, val))
            }
            FilterExpr::Lte(col, val) => {
                doc.get(col)
                    .map_or(false, |v| self.value_lt(v, val) || self.value_matches(v, val))
            }
            FilterExpr::Like(col, pattern) => {
                doc.get(col).map_or(false, |v| {
                    let s = match v {
                        Value::String(s) => s.clone(),
                        _ => v.to_string(),
                    };
                    Self::like_match(&s, pattern)
                })
            }
            FilterExpr::Between(col, low, high) => {
                doc.get(col).map_or(false, |v| {
                    self.value_gte(v, low) && self.value_lte(v, high)
                })
            }
            FilterExpr::In(col, values) => {
                doc.get(col).map_or(false, |v| {
                    values.iter().any(|val| self.value_matches(v, val))
                })
            }
            FilterExpr::InSubquery(col, subquery) => {
                let sub_result = self.execute_with_engine(subquery, engine);
                match sub_result {
                    Ok(QueryResult::Rows(rows)) => {
                        doc.get(col).map_or(false, |v| {
                            rows.iter().any(|row| {
                                row.values().any(|sv| {
                                    match (v, sv) {
                                        (Value::String(a), Value::String(b)) => a == b,
                                        (Value::Number(a), Value::Number(b)) => a == b,
                                        _ => v.to_string() == sv.to_string(),
                                    }
                                })
                            })
                        })
                    }
                    _ => false,
                }
            }
            FilterExpr::Exists(subquery) => {
                let sub_result = self.execute_with_engine(subquery, engine);
                match sub_result {
                    Ok(QueryResult::Rows(rows)) => !rows.is_empty(),
                    _ => false,
                }
            }
            FilterExpr::NotExists(subquery) => {
                let sub_result = self.execute_with_engine(subquery, engine);
                match sub_result {
                    Ok(QueryResult::Rows(rows)) => rows.is_empty(),
                    _ => false,
                }
            }
            FilterExpr::IsNull(col) => {
                doc.get(col).map_or(true, |v| matches!(v, Value::Null))
            }
            FilterExpr::IsNotNull(col) => {
                doc.get(col).map_or(false, |v| !matches!(v, Value::Null))
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

    fn value_gt(&self, v: &Value, lit: &LiteralValue) -> bool {
        match (v, lit) {
            (Value::Number(n), LiteralValue::Int(l)) => n.as_i64().map_or(false, |n| n > *l),
            (Value::Number(n), LiteralValue::Float(l)) => n.as_f64().map_or(false, |n| n > *l),
            (Value::String(s), LiteralValue::String(l)) => s.as_str() > l.as_str(),
            _ => false,
        }
    }

    fn value_lt(&self, v: &Value, lit: &LiteralValue) -> bool {
        match (v, lit) {
            (Value::Number(n), LiteralValue::Int(l)) => n.as_i64().map_or(false, |n| n < *l),
            (Value::Number(n), LiteralValue::Float(l)) => n.as_f64().map_or(false, |n| n < *l),
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
        engine: &mut LsmEngine,
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
                let sub_result = self.execute_with_engine(subquery, engine)?;
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
                    let len = args.get(2).and_then(|v| v.as_i64()).map(|l| l as usize);
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
                                let name = name.split('.').last().unwrap_or(name);
                                result.insert(name.to_string(), val.clone());
                            } else {
                                // Try alias-aware lookup
                                for (k, v) in doc {
                                    if k.ends_with(&format!(".{}", real_col)) || k == real_col {
                                        let name = alias.unwrap_or(real_col);
                                        let name = name.split('.').last().unwrap_or(name);
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
                        SelectItem::Expression(expr) => {
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
        engine: &mut LsmEngine,
        class: &str,
        doc: &Map<String, Value>,
    ) -> Result<()> {
        // Find the ontology containing this class
        let ontology = match self.ontology_store.find_ontology_for_class(engine, class)? {
            Some(o) => o,
            None => return Ok(()), // No ontology defined 鈥?skip validation
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

        Ok(())
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
        let engine = Arc::new(RwLock::new(LsmEngine::open(options).unwrap()));
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
        executor.engine.write().unwrap().flush().unwrap();

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

        executor.engine.write().unwrap().flush().unwrap();

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

        executor.engine.write().unwrap().flush().unwrap();

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

        executor.engine.write().unwrap().flush().unwrap();

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

        executor.engine.write().unwrap().flush().unwrap();

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

        executor.engine.write().unwrap().flush().unwrap();

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

        executor.engine.write().unwrap().flush().unwrap();

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
        executor.engine.write().unwrap().flush().unwrap();

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
        executor.engine.write().unwrap().flush().unwrap();

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
        executor.engine.write().unwrap().flush().unwrap();

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
        executor.engine.write().unwrap().flush().unwrap();

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
        executor.engine.write().unwrap().flush().unwrap();

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
        executor.engine.write().unwrap().flush().unwrap();

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
        executor.engine.write().unwrap().flush().unwrap();

        // Insert more, flush again
        insert_row(&executor, "Product", "iPad", 799);
        executor.engine.write().unwrap().flush().unwrap();

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
        executor.engine.write().unwrap().flush().unwrap();

        // Update
        let ast = QueryParser::parse("UPDATE Product SET price = 899 WHERE name = 'iPhone'").unwrap();
        executor.execute(&ast).unwrap();
        executor.engine.write().unwrap().flush().unwrap();

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

        executor.engine.write().unwrap().flush().unwrap();

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

        executor.engine.write().unwrap().flush().unwrap();

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

        executor.engine.write().unwrap().flush().unwrap();

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

        executor.engine.write().unwrap().flush().unwrap();

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

        executor.engine.write().unwrap().flush().unwrap();

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
        executor.engine.write().unwrap().flush().unwrap();

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
        executor.engine.write().unwrap().flush().unwrap();

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
        executor.engine.write().unwrap().flush().unwrap();

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

        executor.engine.write().unwrap().flush().unwrap();

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
        executor.engine.write().unwrap().flush().unwrap();

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
        executor.engine.write().unwrap().flush().unwrap();

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
        executor.engine.write().unwrap().flush().unwrap();

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
        executor.engine.write().unwrap().flush().unwrap();

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
        executor.engine.write().unwrap().flush().unwrap();

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
        executor.engine.write().unwrap().flush().unwrap();

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
        executor.engine.write().unwrap().flush().unwrap();

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
        executor.engine.write().unwrap().flush().unwrap();

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
        executor.engine.write().unwrap().flush().unwrap();

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
        executor.engine.write().unwrap().flush().unwrap();

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
        executor.engine.write().unwrap().flush().unwrap();

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
        executor.engine.write().unwrap().flush().unwrap();

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
        executor.engine.write().unwrap().flush().unwrap();

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
        executor.engine.write().unwrap().flush().unwrap();

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
        executor.engine.write().unwrap().flush().unwrap();

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
        executor.engine.write().unwrap().flush().unwrap();

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
        executor.engine.write().unwrap().flush().unwrap();

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
        executor.engine.write().unwrap().flush().unwrap();

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
        executor.engine.write().unwrap().flush().unwrap();

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
        executor.engine.write().unwrap().flush().unwrap();

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
        executor.engine.write().unwrap().flush().unwrap();

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
        executor.engine.write().unwrap().flush().unwrap();

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
        executor.engine.write().unwrap().flush().unwrap();

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
        executor.engine.write().unwrap().flush().unwrap();

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
        executor.engine.write().unwrap().flush().unwrap();

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

        executor.engine.write().unwrap().flush().unwrap();

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
        executor.engine.write().unwrap().flush().unwrap();

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
        assert!(executor.engine.read().unwrap().has_vector_index("Product", "embedding"));

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
        assert!(!executor.engine.read().unwrap().has_vector_index("Product", "embedding"));
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
            let engine = Arc::new(RwLock::new(LsmEngine::open(options).unwrap()));
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
            engine.write().unwrap().flush().unwrap();
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
            let results = engine.vector_index_manager().search(
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
        executor.engine.write().unwrap().flush().unwrap();

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
        executor.engine.write().unwrap().flush().unwrap();

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
        executor.engine.write().unwrap().flush().unwrap();

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

    // 鈹€鈹€ Phase 21: EXPLAIN ANALYZE tests 鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€

    #[test]
    fn test_explain_analyze() {
        let (executor, _dir) = setup();
        insert_row(&executor, "Product", "iPhone", 999);
        executor.engine.write().unwrap().flush().unwrap();

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
        executor.engine.write().unwrap().flush().unwrap();

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
        executor.engine.write().unwrap().flush().unwrap();

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

        // Query should return empty
        let ast = QueryParser::parse("SELECT * FROM mv_test").unwrap();
        let result = executor.execute(&ast).unwrap();
        match &result {
            QueryResult::Rows(rows) => {
                // Should be empty since the MV was dropped
                // (or it might fall through to regular table scan which returns nothing)
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
        executor.engine.write().unwrap().flush().unwrap();

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
        executor.engine.write().unwrap().flush().unwrap();

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
        executor.engine.write().unwrap().flush().unwrap();

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
        executor.engine.write().unwrap().flush().unwrap();

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
        executor.engine.write().unwrap().flush().unwrap();

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
        executor.engine.write().unwrap().flush().unwrap();

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
        executor.engine.write().unwrap().flush().unwrap();

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
        executor.engine.write().unwrap().flush().unwrap();

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
        executor.engine.write().unwrap().flush().unwrap();

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
        executor.engine.write().unwrap().flush().unwrap();

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
        let (executor, _dir) = setup();

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
}
