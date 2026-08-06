//! Query planner: converts AST to physical execution plans.
//!
//! Generates multiple candidate plans and selects the lowest-cost one.

use super::cost::{CostEstimate, CostModel, FilterSelectivity, IndexStats, TableStats};
use crate::parser::{FilterExpr, JoinClause, OrderBy, QueryAst, SelectColumns};
use onto_core::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A node in the execution plan tree.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PlanNode {
    /// Sequential scan over a table.
    SeqScan {
        table: String,
        alias: Option<String>,
        filter: Option<FilterExpr>,
        estimated_rows: u64,
    },

    /// Index-based scan.
    IndexScan {
        table: String,
        alias: Option<String>,
        index_column: String,
        filter: Option<FilterExpr>,
        estimated_rows: u64,
    },

    /// Index lookup (point query).
    IndexLookup {
        table: String,
        alias: Option<String>,
        index_column: String,
        key: crate::parser::LiteralValue,
        estimated_rows: u64,
    },

    /// Vector similarity search.
    VectorSearch {
        table: String,
        column: String,
        query_vector: Vec<f32>,
        top_k: usize,
        filter: Option<FilterExpr>,
        estimated_rows: u64,
    },

    /// Filter (WHERE clause).
    Filter {
        input: Box<PlanNode>,
        predicate: FilterExpr,
        estimated_rows: u64,
    },

    /// Projection (SELECT columns).
    Projection {
        input: Box<PlanNode>,
        columns: SelectColumns,
        estimated_rows: u64,
    },

    /// Nested loop join.
    NestedLoopJoin {
        left: Box<PlanNode>,
        right: Box<PlanNode>,
        join_clause: JoinClause,
        estimated_rows: u64,
    },

    /// Hash join.
    HashJoin {
        left: Box<PlanNode>,
        right: Box<PlanNode>,
        join_clause: JoinClause,
        estimated_rows: u64,
    },

    /// Sort.
    Sort {
        input: Box<PlanNode>,
        order_by: OrderBy,
        estimated_rows: u64,
    },

    /// Aggregation (GROUP BY).
    Aggregation {
        input: Box<PlanNode>,
        group_by: Vec<String>,
        aggregates: Vec<AggregateFunc>,
        estimated_rows: u64,
    },

    /// Limit.
    Limit {
        input: Box<PlanNode>,
        count: usize,
        estimated_rows: u64,
    },

    /// Union of two plans.
    Union {
        left: Box<PlanNode>,
        right: Box<PlanNode>,
        all: bool,
        estimated_rows: u64,
    },
}

/// Aggregate function in execution plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AggregateFunc {
    pub func: crate::parser::AggregateFunc,
    pub arg: String,
    pub alias: Option<String>,
}

/// A complete execution plan with cost estimate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionPlan {
    /// The root plan node.
    pub root: PlanNode,
    /// Cost estimate for this plan.
    pub cost: CostEstimate,
    /// Whether this plan uses an index.
    pub uses_index: bool,
    /// Whether this plan produces sorted output.
    pub is_sorted: bool,
}

impl ExecutionPlan {
    /// Create a new execution plan.
    pub fn new(root: PlanNode, cost: CostEstimate) -> Self {
        Self {
            uses_index: cost.uses_index,
            is_sorted: cost.is_sorted,
            root,
            cost,
        }
    }

    /// Get a human-readable description of the plan.
    pub fn describe(&self) -> String {
        let mut output = String::new();
        self.describe_node(&self.root, 0, &mut output);
        output.push_str(&format!(
            "\nTotal Cost: {:.2} | Rows: {} | Index: {} | Sorted: {}",
            self.cost.total_cost, self.cost.rows, self.uses_index, self.is_sorted
        ));
        output
    }

