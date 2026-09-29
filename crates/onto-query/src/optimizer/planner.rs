// Copyright (c) 2024-2026 OntoDB Team
// Licensed under the Business Source License 1.1 (BUSL-1.1).
// See LICENSE for details. Change Date: 2031-09-15.
// On the Change Date, this file will be licensed under Apache License 2.0.
//! Query planner: converts AST to physical execution plans.
//!
//! Generates multiple candidate plans and selects the lowest-cost one.

use super::cost::{CostEstimate, CostModel, FilterSelectivity, IndexStats, TableStats};
use crate::parser::{FilterExpr, JoinClause, JoinOn, JoinType, LiteralValue, OrderBy, QueryAst, SelectColumns, SelectItem};
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

    /// Index nested loop join: for each left row, do an index lookup on the right table.
    IndexNestedLoopJoin {
        left: Box<PlanNode>,
        right_table: String,
        right_alias: Option<String>,
        join_clause: JoinClause,
        index_column: String,
        estimated_rows: u64,
    },

    /// Sort-merge join.
    SortMergeJoin {
        left: Box<PlanNode>,
        right: Box<PlanNode>,
        join_clause: JoinClause,
        estimated_rows: u64,
    },

    /// Sort.
    Sort {
        input: Box<PlanNode>,
        order_by: Vec<OrderBy>,
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

    /// Window function computation.
    WindowFunction {
        input: Box<PlanNode>,
        windows: Vec<PlanWindowExpr>,
        estimated_rows: u64,
    },
}

