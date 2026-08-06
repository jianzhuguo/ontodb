//! Cost model for query optimization.
//!
//! Estimates the cost of different execution strategies based on
//! table statistics, index availability, and selectivity estimates.

use serde::{Deserialize, Serialize};

/// Statistics about a table/class for cost estimation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableStats {
    /// Estimated number of rows in the table.
    pub row_count: u64,
    /// Estimated average row size in bytes.
    pub avg_row_size: u64,
    /// Number of data blocks/pages.
    pub block_count: u64,
    /// Whether the table has a primary key index.
    pub has_primary_index: bool,
    /// Available secondary indexes: column name -> estimated cardinality.
    pub secondary_indexes: Vec<IndexStats>,
    /// Available vector indexes: column name -> config.
    pub vector_indexes: Vec<VectorIndexStats>,
}

/// Statistics about a secondary index.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexStats {
    /// Column name.
    pub column: String,
    /// Estimated number of distinct values (cardinality).
    pub cardinality: u64,
    /// Whether the index is sorted (B+Tree) or hash-based.
    pub is_sorted: bool,
    /// Estimated height of the B+Tree.
    pub tree_height: u32,
}

/// Statistics about a vector index.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorIndexStats {
    /// Column name.
    pub column: String,
    /// Vector dimension.
    pub dimension: usize,
    /// Number of vectors indexed.
    pub vector_count: u64,
    /// HNSW graph layers.
    pub layers: u32,
}

/// Cost estimate for a query plan node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostEstimate {
    /// Estimated number of output rows.
    pub rows: u64,
    /// Estimated I/O cost (page reads).
    pub io_cost: f64,
    /// Estimated CPU cost (comparisons, computations).
    pub cpu_cost: f64,
    /// Total cost (weighted sum).
    pub total_cost: f64,
    /// Whether this plan uses an index.
    pub uses_index: bool,
    /// Whether this plan is sorted (no sort needed downstream).
    pub is_sorted: bool,
}

impl CostEstimate {
    /// Create a new cost estimate.
    pub fn new(rows: u64, io_cost: f64, cpu_cost: f64) -> Self {
        Self {
            rows,
            io_cost,
            cpu_cost,
            total_cost: io_cost + cpu_cost,
            uses_index: false,
            is_sorted: false,
        }
    }

    /// Create a zero-cost estimate (for empty results).
    pub fn zero() -> Self {
        Self {
            rows: 0,
            io_cost: 0.0,
            cpu_cost: 0.0,
            total_cost: 0.0,
            uses_index: false,
            is_sorted: false,
        }
    }

    /// Mark this plan as using an index.
    pub fn with_index(mut self) -> Self {
        self.uses_index = true;
        self
    }

    /// Mark this plan as producing sorted output.
    pub fn with_sorted(mut self) -> Self {
        self.is_sorted = true;
        self
    }
}

/// Cost model for estimating query execution costs.
pub struct CostModel {
    /// Cost of a sequential scan per row (CPU).
    pub seq_scan_cpu_per_row: f64,
    /// Cost of an index lookup (I/O).
    pub index_lookup_io: f64,
    /// Cost of an index scan per row (CPU).
    pub index_scan_cpu_per_row: f64,
    /// Cost of a hash probe (CPU).
    pub hash_probe_cpu: f64,
    /// Cost of a comparison (CPU).
    pub compare_cpu: f64,
    /// Cost of reading a page (I/O).
    pub page_read_io: f64,
    /// Cost of a vector search per candidate (CPU).
    pub vector_search_cpu_per_candidate: f64,
    /// Cost of distance computation (CPU).
    pub distance_compute_cpu: f64,
    /// Selectivity for equality predicates (default: 1/cardinality).
    pub eq_selectivity: f64,
    /// Selectivity for range predicates (default: 1/3).
    pub range_selectivity: f64,
    /// Selectivity for LIKE predicates (default: 1/10).
    pub like_selectivity: f64,
    /// Selectivity for IN predicates (per value).
    pub in_selectivity_per_value: f64,
}

