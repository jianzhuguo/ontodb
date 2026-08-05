//! Query executor: runs parsed queries against the storage and ontology engines.

use crate::parser::{FilterExpr, LiteralValue, QueryAst, SelectColumns};
use onto_core::{CoreError, OntoValue, Result};
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
                filter,
                limit,
                ..
            } => self.execute_select(columns, from, filter, *limit),
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
        filter: &Option<FilterExpr>,
        limit: Option<usize>,
    ) -> Result<QueryResult> {
        let mut engine = self.engine.write().unwrap();

        // For now, scan all entries and filter by class
        // TODO: Use index for class-based lookups
        let mut rows: Vec<Map<String, Value>> = Vec::new();

        // This is a simplified implementation that reads from the MemTable only
        // A full implementation would also scan SSTables
        let prefix = format!("{}::", from);

        // Get all entries (simplified - just check MemTable for now)
        // In a real implementation, we'd iterate over MemTable + SSTables
        for i in 0..10000u64 {
            let key = format!("{}::{}", from, i);
            if let Some(val_bytes) = engine.get(key.as_bytes())? {
                if let Ok(Value::Object(doc)) = serde_json::from_slice::<Value>(&val_bytes) {
                    // Check class filter
                    if doc.get("__class__").and_then(|v| v.as_str()) == Some(from) {
                        if self.matches_filter(&doc, filter) {
                            rows.push(self.project_columns(&doc, columns));
                        }
                    }
                }
            } else {
                break;
            }

            if let Some(limit) = limit {
                if rows.len() >= limit {
                    break;
                }
            }
        }

        Ok(QueryResult::Rows(rows))
    }

    fn execute_delete(&self, class: &str, filter: &Option<FilterExpr>) -> Result<QueryResult> {
        // Simplified: delete matching documents
        // A full implementation would mark entries as tombstones
        Ok(QueryResult::Success(format!(
            "DELETE from {} (not yet fully implemented)",
            class
        )))
    }

    fn execute_update(
        &self,
        class: &str,
        _assignments: &[(String, LiteralValue)],
        _filter: &Option<FilterExpr>,
    ) -> Result<QueryResult> {
        Ok(QueryResult::Success(format!(
            "UPDATE {} (not yet fully implemented)",
            class
        )))
    }

    fn execute_match(
        &self,
        _variable: &str,
        class: &str,
        filter: &Option<FilterExpr>,
        returns: &[String],
    ) -> Result<QueryResult> {
        // MATCH is essentially a SELECT with semantic awareness
        let columns = if returns.is_empty() {
            SelectColumns::All
        } else {
            SelectColumns::Columns(returns.to_vec())
        };

        self.execute_select(&columns, class, filter, None)
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
            SelectColumns::Columns(cols) => {
                let mut result = Map::new();
                for col in cols {
                    if let Some(val) = doc.get(col) {
                        result.insert(col.clone(), val.clone());
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
