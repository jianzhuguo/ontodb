//! Query executor: runs parsed queries against the storage and ontology engines.

use crate::parser::{AggregateFunc, FilterExpr, LiteralValue, QueryAst, SelectColumns, SelectItem};
use onto_core::{CoreError, Result};
use onto_ontology::OntologyStore;
use onto_storage::LsmEngine;
use serde_json::{json, Map, Value};
use std::sync::{Arc, RwLock};

/// Executes parsed queries.
pub struct QueryExecutor {
    engine: Arc<RwLock<LsmEngine>>,
    ontology_store: OntologyStore,
}

impl QueryExecutor {
    pub fn new(engine: Arc<RwLock<LsmEngine>>, ontology_store: OntologyStore) -> Self {
        Self {
            engine,
            ontology_store,
        }
    }

    /// Executes a query and returns results as JSON.
    pub fn execute(&self, ast: &QueryAst) -> Result<QueryResult> {
        match ast {
            QueryAst::CreateOntology { sql } => self.execute_create_ontology(sql),
            QueryAst::Insert {
                class,
                columns,
                values,
            } => self.execute_insert(class, columns, values),
            QueryAst::Select {
                columns,
                from,
                from_alias,
                joins,
                filter,
                group_by,
                having,
                limit,
                ..
            } => self.execute_select(
                columns, from, from_alias.as_deref(), joins, filter,
                group_by.as_ref(), having, *limit,
            ),
            QueryAst::Delete { class, filter } => self.execute_delete(class, filter),
            QueryAst::Update {
                class,
                assignments,
                filter,
            } => self.execute_update(class, assignments, filter),
            QueryAst::Match {
                variable,
                class,
                filter,
                returns,
            } => self.execute_match(variable, class, filter, returns),
        }
    }

    fn execute_create_ontology(&self, sql: &str) -> Result<QueryResult> {
        let ontology = onto_ontology::OntologyParser::parse(sql)?;
        self.ontology_store.save(&ontology)?;

        Ok(QueryResult::Success(format!(
            "Ontology '{}' created with {} classes and {} properties",
            ontology.name,
            ontology.classes.len(),
            ontology.properties.len()
        )))
    }

    fn execute_insert(
        &self,
        class: &str,
        columns: &[String],
        values: &[LiteralValue],
    ) -> Result<QueryResult> {
        // Build document key: <class>::<id>
        // For now, use a simple auto-increment approach
        let key = self.generate_doc_key(class);

        // Build JSON document
        let mut doc = Map::new();
        doc.insert("__class__".to_string(), json!(class));

        for (col, val) in columns.iter().zip(values.iter()) {
            doc.insert(col.clone(), self.literal_to_json(val));
        }

        let value = serde_json::to_vec(&Value::Object(doc))
            .map_err(|e| CoreError::Serialization(e.to_string()))?;

        let mut engine = self.engine.write().unwrap();
        engine.put(key, value)?;

        Ok(QueryResult::Success("1 row inserted".to_string()))
    }