impl Default for CostModel {
    fn default() -> Self {
        Self {
            seq_scan_cpu_per_row: 0.01,
            index_lookup_io: 1.0,
            index_scan_cpu_per_row: 0.005,
            hash_probe_cpu: 0.001,
            compare_cpu: 0.001,
            page_read_io: 1.0,
            vector_search_cpu_per_candidate: 0.1,
            distance_compute_cpu: 0.05,
            eq_selectivity: 0.1,       // 10% by default
            range_selectivity: 0.333,   // 1/3 by default
            like_selectivity: 0.1,      // 10% by default
            in_selectivity_per_value: 0.05, // 5% per value
        }
    }
}

impl CostModel {
    /// Create a new cost model with default parameters.
    pub fn new() -> Self {
        Self::default()
    }

    /// Estimate the cost of a sequential (full table) scan.
    pub fn seq_scan_cost(&self, stats: &TableStats) -> CostEstimate {
        let rows = stats.row_count;
        let io_cost = stats.block_count as f64 * self.page_read_io;
        let cpu_cost = rows as f64 * self.seq_scan_cpu_per_row;
        CostEstimate::new(rows, io_cost, cpu_cost)
    }

    /// Estimate the cost of an index-based lookup (point query).
    pub fn index_lookup_cost(&self, stats: &TableStats, index: &IndexStats) -> CostEstimate {
        let io_cost = index.tree_height as f64 * self.index_lookup_io;
        let cpu_cost = index.tree_height as f64 * self.compare_cpu;
        CostEstimate::new(1, io_cost, cpu_cost)
            .with_index()
            .with_sorted()
    }

    /// Estimate the cost of an index range scan.
    pub fn index_range_scan_cost(
        &self,
        stats: &TableStats,
        index: &IndexStats,
        selectivity: f64,
    ) -> CostEstimate {
        let estimated_rows = (stats.row_count as f64 * selectivity) as u64;
        let io_cost = index.tree_height as f64 * self.index_lookup_io
            + (estimated_rows as f64 / 100.0) * self.page_read_io; // Assume 100 rows per page
        let cpu_cost = estimated_rows as f64 * self.index_scan_cpu_per_row;
        CostEstimate::new(estimated_rows, io_cost, cpu_cost)
            .with_index()
            .with_sorted()
    }

    /// Estimate the cost of a filter (WHERE clause) application.
    pub fn filter_cost(&self, input_rows: u64, filter: &FilterSelectivity) -> CostEstimate {
        let output_rows = (input_rows as f64 * filter.selectivity) as u64;
        let cpu_cost = input_rows as f64 * self.compare_cpu;
        CostEstimate::new(output_rows, 0.0, cpu_cost)
    }

    /// Estimate the cost of a nested loop join.
    pub fn nested_loop_join_cost(
        &self,
        left: &CostEstimate,
        right: &CostEstimate,
    ) -> CostEstimate {
        let rows = left.rows * right.rows;
        let io_cost = left.io_cost + right.io_cost + (left.rows as f64 * right.io_cost);
        let cpu_cost = rows as f64 * self.compare_cpu;
        CostEstimate::new(rows, io_cost, cpu_cost)
    }

    /// Estimate the cost of a hash join.
    pub fn hash_join_cost(
        &self,
        left: &CostEstimate,
        right: &CostEstimate,
    ) -> CostEstimate {
        // Build hash table on smaller side
        let build_cost = std::cmp::min(left.rows, right.rows) as f64 * self.hash_probe_cpu;
        let probe_cost = std::cmp::max(left.rows, right.rows) as f64 * self.hash_probe_cpu;
        let rows = (left.rows as f64 * right.rows as f64 * 0.1) as u64; // Assume 10% join selectivity
        let io_cost = left.io_cost + right.io_cost;
        let cpu_cost = build_cost + probe_cost;
        CostEstimate::new(rows, io_cost, cpu_cost)
    }

    /// Estimate the cost of a sort operation.
    pub fn sort_cost(&self, input: &CostEstimate) -> CostEstimate {
        let n = input.rows as f64;
        let cpu_cost = if n > 0.0 { n * n.log2() * self.compare_cpu } else { 0.0 };
        CostEstimate::new(input.rows, input.io_cost, input.cpu_cost + cpu_cost)
            .with_sorted()
    }

    /// Estimate the cost of an aggregation (GROUP BY).
    pub fn aggregation_cost(&self, input: &CostEstimate) -> CostEstimate {
        let cpu_cost = input.rows as f64 * self.hash_probe_cpu; // Hash-based grouping
        CostEstimate::new(input.rows, input.io_cost, input.cpu_cost + cpu_cost)
    }

