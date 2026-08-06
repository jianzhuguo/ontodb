//! Query executor: runs parsed queries against the storage and ontology engines.

use crate::optimizer::QueryPlanner;
use crate::parser::{AggregateFunc, FilterExpr, LiteralValue, QueryAst, SelectColumns, SelectItem};
use onto_core::{CoreError, Result};
use onto_ontology::{DataType, OntologyStore};
use onto_storage::LsmEngine;
use serde_json::{json, Map, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

/// Executes parsed queries.
pub struct QueryExecutor {
    engine: Arc<RwLock<LsmEngine>>,
    ontology_store: OntologyStore,
    /// Query planner for optimization.
    planner: QueryPlanner,
    /// Monotonic counter for generating unique document keys.
    doc_counter: AtomicU64,
}

impl QueryExecutor {
    pub fn new(engine: Arc<RwLock<LsmEngine>>, ontology_store: OntologyStore) -> Self {
        Self {
            engine,
            ontology_store,
            planner: QueryPlanner::new(),
            doc_counter: AtomicU64::new(0),
        }
    }

    /// Get a reference to the query planner.
    pub fn planner(&self) -> &QueryPlanner {
        &self.planner
    }

    /// Get a mutable reference to the query planner (for updating stats).
    pub fn planner_mut(&mut self) -> &mut QueryPlanner {
        &mut self.planner
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
    fn execute_with_engine(&self, ast: &QueryAst, engine: &mut LsmEngine) -> Result<QueryResult> {
        match ast {
            QueryAst::Explain { query } => {
                // EXPLAIN: generate and return the execution plan
                self.execute_explain(query)
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
            _ => {
                // All other statements run in an auto-committed transaction
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

    /// Executes EXPLAIN: generates and returns the execution plan.
    fn execute_explain(&self, query: &QueryAst) -> Result<QueryResult> {
        let plan = self.planner.plan(query)?;
        let description = plan.describe();

        let plan_json = json!({
            "plan": format_plan_node(&plan.root),
            "cost": {
                "total": plan.cost.total_cost,
                "io": plan.cost.io_cost,
                "cpu": plan.cost.cpu_cost,
                "rows": plan.cost.rows,
            },
            "uses_index": plan.uses_index,
            "is_sorted": plan.is_sorted,
            "description": description,
        });

        Ok(QueryResult::Rows(vec![Map::from_iter(vec![
            ("plan".to_string(), plan_json),
        ])]))
    }

    /// Executes a statement within an existing transaction.
    fn execute_in_txn(&self, ast: &QueryAst, engine: &mut LsmEngine, txn_id: u64) -> Result<QueryResult> {
        match ast {
            QueryAst::Insert {
                class,
                columns,
                values,
            } => self.execute_insert_txn(engine, txn_id, class, columns, values),
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
                ..
            } => self.execute_select_txn(
                engine, txn_id, *distinct, columns, from, from_alias.as_deref(), joins, filter,
                group_by.as_ref(), having, order_by.as_ref(), *limit,
            ),
            QueryAst::Delete { class, filter } => self.execute_delete_txn(engine, txn_id, class, filter),
            QueryAst::Update {
                class,
                assignments,
                filter,
            } => self.execute_update_txn(engine, txn_id, class, assignments, filter),
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
        order_by: Option<&crate::parser::OrderBy>,
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
                    }
                }
            }

            // Apply HAVING filter
            if self.matches_filter(engine, &result_row, having) {
                result_rows.push(result_row);
            }
        }

        // Sort if ORDER BY specified
        if let Some(ob) = order_by {
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

    // ═══════════════════════════════════════════════════════════════
    //  Transactional versions (use txn_* API)
    // ═══════════════════════════════════════════════════════════════

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

    fn execute_select_txn(
        &self,
        engine: &mut LsmEngine,
        txn_id: u64,
        distinct: bool,
        columns: &SelectColumns,
        from: &str,
        from_alias: Option<&str>,
        joins: &[crate::parser::JoinClause],
        filter: &Option<FilterExpr>,
        group_by: Option<&crate::parser::GroupByClause>,
        having: &Option<FilterExpr>,
        order_by: Option<&crate::parser::OrderBy>,
        limit: Option<usize>,
    ) -> Result<QueryResult> {
        // Try index-accelerated scan for filters that can use an index
        let mut left_rows = match Self::try_index_scan(engine, txn_id, from, filter)? {
            Some(rows) => rows,
            None => Self::full_scan(engine, txn_id, from)?,
        };

        // JOIN expansion (same logic as non-txn version)
        if !joins.is_empty() {
            for join in joins {
                let join_prefix = format!("{}::", join.table);
                let right_entries = engine.txn_scan_prefix(txn_id, join_prefix.as_bytes())?;
                let right_alias = join.alias.as_deref().unwrap_or(&join.table);
                let (left_col, right_col) = Self::resolve_join_columns(&join.on)?;

                let mut right_rows: Vec<Map<String, Value>> = Vec::new();
                for (_key, val_bytes) in &right_entries {
                    if let Ok(Value::Object(doc)) = serde_json::from_slice::<Value>(val_bytes) {
                        if doc.get("__class__").and_then(|v| v.as_str()) == Some(&join.table) {
                            right_rows.push(doc);
                        }
                    }
                }

                let mut new_rows: Vec<Map<String, Value>> = Vec::new();
                for left_row in &left_rows {
                    let left_val = Self::resolve_column_value(left_row, &left_col);
                    for right_row in &right_rows {
                        let right_val = Self::resolve_column_value(right_row, &right_col);
                        if left_val.is_some() && right_val.is_some() && left_val == right_val {
                            let mut merged = Map::new();
                            for (k, v) in left_row {
                                let key = match from_alias {
                                    Some(alias) => format!("{}.{}", alias, k),
                                    None => k.clone(),
                                };
                                merged.insert(key, v.clone());
                                if from_alias.is_some() && !merged.contains_key(k) {
                                    merged.insert(k.clone(), v.clone());
                                }
                            }
                            for (k, v) in right_row {
                                let key = format!("{}.{}", right_alias, k);
                                merged.insert(key, v.clone());
                                if !merged.contains_key(k) {
                                    merged.insert(k.clone(), v.clone());
                                }
                            }
                            new_rows.push(merged);
                        }
                    }
                }
                left_rows = new_rows;
            }
        }

        // Apply WHERE filter
        let mut filtered: Vec<Map<String, Value>> = Vec::new();
        for doc in left_rows {
            if self.matches_filter(engine, &doc, filter) {
                filtered.push(doc);
            }
        }

        // Aggregate or normal query
        let has_aggregates = Self::columns_have_aggregates(columns);
        if group_by.is_some() || has_aggregates {
            let mut result = self.execute_aggregation(engine, columns, &filtered, group_by, having, order_by, limit)?;
            if let QueryResult::Rows(ref mut rows) = result {
                if distinct {
                    Self::dedup_rows(rows);
                }
            }
            return Ok(result);
        }

        // Sort before projection so ORDER BY columns are available
        let mut sorted = filtered;
        if let Some(ob) = order_by {
            Self::sort_rows(&mut sorted, &ob.column, ob.ascending);
        }

        let mut rows: Vec<Map<String, Value>> = Vec::new();
        for doc in sorted {
            rows.push(self.project_columns(&doc, columns));
        }
        if distinct {
            Self::dedup_rows(&mut rows);
        }
        if let Some(limit) = limit {
            rows.truncate(limit);
        }
        Ok(QueryResult::Rows(rows))
    }

    fn execute_delete_txn(
        &self,
        engine: &mut LsmEngine,
        txn_id: u64,
        class: &str,
        filter: &Option<FilterExpr>,
    ) -> Result<QueryResult> {
        let prefix = format!("{}::", class);
        let entries = engine.txn_scan_prefix(txn_id, prefix.as_bytes())?;

        let mut deleted = 0usize;
        for (key, val_bytes) in entries {
            if let Ok(Value::Object(doc)) = serde_json::from_slice::<Value>(&val_bytes) {
                if doc.get("__class__").and_then(|v| v.as_str()) == Some(class) {
                    if self.matches_filter(engine, &doc, filter) {
                        engine.txn_delete(txn_id, key)?;
                        deleted += 1;
                    }
                }
            }
        }
        Ok(QueryResult::Success(format!("{} row(s) deleted", deleted)))
    }

    fn execute_update_txn(
        &self,
        engine: &mut LsmEngine,
        txn_id: u64,
        class: &str,
        assignments: &[(String, LiteralValue)],
        filter: &Option<FilterExpr>,
    ) -> Result<QueryResult> {
        let prefix = format!("{}::", class);
        let entries = engine.txn_scan_prefix(txn_id, prefix.as_bytes())?;

        let mut updated = 0usize;
        for (key, val_bytes) in entries {
            if let Ok(Value::Object(mut doc)) = serde_json::from_slice::<Value>(&val_bytes) {
                if doc.get("__class__").and_then(|v| v.as_str()) == Some(class) {
                    if self.matches_filter(engine, &doc, filter) {
                        for (col, val) in assignments {
                            let json_val = self.literal_to_json(val);
                            let json_val = Self::try_parse_vector(json_val);
                            doc.insert(col.clone(), json_val);
                        }
                        // Validate the updated document against ontology schema
                        self.validate_document(engine, class, &doc)?;
                        let new_value = serde_json::to_vec(&Value::Object(doc))
                            .map_err(|e| CoreError::Serialization(e.to_string()))?;
                        engine.txn_put(txn_id, key, new_value)?;
                        updated += 1;
                    }
                }
            }
        }
        Ok(QueryResult::Success(format!("{} row(s) updated", updated)))
    }

    fn execute_match_txn(
        &self,
        engine: &mut LsmEngine,
        txn_id: u64,
        _variable: &str,
        class: &str,
        filter: &Option<FilterExpr>,
        returns: &[String],
    ) -> Result<QueryResult> {
        let columns = if returns.is_empty() {
            SelectColumns::All
        } else {
            SelectColumns::Columns(
                returns.iter().map(|r| SelectItem::Column(r.clone())).collect(),
            )
        };
        self.execute_select_txn(engine, txn_id, false, &columns, class, None, &[], filter, None, &None, None, None)
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
    /// Returns Some(rows) if an index was used, None if a full scan is needed.
    fn try_index_scan(
        engine: &mut LsmEngine,
        txn_id: u64,
        class: &str,
        filter: &Option<FilterExpr>,
    ) -> Result<Option<Vec<Map<String, Value>>>> {
        let filter = match filter {
            Some(f) => f,
            None => return Ok(None),
        };

        // Extract the column name and check if an index exists
        let col = match filter {
            FilterExpr::Eq(c, _)
            | FilterExpr::Ne(c, _)
            | FilterExpr::Gt(c, _)
            | FilterExpr::Lt(c, _)
            | FilterExpr::Gte(c, _)
            | FilterExpr::Lte(c, _)
            | FilterExpr::Between(c, _, _)
            | FilterExpr::In(c, _) => c.clone(),
            _ => return Ok(None), // Complex filters can't use a single index
        };

        if !engine.has_index(class, &col) {
            return Ok(None); // No index on this column
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
                // gte = gt + eq
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

        // Fetch the actual rows by primary keys
        let rows = Self::fetch_rows_by_pks(engine, txn_id, &pkeys)?;
        Ok(Some(rows))
    }

    /// Fetches rows by their primary keys within a transaction.
    fn fetch_rows_by_pks(
        engine: &mut LsmEngine,
        txn_id: u64,
        pkeys: &[Vec<u8>],
    ) -> Result<Vec<Map<String, Value>>> {
        let mut rows = Vec::new();
        for pk in pkeys {
            if let Ok(Some(val_bytes)) = engine.txn_get(txn_id, pk) {
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

    /// Performs a full prefix scan (non-indexed path).
    fn full_scan(
        engine: &mut LsmEngine,
        txn_id: u64,
        class: &str,
    ) -> Result<Vec<serde_json::Map<String, serde_json::Value>>> {
        let prefix = format!("{}::", class);
        let entries = engine.txn_scan_prefix(txn_id, prefix.as_bytes())?;

        let mut rows = Vec::new();
        for (_key, val_bytes) in &entries {
            if let Ok(serde_json::Value::Object(doc)) = serde_json::from_slice::<serde_json::Value>(val_bytes) {
                if doc.get("__class__").and_then(|v| v.as_str()) == Some(class) {
                    rows.push(doc);
                }
            }
        }
        Ok(rows)
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
                    }
                }
                result
            }
        }
    }

    // ═══════════════════════════════════════════════════════════════
    //  Schema Validation
    // ═══════════════════════════════════════════════════════════════

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
        PlanNode::Sort { input, order_by, estimated_rows, .. } => {
            json!({
                "type": "Sort",
                "input": format_plan_node(input),
                "order_by": {
                    "column": order_by.column,
                    "ascending": order_by.ascending,
                },
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
        let dir = tempdir().unwrap();
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            memtable_size_limit: 1024 * 1024,
            ..Default::default()
        };
        let engine = Arc::new(RwLock::new(LsmEngine::open(options).unwrap()));
        let ontology_store = OntologyStore::new(engine.clone());
        let executor = QueryExecutor::new(engine, ontology_store);
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

    // ── UPDATE edge cases ──────────────────────────────────────────

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

    // ── DELETE edge cases ──────────────────────────────────────────

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

    // ── Full lifecycle ─────────────────────────────────────────────

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

    // ── Data persistence across flush ──────────────────────────────

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

    // ── JOIN tests ────────────────────────────────────────────────

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

    // ── GROUP BY and aggregate tests ──────────────────────────────

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

    // ── ORDER BY tests ────────────────────────────────────────────

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

    // ── DISTINCT tests ────────────────────────────────────────────

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

    // ── LIKE tests ────────────────────────────────────────────────

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

    // ── BETWEEN tests ─────────────────────────────────────────────

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

    // ── IN tests ──────────────────────────────────────────────────

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

    // ── UNION tests ───────────────────────────────────────────────

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

    // ── Subquery tests ────────────────────────────────────────────

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

    // ── Index-accelerated range query tests ───────────────────────

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

        // Range query: price > 500 — should use index
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

    // ── Index consistency on UPDATE/DELETE ───────────────────────

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

    // ── Vector search tests ───────────────────────────────────────

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
}