    fn describe_node(&self, node: &PlanNode, depth: usize, output: &mut String) {
        let indent = "  ".repeat(depth);
        match node {
            PlanNode::SeqScan { table, alias, estimated_rows, .. } => {
                output.push_str(&format!(
                    "{}SeqScan on {}{} (rows: {})\n",
                    indent,
                    table,
                    alias.as_deref().map(|a| format!(" AS {}", a)).unwrap_or_default(),
                    estimated_rows
                ));
            }
            PlanNode::IndexScan { table, index_column, estimated_rows, .. } => {
                output.push_str(&format!(
                    "{}IndexScan on {} using {} (rows: {})\n",
                    indent, table, index_column, estimated_rows
                ));
            }
            PlanNode::IndexLookup { table, index_column, estimated_rows, .. } => {
                output.push_str(&format!(
                    "{}IndexLookup on {} using {} (rows: {})\n",
                    indent, table, index_column, estimated_rows
                ));
            }
            PlanNode::VectorSearch { table, column, top_k, estimated_rows, .. } => {
                output.push_str(&format!(
                    "{}VectorSearch on {}.{} top {} (rows: {})\n",
                    indent, table, column, top_k, estimated_rows
                ));
            }
            PlanNode::Filter { input, estimated_rows, .. } => {
                output.push_str(&format!("{}Filter (rows: {})\n", indent, estimated_rows));
                self.describe_node(input, depth + 1, output);
            }
            PlanNode::Projection { input, estimated_rows, .. } => {
                output.push_str(&format!("{}Projection (rows: {})\n", indent, estimated_rows));
                self.describe_node(input, depth + 1, output);
            }
            PlanNode::NestedLoopJoin { left, right, estimated_rows, .. } => {
                output.push_str(&format!("{}NestedLoopJoin (rows: {})\n", indent, estimated_rows));
                self.describe_node(left, depth + 1, output);
                self.describe_node(right, depth + 1, output);
            }
            PlanNode::HashJoin { left, right, estimated_rows, .. } => {
                output.push_str(&format!("{}HashJoin (rows: {})\n", indent, estimated_rows));
                self.describe_node(left, depth + 1, output);
                self.describe_node(right, depth + 1, output);
            }
            PlanNode::Sort { input, order_by, estimated_rows, .. } => {
                output.push_str(&format!(
                    "{}Sort by {} {} (rows: {})\n",
                    indent,
                    order_by.column,
                    if order_by.ascending { "ASC" } else { "DESC" },
                    estimated_rows
                ));
                self.describe_node(input, depth + 1, output);
            }
            PlanNode::Aggregation { input, group_by, estimated_rows, .. } => {
                output.push_str(&format!(
                    "{}Aggregation GROUP BY {} (rows: {})\n",
                    indent,
                    group_by.join(", "),
                    estimated_rows
                ));
                self.describe_node(input, depth + 1, output);
            }
            PlanNode::Limit { input, count, estimated_rows, .. } => {
                output.push_str(&format!(
                    "{}Limit {} (rows: {})\n",
                    indent, count, estimated_rows
                ));
                self.describe_node(input, depth + 1, output);
            }
            PlanNode::Union { left, right, all, estimated_rows, .. } => {
                output.push_str(&format!(
                    "{}Union {} (rows: {})\n",
                    indent,
                    if *all { "ALL" } else { "DISTINCT" },
                    estimated_rows
                ));
                self.describe_node(left, depth + 1, output);
                self.describe_node(right, depth + 1, output);
            }
        }
    }
}

/// Query planner that generates and selects execution plans.
pub struct QueryPlanner {
    /// Cost model for estimating plan costs.
    cost_model: CostModel,
    /// Table statistics cache.
    stats: HashMap<String, TableStats>,
}

impl QueryPlanner {
    /// Create a new query planner.
    pub fn new() -> Self {
        Self {
            cost_model: CostModel::new(),
            stats: HashMap::new(),
        }
    }

    /// Create a planner with a custom cost model.
    pub fn with_cost_model(cost_model: CostModel) -> Self {
        Self {
            cost_model,
            stats: HashMap::new(),
        }
    }

    /// Update table statistics.
    pub fn update_stats(&mut self, table: String, stats: TableStats) {
        self.stats.insert(table, stats);
    }

    /// Get the cost model.
    pub fn cost_model(&self) -> &CostModel {
        &self.cost_model
    }