    /// Estimate the cost of a vector search.
    pub fn vector_search_cost(
        &self,
        stats: &TableStats,
        vector_stats: &VectorIndexStats,
        top_k: u64,
    ) -> CostEstimate {
        // HNSW search cost: O(log n) candidates explored
        let candidates = (vector_stats.vector_count as f64).log2() as u64 * 10;
        let io_cost = vector_stats.layers as f64 * self.page_read_io;
        let cpu_cost = candidates as f64 * self.vector_search_cpu_per_candidate
            + candidates as f64 * self.distance_compute_cpu;
        CostEstimate::new(top_k.min(vector_stats.vector_count), io_cost, cpu_cost)
            .with_index()
    }

    /// Estimate selectivity for a filter expression.
    pub fn estimate_selectivity(
        &self,
        stats: &TableStats,
        filter: &crate::parser::FilterExpr,
    ) -> FilterSelectivity {
        match filter {
            crate::parser::FilterExpr::Eq(col, _) => {
                // Try to use index cardinality for better estimate
                if let Some(index) = stats.secondary_indexes.iter().find(|i| &i.column == col) {
                    FilterSelectivity {
                        selectivity: 1.0 / index.cardinality as f64,
                        can_use_index: true,
                        index_column: Some(col.clone()),
                    }
                } else {
                    FilterSelectivity {
                        selectivity: self.eq_selectivity,
                        can_use_index: false,
                        index_column: None,
                    }
                }
            }
            crate::parser::FilterExpr::Ne(_, _) => {
                FilterSelectivity {
                    selectivity: 1.0 - self.eq_selectivity,
                    can_use_index: false,
                    index_column: None,
                }
            }
            crate::parser::FilterExpr::Gt(col, _)
            | crate::parser::FilterExpr::Lt(col, _)
            | crate::parser::FilterExpr::Gte(col, _)
            | crate::parser::FilterExpr::Lte(col, _) => {
                let can_use_index = stats
                    .secondary_indexes
                    .iter()
                    .any(|i| &i.column == col);
                FilterSelectivity {
                    selectivity: self.range_selectivity,
                    can_use_index,
                    index_column: if can_use_index { Some(col.clone()) } else { None },
                }
            }
            crate::parser::FilterExpr::Like(_, _) => {
                FilterSelectivity {
                    selectivity: self.like_selectivity,
                    can_use_index: false,
                    index_column: None,
                }
            }
            crate::parser::FilterExpr::Between(_, _, _) => {
                FilterSelectivity {
                    selectivity: self.range_selectivity * 0.5, // BETWEEN is more selective
                    can_use_index: false,
                    index_column: None,
                }
            }
            crate::parser::FilterExpr::In(_, values) => {
                FilterSelectivity {
                    selectivity: (values.len() as f64 * self.in_selectivity_per_value).min(1.0),
                    can_use_index: false,
                    index_column: None,
                }
            }
            crate::parser::FilterExpr::InSubquery(_, _) => {
                FilterSelectivity {
                    selectivity: 0.1, // Conservative estimate
                    can_use_index: false,
                    index_column: None,
                }
            }
            crate::parser::FilterExpr::And(left, right) => {
                let l = self.estimate_selectivity(stats, left);
                let r = self.estimate_selectivity(stats, right);
                FilterSelectivity {
                    selectivity: l.selectivity * r.selectivity, // Independence assumption
                    can_use_index: l.can_use_index || r.can_use_index,
                    index_column: l.index_column.or(r.index_column),
                }
            }
            crate::parser::FilterExpr::Or(left, right) => {
                let l = self.estimate_selectivity(stats, left);
                let r = self.estimate_selectivity(stats, right);
                FilterSelectivity {
                    selectivity: l.selectivity + r.selectivity - l.selectivity * r.selectivity,
                    can_use_index: l.can_use_index && r.can_use_index, // Both must use index
                    index_column: None, // OR typically can't use single index
                }
            }
        }
    }
}

/// Selectivity estimate for a filter expression.
#[derive(Debug, Clone)]
pub struct FilterSelectivity {
    /// Estimated selectivity (0.0 to 1.0).
    pub selectivity: f64,
    /// Whether this filter can use an index.
    pub can_use_index: bool,
    /// The column that can use an index (if any).
    pub index_column: Option<String>,
}
