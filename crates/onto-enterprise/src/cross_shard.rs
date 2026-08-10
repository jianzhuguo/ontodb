//! Cross-shard query coordinator for OntoDB Enterprise.
//!
//! Handles:
//! - Fan-out queries to multiple shards
//! - Result aggregation (merge, union, count)
//! - Distributed aggregation (SUM, COUNT, AVG, MIN, MAX)
//! - Error handling for partial failures

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Aggregation function for cross-shard results.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Aggregation {
    /// No aggregation, just concatenate rows.
    None,
    /// Count rows.
    Count,
    /// Sum a numeric column.
    Sum(String),
    /// Average a numeric column.
    Avg(String),
    /// Minimum value.
    Min(String),
    /// Maximum value.
    Max(String),
    /// Group by a column and count.
    GroupByCount(String),
}

/// Cross-shard query plan.
#[derive(Debug, Clone)]
pub struct CrossShardPlan {
    /// Target shard IDs.
    pub shards: Vec<u32>,
    /// SQL query to execute on each shard.
    pub query: String,
    /// Aggregation to apply after gathering results.
    pub aggregation: Aggregation,
    /// ORDER BY column (if any).
    pub order_by: Option<String>,
    /// LIMIT (if any).
    pub limit: Option<usize>,
}

/// Result from a single shard.
#[derive(Debug, Clone)]
pub struct ShardResult {
    /// Shard ID.
    pub shard_id: u32,
    /// Query result rows.
    pub rows: Vec<HashMap<String, serde_json::Value>>,
    /// Error message (if query failed on this shard).
    pub error: Option<String>,
    /// Query execution time in milliseconds.
    pub elapsed_ms: f64,
}

/// Aggregated cross-shard result.
#[derive(Debug, Clone)]
pub struct CrossShardResult {
    /// Merged/aggregated rows.
    pub rows: Vec<HashMap<String, serde_json::Value>>,
    /// Total rows before aggregation.
    pub total_rows: usize,
    /// Shards that succeeded.
    pub succeeded_shards: Vec<u32>,
    /// Shards that failed.
    pub failed_shards: Vec<u32>,
    /// Total execution time in milliseconds.
    pub elapsed_ms: f64,
}

/// Cross-shard query coordinator.
pub struct CrossShardCoordinator {
    /// Shard results collected so far.
    results: Vec<ShardResult>,
}

impl CrossShardCoordinator {
    /// Create a new coordinator.
    pub fn new() -> Self {
        Self {
            results: Vec::new(),
        }
    }

    /// Add a shard result.
    pub fn add_result(&mut self, result: ShardResult) {
        self.results.push(result);
    }

    /// Aggregate results from all shards.
    pub fn aggregate(&self, plan: &CrossShardPlan) -> CrossShardResult {
        let mut succeeded = Vec::new();
        let mut failed = Vec::new();
        let mut all_rows = Vec::new();
        let mut total_elapsed: f64 = 0.0;

        for result in &self.results {
            if let Some(ref err) = result.error {
                tracing::warn!("Shard {} failed: {}", result.shard_id, err);
                failed.push(result.shard_id);
            } else {
                succeeded.push(result.shard_id);
                all_rows.extend(result.rows.clone());
            }
            total_elapsed = total_elapsed.max(result.elapsed_ms);
        }

        let total_rows = all_rows.len();

        // Apply aggregation
        let aggregated = match &plan.aggregation {
            Aggregation::None => all_rows,
            Aggregation::Count => {
                let count = all_rows.len();
                let mut row = HashMap::new();
                row.insert("count".to_string(), serde_json::Value::Number(count.into()));
                vec![row]
            }
            Aggregation::Sum(col) => {
                let sum: f64 = all_rows.iter()
                    .filter_map(|r| r.get(col.as_str()))
                    .filter_map(|v| v.as_f64())
                    .sum();
                let mut row = HashMap::new();
                row.insert(format!("sum_{}", col), serde_json::json!(sum));
                vec![row]
            }
            Aggregation::Avg(col) => {
                let values: Vec<f64> = all_rows.iter()
                    .filter_map(|r| r.get(col.as_str()))
                    .filter_map(|v| v.as_f64())
                    .collect();
                let avg = if values.is_empty() { 0.0 } else { values.iter().sum::<f64>() / values.len() as f64 };
                let mut row = HashMap::new();
                row.insert(format!("avg_{}", col), serde_json::json!(avg));
                vec![row]
            }
            Aggregation::Min(col) => {
                let min = all_rows.iter()
                    .filter_map(|r| r.get(col.as_str()))
                    .filter_map(|v| v.as_f64())
                    .reduce(|a, b| a.min(b));
                let mut row = HashMap::new();
                row.insert(format!("min_{}", col), serde_json::json!(min));
                vec![row]
            }
            Aggregation::Max(col) => {
                let max = all_rows.iter()
                    .filter_map(|r| r.get(col.as_str()))
                    .filter_map(|v| v.as_f64())
                    .reduce(|a, b| a.max(b));
                let mut row = HashMap::new();
                row.insert(format!("max_{}", col), serde_json::json!(max));
                vec![row]
            }
            Aggregation::GroupByCount(col) => {
                let mut groups: HashMap<String, usize> = HashMap::new();
                for row in &all_rows {
                    if let Some(val) = row.get(col.as_str()) {
                        let key = val.to_string();
                        *groups.entry(key).or_insert(0) += 1;
                    }
                }
                groups.into_iter().map(|(k, v)| {
                    let mut row = HashMap::new();
                    row.insert(col.clone(), serde_json::Value::String(k));
                    row.insert("count".to_string(), serde_json::Value::Number(v.into()));
                    row
                }).collect()
            }
        };

        CrossShardResult {
            rows: aggregated,
            total_rows,
            succeeded_shards: succeeded,
            failed_shards: failed,
            elapsed_ms: total_elapsed,
        }
    }