    /// Plan a query from an AST.
    /// Returns the lowest-cost execution plan.
    pub fn plan(&self, ast: &QueryAst) -> Result<ExecutionPlan> {
        match ast {
            QueryAst::Select {
                distinct: _,
                columns,
                from,
                from_alias,
                joins,
                filter,
                group_by,
                having,
                order_by,
                limit,
            } => self.plan_select(
                columns,
                from,
                from_alias.as_deref(),
                joins,
                filter,
                group_by.as_ref(),
                having,
                order_by.as_ref(),
                *limit,
            ),
            QueryAst::VectorSearch {
                class,
                column,
                query_vector,
                top_k,
                filter,
            } => self.plan_vector_search(class, column, query_vector, *top_k, filter),
            QueryAst::Union { left, right, all } => self.plan_union(left, right, *all),
            _ => {
                // For non-SELECT queries, use a simple passthrough plan
                Ok(ExecutionPlan::new(
                    PlanNode::SeqScan {
                        table: "unknown".to_string(),
                        alias: None,
                        filter: None,
                        estimated_rows: 0,
                    },
                    CostEstimate::zero(),
                ))
            }
        }
    }

    /// Plan a SELECT query.
    fn plan_select(
        &self,
        columns: &SelectColumns,
        from: &str,
        from_alias: Option<&str>,
        joins: &[JoinClause],
        filter: &Option<FilterExpr>,
        group_by: Option<&crate::parser::GroupByClause>,
        having: &Option<FilterExpr>,
        order_by: Option<&OrderBy>,
        limit: Option<usize>,
    ) -> Result<ExecutionPlan> {
        let stats = self.stats.get(from).cloned().unwrap_or_else(|| TableStats {
            row_count: 1000,
            avg_row_size: 100,
            block_count: 10,
            has_primary_index: false,
            secondary_indexes: Vec::new(),
            vector_indexes: Vec::new(),
        });

        // Generate candidate plans
        let mut candidates = Vec::new();

        // Candidate 1: Sequential scan
        let seq_plan = self.plan_seq_scan(from, from_alias, &stats, filter, joins, group_by, having, order_by, limit, columns);
        candidates.push(seq_plan);

        // Candidate 2: Index scan (if applicable)
        if let Some(filter_expr) = filter {
            let selectivity = self.cost_model.estimate_selectivity(&stats, filter_expr);
            if selectivity.can_use_index {
                if let Some(index_col) = &selectivity.index_column {
                    if let Some(index) = stats.secondary_indexes.iter().find(|i| &i.column == index_col) {
                        let index_plan = self.plan_index_scan(
                            from,
                            from_alias,
                            &stats,
                            index,
                            filter,
                            joins,
                            group_by,
                            having,
                            order_by,
                            limit,
                            columns,
                        );
                        candidates.push(index_plan);
                    }
                }
            }
        }

        // Select the best plan
        candidates.sort_by(|a, b| a.cost.total_cost.partial_cmp(&b.cost.total_cost).unwrap_or(std::cmp::Ordering::Equal));
        Ok(candidates.into_iter().next().unwrap())
    }