/// Window function expression in execution plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanWindowExpr {
    /// The window function type.
    pub func: crate::parser::WindowFunc,
    /// Function argument (column name or "*").
    pub arg: Option<String>,
    /// OVER clause specification.
    pub over: crate::parser::WindowSpec,
    /// Output column alias.
    pub alias: Option<String>,
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
            PlanNode::SeqScan {
                table,
                alias,
                estimated_rows,
                ..
            } => {
                output.push_str(&format!(
                    "{}SeqScan on {}{} (rows: {})\n",
                    indent,
                    table,
                    alias
                        .as_deref()
                        .map(|a| format!(" AS {}", a))
                        .unwrap_or_default(),
                    estimated_rows
                ));
            }
            PlanNode::IndexScan {
                table,
                index_column,
                estimated_rows,
                ..
            } => {
                output.push_str(&format!(
                    "{}IndexScan on {} using {} (rows: {})\n",
                    indent, table, index_column, estimated_rows
                ));
            }
            PlanNode::IndexLookup {
                table,
                index_column,
                estimated_rows,
                ..
            } => {
                output.push_str(&format!(
                    "{}IndexLookup on {} using {} (rows: {})\n",
                    indent, table, index_column, estimated_rows
                ));
            }
            PlanNode::VectorSearch {
                table,
                column,
                top_k,
                estimated_rows,
                ..
            } => {
                output.push_str(&format!(
                    "{}VectorSearch on {}.{} top {} (rows: {})\n",
                    indent, table, column, top_k, estimated_rows
                ));
            }
            PlanNode::Filter {
                input,
                estimated_rows,
                ..
            } => {
                output.push_str(&format!("{}Filter (rows: {})\n", indent, estimated_rows));
                self.describe_node(input, depth + 1, output);
            }
            PlanNode::Projection {
                input,
                estimated_rows,
                ..
            } => {
                output.push_str(&format!(
                    "{}Projection (rows: {})\n",
                    indent, estimated_rows
                ));
                self.describe_node(input, depth + 1, output);
            }
            PlanNode::NestedLoopJoin {
                left,
                right,
                estimated_rows,
                ..
            } => {
                output.push_str(&format!(
                    "{}NestedLoopJoin (rows: {})\n",
                    indent, estimated_rows
                ));
                self.describe_node(left, depth + 1, output);
                self.describe_node(right, depth + 1, output);
            }
            PlanNode::HashJoin {
                left,
                right,
                estimated_rows,
                ..
            } => {
                output.push_str(&format!("{}HashJoin (rows: {})\n", indent, estimated_rows));
                self.describe_node(left, depth + 1, output);
                self.describe_node(right, depth + 1, output);
            }
            PlanNode::IndexNestedLoopJoin {
                left,
                right_table,
                index_column,
                estimated_rows,
                ..
            } => {
                output.push_str(&format!(
                    "{}IndexNestedLoopJoin on {} using {} (rows: {})\n",
                    indent, right_table, index_column, estimated_rows
                ));
                self.describe_node(left, depth + 1, output);
            }
            PlanNode::SortMergeJoin {
                left,
                right,
                estimated_rows,
                ..
            } => {
                output.push_str(&format!(
                    "{}SortMergeJoin (rows: {})\n",
                    indent, estimated_rows
                ));
                self.describe_node(left, depth + 1, output);
                self.describe_node(right, depth + 1, output);
            }
            PlanNode::Sort {
                input,
                order_by,
                estimated_rows,
                ..
            } => {
                let ob_desc: Vec<String> = order_by
                    .iter()
                    .map(|ob| {
                        format!(
                            "{} {}",
                            ob.column,
                            if ob.ascending { "ASC" } else { "DESC" }
                        )
                    })
                    .collect();
                output.push_str(&format!(
                    "{}Sort by {} (rows: {})\n",
                    indent,
                    ob_desc.join(", "),
                    estimated_rows
                ));
                self.describe_node(input, depth + 1, output);
            }
            PlanNode::Aggregation {
                input,
                group_by,
                estimated_rows,
                ..
            } => {
                output.push_str(&format!(
                    "{}Aggregation GROUP BY {} (rows: {})\n",
                    indent,
                    group_by.join(", "),
                    estimated_rows
                ));
                self.describe_node(input, depth + 1, output);
            }
            PlanNode::Limit {
                input,
                count,
                estimated_rows,
                ..
            } => {
                output.push_str(&format!(
                    "{}Limit {} (rows: {})\n",
                    indent, count, estimated_rows
                ));
                self.describe_node(input, depth + 1, output);
            }
            PlanNode::Union {
                left,
                right,
                all,
                estimated_rows,
                ..
            } => {
                output.push_str(&format!(
                    "{}Union {} (rows: {})\n",
                    indent,
                    if *all { "ALL" } else { "DISTINCT" },
                    estimated_rows
                ));
                self.describe_node(left, depth + 1, output);
                self.describe_node(right, depth + 1, output);
            }
            PlanNode::WindowFunction {
                input,
                windows,
                estimated_rows,
                ..
            } => {
                let win_desc: Vec<String> = windows
                    .iter()
                    .map(|w| {
                        let alias = w.alias.as_deref().unwrap_or("unnamed");
                        format!(
                            "{:?}({}) as {}",
                            w.func,
                            w.arg.as_deref().unwrap_or("*"),
                            alias
                        )
                    })
                    .collect();
                output.push_str(&format!(
                    "{}WindowFunction [{}] (rows: {})\n",
                    indent,
                    win_desc.join(", "),
                    estimated_rows
                ));
                self.describe_node(input, depth + 1, output);
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
                offset: _,
            } => self.plan_select(
                columns,
                from,
                from_alias.as_deref(),
                joins,
                filter,
                group_by.as_ref(),
                having,
                order_by,
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

    /// Resolve cross joins (comma-separated tables) by extracting join conditions
    /// from the WHERE clause. For `FROM A a, B b WHERE a.id = b.a_id AND ...`:
    /// - Creates a proper JoinClause with ON condition
    /// - Removes the join condition from WHERE
    fn resolve_cross_joins(
        main_table: &str,
        main_alias: Option<&str>,
        joins: &[JoinClause],
        filter: &Option<FilterExpr>,
    ) -> (Vec<JoinClause>, Option<FilterExpr>) {
        let mut resolved = Vec::new();
        let mut remaining_preds: Vec<FilterExpr> = Vec::new();

        // Split WHERE into individual predicates
        let preds = match filter {
            Some(f) => Self::split_and_predicates(f),
            None => {
                return (joins.to_vec(), None);
            }
        };

        // Separate cross joins from normal joins
        let cross_joins: Vec<&JoinClause> = joins
            .iter()
            .filter(|j| matches!(j.join_type, JoinType::Cross))
            .collect();
        let normal_joins: Vec<JoinClause> = joins
            .iter()
            .filter(|j| !matches!(j.join_type, JoinType::Cross))
            .cloned()
            .collect();

        // Build alias -> table mapping
        let mut alias_map: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        alias_map.insert(main_table.to_string(), main_table.to_string());
        if let Some(a) = main_alias {
            alias_map.insert(a.to_string(), main_table.to_string());
        }
        for j in &cross_joins {
            alias_map.insert(j.table.clone(), j.table.clone());
            if let Some(a) = &j.alias {
                alias_map.insert(a.clone(), j.table.clone());
            }
        }

        // Track which cross joins have been resolved
        let mut resolved_cross: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        let mut cross_join_map: std::collections::HashMap<String, &JoinClause> =
            std::collections::HashMap::new();
        for j in &cross_joins {
            let key = j.alias.clone().unwrap_or_else(|| j.table.clone());
            cross_join_map.insert(key, j);
        }

        for pred in preds {
            // Check if this is an equality predicate referencing two different tables.
            // The parser may represent `c.source = v.id` as Eq("c.source", String("v.id"))
            // since it doesn't distinguish column refs from string literals.
            let col_pair = match &pred {
                FilterExpr::Eq(col_a, LiteralValue::String(col_b))
                    if col_a.contains('.') && col_b.contains('.') =>
                {
                    Some((col_a.clone(), col_b.clone()))
                }
                _ => None,
            };

            if let Some((col_a, col_b)) = col_pair {
                let cross_refs: Vec<JoinClause> = cross_joins.iter().map(|j| (*j).clone()).collect();
                let table_a = Self::table_for_column(&col_a, main_table, main_alias, &cross_refs);
                let table_b = Self::table_for_column(&col_b, main_table, main_alias, &cross_refs);

                if let (Some(ta), Some(tb)) = (&table_a, &table_b) {
                    if ta != tb {
                        // This is a cross-table join condition
                        let join_key = if ta == main_table || Some(ta.as_str()) == main_alias {
                            tb.clone()
                        } else {
                            ta.clone()
                        };

                        if let Some(cj) = cross_join_map.get(&join_key) {
                            let key = cj.alias.clone().unwrap_or_else(|| cj.table.clone());
                            if !resolved_cross.contains(&key) {
                                resolved.push(JoinClause {
                                    table: cj.table.clone(),
                                    alias: cj.alias.clone(),
                                    on: JoinOn {
                                        left: col_a,
                                        right: col_b,
                                    },
                                    join_type: JoinType::Inner,
                                });
                                resolved_cross.insert(key);
                            }
                            continue; // Don't add to remaining
                        }
                    }
                }
            }
            remaining_preds.push(pred);
        }

        // Add unresolved cross joins as-is (shouldn't happen for well-formed queries)
        for j in &cross_joins {
            let key = j.alias.clone().unwrap_or_else(|| j.table.clone());
            if !resolved_cross.contains(&key) {
                resolved.push((*j).clone());
            }
        }

        // Add normal joins
        resolved.extend(normal_joins);

        let remaining = if remaining_preds.is_empty() {
            None
        } else {
            Some(
                remaining_preds
                    .into_iter()
                    .reduce(|a, b| FilterExpr::And(Box::new(a), Box::new(b)))
                    .unwrap(),
            )
        };

        (resolved, remaining)
    }

    /// Determine which table a column belongs to based on its prefix.
    fn table_for_column(
        col: &str,
        main_table: &str,
        main_alias: Option<&str>,
        joins: &[JoinClause],
    ) -> Option<String> {
        if let Some(dot_pos) = col.find('.') {
            let prefix = &col[..dot_pos];
            if prefix == main_table || Some(prefix) == main_alias {
                return Some(main_table.to_string());
            }
            for j in joins {
                if Some(prefix) == j.alias.as_deref() || prefix == j.table {
                    return Some(j.table.clone());
                }
            }
        }
        None
    }

    /// Plan a SELECT query with predicate pushdown optimization.
    fn plan_select(
        &self,
        columns: &SelectColumns,
        from: &str,
        from_alias: Option<&str>,
        joins: &[JoinClause],
        filter: &Option<FilterExpr>,
        group_by: Option<&crate::parser::GroupByClause>,
        having: &Option<FilterExpr>,
        order_by: &[OrderBy],
        limit: Option<usize>,
    ) -> Result<ExecutionPlan> {
        // Resolve cross joins: extract join conditions from WHERE for comma-separated tables
        let (resolved_joins, resolved_filter) =
            Self::resolve_cross_joins(from, from_alias, joins, filter);

        let stats = self.stats.get(from).cloned().unwrap_or_else(|| TableStats {
            row_count: 1000,
            avg_row_size: 100,
            block_count: 10,
            has_primary_index: false,
            secondary_indexes: Vec::new(),
            vector_indexes: Vec::new(),
            histograms: Vec::new(),
        });

        // Apply predicate pushdown optimization
        let (pushed_filters, remaining_filter) =
            self.pushdown_predicates(from, from_alias, &resolved_joins, &resolved_filter);

        // Generate candidate plans
        let mut candidates = Vec::new();

        // Candidate 1: Sequential scan with pushed predicates
        let main_filter = pushed_filters
            .get(from)
            .cloned()
            .flatten()
            .map(Self::strip_filter_prefix);
        let seq_plan = self.plan_seq_scan(
            from,
            from_alias,
            &stats,
            &main_filter,
            joins,
            group_by,
            having,
            order_by,
            limit,
            columns,
            &pushed_filters,
        );
        candidates.push(seq_plan);

        // Candidate 2: Index scan (if applicable)
        if let Some(filter_expr) = &main_filter {
            let selectivity = self.cost_model.estimate_selectivity(&stats, filter_expr);
            if selectivity.can_use_index {
                if let Some(index_col) = &selectivity.index_column {
                    if let Some(index) = stats
                        .secondary_indexes
                        .iter()
                        .find(|i| &i.column == index_col)
                    {
                        let index_plan = self.plan_index_scan(
                            from,
                            from_alias,
                            &stats,
                            index,
                            &main_filter,
                            joins,
                            group_by,
                            having,
                            order_by,
                            limit,
                            columns,
                            &pushed_filters,
                        );
                        candidates.push(index_plan);
                    }
                }
            }
        }

        // Select the best plan
        candidates.sort_by(|a, b| {
            a.cost
                .total_cost
                .partial_cmp(&b.cost.total_cost)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let mut best_plan = candidates.into_iter().next().unwrap();

        // Add remaining filter if any predicates couldn't be pushed down
        if let Some(remaining) = remaining_filter {
            let filter_cost = self.cost_model.filter_cost(
                best_plan.cost.rows,
                &FilterSelectivity {
                    selectivity: 0.5, // Conservative estimate
                    can_use_index: false,
                    index_column: None,
                },
            );
            best_plan = ExecutionPlan::new(
                PlanNode::Filter {
                    input: Box::new(best_plan.root),
                    predicate: remaining,
                    estimated_rows: filter_cost.rows,
                },
                CostEstimate::new(
                    filter_cost.rows,
                    best_plan.cost.io_cost,
                    best_plan.cost.cpu_cost + filter_cost.cpu_cost,
                ),
            );
        }

        // Extract window functions from columns (if any)
        let window_funcs = match columns {
            SelectColumns::Columns(items) => items
                .iter()
                .filter_map(|item| {
                    if let SelectItem::WindowFunction(w) = item {
                        Some(PlanWindowExpr {
                            func: w.func.clone(),
                            arg: w.arg.clone(),
                            over: w.over.clone(),
                            alias: w.alias.clone(),
                        })
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        };

        // Add WindowFunction node if window functions are present
        if !window_funcs.is_empty() {
            let win_cost = CostEstimate::new(
                best_plan.cost.rows,
                best_plan.cost.io_cost,
                best_plan.cost.cpu_cost * 1.2, // Window functions add CPU cost
            );
            best_plan = ExecutionPlan::new(
                PlanNode::WindowFunction {
                    input: Box::new(best_plan.root),
                    windows: window_funcs,
                    estimated_rows: best_plan.cost.rows,
                },
                win_cost,
            );
        }

        // Add projection as the final step (after filter and window functions)
        best_plan = ExecutionPlan::new(
            PlanNode::Projection {
                input: Box::new(best_plan.root),
                columns: columns.clone(),
                estimated_rows: best_plan.cost.rows,
            },
            best_plan.cost,
        );

        Ok(best_plan)
    }

    /// Push predicates down to their respective tables.
    /// Returns (pushed_filters_by_table, remaining_filter)
    fn pushdown_predicates(
        &self,
        main_table: &str,
        main_alias: Option<&str>,
        joins: &[JoinClause],
        filter: &Option<FilterExpr>,
    ) -> (HashMap<String, Option<FilterExpr>>, Option<FilterExpr>) {
        let mut pushed_filters: HashMap<String, Option<FilterExpr>> = HashMap::new();
        pushed_filters.insert(main_table.to_string(), None);

        // Initialize filters for join tables
        for join in joins {
            pushed_filters.entry(join.table.clone()).or_insert(None);
        }

        let filter = match filter {
            Some(f) => f.clone(),
            None => return (pushed_filters, None),
        };

        // Split AND conditions
        let predicates = Self::split_and_predicates(&filter);

        let mut remaining_predicates = Vec::new();

        for pred in predicates {
            let table = Self::determine_table_for_predicate(&pred, main_table, main_alias, joins);

            if let Some(tbl) = table {
                // Push predicate to the appropriate table
                let existing = pushed_filters.get_mut(&tbl).unwrap();
                *existing = Some(match existing.take() {
                    Some(existing_filter) => {
                        FilterExpr::And(Box::new(existing_filter), Box::new(pred))
                    }
                    None => pred,
                });
            } else {
                // Can't determine table, keep as remaining
                remaining_predicates.push(pred);
            }
        }

        let remaining = if remaining_predicates.is_empty() {
            None
        } else {
            Some(
                remaining_predicates
                    .into_iter()
                    .reduce(|a, b| FilterExpr::And(Box::new(a), Box::new(b)))
                    .unwrap(),
            )
        };

        (pushed_filters, remaining)
    }

    /// Strip table prefix from column names in a filter expression.
    /// `t.column_name` → `column_name`, `column_name` → `column_name`
    fn strip_filter_prefix(filter: FilterExpr) -> FilterExpr {
        let strip = |col: &str| -> String {
            col.split('.').next_back().unwrap_or(col).to_string()
        };
        match filter {
            FilterExpr::Eq(c, v) => FilterExpr::Eq(strip(&c), v),
            FilterExpr::Ne(c, v) => FilterExpr::Ne(strip(&c), v),
            FilterExpr::Gt(c, v) => FilterExpr::Gt(strip(&c), v),
            FilterExpr::Lt(c, v) => FilterExpr::Lt(strip(&c), v),
            FilterExpr::Gte(c, v) => FilterExpr::Gte(strip(&c), v),
            FilterExpr::Lte(c, v) => FilterExpr::Lte(strip(&c), v),
            FilterExpr::Like(c, v) => FilterExpr::Like(strip(&c), v),
            FilterExpr::Between(c, lo, hi) => FilterExpr::Between(strip(&c), lo, hi),
            FilterExpr::In(c, v) => FilterExpr::In(strip(&c), v),
            FilterExpr::IsNull(c) => FilterExpr::IsNull(strip(&c)),
            FilterExpr::IsNotNull(c) => FilterExpr::IsNotNull(strip(&c)),
            FilterExpr::And(l, r) => FilterExpr::And(
                Box::new(Self::strip_filter_prefix(*l)),
                Box::new(Self::strip_filter_prefix(*r)),
            ),
            FilterExpr::Or(l, r) => FilterExpr::Or(
                Box::new(Self::strip_filter_prefix(*l)),
                Box::new(Self::strip_filter_prefix(*r)),
            ),
            FilterExpr::Not(e) => FilterExpr::Not(Box::new(Self::strip_filter_prefix(*e))),
            other => other,
        }
    }

    /// Split AND predicates into individual predicates.
    fn split_and_predicates(expr: &FilterExpr) -> Vec<FilterExpr> {
        match expr {
            FilterExpr::And(left, right) => {
                let mut result = Self::split_and_predicates(left);
                result.extend(Self::split_and_predicates(right));
                result
            }
            _ => vec![expr.clone()],
        }
    }

    /// Determine which table a predicate belongs to.
    /// Returns None if the predicate references a join alias (should be applied after join).
    fn determine_table_for_predicate(
        pred: &FilterExpr,
        main_table: &str,
        main_alias: Option<&str>,
        joins: &[JoinClause],
    ) -> Option<String> {
        let col = match pred {
            FilterExpr::Eq(col, _)
            | FilterExpr::Ne(col, _)
            | FilterExpr::Gt(col, _)
            | FilterExpr::Lt(col, _)
            | FilterExpr::Gte(col, _)
            | FilterExpr::Lte(col, _) => col.clone(),
            FilterExpr::Like(col, _) => col.clone(),
            FilterExpr::Between(col, _, _) => col.clone(),
            FilterExpr::In(col, _) => col.clone(),
            FilterExpr::InSubquery(col, _) => col.clone(),
            FilterExpr::IsNull(col) => col.clone(),
            FilterExpr::IsNotNull(col) => col.clone(),
            _ => return None, // Can't determine for complex predicates
        };

        // Check if column has table prefix
        if let Some(dot_pos) = col.find('.') {
            let table_prefix = &col[..dot_pos];
            // Match against main table or alias - can push down
            if table_prefix == main_table || Some(table_prefix) == main_alias {
                return Some(main_table.to_string());
            }
            // Match against join tables - push down to reduce scan size
            for join in joins {
                if table_prefix == join.table || Some(table_prefix) == join.alias.as_deref() {
                    return Some(join.table.clone());
                }
            }
        }

        // No prefix - assume main table
        Some(main_table.to_string())
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
        _having: &Option<FilterExpr>,
        order_by: &[OrderBy],
        limit: Option<usize>,
        columns: &SelectColumns,
        pushed_filters: &HashMap<String, Option<FilterExpr>>,
    ) -> ExecutionPlan {
        // Start with seq scan — push filter directly into the node for scan-time evaluation
        let scan_cost = self.cost_model.seq_scan_cost(stats);
        let mut current_cost = scan_cost;

        // Estimate filter cost separately for the cost model
        if let Some(filter_expr) = filter {
            let filter_selectivity = self.cost_model.estimate_selectivity(stats, filter_expr);
            let filter_cost = self
                .cost_model
                .filter_cost(current_cost.rows, &filter_selectivity);
            current_cost = CostEstimate::new(
                filter_cost.rows,
                current_cost.io_cost,
                current_cost.cpu_cost + filter_cost.cpu_cost,
            );
        }

        let mut current_node = PlanNode::SeqScan {
            table: table.to_string(),
            alias: alias.map(|s| s.to_string()),
            filter: filter.clone(),
            estimated_rows: current_cost.rows,
        };

        // Apply joins - choose between HashJoin and SortMergeJoin
        // If we have a LIMIT and no GROUP BY, we can push it into the right side of joins
        let can_push_limit = limit.is_some() && group_by.is_none();
        let ordered_joins = self.reorder_joins(joins);
        for join in &ordered_joins {
            let right_stats = self
                .stats
                .get(&join.table)
                .cloned()
                .unwrap_or_else(|| TableStats {
                    row_count: 100,
                    avg_row_size: 100,
                    block_count: 1,
                    has_primary_index: false,
                    secondary_indexes: Vec::new(),
                    vector_indexes: Vec::new(),
                    histograms: Vec::new(),
                });

            // Check if join column has an index for potential index scan
            let right_col = join
                .on
                .right
                .split('.')
                .next_back()
                .unwrap_or(&join.on.right);
            let has_join_index = right_stats
                .secondary_indexes
                .iter()
                .any(|i| i.column == right_col);

            // For LIMIT pushdown: if we can push limit, use it for the right side scan
            let effective_limit = if can_push_limit { limit } else { None };

            // Get pushed filter for this join table, strip table prefix for scan-time evaluation
            let join_filter = pushed_filters
                .get(&join.table)
                .cloned()
                .flatten()
                .map(Self::strip_filter_prefix);

            // Check if any pushed filter column has an index (e.g., genesymbol).
            // If so, use index scan on that column — highly selective predicates
            // benefit greatly from index access even though the join column differs.
            let filter_index_col = join_filter.as_ref().and_then(|f| {
                let col = match f {
                    FilterExpr::Eq(c, _)
                    | FilterExpr::Ne(c, _)
                    | FilterExpr::Gt(c, _)
                    | FilterExpr::Lt(c, _)
                    | FilterExpr::Gte(c, _)
                    | FilterExpr::Lte(c, _) => c.clone(),
                    FilterExpr::Like(c, _) => c.clone(),
                    FilterExpr::Between(c, _, _) => c.clone(),
                    FilterExpr::In(c, _) => c.clone(),
                    FilterExpr::IsNull(c) | FilterExpr::IsNotNull(c) => c.clone(),
                    _ => return None,
                };
                if right_stats
                    .secondary_indexes
                    .iter()
                    .any(|i| i.column == col)
                {
                    Some(col)
                } else {
                    None
                }
            });

            let (right_plan, right_plan_rows) = if let Some(ref filt_col) = filter_index_col {
                // Index scan on pushed filter column (e.g., genesymbol = 'TP53')
                let index = right_stats
                    .secondary_indexes
                    .iter()
                    .find(|i| &i.column == filt_col)
                    .unwrap();
                let selectivity = join_filter
                    .as_ref()
                    .map(|f| self.cost_model.estimate_selectivity(&right_stats, f).selectivity)
                    .unwrap_or(1.0);
                let index_cost = self
                    .cost_model
                    .index_range_scan_cost(&right_stats, index, selectivity);
                (
                    PlanNode::IndexScan {
                        table: join.table.clone(),
                        alias: join.alias.clone(),
                        index_column: filt_col.clone(),
                        filter: join_filter.clone(),
                        estimated_rows: index_cost.rows,
                    },
                    index_cost.rows,
                )
            } else if has_join_index {
                // Index scan on join column
                let index = right_stats
                    .secondary_indexes
                    .iter()
                    .find(|i| i.column == right_col)
                    .unwrap();
                let index_cost = self
                    .cost_model
                    .index_range_scan_cost(&right_stats, index, 1.0);
                (
                    PlanNode::IndexScan {
                        table: join.table.clone(),
                        alias: join.alias.clone(),
                        index_column: right_col.to_string(),
                        filter: join_filter.clone(),
                        estimated_rows: index_cost.rows,
                    },
                    index_cost.rows,
                )
            } else {
                // Sequential scan with optional limit pushdown
                let right_cost = self.cost_model.seq_scan_cost(&right_stats);
                let scan_rows = if let Some(lim) = effective_limit {
                    right_cost.rows.min(lim as u64)
                } else {
                    right_cost.rows
                };
                (
                    PlanNode::SeqScan {
                        table: join.table.clone(),
                        alias: join.alias.clone(),
                        filter: join_filter,
                        estimated_rows: scan_rows,
                    },
                    scan_rows,
                )
            };

            // Use right plan's actual row estimate for join cost (not full table size).
            let right_cost =
                CostEstimate::new(right_plan_rows, 0.0, right_plan_rows as f64 * self.cost_model.index_scan_cpu_per_row);
            let hash_cost = self.cost_model.hash_join_cost(&current_cost, &right_cost);
            let sort_merge_cost = self
                .cost_model
                .sort_merge_join_cost(&current_cost, &right_cost);

            // Check if the right table's join column has an index for IndexNestedLoopJoin.
            // When the left side is small (e.g., filtered to 400 rows) and the right table
            // is large with an index on the join column, INLJ avoids scanning the entire
            // right table — each left row does one index lookup instead.
            let has_join_index = right_stats
                .secondary_indexes
                .iter()
                .any(|i| i.column == right_col);

            // IndexNestedLoopJoin cost: left_rows × index_lookup_cost
            let inlj_cost = if has_join_index {
                let index = right_stats
                    .secondary_indexes
                    .iter()
                    .find(|i| i.column == right_col)
                    .unwrap();
                let lookup_per_row = self.cost_model.index_lookup_cost(&right_stats, index);
                let total_io = current_cost.io_cost + current_cost.rows as f64 * lookup_per_row.io_cost;
                let total_cpu = current_cost.cpu_cost + current_cost.rows as f64 * lookup_per_row.cpu_cost;
                let estimated_output = (current_cost.rows as f64 * self.cost_model.join_selectivity) as u64;
                CostEstimate::new(estimated_output, total_io, total_cpu)
            } else {
                CostEstimate::new(u64::MAX, f64::MAX, f64::MAX)
            };

            // Choose join strategy: INLJ > SortMerge > Hash (by cost)
            if has_join_index && current_cost.rows < 100000 && inlj_cost.total_cost < hash_cost.total_cost {
                // Index Nested Loop Join: small left side + indexed right side
                current_node = PlanNode::IndexNestedLoopJoin {
                    left: Box::new(current_node),
                    right_table: join.table.clone(),
                    right_alias: join.alias.clone(),
                    join_clause: join.clone(),
                    index_column: right_col.to_string(),
                    estimated_rows: inlj_cost.rows,
                };
                current_cost = inlj_cost;
            } else {
                // Prefer SortMergeJoin for large tables or when data is already sorted
                let use_sort_merge = current_cost.rows > 10000
                    || right_cost.rows > 10000
                    || (current_cost.is_sorted && right_cost.is_sorted);

                if use_sort_merge && sort_merge_cost.total_cost < hash_cost.total_cost {
                    current_node = PlanNode::SortMergeJoin {
                        left: Box::new(current_node),
                        right: Box::new(right_plan),
                        join_clause: join.clone(),
                        estimated_rows: sort_merge_cost.rows,
                    };
                    current_cost = sort_merge_cost;
                } else {
                    current_node = PlanNode::HashJoin {
                        left: Box::new(current_node),
                        right: Box::new(right_plan),
                        join_clause: join.clone(),
                        estimated_rows: hash_cost.rows,
                    };
                    current_cost = hash_cost;
                }
            }
        }

        // Apply aggregation
        if let Some(gb) = group_by {
            let agg_cost = self.cost_model.aggregation_cost(&current_cost);
            current_node = PlanNode::Aggregation {
                input: Box::new(current_node),
                group_by: gb.columns.clone(),
                aggregates: Self::extract_aggregates(columns),
                estimated_rows: agg_cost.rows,
            };
            current_cost = agg_cost;
        }

        // Apply sort
        if !order_by.is_empty() {
            let sort_cost = self.cost_model.sort_cost(&current_cost);
            current_node = PlanNode::Sort {
                input: Box::new(current_node),
                order_by: order_by.to_vec(),
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
            current_cost =
                CostEstimate::new(limited_rows, current_cost.io_cost, current_cost.cpu_cost);
        }

        // Note: Projection is added in plan_select after remaining filter

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
        _having: &Option<FilterExpr>,
        order_by: &[OrderBy],
        limit: Option<usize>,
        columns: &SelectColumns,
        pushed_filters: &HashMap<String, Option<FilterExpr>>,
    ) -> ExecutionPlan {
        let filter_selectivity = filter
            .as_ref()
            .map(|f| self.cost_model.estimate_selectivity(stats, f))
            .unwrap_or(FilterSelectivity {
                selectivity: 1.0,
                can_use_index: false,
                index_column: None,
            });

        let index_cost =
            self.cost_model
                .index_range_scan_cost(stats, index, filter_selectivity.selectivity);
        let mut current_node = PlanNode::IndexScan {
            table: table.to_string(),
            alias: alias.map(|s| s.to_string()),
            index_column: index.column.clone(),
            filter: filter.clone(),
            estimated_rows: index_cost.rows,
        };
        let mut current_cost = index_cost;

        // Apply joins - use hash join for better performance
        let ordered_joins = self.reorder_joins(joins);
        for join in &ordered_joins {
            let right_stats = self
                .stats
                .get(&join.table)
                .cloned()
                .unwrap_or_else(|| TableStats {
                    row_count: 100,
                    avg_row_size: 100,
                    block_count: 1,
                    has_primary_index: false,
                    secondary_indexes: Vec::new(),
                    vector_indexes: Vec::new(),
                    histograms: Vec::new(),
                });

            let right_col = join
                .on
                .right
                .split('.')
                .next_back()
                .unwrap_or(&join.on.right);
            let has_join_index = right_stats
                .secondary_indexes
                .iter()
                .any(|i| i.column == right_col);

            // Get pushed filter for this join table
            let join_filter = pushed_filters
                .get(&join.table)
                .cloned()
                .flatten()
                .map(Self::strip_filter_prefix);

            // Check if any pushed filter column has an index
            let filter_index_col = join_filter.as_ref().and_then(|f| {
                let col = match f {
                    FilterExpr::Eq(c, _)
                    | FilterExpr::Ne(c, _)
                    | FilterExpr::Gt(c, _)
                    | FilterExpr::Lt(c, _)
                    | FilterExpr::Gte(c, _)
                    | FilterExpr::Lte(c, _) => c.clone(),
                    FilterExpr::Like(c, _) => c.clone(),
                    FilterExpr::Between(c, _, _) => c.clone(),
                    FilterExpr::In(c, _) => c.clone(),
                    FilterExpr::IsNull(c) | FilterExpr::IsNotNull(c) => c.clone(),
                    _ => return None,
                };
                if right_stats
                    .secondary_indexes
                    .iter()
                    .any(|i| i.column == col)
                {
                    Some(col)
                } else {
                    None
                }
            });

            let (right_plan, right_plan_rows) = if let Some(ref filt_col) = filter_index_col {
                let index = right_stats
                    .secondary_indexes
                    .iter()
                    .find(|i| &i.column == filt_col)
                    .unwrap();
                let selectivity = join_filter
                    .as_ref()
                    .map(|f| self.cost_model.estimate_selectivity(&right_stats, f).selectivity)
                    .unwrap_or(1.0);
                let index_cost = self
                    .cost_model
                    .index_range_scan_cost(&right_stats, index, selectivity);
                (
                    PlanNode::IndexScan {
                        table: join.table.clone(),
                        alias: join.alias.clone(),
                        index_column: filt_col.clone(),
                        filter: join_filter.clone(),
                        estimated_rows: index_cost.rows,
                    },
                    index_cost.rows,
                )
            } else if has_join_index {
                let index = right_stats
                    .secondary_indexes
                    .iter()
                    .find(|i| i.column == right_col)
                    .unwrap();
                let index_cost = self
                    .cost_model
                    .index_range_scan_cost(&right_stats, index, 1.0);
                (
                    PlanNode::IndexScan {
                        table: join.table.clone(),
                        alias: join.alias.clone(),
                        index_column: right_col.to_string(),
                        filter: join_filter,
                        estimated_rows: index_cost.rows,
                    },
                    index_cost.rows,
                )
            } else {
                let right_cost = self.cost_model.seq_scan_cost(&right_stats);
                (
                    PlanNode::SeqScan {
                        table: join.table.clone(),
                        alias: join.alias.clone(),
                        filter: join_filter,
                        estimated_rows: right_cost.rows,
                    },
                    right_cost.rows,
                )
            };

            // Use right plan's actual row estimate for join cost
            let right_cost =
                CostEstimate::new(right_plan_rows, 0.0, right_plan_rows as f64 * self.cost_model.index_scan_cpu_per_row);
            let join_cost = self.cost_model.hash_join_cost(&current_cost, &right_cost);

            current_node = PlanNode::HashJoin {
                left: Box::new(current_node),
                right: Box::new(right_plan),
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
                aggregates: Self::extract_aggregates(columns),
                estimated_rows: agg_cost.rows,
            };
            current_cost = agg_cost;
        }

        // Index scan is already sorted, so no need for sort unless ORDER BY is on different column
        if !order_by.is_empty() {
            let needs_sort = order_by.iter().any(|ob| ob.column != index.column);
            if needs_sort {
                let sort_cost = self.cost_model.sort_cost(&current_cost);
                current_node = PlanNode::Sort {
                    input: Box::new(current_node),
                    order_by: order_by.to_vec(),
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
            current_cost =
                CostEstimate::new(limited_rows, current_cost.io_cost, current_cost.cpu_cost);
        }

        // Note: Projection is added in plan_select after remaining filter

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
        let stats = self
            .stats
            .get(table)
            .cloned()
            .unwrap_or_else(|| TableStats {
                row_count: 1000,
                avg_row_size: 100,
                block_count: 10,
                has_primary_index: false,
                secondary_indexes: Vec::new(),
                vector_indexes: Vec::new(),
                histograms: Vec::new(),
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

        let cost = self
            .cost_model
            .vector_search_cost(&stats, &vector_stats, top_k as u64);

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

    /// Reorder joins for optimal execution.
    /// Strategy: smallest tables first (left-deep tree).
    /// This minimizes the size of the hash table built in hash joins.
    fn reorder_joins(&self, joins: &[JoinClause]) -> Vec<JoinClause> {
        if joins.len() <= 1 {
            return joins.to_vec();
        }

        // Sort joins by estimated table size (smallest first)
        let mut ordered: Vec<(usize, &JoinClause, u64)> = joins
            .iter()
            .enumerate()
            .map(|(i, j)| {
                let stats = self.stats.get(&j.table);
                let row_count = stats.map(|s| s.row_count).unwrap_or(100);
                (i, j, row_count)
            })
            .collect();

        ordered.sort_by_key(|&(_, _, rows)| rows);

        ordered.into_iter().map(|(_, j, _)| j.clone()).collect()
    }

    /// Plan a UNION query.
    fn plan_union(&self, left: &QueryAst, right: &QueryAst, all: bool) -> Result<ExecutionPlan> {
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

    /// Extract aggregate functions from SELECT columns.
    fn extract_aggregates(columns: &SelectColumns) -> Vec<AggregateFunc> {
        match columns {
            SelectColumns::All => Vec::new(),
            SelectColumns::Columns(items) => items
                .iter()
                .filter_map(|item| {
                    if let SelectItem::Aggregate(agg) = item {
                        Some(AggregateFunc {
                            func: agg.func.clone(),
                            arg: agg.arg.clone(),
                            alias: agg.alias.clone(),
                        })
                    } else {
                        None
                    }
                })
                .collect(),
        }
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