    /// Get the number of results collected.
    pub fn result_count(&self) -> usize {
        self.results.len()
    }
}

impl Default for CrossShardCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_result(shard_id: u32, values: Vec<i64>) -> ShardResult {
        ShardResult {
            shard_id,
            rows: values.into_iter().map(|v| {
                let mut row = HashMap::new();
                row.insert("value".to_string(), serde_json::json!(v));
                row
            }).collect(),
            error: None,
            elapsed_ms: 10.0,
        }
    }

    #[test]
    fn test_aggregate_none() {
        let mut coord = CrossShardCoordinator::new();
        coord.add_result(make_result(0, vec![1, 2, 3]));
        coord.add_result(make_result(1, vec![4, 5]));

        let plan = CrossShardPlan {
            shards: vec![0, 1],
            query: "SELECT *".to_string(),
            aggregation: Aggregation::None,
            order_by: None,
            limit: None,
        };

        let result = coord.aggregate(&plan);
        assert_eq!(result.rows.len(), 5);
        assert_eq!(result.total_rows, 5);
        assert_eq!(result.succeeded_shards.len(), 2);
        assert_eq!(result.failed_shards.len(), 0);
    }

    #[test]
    fn test_aggregate_count() {
        let mut coord = CrossShardCoordinator::new();
        coord.add_result(make_result(0, vec![1, 2, 3]));
        coord.add_result(make_result(1, vec![4, 5]));

        let plan = CrossShardPlan {
            shards: vec![0, 1],
            query: "SELECT COUNT(*)".to_string(),
            aggregation: Aggregation::Count,
            order_by: None,
            limit: None,
        };

        let result = coord.aggregate(&plan);
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0].get("count").unwrap().as_i64().unwrap(), 5);
    }

    #[test]
    fn test_aggregate_sum() {
        let mut coord = CrossShardCoordinator::new();
        coord.add_result(make_result(0, vec![10, 20]));
        coord.add_result(make_result(1, vec![30, 40]));

        let plan = CrossShardPlan {
            shards: vec![0, 1],
            query: "SELECT SUM(value)".to_string(),
            aggregation: Aggregation::Sum("value".to_string()),
            order_by: None,
            limit: None,
        };

        let result = coord.aggregate(&plan);
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0].get("sum_value").unwrap().as_f64().unwrap(), 100.0);
    }

    #[test]
    fn test_aggregate_avg() {
        let mut coord = CrossShardCoordinator::new();
        coord.add_result(make_result(0, vec![10, 20]));
        coord.add_result(make_result(1, vec![30, 40]));

        let plan = CrossShardPlan {
            shards: vec![0, 1],
            query: "SELECT AVG(value)".to_string(),
            aggregation: Aggregation::Avg("value".to_string()),
            order_by: None,
            limit: None,
        };

        let result = coord.aggregate(&plan);
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0].get("avg_value").unwrap().as_f64().unwrap(), 25.0);
    }

    #[test]
    fn test_aggregate_min_max() {
        let mut coord = CrossShardCoordinator::new();
        coord.add_result(make_result(0, vec![5, 10]));
        coord.add_result(make_result(1, vec![3, 8]));

        let plan_min = CrossShardPlan {
            shards: vec![0, 1],
            query: "".to_string(),
            aggregation: Aggregation::Min("value".to_string()),
            order_by: None,
            limit: None,
        };
        let result = coord.aggregate(&plan_min);
        assert_eq!(result.rows[0].get("min_value").unwrap().as_f64().unwrap(), 3.0);

        let plan_max = CrossShardPlan {
            shards: vec![0, 1],
            query: "".to_string(),
            aggregation: Aggregation::Max("value".to_string()),
            order_by: None,
            limit: None,
        };
        let result = coord.aggregate(&plan_max);
        assert_eq!(result.rows[0].get("max_value").unwrap().as_f64().unwrap(), 10.0);
    }

    #[test]
    fn test_partial_failure() {
        let mut coord = CrossShardCoordinator::new();
        coord.add_result(make_result(0, vec![1, 2]));
        coord.add_result(ShardResult {
            shard_id: 1,
            rows: vec![],
            error: Some("connection timeout".to_string()),
            elapsed_ms: 5000.0,
        });

        let plan = CrossShardPlan {
            shards: vec![0, 1],
            query: "".to_string(),
            aggregation: Aggregation::None,
            order_by: None,
            limit: None,
        };

        let result = coord.aggregate(&plan);
        assert_eq!(result.succeeded_shards, vec![0]);
        assert_eq!(result.failed_shards, vec![1]);
        assert_eq!(result.rows.len(), 2); // Only shard 0 results
    }
}