    /// Create a sequential scan plan.
    fn plan_seq_scan(
        &self,
        table: &str,
        alias: Option<&str>,
        stats: &TableStats,
        filter: &Option<FilterExpr>,
        joins: &[JoinClause],
        group_by: Option<&crate::parser::GroupByClause>,
        having: &Option<FilterExpr>,
        order_by: Option<&OrderBy>,
        limit: Option<usize>,
        columns: &SelectColumns,
    ) -> ExecutionPlan {
        // Start with seq scan
        let scan_cost = self.cost_model.seq_scan_cost(stats);
        let mut current_node = PlanNode::SeqScan {
            table: table.to_string(),
            alias: alias.map(|s| s.to_string()),
            filter: None, // Filter applied separately
            estimated_rows: scan_cost.rows,
        };
        let mut current_cost = scan_cost;

        // Apply filter
        if let Some(filter_expr) = filter {
            let filter_selectivity = self.cost_model.estimate_selectivity(stats, filter_expr);
            let filter_cost = self.cost_model.filter_cost(current_cost.rows, &filter_selectivity);
            current_node = PlanNode::Filter {
                input: Box::new(current_node),
                predicate: filter_expr.clone(),
                estimated_rows: filter_cost.rows,
            };
            current_cost = CostEstimate::new(
                filter_cost.rows,
                current_cost.io_cost,
                current_cost.cpu_cost + filter_cost.cpu_cost,
            );
        }

        // Apply joins
        for join in joins {
            let right_stats = self.stats.get(&join.table).cloned().unwrap_or_else(|| TableStats {
                row_count: 100,
                avg_row_size: 100,
                block_count: 1,
                has_primary_index: false,
                secondary_indexes: Vec::new(),
                vector_indexes: Vec::new(),
            });
            let right_cost = self.cost_model.seq_scan_cost(&right_stats);
            let join_cost = self.cost_model.nested_loop_join_cost(&current_cost, &right_cost);

            current_node = PlanNode::NestedLoopJoin {
                left: Box::new(current_node),
                right: Box::new(PlanNode::SeqScan {
                    table: join.table.clone(),
                    alias: join.alias.clone(),
                    filter: None,
                    estimated_rows: right_cost.rows,
                }),
                join_clause: join.clone(),
                estimated_rows: join_cost.rows,
            };
            current_cost = join_cost;
        }

        // Apply aggregation
        if let Some(gb) = group_by {
            let agg_cost = self.cost_model.aggregation_cost(&current_cost);
            current_node = PlanNode::Aggregation {
                input: Box::new(current_node),
                group_by: gb.columns.clone(),
                aggregates: Vec::new(), // TODO: extract from columns
                estimated_rows: agg_cost.rows,
            };
            current_cost = agg_cost;
        }

        // Apply sort
        if let Some(ob) = order_by {
            let sort_cost = self.cost_model.sort_cost(&current_cost);
            current_node = PlanNode::Sort {
                input: Box::new(current_node),
                order_by: ob.clone(),
                estimated_rows: sort_cost.rows,
            };
            current_cost = sort_cost;
            current_cost.is_sorted = true;
        }

        // Apply limit
        if let Some(limit_count) = limit {
            let limited_rows = current_cost.rows.min(limit_count as u64);
            current_node = PlanNode::Limit {
                input: Box::new(current_node),
                count: limit_count,
                estimated_rows: limited_rows,
            };
            current_cost = CostEstimate::new(limited_rows, current_cost.io_cost, current_cost.cpu_cost);
        }

        // Apply projection
        current_node = PlanNode::Projection {
            input: Box::new(current_node),
            columns: columns.clone(),
            estimated_rows: current_cost.rows,
        };

        ExecutionPlan::new(current_node, current_cost)
    }

    /// Create an index scan plan.
    fn plan_index_scan(
        &self,
        table: &str,
        alias: Option<&str>,
        stats: &TableStats,
        index: &IndexStats,
        filter: &Option<FilterExpr>,
        joins: &[JoinClause],
        group_by: Option<&crate::parser::GroupByClause>,
        having: &Option<FilterExpr>,
        order_by: Option<&OrderBy>,
        limit: Option<usize>,
        columns: &SelectColumns,
    ) -> ExecutionPlan {
        let filter_selectivity = filter
            .as_ref()
            .map(|f| self.cost_model.estimate_selectivity(stats, f))
            .unwrap_or(FilterSelectivity {
                selectivity: 1.0,
                can_use_index: false,
                index_column: None,
            });

        let index_cost = self.cost_model.index_range_scan_cost(stats, index, filter_selectivity.selectivity);
        let mut current_node = PlanNode::IndexScan {
            table: table.to_string(),
            alias: alias.map(|s| s.to_string()),
            index_column: index.column.clone(),
            filter: None,
            estimated_rows: index_cost.rows,
        };
        let mut current_cost = index_cost;

        // Apply joins
        for join in joins {
            let right_stats = self.stats.get(&join.table).cloned().unwrap_or_else(|| TableStats {
                row_count: 100,
                avg_row_size: 100,
                block_count: 1,
                has_primary_index: false,
                secondary_indexes: Vec::new(),
                vector_indexes: Vec::new(),
            });
            let right_cost = self.cost_model.seq_scan_cost(&right_stats);
            let join_cost = self.cost_model.nested_loop_join_cost(&current_cost, &right_cost);

            current_node = PlanNode::NestedLoopJoin {
                left: Box::new(current_node),
                right: Box::new(PlanNode::SeqScan {
                    table: join.table.clone(),
                    alias: join.alias.clone(),
                    filter: None,
                    estimated_rows: right_cost.rows,
                }),
                join_clause: join.clone(),
                estimated_rows: join_cost.rows,
            };
            current_cost = join_cost;
        }

        // Apply aggregation
        if let Some(gb) = group_by {
            let agg_cost = self.cost_model.aggregation_cost(&current_cost);
            current_node = PlanNode::Aggregation {
                input: Box::new(current_node),
                group_by: gb.columns.clone(),
                aggregates: Vec::new(),
                estimated_rows: agg_cost.rows,
            };
            current_cost = agg_cost;
        }

        // Index scan is already sorted, so no need for sort unless ORDER BY is on different column
        if let Some(ob) = order_by {
            if ob.column != index.column {
                let sort_cost = self.cost_model.sort_cost(&current_cost);
                current_node = PlanNode::Sort {
                    input: Box::new(current_node),
                    order_by: ob.clone(),
                    estimated_rows: sort_cost.rows,
                };
                current_cost = sort_cost;
            }
        }

        // Apply limit
        if let Some(limit_count) = limit {
            let limited_rows = current_cost.rows.min(limit_count as u64);
            current_node = PlanNode::Limit {
                input: Box::new(current_node),
                count: limit_count,
                estimated_rows: limited_rows,
            };
            current_cost = CostEstimate::new(limited_rows, current_cost.io_cost, current_cost.cpu_cost);
        }

        // Apply projection
        current_node = PlanNode::Projection {
            input: Box::new(current_node),
            columns: columns.clone(),
            estimated_rows: current_cost.rows,
        };

        ExecutionPlan::new(current_node, current_cost).with_index()
    }