    fn execute_select(
        &self,
        columns: &SelectColumns,
        from: &str,
        from_alias: Option<&str>,
        joins: &[crate::parser::JoinClause],
        filter: &Option<FilterExpr>,
        group_by: Option<&crate::parser::GroupByClause>,
        having: &Option<FilterExpr>,
        limit: Option<usize>,
    ) -> Result<QueryResult> {
        let mut engine = self.engine.write().unwrap();

        // ── Step 1: Scan and join rows ────────────────────────────────
        let prefix = format!("{}::", from);
        let left_entries = engine.scan_prefix(prefix.as_bytes())?;

        let mut all_rows: Vec<Map<String, Value>> = Vec::new();
        for (_key, val_bytes) in &left_entries {
            if let Ok(Value::Object(doc)) = serde_json::from_slice::<Value>(val_bytes) {
                if doc.get("__class__").and_then(|v| v.as_str()) == Some(from) {
                    all_rows.push(doc);
                }
            }
        }

        // JOIN expansion
        for join in joins {
            let join_prefix = format!("{}::", join.table);
            let right_entries = engine.scan_prefix(join_prefix.as_bytes())?;
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
            for left_row in &all_rows {
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
            all_rows = new_rows;
        }

        // ── Step 2: Apply WHERE filter ────────────────────────────────
        let filtered: Vec<Map<String, Value>> = all_rows
            .into_iter()
            .filter(|doc| self.matches_filter(doc, filter))
            .collect();

        // ── Step 3: Check if aggregation is needed ────────────────────
        let has_aggregates = Self::columns_have_aggregates(columns);

        if group_by.is_some() || has_aggregates {
            // Aggregate query
            let result = self.execute_aggregation(
                columns, &filtered, group_by, having, limit,
            );
            return result;
        }

        // ── Step 4: Non-aggregate query (existing logic) ──────────────
        let mut rows: Vec<Map<String, Value>> = Vec::new();
        for doc in filtered {
            rows.push(self.project_columns(&doc, columns));
            if let Some(limit) = limit {
                if rows.len() >= limit {
                    break;
                }
            }
        }

        Ok(QueryResult::Rows(rows))
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
        columns: &SelectColumns,
        rows: &[Map<String, Value>],
        group_by: Option<&crate::parser::GroupByClause>,
        having: &Option<FilterExpr>,
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
            if self.matches_filter(&result_row, having) {
                result_rows.push(result_row);
            }

            if let Some(limit) = limit {
                if result_rows.len() >= limit {
                    break;
                }
            }
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

    fn execute_delete(&self, class: &str, filter: &Option<FilterExpr>) -> Result<QueryResult> {
        let mut engine = self.engine.write().unwrap();

        let prefix = format!("{}::", class);
        let entries = engine.scan_prefix(prefix.as_bytes())?;

        let mut deleted = 0usize;
        for (key, val_bytes) in entries {
            if let Ok(Value::Object(doc)) = serde_json::from_slice::<Value>(&val_bytes) {
                if doc.get("__class__").and_then(|v| v.as_str()) == Some(class) {
                    if self.matches_filter(&doc, filter) {
                        engine.delete(key)?;
                        deleted += 1;
                    }
                }
            }
        }

        Ok(QueryResult::Success(format!("{} row(s) deleted", deleted)))
    }

    fn execute_update(
        &self,
        class: &str,
        assignments: &[(String, LiteralValue)],
        filter: &Option<FilterExpr>,
    ) -> Result<QueryResult> {
        let mut engine = self.engine.write().unwrap();

        let prefix = format!("{}::", class);
        let entries = engine.scan_prefix(prefix.as_bytes())?;

        let mut updated = 0usize;
        for (key, val_bytes) in entries {
            if let Ok(Value::Object(mut doc)) = serde_json::from_slice::<Value>(&val_bytes) {
                if doc.get("__class__").and_then(|v| v.as_str()) == Some(class) {
                    if self.matches_filter(&doc, filter) {
                        // Apply assignments
                        for (col, val) in assignments {
                            doc.insert(col.clone(), self.literal_to_json(val));
                        }
                        let new_value = serde_json::to_vec(&Value::Object(doc))
                            .map_err(|e| CoreError::Serialization(e.to_string()))?;
                        engine.put(key, new_value)?;
                        updated += 1;
                    }
                }
            }
        }

        Ok(QueryResult::Success(format!("{} row(s) updated", updated)))
    }

    fn execute_match(
        &self,
        _variable: &str,
        class: &str,
        filter: &Option<FilterExpr>,
        returns: &[String],
    ) -> Result<QueryResult> {
        let columns = if returns.is_empty() {
            SelectColumns::All
        } else {
            SelectColumns::Columns(
                returns
                    .iter()
                    .map(|r| SelectItem::Column(r.clone()))
                    .collect(),
            )
        };

        self.execute_select(&columns, class, None, &[], filter, None, &None, None)
    }

    fn generate_doc_key(&self, class: &str) -> Vec<u8> {
        // Simple key generation: use timestamp-based approach
        // TODO: proper auto-increment or UUID
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        format!("{}::{:020}", class, now).into_bytes()
    }

    fn matches_filter(&self, doc: &Map<String, Value>, filter: &Option<FilterExpr>) -> bool {
        match filter {
            None => true,
            Some(expr) => self.eval_filter(doc, expr),
        }
    }

    fn eval_filter(&self, doc: &Map<String, Value>, expr: &FilterExpr) -> bool {
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
            FilterExpr::And(left, right) => {
                self.eval_filter(doc, left) && self.eval_filter(doc, right)
            }
            FilterExpr::Or(left, right) => {
                self.eval_filter(doc, left) || self.eval_filter(doc, right)
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

    fn literal_to_json(&self, lit: &LiteralValue) -> Value {
        match lit {
            LiteralValue::Null => Value::Null,
            LiteralValue::Bool(b) => json!(b),
            LiteralValue::Int(i) => json!(i),
            LiteralValue::Float(f) => json!(f),
            LiteralValue::String(s) => json!(s),
        }
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
}