    /// Plan a vector search query.
    fn plan_vector_search(
        &self,
        table: &str,
        column: &str,
        query_vector: &[f32],
        top_k: usize,
        filter: &Option<FilterExpr>,
    ) -> Result<ExecutionPlan> {
        let stats = self.stats.get(table).cloned().unwrap_or_else(|| TableStats {
            row_count: 1000,
            avg_row_size: 100,
            block_count: 10,
            has_primary_index: false,
            secondary_indexes: Vec::new(),
            vector_indexes: Vec::new(),
        });

        let vector_stats = stats
            .vector_indexes
            .iter()
            .find(|v| v.column == column)
            .cloned()
            .unwrap_or_else(|| super::cost::VectorIndexStats {
                column: column.to_string(),
                dimension: query_vector.len(),
                vector_count: stats.row_count,
                layers: 4,
            });

        let cost = self.cost_model.vector_search_cost(&stats, &vector_stats, top_k as u64);

        let node = PlanNode::VectorSearch {
            table: table.to_string(),
            column: column.to_string(),
            query_vector: query_vector.to_vec(),
            top_k,
            filter: filter.clone(),
            estimated_rows: cost.rows,
        };

        Ok(ExecutionPlan::new(node, cost).with_index())
    }

    /// Plan a UNION query.
    fn plan_union(
        &self,
        left: &QueryAst,
        right: &QueryAst,
        all: bool,
    ) -> Result<ExecutionPlan> {
        let left_plan = self.plan(left)?;
        let right_plan = self.plan(right)?;

        let total_rows = left_plan.cost.rows + right_plan.cost.rows;
        let total_cost = CostEstimate::new(
            total_rows,
            left_plan.cost.io_cost + right_plan.cost.io_cost,
            left_plan.cost.cpu_cost + right_plan.cost.cpu_cost,
        );

        let node = PlanNode::Union {
            left: Box::new(left_plan.root),
            right: Box::new(right_plan.root),
            all,
            estimated_rows: total_rows,
        };

        Ok(ExecutionPlan::new(node, total_cost))
    }
}

impl Default for QueryPlanner {
    fn default() -> Self {
        Self::new()
    }
}

/// Extension methods for ExecutionPlan.
trait ExecutionPlanExt {
    fn with_index(self) -> Self;
}

impl ExecutionPlanExt for ExecutionPlan {
    fn with_index(mut self) -> Self {
        self.uses_index = true;
        self
    }
}
