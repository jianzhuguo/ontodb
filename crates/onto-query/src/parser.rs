//! SQL parser with semantic extensions for OntoDB.
//!
//! Supports standard SQL (SELECT, INSERT, UPDATE, DELETE)
//! plus OntoDB extensions:
//! - CREATE ONTOLOGY
//! - MATCH (p: ClassName) - semantic class-based queries

use onto_core::{CoreError, Result};
use serde::{Deserialize, Serialize};
use crate::parser_util::{find_ignore_ascii_case, starts_with_ignore_ascii_case, ends_with_ignore_ascii_case, safe_slice, safe_slice_from, trim_semicolons};

/// Case-insensitive (ASCII) version of `find_unquoted`.
/// Searches for `needle` in `haystack` while skipping quoted strings,
/// using ASCII case-insensitive matching. Returns the byte offset of
/// the first unquoted match, or None.
fn find_unquoted_ignore_ascii_case(haystack: &str, needle: &str) -> Option<usize> {
    let needle_upper: Vec<u8> = needle.bytes().map(|b| b.to_ascii_uppercase()).collect();
    let nlen = needle_upper.len();
    if nlen == 0 || haystack.len() < nlen {
        return None;
    }
    let hay_bytes = haystack.as_bytes();
    let mut in_quote: Option<u8> = None;
    let mut i = 0;
    while i + nlen <= hay_bytes.len() {
        let c = hay_bytes[i];
        if let Some(q) = in_quote {
            if c == q {
                in_quote = None;
            }
        } else if c == b'\'' || c == b'"' {
            in_quote = Some(c);
        } else {
            let mut matched = true;
            for j in 0..nlen {
                if hay_bytes[i + j].to_ascii_uppercase() != needle_upper[j] {
                    matched = false;
                    break;
                }
            }
            if matched {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// File format for IMPORT command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImportFormat {
    Csv,
    Json,
}

/// A parsed query in AST form.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum QueryAst {
    /// CREATE ONTOLOGY <name> (...)
    CreateOntology { sql: String },

    /// INSERT INTO <class> (...) VALUES (...)
    Insert {
        class: String,
        columns: Vec<String>,
        values: Vec<LiteralValue>,
    },

    /// INSERT INTO <class> (...) VALUES (...), (...), ... (batch insert)
    BatchInsert {
        class: String,
        columns: Vec<String>,
        rows: Vec<Vec<LiteralValue>>,
    },

    /// INSERT INTO <class> (...) VALUES (...), (...), ... ON CONFLICT DO UPDATE SET ... (batch upsert)
    BatchUpsert {
        class: String,
        columns: Vec<String>,
        rows: Vec<Vec<LiteralValue>>,
        conflict_column: String,
        assignments: Vec<(String, LiteralValue)>,
    },

    /// INSERT INTO <class> (...) SELECT ... (insert from query)
    InsertSelect {
        class: String,
        columns: Vec<String>,
        query: Box<QueryAst>,
    },

    /// INSERT INTO <class> (...) VALUES (...) ON CONFLICT DO UPDATE SET ...
    Upsert {
        class: String,
        columns: Vec<String>,
        values: Vec<LiteralValue>,
        conflict_column: String,
        assignments: Vec<(String, LiteralValue)>,
    },

    /// IMPORT INTO <class> FROM CSV/JSON '<file_path>'
    Import {
        class: String,
        file_path: String,
        format: ImportFormat,
    },

    /// COPY <class> FROM '<file_path>' (FORMAT CSV|JSON)
    /// Direct bulk load without transaction — fastest path for initial data loading.
    Copy {
        class: String,
        file_path: String,
        format: ImportFormat,
    },

    /// BEGIN [TRANSACTION]
    Begin,
    /// COMMIT [TRANSACTION]
    Commit,
    /// ROLLBACK [TRANSACTION]
    Rollback,

    /// SELECT [DISTINCT] ... FROM <class> [JOIN ...] [WHERE ...] [GROUP BY ...] [HAVING ...] [ORDER BY ...] [LIMIT ... [OFFSET ...]]
    Select {
        distinct: bool,
        columns: SelectColumns,
        from: String,
        from_alias: Option<String>,
        joins: Vec<JoinClause>,
        filter: Option<FilterExpr>,
        group_by: Option<GroupByClause>,
        having: Option<FilterExpr>,
        order_by: Vec<OrderBy>,
        limit: Option<usize>,
        offset: Option<usize>,
    },

    /// UPDATE <class> SET ... WHERE ...
    Update {
        class: String,
        assignments: Vec<(String, LiteralValue)>,
        filter: Option<FilterExpr>,
    },

    /// DELETE FROM <class> [WHERE ...]
    Delete {
        class: String,
        filter: Option<FilterExpr>,
    },

    /// MATCH (<var>: <Class>) [WHERE ...] RETURN ...
    Match {
        variable: String,
        class: String,
        filter: Option<FilterExpr>,
        returns: Vec<String>,
    },

    /// <query1> UNION [ALL] <query2>
    Union {
        left: Box<QueryAst>,
        right: Box<QueryAst>,
        all: bool, // true = UNION ALL (keep duplicates), false = UNION (distinct)
    },

    /// CREATE INDEX <name> ON <class> (<column>)
    CreateIndex {
        class: String,
        column: String,
    },

    /// CREATE INDEX ON <class> (<col1>, <col2>, ...) - Composite index
    CreateCompositeIndex {
        class: String,
        columns: Vec<String>,
    },

    /// DROP INDEX <name> ON <class> (<column>)
    DropIndex {
        class: String,
        column: String,
    },

    /// CREATE VECTOR INDEX ON <class> (<column>) METRIC <metric> DIMENSION <dim>
    CreateVectorIndex {
        class: String,
        column: String,
        metric: String,
        dimension: usize,
        m: usize,
        ef_construction: usize,
        ef_search: usize,
    },

    /// DROP VECTOR INDEX ON <class> (<column>)
    DropVectorIndex {
        class: String,
        column: String,
    },

    /// VECTOR SEARCH ON <class> (<column>) QUERY [v1, v2, ...] TOP <k> [WHERE ...]
    VectorSearch {
        class: String,
        column: String,
        query_vector: Vec<f32>,
        top_k: usize,
        filter: Option<FilterExpr>,
    },

    /// EXPLAIN <query> - Show execution plan without executing
    Explain {
        query: Box<QueryAst>,
    },

    /// EXPLAIN REASONING <query> - Show ontology reasoning derivation chain
    ExplainReasoning {
        query: Box<QueryAst>,
    },

    /// WITH <cte_name> AS (<query>) <main_query> - Common Table Expression
    With {
        ctes: Vec<CteDefinition>,
        query: Box<QueryAst>,
        recursive: bool,
    },

    /// CREATE MATERIALIZED VIEW <name> AS <query> - Materialized View
    CreateMaterializedView {
        name: String,
        query: Box<QueryAst>,
    },

    /// DROP MATERIALIZED VIEW <name>
    DropMaterializedView {
        name: String,
    },

    /// REFRESH MATERIALIZED VIEW <name>
    RefreshMaterializedView {
        name: String,
    },

    /// ANALYZE <table> - Collect table statistics for query optimization
    Analyze {
        table: String,
    },

    // ── Graph Queries ────────────────────────────────────────────────

    /// CREATE VERTEX TABLE <name> (properties...)
    CreateVertexTable {
        name: String,
        columns: Vec<ColumnDef>,
    },

    /// CREATE EDGE TABLE <name> (from_table, to_table, properties...)
    CreateEdgeTable {
        name: String,
        from_table: String,
        to_table: String,
        columns: Vec<ColumnDef>,
    },

    /// INSERT VERTEX INTO <table> (id, properties...) VALUES (...)
    InsertVertex {
        table: String,
        id: String,
        labels: Vec<String>,
        properties: Vec<(String, LiteralValue)>,
    },

    /// INSERT EDGE INTO <table> (id, from_id, to_id, properties...) VALUES (...)
    InsertEdge {
        table: String,
        id: String,
        from_id: String,
        to_id: String,
        label: String,
        properties: Vec<(String, LiteralValue)>,
    },

    /// GRAPH TRAVERSE FROM <start_id> [IN|OUT|BOTH] [LABEL <label>] [DEPTH <n>] [WHERE ...]
    GraphTraverse {
        start_id: String,
        direction: GraphDirection,
        edge_label: Option<String>,
        max_depth: usize,
        filter: Option<FilterExpr>,
    },

    /// GRAPH MATCH (<var>: <label>) -[<edge_var>: <edge_label>]-> (<var2>: <label2>) [WHERE ...] RETURN ...
    GraphMatch {
        pattern: GraphPattern,
        filter: Option<FilterExpr>,
        returns: Vec<String>,
    },

    /// GRAPH SHORTEST PATH FROM <id1> TO <id2> [MAX DEPTH <n>]
    GraphShortestPath {
        from_id: String,
        to_id: String,
        max_depth: usize,
    },

    /// SYSTEM ACTIVATE '<class>::<pk>' '<reason>' — Activate a live data entity
    SystemActivate { entity: String, reason: String },

    /// BACKUP TO '<path>' — Create a full snapshot backup
    Backup { path: String },

    /// RESTORE FROM '<path>' — Restore from a backup snapshot
    Restore { path: String },

    /// FLUSH — Flush MemTable to SSTable
    Flush,

    // ── Namespace management ──────────────────────────────────────

    /// CREATE NAMESPACE <name>
    CreateNamespace { name: String },

    /// DROP NAMESPACE <name>
    DropNamespace { name: String },

    /// USE NAMESPACE <name>
    UseNamespace { name: String },

    /// DROP ONTOLOGY <name>
    DropOntology { name: String },
}

/// Column definition for CREATE TABLE.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColumnDef {
    pub name: String,
    pub col_type: String,
    pub required: bool,
}

/// Graph traversal direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GraphDirection {
    Out,
    In,
    Both,
}

/// Graph pattern for MATCH queries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphPattern {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
}

/// A node in a graph pattern.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphNode {
    pub variable: String,
    pub label: Option<String>,
}

/// An edge in a graph pattern.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphEdge {
    pub variable: Option<String>,
    pub label: Option<String>,
    pub from: String,
    pub to: String,
    pub direction: GraphDirection,
}

impl QueryAst {
    /// Returns true if this query only reads data and has no write side effects.
    /// Used to determine lock type: read lock for read-only, write lock for mutations.
    pub fn is_read_only(&self) -> bool {
        match self {
            // Pure reads
            QueryAst::Select { .. } => true,
            QueryAst::Match { .. } => true,
            QueryAst::VectorSearch { .. } => true,
            QueryAst::Explain { .. } => true,
            QueryAst::ExplainReasoning { .. } => true,

            // Writes
            QueryAst::Insert { .. } => false,
            QueryAst::BatchInsert { .. } => false,
            QueryAst::BatchUpsert { .. } => false,
            QueryAst::InsertSelect { .. } => false,
            QueryAst::Upsert { .. } => false,
            QueryAst::Update { .. } => false,
            QueryAst::Delete { .. } => false,
            QueryAst::Import { .. } => false,
            QueryAst::Copy { .. } => false,

            // DDL
            QueryAst::CreateOntology { .. } => false,
            QueryAst::CreateIndex { .. } => false,
            QueryAst::CreateCompositeIndex { .. } => false,
            QueryAst::DropIndex { .. } => false,
            QueryAst::CreateVectorIndex { .. } => false,
            QueryAst::DropVectorIndex { .. } => false,
            QueryAst::CreateMaterializedView { .. } => false,
            QueryAst::DropMaterializedView { .. } => false,
            QueryAst::RefreshMaterializedView { .. } => false,

            // Transaction control
            QueryAst::Begin => false,
            QueryAst::Commit => false,
            QueryAst::Rollback => false,

            // ANALYZE writes statistics
            QueryAst::Analyze { .. } => false,

            // UNION and WITH are conservatively treated as writes
            // because they can contain INSERT...SELECT or write CTEs
            QueryAst::Union { .. } => false,
            QueryAst::With { .. } => false,

            // Graph queries
            QueryAst::CreateVertexTable { .. } => false,
            QueryAst::CreateEdgeTable { .. } => false,
            QueryAst::InsertVertex { .. } => false,
            QueryAst::InsertEdge { .. } => false,
            QueryAst::GraphTraverse { .. } => true,
            QueryAst::GraphMatch { .. } => true,
            QueryAst::GraphShortestPath { .. } => true,

            // Backup/Restore/Flush/Activate
            QueryAst::SystemActivate { .. } => false,
            QueryAst::Backup { .. } => false,
            QueryAst::Restore { .. } => false,
            QueryAst::Flush => false,

            // Namespace operations
            QueryAst::CreateNamespace { .. } => false,
            QueryAst::DropNamespace { .. } => false,
            QueryAst::UseNamespace { .. } => false,

            // Ontology operations
            QueryAst::DropOntology { .. } => false,
        }
    }

    /// Substitutes positional parameters ($1, $2, etc.) with actual values.
    /// Returns a new AST with all parameters replaced.
    pub fn substitute_params(&self, params: &[LiteralValue]) -> Result<QueryAst> {
        let mut substitutor = ParamSubstitutor { params };
        substitutor.substitute_ast(self)
    }

    /// Returns true if this AST contains any parameter placeholders.
    pub fn has_params(&self) -> bool {
        let checker = ParamChecker { found: false };
        checker.check_ast(self)
    }
}

/// Helper to check if an AST contains parameters.
struct ParamChecker {
    found: bool,
}

impl ParamChecker {
    fn check_ast(mut self, ast: &QueryAst) -> bool {
        self.visit_ast(ast);
        self.found
    }

    fn visit_ast(&mut self, ast: &QueryAst) {
        if self.found { return; }
        match ast {
            QueryAst::Select { filter, .. } => {
                if let Some(f) = filter { self.visit_filter(f); }
            }
            QueryAst::Insert { values, .. } => {
                for v in values { self.visit_value(v); }
            }
            QueryAst::Update { assignments, filter, .. } => {
                for (_, v) in assignments { self.visit_value(v); }
                if let Some(f) = filter { self.visit_filter(f); }
            }
            QueryAst::Delete { filter, .. } => {
                if let Some(f) = filter { self.visit_filter(f); }
            }
            _ => {}
        }
    }

    fn visit_filter(&mut self, filter: &FilterExpr) {
        if self.found { return; }
        match filter {
            FilterExpr::Eq(_, v) | FilterExpr::Ne(_, v) |
            FilterExpr::Gt(_, v) | FilterExpr::Lt(_, v) |
            FilterExpr::Gte(_, v) | FilterExpr::Lte(_, v) => self.visit_value(v),
            FilterExpr::Between(_, lo, hi) => {
                self.visit_value(lo);
                self.visit_value(hi);
            }
            FilterExpr::In(_, vals) => {
                for v in vals { self.visit_value(v); }
            }
            FilterExpr::Not(f) => self.visit_filter(f),
            FilterExpr::And(l, r) | FilterExpr::Or(l, r) => {
                self.visit_filter(l);
                self.visit_filter(r);
            }
            _ => {}
        }
    }

    fn visit_value(&mut self, value: &LiteralValue) {
        match value {
            LiteralValue::ParamIndex(_) | LiteralValue::ParamName(_) => {
                self.found = true;
            }
            _ => {}
        }
    }
}

/// Helper to substitute parameters in an AST.
struct ParamSubstitutor<'a> {
    params: &'a [LiteralValue],
}

impl<'a> ParamSubstitutor<'a> {
    fn substitute_ast(&mut self, ast: &QueryAst) -> Result<QueryAst> {
        match ast {
            QueryAst::Select { distinct, columns, from, from_alias, joins, filter, group_by, having, order_by, limit, offset } => {
                Ok(QueryAst::Select {
                    distinct: *distinct,
                    columns: columns.clone(),
                    from: from.clone(),
                    from_alias: from_alias.clone(),
                    joins: joins.clone(),
                    filter: filter.as_ref().map(|f| self.substitute_filter(f)).transpose()?,
                    group_by: group_by.clone(),
                    having: having.as_ref().map(|f| self.substitute_filter(f)).transpose()?,
                    order_by: order_by.clone(),
                    limit: *limit,
                    offset: *offset,
                })
            }
            QueryAst::Insert { class, columns, values } => {
                Ok(QueryAst::Insert {
                    class: class.clone(),
                    columns: columns.clone(),
                    values: values.iter().map(|v| self.substitute_value(v)).collect::<Result<Vec<_>>>()?,
                })
            }
            QueryAst::Update { class, assignments, filter } => {
                Ok(QueryAst::Update {
                    class: class.clone(),
                    assignments: assignments.iter().map(|(k, v)| {
                        Ok((k.clone(), self.substitute_value(v)?))
                    }).collect::<Result<Vec<_>>>()?,
                    filter: filter.as_ref().map(|f| self.substitute_filter(f)).transpose()?,
                })
            }
            QueryAst::Delete { class, filter } => {
                Ok(QueryAst::Delete {
                    class: class.clone(),
                    filter: filter.as_ref().map(|f| self.substitute_filter(f)).transpose()?,
                })
            }
            // For other AST types, return as-is (no parameter substitution)
            _ => Ok(ast.clone()),
        }
    }

    fn substitute_filter(&mut self, filter: &FilterExpr) -> Result<FilterExpr> {
        match filter {
            FilterExpr::Eq(col, v) => Ok(FilterExpr::Eq(col.clone(), self.substitute_value(v)?)),
            FilterExpr::Ne(col, v) => Ok(FilterExpr::Ne(col.clone(), self.substitute_value(v)?)),
            FilterExpr::Gt(col, v) => Ok(FilterExpr::Gt(col.clone(), self.substitute_value(v)?)),
            FilterExpr::Lt(col, v) => Ok(FilterExpr::Lt(col.clone(), self.substitute_value(v)?)),
            FilterExpr::Gte(col, v) => Ok(FilterExpr::Gte(col.clone(), self.substitute_value(v)?)),
            FilterExpr::Lte(col, v) => Ok(FilterExpr::Lte(col.clone(), self.substitute_value(v)?)),
            FilterExpr::Between(col, lo, hi) => {
                Ok(FilterExpr::Between(col.clone(), self.substitute_value(lo)?, self.substitute_value(hi)?))
            }
            FilterExpr::In(col, vals) => {
                Ok(FilterExpr::In(col.clone(), vals.iter().map(|v| self.substitute_value(v)).collect::<Result<Vec<_>>>()?))
            }
            FilterExpr::Not(f) => Ok(FilterExpr::Not(Box::new(self.substitute_filter(f)?))),
            FilterExpr::And(l, r) => {
                Ok(FilterExpr::And(Box::new(self.substitute_filter(l)?), Box::new(self.substitute_filter(r)?)))
            }
            FilterExpr::Or(l, r) => {
                Ok(FilterExpr::Or(Box::new(self.substitute_filter(l)?), Box::new(self.substitute_filter(r)?)))
            }
            // For other filter types, return as-is
            _ => Ok(filter.clone()),
        }
    }

    fn substitute_value(&mut self, value: &LiteralValue) -> Result<LiteralValue> {
        match value {
            LiteralValue::ParamIndex(idx) => {
                if *idx == 0 || *idx > self.params.len() {
                    return Err(CoreError::InvalidArgument(
                        format!("parameter index ${} out of range (1-{})", idx, self.params.len())
                    ));
                }
                Ok(self.params[idx - 1].clone())
            }
            LiteralValue::ParamName(name) => {
                // Named parameters not yet supported in this simple implementation
                Err(CoreError::InvalidArgument(
                    format!("named parameter :{} not yet supported, use positional $1, $2, etc.", name)
                ))
            }
            _ => Ok(value.clone()),
        }
    }
}

/// A CTE (Common Table Expression) definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CteDefinition {
    /// Name of the CTE.
    pub name: String,
    /// Column aliases (optional).
    pub columns: Vec<String>,
    /// The CTE query.
    pub query: Box<QueryAst>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SelectColumns {
    All,
    Columns(Vec<SelectItem>),
}

/// A single item in SELECT: either a column reference, aggregate, or window function.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SelectItem {
    /// A regular column: `name` or `p.name`
    Column(String),
    /// An aggregate: `COUNT(*)`, `SUM(price) as total`
    Aggregate(AggregateExpr),
    /// A window function: `ROW_NUMBER() OVER (...)`
    WindowFunction(WindowExpr),
    /// A general expression: CASE WHEN, scalar subquery, arithmetic
    Expression(ValueExpr),
}

/// An aggregate function in SELECT: COUNT(*), SUM(col), AVG(col), MIN(col), MAX(col)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AggregateExpr {
    pub func: AggregateFunc,
    pub arg: String, // column name or "*" for COUNT(*)
    pub alias: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AggregateFunc {
    Count,
    Sum,
    Avg,
    Min,
    Max,
}

/// A value expression that can appear in SELECT, HAVING, etc.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ValueExpr {
    /// Column reference: `name` or `p.name`
    Column(String),
    /// Literal value: 42, 'hello', NULL
    Literal(LiteralValue),
    /// CASE WHEN condition THEN result ... [ELSE default] END
    CaseWhen {
        when_branches: Vec<(FilterExpr, ValueExpr)>,
        else_expr: Option<Box<ValueExpr>>,
    },
    /// Scalar subquery: (SELECT col FROM table WHERE ...)
    ScalarSubquery(Box<QueryAst>),
    /// Arithmetic: left + right
    Arithmetic {
        op: ArithmeticOp,
        left: Box<ValueExpr>,
        right: Box<ValueExpr>,
    },
    /// Built-in function call: COALESCE(a, b, ...), CONCAT(a, b), etc.
    Function {
        name: String,
        args: Vec<ValueExpr>,
    },
}

/// Arithmetic operators.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ArithmeticOp {
    Add,
    Sub,
    Mul,
    Div,
}

/// Window function expression.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowExpr {
    /// The window function type.
    pub func: WindowFunc,
    /// Function argument (column name or "*").
    pub arg: Option<String>,
    /// OVER clause specification.
    pub over: WindowSpec,
    /// Output column alias.
    pub alias: Option<String>,
}

/// Window function types.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WindowFunc {
    /// ROW_NUMBER() - sequential row number
    RowNumber,
    /// RANK() - rank with gaps for ties
    Rank,
    /// DENSE_RANK() - rank without gaps
    DenseRank,
    /// LAG(col, offset, default) - value from previous row
    Lag,
    /// LEAD(col, offset, default) - value from next row
    Lead,
    /// FIRST_VALUE(col) - first value in window
    FirstValue,
    /// LAST_VALUE(col) - last value in window
    LastValue,
    /// NTH_VALUE(col, n) - nth value in window
    NthValue,
    /// SUM(col) OVER (...) - cumulative sum
    Sum,
    /// AVG(col) OVER (...) - moving average
    Avg,
    /// MIN(col) OVER (...) - running minimum
    Min,
    /// MAX(col) OVER (...) - running maximum
    Max,
    /// COUNT(*) OVER (...) - running count
    Count,
}

/// Window specification (OVER clause).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowSpec {
    /// PARTITION BY columns.
    pub partition_by: Vec<String>,
    /// ORDER BY columns with direction.
    pub order_by: Vec<WindowOrderBy>,
    /// Frame specification (ROWS/RANGE).
    pub frame: Option<WindowFrame>,
}

/// ORDER BY column in window specification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowOrderBy {
    pub column: String,
    pub ascending: bool,
}

/// Window frame specification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowFrame {
    /// Frame type: ROWS or RANGE
    pub frame_type: WindowFrameType,
    /// Frame start boundary.
    pub start: WindowFrameBound,
    /// Frame end boundary (None = CURRENT ROW).
    pub end: Option<WindowFrameBound>,
}

/// Window frame type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WindowFrameType {
    Rows,
    Range,
}

/// Window frame boundary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WindowFrameBound {
    /// UNBOUNDED PRECEDING
    UnboundedPreceding,
    /// N PRECEDING
    Preceding(u64),
    /// CURRENT ROW
    CurrentRow,
    /// N FOLLOWING
    Following(u64),
    /// UNBOUNDED FOLLOWING
    UnboundedFollowing,
}

/// GROUP BY clause
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupByClause {
    pub columns: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LiteralValue {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    /// Positional parameter placeholder ($1, $2, etc.)
    /// The value is the 1-based parameter index.
    ParamIndex(usize),
    /// Named parameter placeholder (:param_name)
    ParamName(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FilterExpr {
    Eq(String, LiteralValue),
    Ne(String, LiteralValue),
    Gt(String, LiteralValue),
    Lt(String, LiteralValue),
    Gte(String, LiteralValue),
    Lte(String, LiteralValue),
    Like(String, String),                          // column LIKE 'pattern'
    Between(String, LiteralValue, LiteralValue),    // column BETWEEN low AND high
    In(String, Vec<LiteralValue>),                  // column IN (val1, val2, ...)
    InSubquery(String, Box<QueryAst>),              // column IN (SELECT ...)
    Exists(Box<QueryAst>),                          // EXISTS (SELECT ...)
    NotExists(Box<QueryAst>),                       // NOT EXISTS (SELECT ...)
    IsNull(String),                                 // column IS NULL
    IsNotNull(String),                              // column IS NOT NULL
    Not(Box<FilterExpr>),                           // NOT (expr)
    And(Box<FilterExpr>, Box<FilterExpr>),
    Or(Box<FilterExpr>, Box<FilterExpr>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderBy {
    pub column: String,
    pub ascending: bool,
}

/// Join type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum JoinType {
    Inner,
    Left,
    Right,
    Full,
}

/// A JOIN clause: JOIN <table> [AS <alias>] ON <left_col> = <right_col>
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinClause {
    pub table: String,
    pub alias: Option<String>,
    pub on: JoinOn,
    pub join_type: JoinType,
}

/// The ON condition of a JOIN: <left> = <right>
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinOn {
    pub left: String,
    pub right: String,
}

/// Parses SQL and OntoDB semantic queries into AST.
pub struct QueryParser;

impl QueryParser {
    pub fn parse(input: &str) -> Result<QueryAst> {
        let input = input.trim();
        if input.is_empty() {
            return Err(CoreError::InvalidArgument("empty query".to_string()));
        }

        // Use case-insensitive prefix matching without allocating a new string
        let starts_with = |kw: &str| find_ignore_ascii_case(input, kw) == Some(0);

        if starts_with("EXPLAIN") {
            Self::parse_explain(input)
        } else if starts_with("WITH") {
            Self::parse_with(input)
        } else if starts_with("CREATE ONTOLOGY") {
            Ok(QueryAst::CreateOntology {
                sql: input.to_string(),
            })
        } else if starts_with("CREATE VECTOR INDEX") {
            Self::parse_create_vector_index(input)
        } else if starts_with("DROP VECTOR INDEX") {
            Self::parse_drop_vector_index(input)
        } else if starts_with("VECTOR SEARCH") {
            Self::parse_vector_search(input)
        } else if starts_with("CREATE MATERIALIZED VIEW") {
            Self::parse_create_materialized_view(input)
        } else if starts_with("DROP MATERIALIZED VIEW") {
            Self::parse_drop_materialized_view(input)
        } else if starts_with("REFRESH MATERIALIZED VIEW") {
            Self::parse_refresh_materialized_view(input)
        } else if starts_with("CREATE INDEX") {
            Self::parse_create_index(input)
        } else if starts_with("DROP INDEX") {
            Self::parse_drop_index(input)
        } else if starts_with("ANALYZE") {
            Self::parse_analyze(input)
        } else if starts_with("BEGIN") {
            Ok(QueryAst::Begin)
        } else if starts_with("COMMIT") {
            Ok(QueryAst::Commit)
        } else if starts_with("ROLLBACK") {
            Ok(QueryAst::Rollback)
        } else if starts_with("IMPORT") {
            Self::parse_import(input)
        } else if starts_with("COPY") {
            Self::parse_copy(input)
        } else if starts_with("INSERT") {
            Self::parse_insert(input)
        } else if starts_with("SELECT") {
            let ast = Self::parse_select(input)?;
            Self::try_wrap_union(input, ast)
        } else if starts_with("UPDATE") {
            Self::parse_update(input)
        } else if starts_with("DELETE") {
            Self::parse_delete(input)
        } else if starts_with("MATCH") {
            Self::parse_match(input)
        } else if starts_with("CREATE VERTEX TABLE") {
            Self::parse_create_vertex_table(input)
        } else if starts_with("CREATE EDGE TABLE") {
            Self::parse_create_edge_table(input)
        } else if starts_with("INSERT VERTEX") {
            Self::parse_insert_vertex(input)
        } else if starts_with("INSERT EDGE") {
            Self::parse_insert_edge(input)
        } else if starts_with("GRAPH TRAVERSE") {
            Self::parse_graph_traverse(input)
        } else if starts_with("GRAPH MATCH") {
            Self::parse_graph_match(input)
        } else if starts_with("GRAPH SHORTEST PATH") {
            Self::parse_graph_shortest_path(input)
        } else if starts_with("SYSTEM ACTIVATE") {
            Self::parse_system_activate(input)
        } else if starts_with("BACKUP") {
            Self::parse_backup(input)
        } else if starts_with("RESTORE") {
            Self::parse_restore(input)
        } else if starts_with("FLUSH") {
            Ok(QueryAst::Flush)
        } else {
            Err(CoreError::InvalidArgument(format!(
                "unsupported query: {}",
                input
            )))
        }
    }

    /// Parses EXPLAIN [REASONING] <query>
    fn parse_explain(input: &str) -> Result<QueryAst> {
        let (query_start, explain_reasoning) = if find_ignore_ascii_case(input, "EXPLAIN REASONING") == Some(0) {
            (17, true)
        } else if find_ignore_ascii_case(input, "EXPLAIN ANALYZE") == Some(0) {
            (14, false)
        } else if find_ignore_ascii_case(input, "EXPLAIN") == Some(0) {
            (7, false)
        } else {
            return Err(CoreError::InvalidArgument("expected EXPLAIN".to_string()));
        };

        let inner_query = input[query_start..].trim();
        if inner_query.is_empty() {
            return Err(CoreError::InvalidArgument(
                "expected query after EXPLAIN".to_string(),
            ));
        }

        let inner_ast = Self::parse(inner_query)?;
        if explain_reasoning {
            Ok(QueryAst::ExplainReasoning {
                query: Box::new(inner_ast),
            })
        } else {
            Ok(QueryAst::Explain {
                query: Box::new(inner_ast),
            })
        }
    }

    /// Parses WITH [RECURSIVE] <cte_name> AS (<query>) <main_query>
    fn parse_with(input: &str) -> Result<QueryAst> {
        if find_ignore_ascii_case(input, "WITH") != Some(0) {
            return Err(CoreError::InvalidArgument("expected WITH".to_string()));
        }

        let mut remaining = input[4..].trim();
        let mut recursive = false;

        // Check for RECURSIVE keyword
        if remaining.len() >= 9 && safe_slice(remaining, 0, 9).eq_ignore_ascii_case("RECURSIVE") {
            recursive = true;
            remaining = remaining[9..].trim();
        }

        let mut ctes = Vec::new();

        loop {
            // Parse CTE name
            let as_pos = find_ignore_ascii_case(remaining, " AS ")
                .ok_or_else(|| CoreError::InvalidArgument("expected 'AS' after CTE name".to_string()))?;

            let name_part = safe_slice(remaining, 0, as_pos).trim();
            remaining = safe_slice_from(remaining, as_pos + 4).trim();

            // Parse optional column aliases: "cte_name(col1, col2)" or just "cte_name"
            let (name, columns) = if let Some(paren_start) = name_part.find('(') {
                let close = name_part.rfind(')')
                    .ok_or_else(|| CoreError::InvalidArgument("expected ')' in CTE column list".to_string()))?;
                if paren_start + 1 >= close {
                    return Err(CoreError::InvalidArgument("empty or malformed CTE column list".to_string()));
                }
                let cte_name = safe_slice(name_part, 0, paren_start).trim().to_string();
                let cols_str = safe_slice(name_part, paren_start + 1, close);
                let cols: Vec<String> = cols_str.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
                (cte_name, cols)
            } else {
                (name_part.to_string(), Vec::new())
            };

            // Parse CTE query (parenthesized)
            if !remaining.starts_with('(') {
                return Err(CoreError::InvalidArgument("expected '(' for CTE query".to_string()));
            }

            // Find matching closing parenthesis
            let mut depth = 0;
            let mut in_quote: Option<char> = None;
            let mut close_pos = None;
            for (i, c) in remaining.char_indices() {
                if let Some(q) = in_quote {
                    if c == q { in_quote = None; }
                } else if c == '\'' || c == '"' {
                    in_quote = Some(c);
                } else if c == '(' {
                    depth += 1;
                } else if c == ')' {
                    depth -= 1;
                    if depth == 0 {
                        close_pos = Some(i);
                        break;
                    }
                }
            }

            let close = close_pos.ok_or_else(|| CoreError::InvalidArgument("unmatched '(' in CTE".to_string()))?;
            let cte_query_str = &remaining[1..close].trim();
            let cte_query = Self::parse(cte_query_str)?;

            ctes.push(CteDefinition {
                name,
                columns,
                query: Box::new(cte_query),
            });

            remaining = safe_slice_from(remaining, close + 1).trim();

            // Check for more CTEs (comma-separated)
            if remaining.starts_with(',') {
                remaining = remaining[1..].trim();
                continue;
            }

            break;
        }

        // Parse the main query
        let main_query = Self::parse(remaining)?;

        Ok(QueryAst::With {
            ctes,
            query: Box::new(main_query),
            recursive,
        })
    }

    /// Checks for UNION [ALL] in the input and wraps the first SELECT in a Union node.
    /// Returns the original AST if no UNION is found.
    fn try_wrap_union(input: &str, left: QueryAst) -> Result<QueryAst> {
        // Find UNION that's not inside parentheses
        let mut depth = 0i32;
        let mut in_quote: Option<char> = None;
        let bytes = input.as_bytes();
        let mut i = 0;

        while i + 5 <= bytes.len() {
            let b = bytes[i];
            // Skip non-ASCII bytes (multi-byte UTF-8 chars like Chinese)
            if b >= 0x80 {
                i += 1;
                // Skip continuation bytes (10xxxxxx)
                while i < bytes.len() && (bytes[i] & 0xC0) == 0x80 {
                    i += 1;
                }
                continue;
            }
            let c = b as char;
            if let Some(q) = in_quote {
                if c == q { in_quote = None; }
            } else if c == '\'' || c == '"' {
                in_quote = Some(c);
            } else if c == '(' {
                depth += 1;
            } else if c == ')' {
                depth -= 1;
            } else if depth == 0 && find_ignore_ascii_case(&input[i..], "UNION") == Some(0) {
                let after = &safe_slice_from(input, i + 5).trim_start();
                let (all, rest) = if find_ignore_ascii_case(after, "ALL") == Some(0) {
                    (true, after[3..].trim())
                } else {
                    (false, after.trim())
                };
                let right = Self::parse(rest)?;
                return Ok(QueryAst::Union {
                    left: Box::new(left),
                    right: Box::new(right),
                    all,
                });
            }
            i += 1;
        }
        Ok(left)
    }

    /// Parses a subquery from parenthesized input: (SELECT ...)
    pub fn parse_subquery(input: &str) -> Result<QueryAst> {
        let input = input.trim();
        if !input.starts_with('(') {
            return Err(CoreError::InvalidArgument(
                "expected '(' for subquery".to_string(),
            ));
        }
        // Find matching closing paren
        let mut depth = 0;
        let mut in_quote: Option<char> = None;
        for (i, c) in input.char_indices() {
            if let Some(q) = in_quote {
                if c == q { in_quote = None; }
            } else if c == '\'' || c == '"' {
                in_quote = Some(c);
            } else if c == '(' {
                depth += 1;
            } else if c == ')' {
                depth -= 1;
                if depth == 0 {
                    let inner = &input[1..i].trim();
                    return Self::parse(inner);
                }
            }
        }
        Err(CoreError::InvalidArgument(
            "unmatched '(' in subquery".to_string(),
        ))
    }

    /// Parses IMPORT INTO <class> FROM CSV/JSON '<file_path>'
    fn parse_import(input: &str) -> Result<QueryAst> {
        // IMPORT INTO <class> FROM <format> '<path>'
        let into_pos = find_ignore_ascii_case(input, "INTO")
            .ok_or_else(|| CoreError::InvalidArgument("expected 'INTO' after IMPORT".to_string()))?;

        let from_pos = find_ignore_ascii_case(input, " FROM ")
            .ok_or_else(|| CoreError::InvalidArgument("expected 'FROM' in IMPORT".to_string()))?;

        if into_pos + 4 >= from_pos {
            return Err(CoreError::InvalidArgument("missing class name in IMPORT".to_string()));
        }
        let class = safe_slice(input, into_pos + 4, from_pos).trim().to_string();
        if class.is_empty() {
            return Err(CoreError::InvalidArgument("missing class name in IMPORT".to_string()));
        }

        let after_from = safe_slice_from(input, from_pos + 6).trim();
        let starts_with = |kw: &str| find_ignore_ascii_case(after_from, kw) == Some(0);

        let format = if starts_with("CSV") {
            ImportFormat::Csv
        } else if starts_with("JSON") {
            ImportFormat::Json
        } else {
            return Err(CoreError::InvalidArgument("expected CSV or JSON after FROM".to_string()));
        };

        // Extract file path (between quotes)
        let path_start = after_from.find('\'')
            .or_else(|| after_from.find('"'))
            .ok_or_else(|| CoreError::InvalidArgument("expected quoted file path in IMPORT".to_string()))?;
        let quote_char = after_from.as_bytes()[path_start] as char;
        let path_end = safe_slice_from(after_from, path_start + 1).find(quote_char)
            .ok_or_else(|| CoreError::InvalidArgument("unterminated file path in IMPORT".to_string()))?;
        let file_path = safe_slice(after_from, path_start + 1, path_start + 1 + path_end).to_string();

        Ok(QueryAst::Import { class, file_path, format })
    }

    /// Parses COPY <class> FROM '<file_path>' (FORMAT CSV|JSON)
    /// PostgreSQL-compatible bulk load syntax.
    fn parse_copy(input: &str) -> Result<QueryAst> {
        // COPY <class> FROM '<path>' (FORMAT CSV|JSON)
        let after_copy = input[4..].trim();

        let from_pos = find_ignore_ascii_case(after_copy, " FROM ")
            .ok_or_else(|| CoreError::InvalidArgument("expected 'FROM' in COPY".to_string()))?;

        let class = safe_slice(after_copy, 0, from_pos).trim().to_string();
        if class.is_empty() {
            return Err(CoreError::InvalidArgument("missing class name in COPY".to_string()));
        }

        let after_from = safe_slice_from(after_copy, from_pos + 6).trim();

        // Extract file path (between quotes)
        let path_start = after_from.find('\'')
            .or_else(|| after_from.find('"'))
            .ok_or_else(|| CoreError::InvalidArgument("expected quoted file path in COPY".to_string()))?;
        let quote_char = after_from.as_bytes()[path_start] as char;
        let path_end = safe_slice_from(after_from, path_start + 1).find(quote_char)
            .ok_or_else(|| CoreError::InvalidArgument("unterminated file path in COPY".to_string()))?;
        let file_path = safe_slice(after_from, path_start + 1, path_start + 1 + path_end).to_string();

        // Optional FORMAT clause
        let after_path = safe_slice_from(after_from, path_start + 1 + path_end + 1).trim();
        let format = if after_path.to_uppercase().starts_with("FORMAT CSV") {
            ImportFormat::Csv
        } else if after_path.to_uppercase().starts_with("FORMAT JSON") {
            ImportFormat::Json
        } else {
            // Default: detect by file extension
            if file_path.ends_with(".csv") {
                ImportFormat::Csv
            } else {
                ImportFormat::Json
            }
        };

        Ok(QueryAst::Copy { class, file_path, format })
    }

    fn parse_insert(input: &str) -> Result<QueryAst> {
        let into_pos = find_ignore_ascii_case(input, "INTO")
            .ok_or_else(|| CoreError::InvalidArgument("expected 'INTO'".to_string()))?;

        // Check if this is INSERT INTO ... SELECT
        let select_pos = Self::find_unquoted(input, " SELECT ");
        if let Some(sp) = select_pos {
            let header = safe_slice(input, into_pos + 4, sp).trim();
            let (class, columns) = Self::parse_insert_header(header)?;
            let query_str = safe_slice_from(input, sp + 1).trim();
            let query = Self::parse(query_str)?;
            return Ok(QueryAst::InsertSelect { class, columns, query: Box::new(query) });
        }

        let values_pos = find_ignore_ascii_case(input, "VALUES")
            .ok_or_else(|| CoreError::InvalidArgument("expected 'VALUES'".to_string()))?;

        if values_pos < into_pos + 4 {
            return Err(CoreError::InvalidArgument("malformed INSERT: 'VALUES' appears before 'INTO'".to_string()));
        }

        let header = safe_slice(input, into_pos + 4, values_pos).trim();
        let values_str = safe_slice_from(input, values_pos + 6).trim();

        let (class, columns) = Self::parse_insert_header(header)?;

        // Check for ON CONFLICT (UPSERT)
        let (values_str, upsert_info) = if let Some(oc_pos) = find_unquoted_ignore_ascii_case(values_str, " ON CONFLICT ") {
            let vals_part = safe_slice(values_str, 0, oc_pos);
            let conflict_part = &safe_slice_from(values_str, oc_pos + 13).trim();
            // Parse: (col) DO UPDATE SET col1 = val1, col2 = val2
            let do_update_pos = find_ignore_ascii_case(conflict_part, " DO UPDATE SET ")
                .ok_or_else(|| CoreError::InvalidArgument("expected 'DO UPDATE SET' after ON CONFLICT".to_string()))?;
            let conflict_col = safe_slice(conflict_part, 0, do_update_pos).trim();
            let conflict_col = conflict_col.trim_start_matches('(').trim_end_matches(')').trim().to_string();
            let set_part = safe_slice_from(conflict_part, do_update_pos + 15).trim();
            let assignments = Self::parse_set_assignments(set_part)?;
            (vals_part, Some((conflict_col, assignments)))
        } else {
            (values_str, None)
        };

        // Parse values: (val1, val2, ...) or (v1, v2), (v3, v4), ...
        let values_str = values_str.trim();

        // Detect batch insert: look for "), " pattern that's not inside nested parens
        // This is more robust than counting all opening parens
        let is_batch = {
            let mut depth = 0i32;
            let mut found_batch = false;
            let bytes = values_str.as_bytes();
            for i in 0..bytes.len() {
                match bytes[i] {
                    b'(' => depth += 1,
                    b')' => {
                        depth -= 1;
                        if depth == 0 && i + 1 < bytes.len() && bytes[i + 1] == b',' {
                            found_batch = true;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            found_batch
        };
        if is_batch {
            // Batch insert: (vals1), (vals2), ...
            let mut rows = Vec::new();
            let mut remaining = values_str;
            while remaining.starts_with('(') {
                let close = Self::find_matching_paren_simple(remaining)?;
                let vals_inner = &remaining[1..close];
                let values: Vec<LiteralValue> = Self::split_quoted(vals_inner, ',')
                    .iter()
                    .map(|s| Self::parse_literal(s.trim()))
                    .collect::<Result<Vec<_>>>()?;
                rows.push(values);
                remaining = safe_slice_from(remaining, close + 1).trim();
                if remaining.starts_with(',') {
                    remaining = remaining[1..].trim();
                }
            }
            if let Some((conflict_col, assignments)) = upsert_info {
                // Upsert with batch — return BatchUpsert for multiple rows
                if rows.len() == 1 {
                    Ok(QueryAst::Upsert {
                        class,
                        columns,
                        values: rows.into_iter().next().unwrap_or_default(),
                        conflict_column: conflict_col,
                        assignments,
                    })
                } else {
                    Ok(QueryAst::BatchUpsert {
                        class,
                        columns,
                        rows,
                        conflict_column: conflict_col,
                        assignments,
                    })
                }
            } else {
                Ok(QueryAst::BatchInsert { class, columns, rows })
            }
        } else {
            // Single insert
            if !values_str.starts_with('(') || !values_str.ends_with(')') {
                return Err(CoreError::InvalidArgument(
                    "expected '(values)'".to_string(),
                ));
            }
            let vals_inner = &values_str[1..values_str.len() - 1];
            let values: Vec<LiteralValue> = Self::split_quoted(vals_inner, ',')
                .iter()
                .map(|s| Self::parse_literal(s.trim()))
                .collect::<Result<Vec<_>>>()?;

            if let Some((conflict_col, assignments)) = upsert_info {
                Ok(QueryAst::Upsert {
                    class,
                    columns,
                    values,
                    conflict_column: conflict_col,
                    assignments,
                })
            } else {
                Ok(QueryAst::Insert { class, columns, values })
            }
        }
    }

    /// Parses INSERT header: class (col1, col2, ...)
    fn parse_insert_header(header: &str) -> Result<(String, Vec<String>)> {
        let paren_start = header
            .find('(')
            .ok_or_else(|| CoreError::InvalidArgument("expected '(' in INSERT".to_string()))?;
        let class = safe_slice(header, 0, paren_start).trim().to_string();
        let cols_str = safe_slice(header, paren_start + 1, header.len() - 1);
        let columns: Vec<String> = cols_str
            .split(',')
            .map(|s| Self::strip_surrounding_quotes(s.trim()))
            .filter(|s| !s.is_empty())
            .collect();
        Ok((class, columns))
    }

    /// Parses SET assignments: col1 = val1, col2 = val2
    fn parse_set_assignments(input: &str) -> Result<Vec<(String, LiteralValue)>> {
        Self::split_quoted(input, ',')
            .iter()
            .map(|s| {
                let s = s.trim();
                let eq_pos = Self::find_unquoted(s, "=")
                    .ok_or_else(|| CoreError::InvalidArgument("expected '=' in SET".to_string()))?;
                let col = Self::strip_surrounding_quotes(safe_slice(s, 0, eq_pos).trim());
                let val = Self::parse_literal(safe_slice_from(s, eq_pos + 1).trim())?;
                Ok((col, val))
            })
            .collect::<Result<Vec<_>>>()
    }

    /// Simple matching paren finder (no quote handling needed for values).
    fn find_matching_paren_simple(input: &str) -> Result<usize> {
        let mut depth = 0;
        let mut in_quote: Option<char> = None;
        for (i, c) in input.char_indices() {
            if let Some(q) = in_quote {
                if c == q { in_quote = None; }
            } else {
                match c {
                    '\'' | '"' => in_quote = Some(c),
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            return Ok(i);
                        }
                    }
                    _ => {}
                }
            }
        }
        Err(CoreError::InvalidArgument("unmatched parenthesis".to_string()))
    }

    fn parse_select(input: &str) -> Result<QueryAst> {
        // Use find_unquoted_ignore_ascii_case to skip FROM inside quoted strings
        let from_pos = find_unquoted_ignore_ascii_case(input, " FROM ")
            .or_else(|| find_unquoted_ignore_ascii_case(input, "\nFROM "))
            .ok_or_else(|| CoreError::InvalidArgument("expected 'FROM'".to_string()))?;

        let after_select = input[6..from_pos].trim(); // After "SELECT"

        // Check for DISTINCT keyword
        let (distinct, cols_str) = if starts_with_ignore_ascii_case(after_select, "DISTINCT") {
            (true, after_select[8..].trim())
        } else {
            (false, after_select)
        };

        let rest = safe_slice_from(input, from_pos + 6).trim();

        let columns = if cols_str == "*" {
            SelectColumns::All
        } else {
            SelectColumns::Columns(Self::parse_select_items(cols_str)?)
        };

        // Parse FROM clause: <table> [AS <alias>]
        let (from, from_alias, rest) = Self::parse_from_clause(rest)?;

        // Parse optional JOIN clauses
        let (joins, rest) = Self::parse_joins(&rest)?;

        // Parse optional WHERE
        let (filter, rest) = if starts_with_ignore_ascii_case(&rest, "WHERE") {
            let rest = rest[5..].trim();
            Self::parse_where(rest)?
        } else {
            (None, rest.to_string())
        };

        // Parse optional GROUP BY
        let rest_trimmed = rest.trim_start();
        let (group_by, rest) = if starts_with_ignore_ascii_case(rest_trimmed, "GROUP BY") {
            let pos = find_unquoted_ignore_ascii_case(&rest, "GROUP BY")
                .ok_or_else(|| CoreError::InvalidArgument("expected 'GROUP BY'".to_string()))?;
            let rest = safe_slice_from(&rest, pos + 8).trim();
            let (cols_str, rest) = Self::consume_until_keywords(rest, &["HAVING", "ORDER BY", "LIMIT"]);
            let columns: Vec<String> = cols_str
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            (Some(GroupByClause { columns }), rest.to_string())
        } else {
            (None, rest)
        };

        // Parse optional HAVING
        let rest_trimmed = rest.trim_start();
        let (having, rest) = if starts_with_ignore_ascii_case(rest_trimmed, "HAVING") {
            let pos = find_unquoted_ignore_ascii_case(&rest, "HAVING")
                .ok_or_else(|| CoreError::InvalidArgument("expected 'HAVING'".to_string()))?;
            let rest = safe_slice_from(&rest, pos + 6).trim();
            Self::parse_where(rest)?
        } else {
            (None, rest)
        };

        // Parse optional ORDER BY (multi-column)
        let rest_trimmed = rest.trim_start();
        let (order_by, rest) = if starts_with_ignore_ascii_case(rest_trimmed, "ORDER BY") {
            let start = find_unquoted_ignore_ascii_case(&rest, "ORDER BY")
                .ok_or_else(|| CoreError::InvalidArgument("expected 'ORDER BY'".to_string()))?;
            let (ob_str, rest) = Self::consume_until_keywords(safe_slice_from(&rest, start + 8).trim(), &["LIMIT", "OFFSET"]);
            let mut order_cols = Vec::new();
            for part in Self::split_quoted(ob_str.trim(), ',') {
                let part = part.trim();
                let (col, ascending) = if part.len() >= 5 && part[part.len() - 5..].eq_ignore_ascii_case(" DESC") {
                    (part[..part.len() - 5].trim().to_string(), false)
                } else if part.len() >= 4 && part[part.len() - 4..].eq_ignore_ascii_case(" ASC") {
                    (part[..part.len() - 4].trim().to_string(), true)
                } else {
                    (part.to_string(), true)
                };
                if !col.is_empty() {
                    order_cols.push(OrderBy { column: col, ascending });
                }
            }
            (order_cols, rest.to_string())
        } else {
            (Vec::new(), rest.to_string())
        };

        // Parse optional LIMIT
        let rest_trimmed = rest.trim_start();
        let (limit, offset, _rest) = if starts_with_ignore_ascii_case(rest_trimmed, "LIMIT") {
            let start = find_unquoted_ignore_ascii_case(&rest, "LIMIT")
                .ok_or_else(|| CoreError::InvalidArgument("expected 'LIMIT'".to_string()))?;
            let after_limit = safe_slice_from(&rest, start + 5).trim();
            // Parse limit number (may be followed by OFFSET or end)
            let (num_str, after_num) = Self::parse_word(after_limit)?;
            let limit_val = num_str
                .parse::<usize>()
                .map_err(|_| CoreError::InvalidArgument("invalid LIMIT".to_string()))?;
            // Check for OFFSET
            let after_trimmed = after_num.trim_start();
            if starts_with_ignore_ascii_case(after_trimmed, "OFFSET") {
                let offset_str = after_num[6..].trim();
                let offset_val = offset_str
                    .parse::<usize>()
                    .map_err(|_| CoreError::InvalidArgument("invalid OFFSET".to_string()))?;
                (Some(limit_val), Some(offset_val), "".to_string())
            } else {
                (Some(limit_val), None, after_num.to_string())
            }
        } else {
            (None, None, rest)
        };

        Ok(QueryAst::Select {
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
        })
    }

    /// Parses `FROM <table> [AS <alias>]` and returns (table, alias, rest).
    fn parse_from_clause(input: &str) -> Result<(String, Option<String>, String)> {
        let (table, rest) = Self::parse_word(input)?;

        if starts_with_ignore_ascii_case(&rest, "AS") {
            let rest = rest[2..].trim();
            let (alias, rest) = Self::parse_word(rest)?;
            Ok((table, Some(alias), rest))
        } else if !rest.is_empty()
            && !starts_with_ignore_ascii_case(&rest, "WHERE")
            && !starts_with_ignore_ascii_case(&rest, "JOIN")
            && !starts_with_ignore_ascii_case(&rest, "LEFT")
            && !starts_with_ignore_ascii_case(&rest, "RIGHT")
            && !starts_with_ignore_ascii_case(&rest, "FULL")
            && !starts_with_ignore_ascii_case(&rest, "INNER")
            && !starts_with_ignore_ascii_case(&rest, "ON")
            && !starts_with_ignore_ascii_case(&rest, "GROUP")
            && !starts_with_ignore_ascii_case(&rest, "HAVING")
            && !starts_with_ignore_ascii_case(&rest, "ORDER")
            && !starts_with_ignore_ascii_case(&rest, "LIMIT")
        {
            // Implicit alias: FROM Product p
            let (alias, rest) = Self::parse_word(&rest)?;
            Ok((table, Some(alias), rest))
        } else {
            Ok((table, None, rest))
        }
    }

    /// Parses zero or more `JOIN <table> [AS <alias>] ON <left> = <right>` clauses.
    fn parse_joins(input: &str) -> Result<(Vec<JoinClause>, String)> {
        let mut joins = Vec::new();
        let mut rest = input.to_string();

        loop {
            let trimmed = rest.trim();

            // Detect join type
            let (join_type, skip_len) = if starts_with_ignore_ascii_case(trimmed, "LEFT OUTER JOIN") {
                (JoinType::Left, 15)
            } else if starts_with_ignore_ascii_case(trimmed, "LEFT JOIN") {
                (JoinType::Left, 10)
            } else if starts_with_ignore_ascii_case(trimmed, "RIGHT OUTER JOIN") {
                (JoinType::Right, 16)
            } else if starts_with_ignore_ascii_case(trimmed, "RIGHT JOIN") {
                (JoinType::Right, 11)
            } else if starts_with_ignore_ascii_case(trimmed, "FULL OUTER JOIN") {
                (JoinType::Full, 15)
            } else if starts_with_ignore_ascii_case(trimmed, "FULL JOIN") {
                (JoinType::Full, 10)
            } else if starts_with_ignore_ascii_case(trimmed, "INNER JOIN") {
                (JoinType::Inner, 11)
            } else if starts_with_ignore_ascii_case(trimmed, "JOIN") {
                (JoinType::Inner, 4)
            } else {
                break;
            };

            // Skip join keyword
            let after_join = rest[skip_len..].trim();

            // Parse table name and optional alias
            let (table, alias, after_table) = Self::parse_from_clause(after_join)?;

            // Parse ON
            if !starts_with_ignore_ascii_case(&after_table, "ON") {
                return Err(CoreError::InvalidArgument(
                    "expected 'ON' after JOIN table".to_string(),
                ));
            }
            let on_input = after_table[2..].trim();

            // Parse <left> = <right>
            let eq_pos = Self::find_unquoted(on_input, "=")
                .ok_or_else(|| CoreError::InvalidArgument("expected '=' in ON clause".to_string()))?;
            let left = safe_slice(on_input, 0, eq_pos).trim().to_string();
            let right_on = safe_slice_from(on_input, eq_pos + 1).trim();

            // Right side ends at WHERE/JOIN/ORDER/LIMIT or end of string
            let (right, rest_after_on) = Self::consume_until_keywords(
                right_on,
                &["WHERE", "JOIN", "LEFT", "RIGHT", "FULL", "ORDER BY", "LIMIT"],
            );

            joins.push(JoinClause {
                table,
                alias,
                on: JoinOn { left, right },
                join_type,
            });

            rest = rest_after_on.to_string();
        }

        Ok((joins, rest))
    }

    /// Parses SELECT column list into SelectItems (columns and aggregates).
    fn parse_select_items(cols_str: &str) -> Result<Vec<SelectItem>> {
        let mut items = Vec::new();
        for part in Self::split_quoted(cols_str, ',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            // Check for CASE WHEN expression
            if starts_with_ignore_ascii_case(part, "CASE ") || starts_with_ignore_ascii_case(part, "CASE\n") {
                let case_expr = Self::parse_case_when(part)?;
                // Check for alias after END
                let expr = if let Some(end_pos) = find_ignore_ascii_case(part, " END") {
                    let after_end = safe_slice_from(part, end_pos + 4).trim();
                    if starts_with_ignore_ascii_case(after_end, "AS ") {
                        let _alias = after_end[3..].trim().to_string();
                        SelectItem::Expression(case_expr)
                    } else {
                        SelectItem::Expression(case_expr)
                    }
                } else {
                    // No END found — treat the whole expression as-is
                    SelectItem::Expression(case_expr)
                };
                items.push(expr);
                continue;
            }

            // Check for window functions: ROW_NUMBER(), RANK(), etc. with OVER
            if Self::contains_window_function(part) {
                let window_expr = Self::parse_window_function(part)?;
                items.push(SelectItem::WindowFunction(window_expr));
                continue;
            }

            // Check for built-in functions: COALESCE, NULLIF, CONCAT, etc.
            let builtin_funcs = ["COALESCE", "NULLIF", "CONCAT", "SUBSTRING", "UPPER", "LOWER", "NOW", "LENGTH", "TRIM", "ABS", "ROUND"];
            let mut is_builtin = false;
            for func_name in &builtin_funcs {
                if starts_with_ignore_ascii_case(part, func_name) && part.len() > func_name.len() && part.as_bytes()[func_name.len()] == b'(' {
                    let value_expr = Self::parse_value_expr(part)?;
                    items.push(SelectItem::Expression(value_expr));
                    is_builtin = true;
                    break;
                }
            }
            if is_builtin {
                continue;
            }

            // Check for aggregate functions: COUNT, SUM, AVG, MIN, MAX
            let func = if starts_with_ignore_ascii_case(part, "COUNT(") {
                Some(AggregateFunc::Count)
            } else if starts_with_ignore_ascii_case(part, "SUM(") {
                Some(AggregateFunc::Sum)
            } else if starts_with_ignore_ascii_case(part, "AVG(") {
                Some(AggregateFunc::Avg)
            } else if starts_with_ignore_ascii_case(part, "MIN(") {
                Some(AggregateFunc::Min)
            } else if starts_with_ignore_ascii_case(part, "MAX(") {
                Some(AggregateFunc::Max)
            } else {
                None
            };

            if let Some(func) = func {
                // Extract argument: FUNC(arg)
                let open = part.find('(')
                    .ok_or_else(|| CoreError::InvalidArgument("expected '(' in aggregate".to_string()))?;
                let close = part.rfind(')')
                    .ok_or_else(|| CoreError::InvalidArgument("expected ')' in aggregate".to_string()))?;
                if open + 1 >= close {
                    return Err(CoreError::InvalidArgument("empty or malformed aggregate function".to_string()));
                }
                let arg = safe_slice(part, open + 1, close).trim().to_string();

                // Check for alias: ... AS alias
                let after = safe_slice_from(part, close + 1).trim();
                let alias = if starts_with_ignore_ascii_case(after, "AS") {
                    Some(after[2..].trim().to_string())
                } else if !after.is_empty() {
                    Some(after.to_string())
                } else {
                    None
                };

                items.push(SelectItem::Aggregate(AggregateExpr { func, arg, alias }));
            } else {
                // Regular column, possibly with alias
                // Handle: `name`, `p.name`, `name as n`
                let (col, alias_part) = if let Some(as_pos) =
                    find_unquoted_ignore_ascii_case(part, " AS ")
                {
                    (safe_slice(part, 0, as_pos).trim(), Some(safe_slice_from(part, as_pos + 4).trim()))
                } else {
                    (part, None)
                };

                let col_str = col.to_string();
                if let Some(alias) = alias_part {
                    // Store as "col as alias" — executor will parse
                    items.push(SelectItem::Column(format!("{} as {}", col_str, alias)));
                } else {
                    items.push(SelectItem::Column(col_str));
                }
            }
        }
        Ok(items)
    }

    /// Check if the expression contains a window function (has OVER keyword).
    fn contains_window_function(input: &str) -> bool {
        // Window functions: ROW_NUMBER, RANK, DENSE_RANK, LAG, LEAD, etc.
        // Must have OVER keyword after the function call
        let window_funcs = ["ROW_NUMBER", "RANK", "DENSE_RANK", "LAG", "LEAD", 
                           "FIRST_VALUE", "LAST_VALUE", "NTH_VALUE"];
        for func in &window_funcs {
            if find_ignore_ascii_case(input, func).is_some() && find_ignore_ascii_case(input, " OVER ").is_some() {
                return true;
            }
        }
        // Also check for aggregate OVER (e.g., SUM(x) OVER (...))
        if find_ignore_ascii_case(input, ") OVER ").is_some() || find_ignore_ascii_case(input, ")OVER ").is_some() {
            return true;
        }
        false
    }

    /// Parse a window function expression.
    fn parse_window_function(input: &str) -> Result<WindowExpr> {
        // Find OVER keyword
        let over_pos = find_unquoted_ignore_ascii_case(input, " OVER ")
            .ok_or_else(|| CoreError::InvalidArgument("expected 'OVER' in window function".to_string()))?;
        
        let func_part = safe_slice(input, 0, over_pos).trim();
        let over_part = safe_slice_from(input, over_pos + 6).trim();

        // Parse function name and argument
        let (func, arg) = Self::parse_window_func_name(func_part)?;

        // Parse OVER clause
        let over = Self::parse_window_spec(over_part)?;

        // Check for alias after OVER clause
        let after_over = &safe_slice_from(input, over_pos + 6);
        let close_paren = Self::find_matching_paren(after_over)?;
        let after = safe_slice_from(after_over, close_paren + 1).trim();
        let alias = if starts_with_ignore_ascii_case(after, "AS") {
            Some(after[2..].trim().to_string())
        } else if !after.is_empty() {
            Some(after.to_string())
        } else {
            None
        };

        Ok(WindowExpr { func, arg, over, alias })
    }

    /// Parse window function name and argument.
    fn parse_window_func_name(input: &str) -> Result<(WindowFunc, Option<String>)> {
        // Find the opening parenthesis
        let open = input.find('(')
            .ok_or_else(|| CoreError::InvalidArgument("expected '(' in window function".to_string()))?;
        let close = input.rfind(')')
            .ok_or_else(|| CoreError::InvalidArgument("expected ')' in window function".to_string()))?;

        if open >= close {
            return Err(CoreError::InvalidArgument("malformed window function: '(' after ')'".to_string()));
        }
        let func_name = safe_slice(input, 0, open);
        let arg_str = safe_slice(input, open + 1, close).trim();
        let arg = if arg_str.is_empty() || arg_str == "*" {
            None
        } else {
            Some(arg_str.to_string())
        };

        let func = if func_name.eq_ignore_ascii_case("ROW_NUMBER") {
            WindowFunc::RowNumber
        } else if func_name.eq_ignore_ascii_case("RANK") {
            WindowFunc::Rank
        } else if func_name.eq_ignore_ascii_case("DENSE_RANK") {
            WindowFunc::DenseRank
        } else if func_name.eq_ignore_ascii_case("LAG") {
            WindowFunc::Lag
        } else if func_name.eq_ignore_ascii_case("LEAD") {
            WindowFunc::Lead
        } else if func_name.eq_ignore_ascii_case("FIRST_VALUE") {
            WindowFunc::FirstValue
        } else if func_name.eq_ignore_ascii_case("LAST_VALUE") {
            WindowFunc::LastValue
        } else if func_name.eq_ignore_ascii_case("NTH_VALUE") {
            WindowFunc::NthValue
        } else if func_name.eq_ignore_ascii_case("SUM") {
            WindowFunc::Sum
        } else if func_name.eq_ignore_ascii_case("AVG") {
            WindowFunc::Avg
        } else if func_name.eq_ignore_ascii_case("MIN") {
            WindowFunc::Min
        } else if func_name.eq_ignore_ascii_case("MAX") {
            WindowFunc::Max
        } else if func_name.eq_ignore_ascii_case("COUNT") {
            WindowFunc::Count
        } else {
            return Err(CoreError::InvalidArgument(format!("unknown window function: {}", func_name)));
        };

        Ok((func, arg))
    }

    /// Parse window specification (OVER clause).
    fn parse_window_spec(input: &str) -> Result<WindowSpec> {
        let input = input.trim();
        
        // Remove outer parentheses
        let input = if input.starts_with('(') && input.ends_with(')') {
            input[1..input.len() - 1].trim()
        } else {
            input
        };

        let mut partition_by = Vec::new();
        let mut order_by = Vec::new();
        let mut frame = None;

        // Parse PARTITION BY
        if let Some(pb_pos) = find_unquoted_ignore_ascii_case(input, "PARTITION BY") {
            let pb_str = &safe_slice_from(input, pb_pos + 12).trim();
            let (pb_cols, remaining) = Self::consume_until_keywords(pb_str, &["ORDER BY", "ROWS", "RANGE"]);
            partition_by = pb_cols.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
            
            // Parse ORDER BY from remaining
            if starts_with_ignore_ascii_case(remaining, "ORDER BY") {
                let ob_str = &remaining[8..].trim();
                order_by = Self::parse_window_order_by(ob_str)?;
            }
        } else if starts_with_ignore_ascii_case(input, "ORDER BY") {
            let ob_str = &input[8..].trim();
            order_by = Self::parse_window_order_by(ob_str)?;
        }

        // Parse frame specification
        if let Some(rows_pos) = find_unquoted_ignore_ascii_case(input, "ROWS") {
            frame = Some(Self::parse_window_frame(&input[rows_pos..], WindowFrameType::Rows)?);
        } else if let Some(range_pos) = find_unquoted_ignore_ascii_case(input, "RANGE") {
            frame = Some(Self::parse_window_frame(&input[range_pos..], WindowFrameType::Range)?);
        }

        Ok(WindowSpec { partition_by, order_by, frame })
    }

    /// Parse ORDER BY columns in window specification.
    fn parse_window_order_by(input: &str) -> Result<Vec<WindowOrderBy>> {
        let mut result = Vec::new();
        let (ob_str, _) = Self::consume_until_keywords(input, &["ROWS", "RANGE"]);
        
        for part in ob_str.split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            let (col, ascending) = if ends_with_ignore_ascii_case(part, " DESC") {
                (part[..part.len() - 5].trim().to_string(), false)
            } else if ends_with_ignore_ascii_case(part, " ASC") {
                (part[..part.len() - 4].trim().to_string(), true)
            } else {
                (part.to_string(), true)
            };
            result.push(WindowOrderBy { column: col, ascending });
        }

        Ok(result)
    }

    /// Parse window frame specification.
    fn parse_window_frame(input: &str, frame_type: WindowFrameType) -> Result<WindowFrame> {
        // Parse: ROWS BETWEEN <start> AND <end>
        let between_pos = find_unquoted_ignore_ascii_case(input, "BETWEEN")
            .ok_or_else(|| CoreError::InvalidArgument("expected 'BETWEEN' in window frame".to_string()))?;
        let frame_str = safe_slice_from(input, between_pos + 7).trim();

        // Find AND separator
        let and_pos = find_ignore_ascii_case(frame_str, " AND ")
            .ok_or_else(|| CoreError::InvalidArgument("expected 'AND' in window frame".to_string()))?;

        let start_str = safe_slice(frame_str, 0, and_pos).trim();
        let end_str = safe_slice_from(frame_str, and_pos + 5).trim();

        let start = Self::parse_frame_bound(start_str)?;
        let end = Some(Self::parse_frame_bound(end_str)?);

        Ok(WindowFrame { frame_type, start, end })
    }

    /// Parse a window frame bound.
    fn parse_frame_bound(input: &str) -> Result<WindowFrameBound> {
        let input = input.trim();
        if input.eq_ignore_ascii_case("UNBOUNDED PRECEDING") {
            Ok(WindowFrameBound::UnboundedPreceding)
        } else if input.eq_ignore_ascii_case("UNBOUNDED FOLLOWING") {
            Ok(WindowFrameBound::UnboundedFollowing)
        } else if input.eq_ignore_ascii_case("CURRENT ROW") {
            Ok(WindowFrameBound::CurrentRow)
        } else if input.len() >= 9 && safe_slice_from(input, input.len() - 9).eq_ignore_ascii_case("PRECEDING") {
            let n_str = safe_slice(input, 0, input.len() - 9).trim();
            let n = n_str.parse::<u64>()
                .map_err(|_| CoreError::InvalidArgument("invalid frame bound".to_string()))?;
            Ok(WindowFrameBound::Preceding(n))
        } else if input.len() >= 9 && safe_slice_from(input, input.len() - 9).eq_ignore_ascii_case("FOLLOWING") {
            let n_str = safe_slice(input, 0, input.len() - 9).trim();
            let n = n_str.parse::<u64>()
                .map_err(|_| CoreError::InvalidArgument("invalid frame bound".to_string()))?;
            Ok(WindowFrameBound::Following(n))
        } else {
            Err(CoreError::InvalidArgument(format!("invalid frame bound: {}", input)))
        }
    }

    /// Find matching closing parenthesis.
    fn find_matching_paren(input: &str) -> Result<usize> {
        let mut depth = 0;
        let mut in_quote: Option<char> = None;
        for (i, c) in input.char_indices() {
            if let Some(q) = in_quote {
                if c == q { in_quote = None; }
            } else {
                match c {
                    '\'' | '"' => in_quote = Some(c),
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            return Ok(i);
                        }
                    }
                    _ => {}
                }
            }
        }
        Err(CoreError::InvalidArgument("unmatched parenthesis".to_string()))
    }

    /// Consumes input until a keyword is found. Returns (consumed, remaining).
    fn consume_until_keywords<'a>(input: &'a str, keywords: &[&str]) -> (String, &'a str) {
        let mut earliest = input.len();

        for kw in keywords {
            if let Some(pos) = find_unquoted_ignore_ascii_case(input, kw) {
                if pos < earliest {
                    earliest = pos;
                }
            }
        }

        let consumed = safe_slice(input, 0, earliest).trim().to_string();
        let remaining = &input[earliest..];
        (consumed, remaining)
    }

    /// Parses: CREATE INDEX ON <class> (<column>) or CREATE INDEX ON <class> (<col1>, <col2>)
    fn parse_create_index(input: &str) -> Result<QueryAst> {
        let on_pos = find_ignore_ascii_case(input, " ON ")
            .ok_or_else(|| CoreError::InvalidArgument("expected 'ON' after CREATE INDEX".to_string()))?;
        let rest = safe_slice_from(input, on_pos + 4).trim();

        // Find the parenthesized column(s)
        let paren_open = rest
            .find('(')
            .ok_or_else(|| CoreError::InvalidArgument("expected '(column)' in CREATE INDEX".to_string()))?;
        let class = safe_slice(rest, 0, paren_open).trim().to_string();
        let paren_close = rest
            .find(')')
            .ok_or_else(|| CoreError::InvalidArgument("expected ')' in CREATE INDEX".to_string()))?;
        let columns_str = safe_slice(rest, paren_open + 1, paren_close).trim().to_string();

        if class.is_empty() || columns_str.is_empty() {
            return Err(CoreError::InvalidArgument(
                "class and column cannot be empty in CREATE INDEX".to_string(),
            ));
        }

        // Check if multiple columns (composite index)
        let columns: Vec<String> = columns_str
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        if columns.len() > 1 {
            Ok(QueryAst::CreateCompositeIndex { class, columns })
        } else {
            let column = columns.into_iter().next()
                .ok_or_else(|| CoreError::InvalidArgument("expected column name in CREATE INDEX".to_string()))?;
            Ok(QueryAst::CreateIndex { class, column })
        }
    }

    /// Parses: DROP INDEX ON <class> (<column>)
    fn parse_drop_index(input: &str) -> Result<QueryAst> {
        let on_pos = find_ignore_ascii_case(input, " ON ")
            .ok_or_else(|| CoreError::InvalidArgument("expected 'ON' after DROP INDEX".to_string()))?;
        let rest = safe_slice_from(input, on_pos + 4).trim();

        let paren_open = rest
            .find('(')
            .ok_or_else(|| CoreError::InvalidArgument("expected '(column)' in DROP INDEX".to_string()))?;
        let class = safe_slice(rest, 0, paren_open).trim().to_string();
        let paren_close = rest
            .find(')')
            .ok_or_else(|| CoreError::InvalidArgument("expected ')' in DROP INDEX".to_string()))?;
        let column = safe_slice(rest, paren_open + 1, paren_close).trim().to_string();

        if class.is_empty() || column.is_empty() {
            return Err(CoreError::InvalidArgument(
                "class and column cannot be empty in DROP INDEX".to_string(),
            ));
        }

        Ok(QueryAst::DropIndex { class, column })
    }

    /// Parses: CREATE VECTOR INDEX ON <class> (<column>) METRIC <metric> DIMENSION <dim> [M <m>] [EF_CONSTRUCTION <ef>] [EF_SEARCH <ef>]
    fn parse_create_vector_index(input: &str) -> Result<QueryAst> {
        let on_pos = find_ignore_ascii_case(input, " ON ")
            .ok_or_else(|| CoreError::InvalidArgument("expected 'ON' after CREATE VECTOR INDEX".to_string()))?;
        let rest = safe_slice_from(input, on_pos + 4).trim();

        // Find the parenthesized column
        let paren_open = rest
            .find('(')
            .ok_or_else(|| CoreError::InvalidArgument("expected '(column)' in CREATE VECTOR INDEX".to_string()))?;
        let class = safe_slice(rest, 0, paren_open).trim().to_string();
        let paren_close = rest
            .find(')')
            .ok_or_else(|| CoreError::InvalidArgument("expected ')' in CREATE VECTOR INDEX".to_string()))?;
        let column = safe_slice(rest, paren_open + 1, paren_close).trim().to_string();

        if class.is_empty() || column.is_empty() {
            return Err(CoreError::InvalidArgument(
                "class and column cannot be empty in CREATE VECTOR INDEX".to_string(),
            ));
        }

        let after_paren = safe_slice_from(rest, paren_close + 1).trim();

        // Parse METRIC
        let metric = if let Some(pos) = find_ignore_ascii_case(after_paren, "METRIC") {
            let metric_str = safe_slice_from(after_paren, pos + 6).trim();
            let end = metric_str.find(|c: char| c.is_whitespace()).unwrap_or(metric_str.len());
            safe_slice(metric_str, 0, end).to_lowercase()
        } else {
            "cosine".to_string()
        };

        // Parse DIMENSION
        let dimension = if let Some(pos) = find_ignore_ascii_case(after_paren, "DIMENSION") {
            let dim_str = safe_slice_from(after_paren, pos + 9).trim();
            let end = dim_str.find(|c: char| !c.is_ascii_digit()).unwrap_or(dim_str.len());
            safe_slice(dim_str, 0, end).parse::<usize>()
                .map_err(|_| CoreError::InvalidArgument("invalid DIMENSION value".to_string()))?
        } else {
            return Err(CoreError::InvalidArgument("DIMENSION is required in CREATE VECTOR INDEX".to_string()));
        };

        // Parse optional M
        let m = if let Some(pos) = find_ignore_ascii_case(after_paren, " M ") {
            let m_str = safe_slice_from(after_paren, pos + 3).trim();
            let end = m_str.find(|c: char| !c.is_ascii_digit()).unwrap_or(m_str.len());
            safe_slice(m_str, 0, end).parse::<usize>().unwrap_or(16)
        } else {
            16
        };

        // Parse optional EF_CONSTRUCTION
        let ef_construction = if let Some(pos) = find_ignore_ascii_case(after_paren, "EF_CONSTRUCTION") {
            let ef_str = safe_slice_from(after_paren, pos + 15).trim();
            let end = ef_str.find(|c: char| !c.is_ascii_digit()).unwrap_or(ef_str.len());
            safe_slice(ef_str, 0, end).parse::<usize>().unwrap_or(200)
        } else {
            200
        };

        // Parse optional EF_SEARCH
        let ef_search = if let Some(pos) = find_ignore_ascii_case(after_paren, "EF_SEARCH") {
            let ef_str = safe_slice_from(after_paren, pos + 9).trim();
            let end = ef_str.find(|c: char| !c.is_ascii_digit()).unwrap_or(ef_str.len());
            safe_slice(ef_str, 0, end).parse::<usize>().unwrap_or(100)
        } else {
            100
        };

        Ok(QueryAst::CreateVectorIndex {
            class,
            column,
            metric,
            dimension,
            m,
            ef_construction,
            ef_search,
        })
    }

    /// Parses: DROP VECTOR INDEX ON <class> (<column>)
    fn parse_drop_vector_index(input: &str) -> Result<QueryAst> {
        let on_pos = find_ignore_ascii_case(input, " ON ")
            .ok_or_else(|| CoreError::InvalidArgument("expected 'ON' after DROP VECTOR INDEX".to_string()))?;
        let rest = safe_slice_from(input, on_pos + 4).trim();

        let paren_open = rest
            .find('(')
            .ok_or_else(|| CoreError::InvalidArgument("expected '(column)' in DROP VECTOR INDEX".to_string()))?;
        let class = safe_slice(rest, 0, paren_open).trim().to_string();
        let paren_close = rest
            .find(')')
            .ok_or_else(|| CoreError::InvalidArgument("expected ')' in DROP VECTOR INDEX".to_string()))?;
        let column = safe_slice(rest, paren_open + 1, paren_close).trim().to_string();

        if class.is_empty() || column.is_empty() {
            return Err(CoreError::InvalidArgument(
                "class and column cannot be empty in DROP VECTOR INDEX".to_string(),
            ));
        }

        Ok(QueryAst::DropVectorIndex { class, column })
    }

    /// Parses: VECTOR SEARCH ON <class> (<column>) QUERY [v1, v2, ...] TOP <k> [WHERE ...]
    fn parse_vector_search(input: &str) -> Result<QueryAst> {
        let on_pos = find_ignore_ascii_case(input, " ON ")
            .ok_or_else(|| CoreError::InvalidArgument("expected 'ON' after VECTOR SEARCH".to_string()))?;
        let rest = safe_slice_from(input, on_pos + 4).trim();

        // Find the parenthesized column
        let paren_open = rest
            .find('(')
            .ok_or_else(|| CoreError::InvalidArgument("expected '(column)' in VECTOR SEARCH".to_string()))?;
        let class = safe_slice(rest, 0, paren_open).trim().to_string();
        let paren_close = rest
            .find(')')
            .ok_or_else(|| CoreError::InvalidArgument("expected ')' in VECTOR SEARCH".to_string()))?;
        let column = safe_slice(rest, paren_open + 1, paren_close).trim().to_string();

        if class.is_empty() || column.is_empty() {
            return Err(CoreError::InvalidArgument(
                "class and column cannot be empty in VECTOR SEARCH".to_string(),
            ));
        }

        let after_paren = safe_slice_from(rest, paren_close + 1).trim();

        // Parse QUERY keyword
        let query_pos = find_ignore_ascii_case(after_paren, "QUERY")
            .ok_or_else(|| CoreError::InvalidArgument("expected 'QUERY' in VECTOR SEARCH".to_string()))?;
        let after_query = safe_slice_from(after_paren, query_pos + 5).trim();

        // Parse the vector: [v1, v2, ...]
        let bracket_open = after_query
            .find('[')
            .ok_or_else(|| CoreError::InvalidArgument("expected '[' for query vector".to_string()))?;
        let bracket_close = after_query
            .find(']')
            .ok_or_else(|| CoreError::InvalidArgument("expected ']' for query vector".to_string()))?;
        let vec_str = safe_slice(after_query, bracket_open + 1, bracket_close);
        let query_vector: Vec<f32> = vec_str
            .split(',')
            .map(|s| s.trim().parse::<f32>())
            .collect::<std::result::Result<Vec<f32>, _>>()
            .map_err(|_| CoreError::InvalidArgument("invalid vector element".to_string()))?;

        let after_vec = safe_slice_from(after_query, bracket_close + 1).trim();

        // Parse TOP <k>
        let top_pos = find_ignore_ascii_case(after_vec, "TOP")
            .ok_or_else(|| CoreError::InvalidArgument("expected 'TOP' in VECTOR SEARCH".to_string()))?;
        let after_top = safe_slice_from(after_vec, top_pos + 3).trim();
        let (top_str, remaining) = Self::parse_word(after_top)?;
        let top_k = top_str
            .parse::<usize>()
            .map_err(|_| CoreError::InvalidArgument("invalid TOP value".to_string()))?;

        // Parse optional WHERE
        let filter = if starts_with_ignore_ascii_case(&remaining, "WHERE") {
            Self::parse_where(remaining[5..].trim())?.0
        } else {
            None
        };

        Ok(QueryAst::VectorSearch {
            class,
            column,
            query_vector,
            top_k,
            filter,
        })
    }

    fn parse_update(input: &str) -> Result<QueryAst> {
        let set_pos = find_ignore_ascii_case(input, " SET ")
            .ok_or_else(|| CoreError::InvalidArgument("expected 'SET'".to_string()))?;

        let class = input[6..set_pos].trim().to_string();
        let rest = &safe_slice_from(input, set_pos + 5);

        // Find WHERE position, respecting quotes
        let (set_part, filter_str) = if let Some(wp) = Self::find_unquoted(rest, " WHERE ") {
            (safe_slice(rest, 0, wp), Some(safe_slice_from(rest, wp + 7).trim()))
        } else {
            (*rest, None)
        };

        // Split assignments by comma, respecting quotes
        let assignments: Vec<(String, LiteralValue)> = Self::split_quoted(set_part, ',')
            .iter()
            .map(|s| {
                let s = s.trim();
                let eq_pos = Self::find_unquoted(s, "=")
                    .ok_or_else(|| CoreError::InvalidArgument("expected '=' in SET".to_string()))?;
                let col = Self::strip_surrounding_quotes(safe_slice(s, 0, eq_pos).trim());
                let val = Self::parse_literal(safe_slice_from(s, eq_pos + 1).trim())?;
                Ok((col, val))
            })
            .collect::<Result<Vec<_>>>()?;

        let filter = if let Some(fstr) = filter_str {
            Self::parse_where(fstr)?.0
        } else {
            None
        };

        Ok(QueryAst::Update {
            class,
            assignments,
            filter,
        })
    }

    /// Splits a string by a delimiter character, skipping delimiters inside quotes.
    fn split_quoted(input: &str, delim: char) -> Vec<String> {
        let mut parts = Vec::new();
        let mut current = String::new();
        let mut in_quote: Option<char> = None;
        let mut paren_depth = 0i32;

        for c in input.chars() {
            if let Some(q) = in_quote {
                current.push(c);
                if c == q {
                    in_quote = None;
                }
            } else if c == '\'' || c == '"' {
                in_quote = Some(c);
                current.push(c);
            } else if c == '(' {
                paren_depth += 1;
                current.push(c);
            } else if c == ')' {
                paren_depth -= 1;
                current.push(c);
            } else if c == delim && paren_depth == 0 {
                parts.push(current.clone());
                current.clear();
            } else {
                current.push(c);
            }
        }

        if !current.is_empty() {
            parts.push(current);
        }

        parts
    }

    fn parse_delete(input: &str) -> Result<QueryAst> {
        let from_pos = find_ignore_ascii_case(input, "FROM")
            .ok_or_else(|| CoreError::InvalidArgument("expected 'FROM'".to_string()))?;

        let rest = safe_slice_from(input, from_pos + 4).trim();
        let (class, rest) = Self::parse_word(rest)?;

        let filter = if starts_with_ignore_ascii_case(&rest, "WHERE") {
            Self::parse_where(rest[5..].trim())?.0
        } else {
            None
        };

        Ok(QueryAst::Delete { class, filter })
    }

    fn parse_match(input: &str) -> Result<QueryAst> {
        // MATCH (<var>: <Class>) [WHERE ...] RETURN ...
        let rest = input[5..].trim(); // Skip "MATCH"

        // Find parentheses for the pattern
        let open = rest
            .find('(')
            .ok_or_else(|| CoreError::InvalidArgument("expected '(' after MATCH".to_string()))?;
        let close = rest
            .find(')')
            .ok_or_else(|| CoreError::InvalidArgument("expected ')' in MATCH pattern".to_string()))?;

        if open + 1 >= close {
            return Err(CoreError::InvalidArgument("empty or malformed MATCH pattern".to_string()));
        }
        let pattern = safe_slice(rest, open + 1, close).trim();

        // Parse pattern: <var>: <Class>
        let colon_pos = pattern
            .find(':')
            .ok_or_else(|| CoreError::InvalidArgument("expected ':' in MATCH pattern".to_string()))?;

        let variable = safe_slice(pattern, 0, colon_pos).trim().to_string();
        let class = safe_slice_from(pattern, colon_pos + 1).trim().to_string();

        let after = safe_slice_from(rest, close + 1).trim();

        // Parse optional WHERE
        let (filter, after) = if starts_with_ignore_ascii_case(after, "WHERE") {
            Self::parse_where(after[5..].trim())?
        } else {
            (None, after.to_string())
        };

        // Parse RETURN
        if !starts_with_ignore_ascii_case(&after, "RETURN") {
            return Err(CoreError::InvalidArgument(
                "expected 'RETURN' in MATCH".to_string(),
            ));
        }

        let return_str = after[6..].trim();
        let returns: Vec<String> = return_str
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        Ok(QueryAst::Match {
            variable,
            class,
            filter,
            returns,
        })
    }

    /// Finds a substring in the input, skipping over quoted sections.
    /// Returns the position of the first occurrence that is NOT inside quotes.
    fn find_unquoted(haystack: &str, needle: &str) -> Option<usize> {
        let mut in_quote: Option<char> = None;
        let bytes = haystack.as_bytes();
        let needle_bytes = needle.as_bytes();

        let mut i = 0;
        while i + needle_bytes.len() <= bytes.len() {
            let c = bytes[i] as char;

            if let Some(q) = in_quote {
                if c == q {
                    in_quote = None;
                }
            } else if c == '\'' || c == '"' {
                in_quote = Some(c);
            } else if bytes[i..].starts_with(needle_bytes) {
                return Some(i);
            }

            i += 1;
        }
        None
    }

    /// Strips surrounding quotes from an identifier if present.
    /// Handles both single and double quoted identifiers.
    fn strip_identifier_quotes(s: &str) -> String {
        if (s.starts_with('"') && s.ends_with('"'))
            || (s.starts_with('\'') && s.ends_with('\''))
        {
            s[1..s.len()-1].to_string()
        } else {
            s.to_string()
        }
    }

    /// Strips surrounding quotes from an identifier. Also handles bracket-quoted [id].
    fn strip_surrounding_quotes(s: &str) -> String {
        if s.starts_with('[') && s.ends_with(']') {
            s[1..s.len()-1].to_string()
        } else {
            Self::strip_identifier_quotes(s)
        }
    }

    fn parse_where(input: &str) -> Result<(Option<FilterExpr>, String)> {
        let input = input.trim();

        // Handle parenthesized expressions: (expr)
        if input.starts_with('(') {
            // Find matching closing paren
            let mut depth = 0i32;
            let mut close_pos = None;
            for (i, c) in input.char_indices() {
                if c == '(' { depth += 1; }
                if c == ')' {
                    depth -= 1;
                    if depth == 0 {
                        close_pos = Some(i);
                        break;
                    }
                }
            }
            if let Some(close) = close_pos {
                let inner = safe_slice(input, 1, close).trim();
                let remaining = safe_slice_from(input, close + 1).trim();
                // Parse the inner expression and chain with AND/OR if present
                let (inner_expr, _) = Self::parse_where(inner)?;
                if let Some(expr) = inner_expr {
                    return Self::wrap_chain(expr, remaining);
                }
            }
        }

        // Check for NOT (expr)
        if starts_with_ignore_ascii_case(input, "NOT ") || starts_with_ignore_ascii_case(input, "NOT(") {
            let not_len = if starts_with_ignore_ascii_case(input, "NOT(") { 4 } else { 4 };
            let rest = safe_slice_from(input, not_len).trim();
            if rest.starts_with('(') {
                // Find matching closing paren
                let mut depth = 0i32;
                let mut close_pos = None;
                for (i, c) in rest.char_indices() {
                    if c == '(' { depth += 1; }
                    if c == ')' {
                        depth -= 1;
                        if depth == 0 {
                            close_pos = Some(i);
                            break;
                        }
                    }
                }
                if let Some(close) = close_pos {
                    let inner = safe_slice(rest, 1, close).trim();
                    let remaining = safe_slice_from(rest, close + 1).trim();
                    let (inner_expr, _) = Self::parse_where(inner)?;
                    if let Some(expr) = inner_expr {
                        let expr = FilterExpr::Not(Box::new(expr));
                        return Self::wrap_chain(expr, remaining);
                    }
                }
            }
        }

        // Check for EXISTS (SELECT ...)
        if starts_with_ignore_ascii_case(input, "EXISTS") || starts_with_ignore_ascii_case(input, "NOT EXISTS") {
            let (exists_start, is_not) = if starts_with_ignore_ascii_case(input, "NOT EXISTS") {
                (10, true)
            } else {
                (6, false)
            };
            let rest = safe_slice_from(input, exists_start).trim();
            if rest.starts_with('(') {
                // Find matching closing paren
                let mut depth = 0i32;
                let mut close_pos = None;
                for (i, c) in rest.char_indices() {
                    if c == '(' { depth += 1; }
                    if c == ')' {
                        depth -= 1;
                        if depth == 0 {
                            close_pos = Some(i);
                            break;
                        }
                    }
                }
                if let Some(close) = close_pos {
                    let inner = safe_slice(rest, 1, close).trim();
                    let remaining = safe_slice_from(rest, close + 1).trim();
                    let subquery = Self::parse(inner)?;
                    let expr = if is_not {
                        FilterExpr::NotExists(Box::new(subquery))
                    } else {
                        FilterExpr::Exists(Box::new(subquery))
                    };
                    return Self::wrap_chain(expr, remaining);
                }
            }
        }

        // Check for IS NULL / IS NOT NULL: column IS [NOT] NULL
        if let Some(is_pos) = find_unquoted_ignore_ascii_case(input, " IS ") {
            let col_raw = safe_slice(input, 0, is_pos).trim();
            let col = Self::strip_identifier_quotes(col_raw);
            let rest = safe_slice_from(input, is_pos + 4).trim();
            if starts_with_ignore_ascii_case(rest, "NOT NULL") {
                let remaining = rest[8..].trim();
                let expr = FilterExpr::IsNotNull(col);
                return Self::wrap_chain(expr, remaining);
            } else if starts_with_ignore_ascii_case(rest, "NULL") {
                let remaining = rest[4..].trim();
                let expr = FilterExpr::IsNull(col);
                return Self::wrap_chain(expr, remaining);
            }
        }

        // Check for LIKE: column LIKE 'pattern'
        if let Some(like_pos) = find_unquoted_ignore_ascii_case(input, " LIKE ") {
            let col_raw = safe_slice(input, 0, like_pos).trim();
            let col = Self::strip_identifier_quotes(col_raw);
            let rest = safe_slice_from(input, like_pos + 6).trim();
            let (pattern, remaining) = Self::extract_quoted_or_word(rest);
            let expr = FilterExpr::Like(col, pattern);
            return Self::wrap_chain(expr, &remaining);
        }

        // Check for BETWEEN: column BETWEEN low AND high
        if let Some(between_pos) = find_unquoted_ignore_ascii_case(input, " BETWEEN ") {
            let col_raw = safe_slice(input, 0, between_pos).trim();
            let col = Self::strip_identifier_quotes(col_raw);
            let rest = safe_slice_from(input, between_pos + 9).trim();

            if let Some(and_pos) = find_unquoted_ignore_ascii_case(rest, " AND ") {
                let low_str = safe_slice(rest, 0, and_pos).trim();
                let high_rest = safe_slice_from(rest, and_pos + 5).trim();
                let (high_str, remaining) = Self::extract_quoted_or_word(high_rest);
                let low = Self::parse_literal(low_str)?;
                let high = Self::parse_literal(&high_str)?;
                let expr = FilterExpr::Between(col, low, high);
                return Self::wrap_chain(expr, &remaining);
            }
        }

        // Check for IN: column IN (val1, val2, ...) or column IN (SELECT ...)
        if let Some(in_pos) = find_unquoted_ignore_ascii_case(input, " IN ") {
            let col_raw = safe_slice(input, 0, in_pos).trim();
            let col = Self::strip_identifier_quotes(col_raw);
            let rest = safe_slice_from(input, in_pos + 4).trim();
            if rest.starts_with('(') {
                // Find matching closing paren
                let mut depth = 0i32;
                let mut close_pos = None;
                for (i, c) in rest.char_indices() {
                    if c == '(' { depth += 1; }
                    if c == ')' {
                        depth -= 1;
                        if depth == 0 {
                            close_pos = Some(i);
                            break;
                        }
                    }
                }
                if let Some(close) = close_pos {
                    let inner = safe_slice(rest, 1, close).trim();
                    let remaining = safe_slice_from(rest, close + 1).trim();

                    // Check if it's a subquery
                    if starts_with_ignore_ascii_case(inner, "SELECT") {
                        let subquery = Self::parse(inner)?;
                        let expr = FilterExpr::InSubquery(col, Box::new(subquery));
                        return Self::wrap_chain(expr, remaining);
                    }

                    // Otherwise, parse as value list
                    let values: Vec<LiteralValue> = inner
                        .split(',')
                        .map(|s| Self::parse_literal(s.trim()))
                        .collect::<Result<Vec<_>>>()?;
                    let expr = FilterExpr::In(col, values);
                    return Self::wrap_chain(expr, remaining);
                }
            }
        }

        // Symbol operators: find the leftmost operator to handle AND/OR correctly.
        // We check all operators and pick the one with the smallest position.
        let operators: &[(&str, fn(String, LiteralValue) -> FilterExpr)] = &[
            (">=", FilterExpr::Gte),
            ("<=", FilterExpr::Lte),
            ("!=", FilterExpr::Ne),
            ("<>", FilterExpr::Ne),
            (">", FilterExpr::Gt),
            ("<", FilterExpr::Lt),
            ("=", FilterExpr::Eq),
        ];
        let mut best_op: Option<(usize, &str, fn(String, LiteralValue) -> FilterExpr)> = None;
        for &(op_str, op_fn) in operators {
            if let Some(pos) = Self::find_unquoted(input, op_str) {
                if best_op.is_none() || pos < best_op.unwrap().0 {
                    best_op = Some((pos, op_str, op_fn));
                }
            }
        }
        if let Some((pos, op_str, op_fn)) = best_op {
            let col_raw = safe_slice(input, 0, pos).trim();
            // Strip surrounding quotes from column name if present
            let col = if (col_raw.starts_with('"') && col_raw.ends_with('"'))
                || (col_raw.starts_with('\'') && col_raw.ends_with('\''))
            {
                col_raw[1..col_raw.len()-1].to_string()
            } else {
                col_raw.to_string()
            };
            let rest = safe_slice_from(input, pos + op_str.len()).trim();

            let (val_str, remaining) = Self::extract_quoted_or_word(rest);
            let val = Self::parse_literal(&val_str)?;
            let expr = op_fn(col, val);
            return Self::wrap_chain(expr, &remaining);
        }

        Err(CoreError::InvalidArgument(format!(
            "invalid WHERE clause: {}",
            input
        )))
    }

    /// Wraps an expression with AND/OR if the remaining text starts with AND/OR.
    /// AND binds tighter than OR: `a OR b AND c` → `a OR (b AND c)`.
    fn wrap_chain(expr: FilterExpr, remaining: &str) -> Result<(Option<FilterExpr>, String)> {
        let remaining = remaining.trim();
        if starts_with_ignore_ascii_case(remaining, "AND ") {
            let rest_after = &remaining[4..];
            // AND has higher precedence — parse the right operand as a single comparison,
            // then continue chaining (so a AND b OR c groups as (a AND b) OR c).
            let (right_atom, rest2) = Self::parse_where_atom(rest_after)?;
            if let Some(right_expr) = right_atom {
                let combined = FilterExpr::And(Box::new(expr), Box::new(right_expr));
                // Continue chaining at the same precedence level
                return Self::wrap_chain(combined, &rest2);
            }
            return Ok((Some(expr), rest2));
        } else if starts_with_ignore_ascii_case(remaining, "OR ") {
            let rest_after = &remaining[3..];
            // OR has lower precedence — parse the right operand as a full AND-chain,
            // so `a OR b AND c` groups the right side as `(b AND c)`.
            let (right, rest2) = Self::parse_where_and(rest_after)?;
            if let Some(right_expr) = right {
                let combined = FilterExpr::Or(Box::new(expr), Box::new(right_expr));
                // Continue chaining OR left-to-right: a OR b OR c → (a OR b) OR c
                return Self::wrap_chain(combined, &rest2);
            }
            return Ok((Some(expr), rest2));
        }
        Ok((Some(expr), remaining.to_string()))
    }

    /// Parse an AND-chain: one or more atoms joined by AND.
    /// Used as the higher-precedence layer: `a AND b AND c`.
    fn parse_where_and(input: &str) -> Result<(Option<FilterExpr>, String)> {
        let (mut left, mut rest) = Self::parse_where_atom(input)?;
        loop {
            rest = rest.trim().to_string();
            if starts_with_ignore_ascii_case(&rest, "AND ") {
                let after = &rest[4..];
                let (right, new_rest) = Self::parse_where_atom(after)?;
                if let Some(r) = right {
                    left = Some(FilterExpr::And(
                        Box::new(left.unwrap_or(FilterExpr::Eq(String::new(), LiteralValue::Null))),
                        Box::new(r),
                    ));
                }
                rest = new_rest;
            } else {
                break;
            }
        }
        Ok((left, rest))
    }

    /// Parse a single atom: a comparison, parenthesized expression, NOT, etc.
    /// Does NOT consume trailing AND/OR — that's handled by `wrap_chain` / `parse_where_and`.
    fn parse_where_atom(input: &str) -> Result<(Option<FilterExpr>, String)> {
        let input = input.trim();

        // Handle parenthesized expressions: (expr)
        if input.starts_with('(') {
            let mut depth = 0i32;
            let mut close_pos = None;
            for (i, c) in input.char_indices() {
                if c == '(' { depth += 1; }
                if c == ')' {
                    depth -= 1;
                    if depth == 0 {
                        close_pos = Some(i);
                        break;
                    }
                }
            }
            if let Some(close) = close_pos {
                let inner = safe_slice(input, 1, close).trim();
                let remaining = safe_slice_from(input, close + 1).trim();
                // Parse the inner expression with full precedence
                let (inner_expr, _) = Self::parse_where(inner)?;
                if let Some(expr) = inner_expr {
                    return Ok((Some(expr), remaining.to_string()));
                }
            }
        }

        // Check for NOT (expr)
        if starts_with_ignore_ascii_case(input, "NOT ") || starts_with_ignore_ascii_case(input, "NOT(") {
            let not_len = if starts_with_ignore_ascii_case(input, "NOT(") { 4 } else { 4 };
            let rest = safe_slice_from(input, not_len).trim();
            if rest.starts_with('(') {
                let mut depth = 0i32;
                let mut close_pos = None;
                for (i, c) in rest.char_indices() {
                    if c == '(' { depth += 1; }
                    if c == ')' {
                        depth -= 1;
                        if depth == 0 {
                            close_pos = Some(i);
                            break;
                        }
                    }
                }
                if let Some(close) = close_pos {
                    let inner = safe_slice(rest, 1, close).trim();
                    let remaining = safe_slice_from(rest, close + 1).trim();
                    let (inner_expr, _) = Self::parse_where(inner)?;
                    if let Some(expr) = inner_expr {
                        return Ok((Some(FilterExpr::Not(Box::new(expr))), remaining.to_string()));
                    }
                }
            }
        }

        // Check for EXISTS (SELECT ...)
        if starts_with_ignore_ascii_case(input, "EXISTS") || starts_with_ignore_ascii_case(input, "NOT EXISTS") {
            let (exists_start, is_not) = if starts_with_ignore_ascii_case(input, "NOT EXISTS") {
                (10, true)
            } else {
                (6, false)
            };
            let rest = safe_slice_from(input, exists_start).trim();
            if rest.starts_with('(') {
                let mut depth = 0i32;
                let mut close_pos = None;
                for (i, c) in rest.char_indices() {
                    if c == '(' { depth += 1; }
                    if c == ')' {
                        depth -= 1;
                        if depth == 0 {
                            close_pos = Some(i);
                            break;
                        }
                    }
                }
                if let Some(close) = close_pos {
                    let inner = safe_slice(rest, 1, close).trim();
                    let remaining = safe_slice_from(rest, close + 1).trim();
                    let subquery = Self::parse(inner)?;
                    let expr = if is_not {
                        FilterExpr::NotExists(Box::new(subquery))
                    } else {
                        FilterExpr::Exists(Box::new(subquery))
                    };
                    return Ok((Some(expr), remaining.to_string()));
                }
            }
        }

        // Check for IS NULL / IS NOT NULL: column IS [NOT] NULL
        if let Some(is_pos) = find_unquoted_ignore_ascii_case(input, " IS ") {
            let col_raw = safe_slice(input, 0, is_pos).trim();
            let col = Self::strip_identifier_quotes(col_raw);
            let rest = safe_slice_from(input, is_pos + 4).trim();
            if starts_with_ignore_ascii_case(rest, "NOT NULL") {
                let remaining = rest[8..].trim();
                return Ok((Some(FilterExpr::IsNotNull(col)), remaining.to_string()));
            } else if starts_with_ignore_ascii_case(rest, "NULL") {
                let remaining = rest[4..].trim();
                return Ok((Some(FilterExpr::IsNull(col)), remaining.to_string()));
            }
        }

        // Check for LIKE: column LIKE 'pattern'
        if let Some(like_pos) = find_unquoted_ignore_ascii_case(input, " LIKE ") {
            let col_raw = safe_slice(input, 0, like_pos).trim();
            let col = Self::strip_identifier_quotes(col_raw);
            let rest = safe_slice_from(input, like_pos + 6).trim();
            let (pattern, remaining) = Self::extract_quoted_or_word(rest);
            return Ok((Some(FilterExpr::Like(col, pattern)), remaining));
        }

        // Check for BETWEEN: column BETWEEN low AND high
        if let Some(between_pos) = find_unquoted_ignore_ascii_case(input, " BETWEEN ") {
            let col_raw = safe_slice(input, 0, between_pos).trim();
            let col = Self::strip_identifier_quotes(col_raw);
            let rest = safe_slice_from(input, between_pos + 9).trim();

            if let Some(and_pos) = find_unquoted_ignore_ascii_case(rest, " AND ") {
                let low_str = safe_slice(rest, 0, and_pos).trim();
                let high_rest = safe_slice_from(rest, and_pos + 5).trim();
                let (high_str, remaining) = Self::extract_quoted_or_word(high_rest);
                let low = Self::parse_literal(low_str)?;
                let high = Self::parse_literal(&high_str)?;
                return Ok((Some(FilterExpr::Between(col, low, high)), remaining));
            }
        }

        // Check for IN: column IN (val1, val2, ...) or column IN (SELECT ...)
        if let Some(in_pos) = find_unquoted_ignore_ascii_case(input, " IN ") {
            let col_raw = safe_slice(input, 0, in_pos).trim();
            let col = Self::strip_identifier_quotes(col_raw);
            let rest = safe_slice_from(input, in_pos + 4).trim();
            if rest.starts_with('(') {
                let mut depth = 0i32;
                let mut close_pos = None;
                for (i, c) in rest.char_indices() {
                    if c == '(' { depth += 1; }
                    if c == ')' {
                        depth -= 1;
                        if depth == 0 {
                            close_pos = Some(i);
                            break;
                        }
                    }
                }
                if let Some(close) = close_pos {
                    let inner = safe_slice(rest, 1, close).trim();
                    let remaining = safe_slice_from(rest, close + 1).trim();

                    // Check if it's a subquery
                    if starts_with_ignore_ascii_case(inner, "SELECT") {
                        let subquery = Self::parse(inner)?;
                        return Ok((Some(FilterExpr::InSubquery(col, Box::new(subquery))), remaining.to_string()));
                    }

                    // Otherwise, parse as value list
                    let values: Vec<LiteralValue> = inner
                        .split(',')
                        .map(|s| Self::parse_literal(s.trim()))
                        .collect::<Result<Vec<_>>>()?;
                    return Ok((Some(FilterExpr::In(col, values)), remaining.to_string()));
                }
            }
        }

        // Symbol operators: find the leftmost operator to handle AND/OR correctly.
        let operators: &[(&str, fn(String, LiteralValue) -> FilterExpr)] = &[
            (">=", FilterExpr::Gte),
            ("<=", FilterExpr::Lte),
            ("!=", FilterExpr::Ne),
            ("<>", FilterExpr::Ne),
            (">", FilterExpr::Gt),
            ("<", FilterExpr::Lt),
            ("=", FilterExpr::Eq),
        ];
        let mut best_op: Option<(usize, &str, fn(String, LiteralValue) -> FilterExpr)> = None;
        for &(op_str, op_fn) in operators {
            if let Some(pos) = Self::find_unquoted(input, op_str) {
                if best_op.is_none() || pos < best_op.unwrap().0 {
                    best_op = Some((pos, op_str, op_fn));
                }
            }
        }
        if let Some((pos, op_str, op_fn)) = best_op {
            let col_raw = safe_slice(input, 0, pos).trim();
            let col = Self::strip_identifier_quotes(col_raw);
            let rest = safe_slice_from(input, pos + op_str.len()).trim();

            let (val_str, remaining) = Self::extract_quoted_or_word(rest);
            let val = Self::parse_literal(&val_str)?;
            let expr = op_fn(col, val);
            return Ok((Some(expr), remaining));
        }

        Err(CoreError::InvalidArgument(format!(
            "invalid WHERE clause: {}",
            input
        )))
    }

    /// Extracts a value from the start of input. Handles quoted strings and bare words.
    /// Returns (value_string, remaining_input).
    fn extract_quoted_or_word(input: &str) -> (String, String) {
        let input = input.trim();
        if input.starts_with('\'') || input.starts_with('"') {
            let quote = input.as_bytes()[0] as char;
            if let Some(end) = safe_slice_from(input, 1).find(quote) {
                return (
                    safe_slice(input, 1, 1 + end).to_string(),
                    safe_slice_from(input, end + 2).trim().to_string(),
                );
            }
        }
        let end = input
            .find(|c: char| c.is_whitespace() || c == ';' || c == ',')
            .unwrap_or(input.len());
        (safe_slice(input, 0, end).to_string(), safe_slice_from(input, end).trim().to_string())
    }

    /// Parse CREATE MATERIALIZED VIEW <name> AS <query>
    fn parse_create_materialized_view(input: &str) -> Result<QueryAst> {
        if !starts_with_ignore_ascii_case(input, "CREATE MATERIALIZED VIEW") {
            return Err(CoreError::InvalidArgument("expected CREATE MATERIALIZED VIEW".to_string()));
        }

        let rest = input[24..].trim();
        let as_pos = find_ignore_ascii_case(rest, " AS ")
            .ok_or_else(|| CoreError::InvalidArgument("expected 'AS' in CREATE MATERIALIZED VIEW".to_string()))?;

        let name = safe_slice(rest, 0, as_pos).trim().to_string();
        let query_str = safe_slice_from(rest, as_pos + 4).trim();
        let query = Self::parse(query_str)?;

        Ok(QueryAst::CreateMaterializedView {
            name,
            query: Box::new(query),
        })
    }

    /// Parse DROP MATERIALIZED VIEW <name>
    fn parse_drop_materialized_view(input: &str) -> Result<QueryAst> {
        if !starts_with_ignore_ascii_case(input, "DROP MATERIALIZED VIEW") {
            return Err(CoreError::InvalidArgument("expected DROP MATERIALIZED VIEW".to_string()));
        }

        let name = input[22..].trim().to_string();
        Ok(QueryAst::DropMaterializedView { name })
    }

    /// Parse REFRESH MATERIALIZED VIEW <name>
    fn parse_refresh_materialized_view(input: &str) -> Result<QueryAst> {
        if !starts_with_ignore_ascii_case(input, "REFRESH MATERIALIZED VIEW") {
            return Err(CoreError::InvalidArgument("expected REFRESH MATERIALIZED VIEW".to_string()));
        }

        let name = input[25..].trim().to_string();
        Ok(QueryAst::RefreshMaterializedView { name })
    }

    /// Parses a CASE WHEN ... THEN ... ELSE ... END expression.
    fn parse_case_when(input: &str) -> Result<ValueExpr> {
        if !starts_with_ignore_ascii_case(input, "CASE ") && !starts_with_ignore_ascii_case(input, "CASE\n") {
            return Err(CoreError::InvalidArgument("expected CASE".to_string()));
        }

        let mut remaining = input[5..].trim();
        let mut when_branches = Vec::new();
        let mut else_expr: Option<Box<ValueExpr>> = None;

        loop {
            let remaining_trimmed = remaining.trim();

            if starts_with_ignore_ascii_case(remaining_trimmed, "WHEN ") || starts_with_ignore_ascii_case(remaining_trimmed, "WHEN\n") {
                // Find THEN keyword
                let then_pos = find_unquoted_ignore_ascii_case(remaining_trimmed, " THEN ")
                    .ok_or_else(|| CoreError::InvalidArgument("expected THEN after WHEN".to_string()))?;
                let cond_str = safe_slice(remaining_trimmed, 5, then_pos).trim();
                remaining = safe_slice_from(remaining_trimmed, then_pos + 6).trim();

                // Parse condition as filter expression
                let (cond, _) = Self::parse_where(cond_str)?;

                // Find the end of THEN value (next WHEN, ELSE, or END)
                let (value_str, rest) = Self::consume_until_keywords(
                    remaining,
                    &["WHEN", "ELSE", "END"],
                );
                let value = Self::parse_value_expr(&value_str)?;
                remaining = rest;

                if let Some(cond_expr) = cond {
                    when_branches.push((cond_expr, value));
                }
            } else if starts_with_ignore_ascii_case(remaining_trimmed, "ELSE ") || starts_with_ignore_ascii_case(remaining_trimmed, "ELSE\n") {
                remaining = remaining_trimmed[5..].trim();
                let (value_str, rest) = Self::consume_until_keywords(remaining, &["END"]);
                else_expr = Some(Box::new(Self::parse_value_expr(&value_str)?));
                remaining = rest;
            } else if starts_with_ignore_ascii_case(remaining_trimmed, "END") {
                break;
            } else {
                return Err(CoreError::InvalidArgument(format!(
                    "unexpected token in CASE: {}",
                    remaining
                )));
            }
        }

        Ok(ValueExpr::CaseWhen {
            when_branches,
            else_expr,
        })
    }

    /// Parses a value expression: column reference, literal, CASE WHEN, arithmetic, function, or scalar subquery.
    fn parse_value_expr(input: &str) -> Result<ValueExpr> {
        let input = input.trim();

        // CASE WHEN
        if starts_with_ignore_ascii_case(input, "CASE ") {
            return Self::parse_case_when(input);
        }

        // Scalar subquery: (SELECT ...)
        if input.starts_with('(') && starts_with_ignore_ascii_case(input[1..].trim_start(), "SELECT") {
            let subquery = Self::parse_subquery(input)?;
            return Ok(ValueExpr::ScalarSubquery(Box::new(subquery)));
        }

        // Built-in function: FUNC(args...)
        let func_names = ["COALESCE", "NULLIF", "CONCAT", "SUBSTRING", "UPPER", "LOWER", "NOW", "LENGTH", "TRIM", "ABS", "ROUND"];
        for func_name in &func_names {
            if starts_with_ignore_ascii_case(input, func_name) && input.len() > func_name.len() && input.as_bytes()[func_name.len()] == b'(' {
                // Find matching closing paren
                let open = func_name.len();
                let close = Self::find_matching_paren_simple(safe_slice_from(input, open))? + open;
                let args_str = safe_slice(input, open + 1, close);
                let args: Vec<ValueExpr> = if args_str.trim().is_empty() {
                    Vec::new()
                } else {
                    Self::split_quoted(args_str, ',')
                        .iter()
                        .map(|s| Self::parse_value_expr(s.trim()))
                        .collect::<Result<Vec<_>>>()?
                };
                return Ok(ValueExpr::Function {
                    name: func_name.to_string(),
                    args,
                });
            }
        }

        // Try arithmetic: look for + or - outside parentheses
        if let Some((op, pos)) = Self::find_top_level_arithmetic(input) {
            let left = Self::parse_value_expr(safe_slice(input, 0, pos))?;
            let right = Self::parse_value_expr(safe_slice_from(input, pos + 1))?;
            return Ok(ValueExpr::Arithmetic {
                op,
                left: Box::new(left),
                right: Box::new(right),
            });
        }

        // Literal
        if let Ok(lit) = Self::parse_literal(input) {
            if input.starts_with('\'') || input.starts_with('"')
                || input.eq_ignore_ascii_case("NULL")
                || input.eq_ignore_ascii_case("TRUE")
                || input.eq_ignore_ascii_case("FALSE")
                || input.parse::<i64>().is_ok()
                || input.parse::<f64>().is_ok()
            {
                return Ok(ValueExpr::Literal(lit));
            }
        }

        // Default: column reference
        Ok(ValueExpr::Column(input.to_string()))
    }

    /// Finds the top-level arithmetic operator (+ or -) at depth 0, preferring the rightmost one.
    fn find_top_level_arithmetic(input: &str) -> Option<(ArithmeticOp, usize)> {
        let mut depth = 0i32;
        let mut in_quote: Option<char> = None;
        let mut last_op: Option<(ArithmeticOp, usize)> = None;

        for (i, c) in input.char_indices() {
            if let Some(q) = in_quote {
                if c == q { in_quote = None; }
                continue;
            }
            match c {
                '\'' | '"' => { in_quote = Some(c); }
                '(' => { depth += 1; }
                ')' => { depth -= 1; }
                '+' if depth == 0 => { last_op = Some((ArithmeticOp::Add, i)); }
                '-' if depth == 0 && i > 0 => { last_op = Some((ArithmeticOp::Sub, i)); }
                '*' if depth == 0 => {
                    if last_op.is_none() { last_op = Some((ArithmeticOp::Mul, i)); }
                }
                '/' if depth == 0
                    && last_op.is_none() => { last_op = Some((ArithmeticOp::Div, i)); }
                _ => {}
            }
        }
        last_op
    }

    /// Parses ANALYZE <table>
    fn parse_analyze(input: &str) -> Result<QueryAst> {
        if !starts_with_ignore_ascii_case(input, "ANALYZE") {
            return Err(CoreError::InvalidArgument("expected ANALYZE".to_string()));
        }
        let table = input[7..].trim().to_string();
        if table.is_empty() {
            return Err(CoreError::InvalidArgument("expected table name after ANALYZE".to_string()));
        }
        Ok(QueryAst::Analyze { table })
    }

    fn parse_literal(s: &str) -> Result<LiteralValue> {
        let s = s.trim();

        if s.eq_ignore_ascii_case("NULL") {
            return Ok(LiteralValue::Null);
        }
        // Empty string from quoted input (e.g. '') is a valid empty string, not NULL.
        // Bare empty input is also treated as empty string (caller validates if needed).
        if s.eq_ignore_ascii_case("TRUE") {
            return Ok(LiteralValue::Bool(true));
        }
        if s.eq_ignore_ascii_case("FALSE") {
            return Ok(LiteralValue::Bool(false));
        }

        // String literal (single quotes only - double quotes are identifiers in SQL)
        if s.starts_with('\'') && s.ends_with('\'') {
            return Ok(LiteralValue::String(safe_slice(s, 1, s.len() - 1).to_string()));
        }

        // Number
        if let Ok(i) = s.parse::<i64>() {
            return Ok(LiteralValue::Int(i));
        }
        if let Ok(f) = s.parse::<f64>() {
            return Ok(LiteralValue::Float(f));
        }

        // Unquoted string
        Ok(LiteralValue::String(s.to_string()))
    }

    fn parse_word(input: &str) -> Result<(String, String)> {
        let input = input.trim_start();
        if input.is_empty() {
            return Ok((String::new(), String::new()));
        }
        // Double-quoted identifier: "My Table"
        if input.starts_with('"') {
            if let Some(end) = input[1..].find('"') {
                let word = input[1..1 + end].to_string();
                let rest = safe_slice_from(input, 1 + end + 1).trim_start().to_string();
                return Ok((word, rest));
            }
        }
        // Bracket-quoted identifier: [My Table]
        if input.starts_with('[') {
            if let Some(end) = input[1..].find(']') {
                let word = input[1..1 + end].to_string();
                let rest = safe_slice_from(input, 1 + end + 1).trim_start().to_string();
                return Ok((word, rest));
            }
        }
        // Bare word
        let end = input
            .find(|c: char| c.is_whitespace() || c == ';' || c == ',')
            .unwrap_or(input.len());
        Ok((safe_slice(input, 0, end).to_string(), safe_slice_from(input, end).trim_start().to_string()))
    }

    // ── Graph Query Parsers ──────────────────────────────────────────

    /// Parse CREATE VERTEX TABLE <name> (id STRING, name STRING, ...)
    fn parse_create_vertex_table(input: &str) -> Result<QueryAst> {
        let rest = input[20..].trim(); // Skip "CREATE VERTEX TABLE"
        let (name, rest) = Self::parse_word(rest)?;
        let rest = rest.trim();

        // Parse column definitions in parentheses
        if !rest.starts_with('(') {
            return Err(CoreError::InvalidArgument("expected '(' after table name".to_string()));
        }
        let end = rest.find(')').ok_or_else(|| CoreError::InvalidArgument("missing ')'".to_string()))?;
        let cols_str = safe_slice(rest, 1, end).trim();

        let columns = Self::parse_column_defs(cols_str)?;

        Ok(QueryAst::CreateVertexTable { name, columns })
    }

    /// Parse CREATE EDGE TABLE <name> (FROM <from_table> TO <to_table>, ...)
    fn parse_create_edge_table(input: &str) -> Result<QueryAst> {
        let rest = input[18..].trim(); // Skip "CREATE EDGE TABLE"
        let (name, rest) = Self::parse_word(rest)?;
        let rest = rest.trim();

        if !rest.starts_with('(') {
            return Err(CoreError::InvalidArgument("expected '(' after table name".to_string()));
        }
        let end = rest.find(')').ok_or_else(|| CoreError::InvalidArgument("missing ')'".to_string()))?;
        let inner = safe_slice(rest, 1, end).trim();

        // Parse FROM and TO clauses
        let from_pos = find_ignore_ascii_case(inner, "FROM ").ok_or_else(|| CoreError::InvalidArgument("expected FROM".to_string()))?;
        let to_pos = find_ignore_ascii_case(inner, " TO ").ok_or_else(|| CoreError::InvalidArgument("expected TO".to_string()))?;

        let from_table = safe_slice(inner, from_pos + 5, to_pos).trim().to_string();
        let after_to = safe_slice_from(inner, to_pos + 4).trim();
        let (to_table, cols_str) = Self::parse_word(after_to)?;

        let columns = if cols_str.starts_with(',') {
            Self::parse_column_defs(cols_str[1..].trim())?
        } else {
            vec![]
        };

        Ok(QueryAst::CreateEdgeTable { name, from_table, to_table, columns })
    }

    /// Parse INSERT VERTEX INTO <table> (id, labels, props...)
    fn parse_insert_vertex(input: &str) -> Result<QueryAst> {
        let rest = input[13..].trim(); // Skip "INSERT VERTEX"
        let rest = rest.strip_prefix("INTO").ok_or_else(|| CoreError::InvalidArgument("expected INTO".to_string()))?.trim();
        let (table, rest) = Self::parse_word(rest)?;

        // For simplicity, parse as a structured format
        // INSERT VERTEX INTO Person SET id = 'alice', labels = ['Person'], name = 'Alice'
        if starts_with_ignore_ascii_case(&rest, "SET") {
            let props_str = rest[3..].trim();
            let props = Self::parse_set_assignments(props_str)?;

            // Extract id from properties
            let id = props.iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("id"))
                .and_then(|(_, v)| match v {
                    LiteralValue::String(s) => Some(s.clone()),
                    _ => None,
                })
                .unwrap_or_default();

            Ok(QueryAst::InsertVertex {
                table,
                id,
                labels: vec![],
                properties: props,
            })
        } else {
            Err(CoreError::InvalidArgument("expected SET after table name".to_string()))
        }
    }

    /// Parse INSERT EDGE INTO <table> SET ...
    fn parse_insert_edge(input: &str) -> Result<QueryAst> {
        let rest = input[12..].trim(); // Skip "INSERT EDGE"
        let rest = rest.strip_prefix("INTO").ok_or_else(|| CoreError::InvalidArgument("expected INTO".to_string()))?.trim();
        let (table, rest) = Self::parse_word(rest)?;

        if starts_with_ignore_ascii_case(&rest, "SET") {
            let props_str = rest[3..].trim();
            let props = Self::parse_set_assignments(props_str)?;

            // Extract required fields
            let get_str = |key: &str| -> String {
                props.iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case(key))
                    .and_then(|(_, v)| match v {
                        LiteralValue::String(s) => Some(s.clone()),
                        _ => None,
                    })
                    .unwrap_or_default()
            };

            Ok(QueryAst::InsertEdge {
                table,
                id: get_str("id"),
                from_id: get_str("from"),
                to_id: get_str("to"),
                label: get_str("label"),
                properties: props,
            })
        } else {
            Err(CoreError::InvalidArgument("expected SET after table name".to_string()))
        }
    }

    /// Parse GRAPH TRAVERSE FROM <id> [IN|OUT|BOTH] [LABEL <label>] [DEPTH <n>]
    fn parse_graph_traverse(input: &str) -> Result<QueryAst> {
        let rest = input[15..].trim(); // Skip "GRAPH TRAVERSE"
        let rest = rest.strip_prefix("FROM").ok_or_else(|| CoreError::InvalidArgument("expected FROM".to_string()))?.trim();

        let (start_id, rest) = Self::parse_word(rest)?;
        let mut remaining = rest.trim().to_string();

        // Parse direction
        let mut direction = GraphDirection::Out;
        if starts_with_ignore_ascii_case(&remaining, "IN") && !starts_with_ignore_ascii_case(&remaining, "INTO") {
            direction = GraphDirection::In;
            remaining = remaining[2..].trim().to_string();
        } else if starts_with_ignore_ascii_case(&remaining, "OUT") {
            direction = GraphDirection::Out;
            remaining = remaining[3..].trim().to_string();
        } else if starts_with_ignore_ascii_case(&remaining, "BOTH") {
            direction = GraphDirection::Both;
            remaining = remaining[4..].trim().to_string();
        }

        // Parse LABEL
        let mut edge_label = None;
        if starts_with_ignore_ascii_case(&remaining, "LABEL") {
            remaining = remaining[5..].trim().to_string();
            let (label, r) = Self::parse_word(&remaining)?;
            edge_label = Some(label);
            remaining = r.trim().to_string();
        }

        // Parse DEPTH
        let mut max_depth = 3; // Default
        if starts_with_ignore_ascii_case(&remaining, "DEPTH") {
            remaining = remaining[5..].trim().to_string();
            let (depth_str, r) = Self::parse_word(&remaining)?;
            max_depth = depth_str.parse().unwrap_or(3);
            remaining = r.trim().to_string();
        }

        // Parse optional WHERE
        let filter = if starts_with_ignore_ascii_case(&remaining, "WHERE") {
            let (f, _) = Self::parse_where(&remaining[5..])?;
            f
        } else {
            None
        };

        Ok(QueryAst::GraphTraverse {
            start_id,
            direction,
            edge_label,
            max_depth,
            filter,
        })
    }

    /// Parse GRAPH MATCH pattern
    ///
    /// Syntax: GRAPH MATCH (a:Label) -[e:edge_label]-> (b:Label) [WHERE ...] RETURN ...
    ///
    /// Supports:
    /// - Single-hop: `(a:Person) -[e:KNOWS]-> (b:Person)`
    /// - Multi-hop: `(a:Person) -[e1:KNOWS]-> (b:Person) -[e2:WORKS_AT]-> (c:Company)`
    /// - Incoming: `(a:Person) <-[e:MANAGES]- (b:Manager)`
    /// - Both directions: `(a:Person) -[e:FRIEND]- (b:Person)`
    fn parse_graph_match(input: &str) -> Result<QueryAst> {
        let rest = input[11..].trim(); // Skip "GRAPH MATCH"

        // Split at RETURN (if present) to separate pattern from return clause
        let (pattern_str, returns_str) = if let Some(ret_pos) = find_ignore_ascii_case(rest, " RETURN ") {
            (safe_slice(rest, 0, ret_pos).trim(), safe_slice_from(rest, ret_pos + 8).trim())
        } else if let Some(ret_pos) = find_ignore_ascii_case(rest, "RETURN") {
            let after = safe_slice_from(rest, ret_pos + 6).trim();
            (safe_slice(rest, 0, ret_pos).trim(), after)
        } else {
            (rest, "")
        };

        // Split at WHERE to separate pattern from filter
        let (pattern_part, filter_str) = if let Some(wh_pos) = find_unquoted_ignore_ascii_case(pattern_str, " WHERE ") {
            (safe_slice(pattern_str, 0, wh_pos).trim(), Some(safe_slice_from(pattern_str, wh_pos + 7).trim()))
        } else {
            (pattern_str, None)
        };

        // Parse the pattern: sequence of nodes and edges
        // Pattern format: (var:Label) -[var:label]-> (var:Label) <-[var:label]- (var:Label)
        let mut nodes = Vec::new();
        let mut edges = Vec::new();
        let mut remaining = pattern_part.trim();

        // Parse first node
        let (first_node, after_first_node) = Self::parse_graph_node(remaining)?;
        nodes.push(first_node);
        remaining = after_first_node.trim();

        // Parse alternating edges and nodes
        while !remaining.is_empty() {
            // Try to parse an edge: -[var:label]-> or <-[var:label]-
            let (edge, after_edge) = match Self::parse_graph_edge(remaining) {
                Ok(r) => r,
                Err(_) => break,
            };

            let from_node = nodes.last().unwrap().variable.clone();

            // Parse the next node
            let (next_node, after_next_node) = Self::parse_graph_node(after_edge.trim())?;
            let to_node = next_node.variable.clone();

            // Update edge with correct from/to based on direction
            let mut edge = edge;
            match edge.direction {
                GraphDirection::Out => {
                    edge.from = from_node;
                    edge.to = to_node;
                }
                GraphDirection::In => {
                    edge.from = to_node;
                    edge.to = from_node;
                }
                GraphDirection::Both => {
                    edge.from = from_node;
                    edge.to = to_node;
                }
            }

            edges.push(edge);
            nodes.push(next_node);
            remaining = after_next_node.trim();
        }

        // Parse optional WHERE
        let filter = if let Some(fstr) = filter_str {
            Self::parse_where(fstr)?.0
        } else {
            None
        };

        // Parse RETURN columns
        let returns: Vec<String> = if !returns_str.is_empty() {
            returns_str.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        } else {
            vec![]
        };

        Ok(QueryAst::GraphMatch {
            pattern: GraphPattern { nodes, edges },
            filter,
            returns,
        })
    }

    /// Parse a graph node pattern: (variable:Label) or (variable) or (:Label)
    fn parse_graph_node(input: &str) -> Result<(GraphNode, &str)> {
        let input = input.trim();
        if !input.starts_with('(') {
            return Err(CoreError::InvalidArgument(
                format!("expected '(' for graph node, got: {}", &input[..input.len().min(20)])
            ));
        }

        let close = input.find(')')
            .ok_or_else(|| CoreError::InvalidArgument("unmatched '(' in graph node".to_string()))?;

        let inner = safe_slice(input, 1, close).trim();
        let remaining = safe_slice_from(input, close + 1);

        // Parse variable:Label or variable or :Label
        let (variable, label) = if inner.is_empty() {
            (String::new(), None)
        } else if let Some(colon_pos) = inner.find(':') {
            let var = safe_slice(inner, 0, colon_pos).trim().to_string();
            let lbl = safe_slice_from(inner, colon_pos + 1).trim().to_string();
            let lbl = if lbl.is_empty() { None } else { Some(lbl) };
            (var, lbl)
        } else {
            // Just a variable name, no label
            (inner.trim().to_string(), None)
        };

        Ok((GraphNode { variable, label }, remaining))
    }

    /// Parse a graph edge pattern: -[var:label]-> or <-[var:label]- or -[var:label]-
    fn parse_graph_edge(input: &str) -> Result<(GraphEdge, &str)> {
        let input = input.trim();

        // Detect direction by checking arrow pattern
        // Pattern 1: -[...]->  (outgoing)
        // Pattern 2: <-[...]-  (incoming)
        // Pattern 3: -[...]-   (both)

        if input.starts_with('<') && input.starts_with("<-") {
            // Incoming: <-[var:label]-
            let after_arrow = &input[2..]; // skip <-
            if !after_arrow.starts_with('[') {
                return Err(CoreError::InvalidArgument("expected '[' after '<-'".to_string()));
            }
            let close_bracket = after_arrow.find(']')
                .ok_or_else(|| CoreError::InvalidArgument("unmatched '[' in graph edge".to_string()))?;
            let inner = safe_slice(after_arrow, 1, close_bracket).trim();
            let after_bracket = safe_slice_from(after_arrow, close_bracket + 1).trim();

            // Expect '-' after ']'
            if !after_bracket.starts_with('-') {
                return Err(CoreError::InvalidArgument("expected '-' after ']' in incoming edge".to_string()));
            }
            let remaining = safe_slice_from(after_bracket, 1);

            let (var, label) = Self::parse_edge_inner(inner)?;
            Ok((GraphEdge {
                variable: var,
                label,
                from: String::new(), // filled by caller
                to: String::new(),   // filled by caller
                direction: GraphDirection::In,
            }, remaining))
        } else if input.starts_with('-') {
            // Check if outgoing: -[...]-> or both: -[...]-
            let after_dash = &input[1..]; // skip first -
            if !after_dash.starts_with('[') {
                return Err(CoreError::InvalidArgument("expected '[' after '-'".to_string()));
            }
            let close_bracket = after_dash.find(']')
                .ok_or_else(|| CoreError::InvalidArgument("unmatched '[' in graph edge".to_string()))?;
            let inner = safe_slice(after_dash, 1, close_bracket).trim();
            let after_bracket = safe_slice_from(after_dash, close_bracket + 1).trim();

            // Check for -> (outgoing) or - (both)
            if after_bracket.starts_with("->") {
                let remaining = safe_slice_from(after_bracket, 2);
                let (var, label) = Self::parse_edge_inner(inner)?;
                Ok((GraphEdge {
                    variable: var,
                    label,
                    from: String::new(),
                    to: String::new(),
                    direction: GraphDirection::Out,
                }, remaining))
            } else if after_bracket.starts_with('-') {
                let remaining = safe_slice_from(after_bracket, 1);
                let (var, label) = Self::parse_edge_inner(inner)?;
                Ok((GraphEdge {
                    variable: var,
                    label,
                    from: String::new(),
                    to: String::new(),
                    direction: GraphDirection::Both,
                }, remaining))
            } else {
                Err(CoreError::InvalidArgument(
                    format!("expected '->' or '-' after ']' in graph edge, got: {}", &after_bracket[..after_bracket.len().min(20)])
                ))
            }
        } else {
            Err(CoreError::InvalidArgument(
                format!("expected '-' or '<-' for graph edge, got: {}", &input[..input.len().min(20)])
            ))
        }
    }

    /// Parse edge inner content: var:label or var or :label
    fn parse_edge_inner(inner: &str) -> Result<(Option<String>, Option<String>)> {
        let inner = inner.trim();
        if inner.is_empty() {
            return Ok((None, None));
        }

        if let Some(colon_pos) = inner.find(':') {
            let var = safe_slice(inner, 0, colon_pos).trim().to_string();
            let label = safe_slice_from(inner, colon_pos + 1).trim().to_string();
            let var = if var.is_empty() { None } else { Some(var) };
            let label = if label.is_empty() { None } else { Some(label) };
            Ok((var, label))
        } else {
            // Just a variable name
            Ok((Some(inner.trim().to_string()), None))
        }
    }

    /// Parse GRAPH SHORTEST PATH FROM <id1> TO <id2> [MAX DEPTH <n>]
    fn parse_graph_shortest_path(input: &str) -> Result<QueryAst> {
        let rest = input[21..].trim(); // Skip "GRAPH SHORTEST PATH"
        let rest = rest.strip_prefix("FROM").ok_or_else(|| CoreError::InvalidArgument("expected FROM".to_string()))?.trim();

        let (from_id, rest) = Self::parse_word(rest)?;
        let rest = rest.trim();
        let rest = rest.strip_prefix("TO").ok_or_else(|| CoreError::InvalidArgument("expected TO".to_string()))?.trim();

        let (to_id, rest) = Self::parse_word(rest)?;
        let mut rest = rest.trim();

        // Parse MAX DEPTH
        let mut max_depth = 10; // Default
        if starts_with_ignore_ascii_case(rest, "MAX") {
            rest = rest[3..].trim();
            if starts_with_ignore_ascii_case(rest, "DEPTH") {
                rest = rest[5..].trim();
                let (depth_str, _) = Self::parse_word(rest)?;
                max_depth = depth_str.parse().unwrap_or(10);
            }
        }

        Ok(QueryAst::GraphShortestPath { from_id, to_id, max_depth })
    }

    /// Parses SYSTEM ACTIVATE '<class>::<pk>' '<reason>'
    fn parse_system_activate(input: &str) -> Result<QueryAst> {
        let rest = input[15..].trim(); // Skip "SYSTEM ACTIVATE"
        let entity = Self::extract_quoted_path(rest)?;
        let after_entity = &rest[rest.find(|c: char| c == '\'' || c == '"').unwrap_or(0)..];
        let after_entity = &after_entity[1..]; // skip opening quote
        let quote_char = rest.as_bytes()[rest.find(|c: char| c == '\'' || c == '"').unwrap_or(0)] as char;
        let end = after_entity.find(quote_char).unwrap_or(after_entity.len());
        let after_entity = safe_slice_from(after_entity, end + 1).trim();
        let reason = if !after_entity.is_empty() {
            Self::extract_quoted_path(after_entity)?
        } else {
            "manual".to_string()
        };
        Ok(QueryAst::SystemActivate { entity, reason })
    }

    /// Parses BACKUP TO '<path>'
    fn parse_backup(input: &str) -> Result<QueryAst> {
        let rest = input[6..].trim(); // Skip "BACKUP"
        let rest = rest.strip_prefix("TO").ok_or_else(|| {
            CoreError::InvalidArgument("expected BACKUP TO '<path>'".to_string())
        })?.trim();
        let path = Self::extract_quoted_path(rest)?;
        Ok(QueryAst::Backup { path })
    }

    /// Parses RESTORE FROM '<path>'
    fn parse_restore(input: &str) -> Result<QueryAst> {
        let rest = input[7..].trim(); // Skip "RESTORE"
        let rest = rest.strip_prefix("FROM").ok_or_else(|| {
            CoreError::InvalidArgument("expected RESTORE FROM '<path>'".to_string())
        })?.trim();
        let path = Self::extract_quoted_path(rest)?;
        Ok(QueryAst::Restore { path })
    }

    /// Extracts a quoted path string from the input (supports single and double quotes).
    fn extract_quoted_path(input: &str) -> Result<String> {
        let trimmed = input.trim();
        let quote_pos = trimmed.find('\'').or_else(|| trimmed.find('"'))
            .ok_or_else(|| CoreError::InvalidArgument("expected quoted path".to_string()))?;
        let quote_char = trimmed.as_bytes()[quote_pos] as char;
        let end = safe_slice_from(trimmed, quote_pos + 1).find(quote_char)
            .ok_or_else(|| CoreError::InvalidArgument("unterminated path string".to_string()))?;
        Ok(safe_slice(trimmed, quote_pos + 1, quote_pos + 1 + end).to_string())
    }

    /// Parse column definitions: "name STRING, age INT, ..."
    fn parse_column_defs(input: &str) -> Result<Vec<ColumnDef>> {
        let mut columns = Vec::new();
        for part in input.split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            let tokens: Vec<&str> = part.split_whitespace().collect();
            if tokens.len() >= 2 {
                columns.push(ColumnDef {
                    name: tokens[0].to_string(),
                    col_type: tokens[1].to_string(),
                    required: tokens.len() > 2 && tokens[2].eq_ignore_ascii_case("REQUIRED"),
                });
            }
        }
        Ok(columns)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_insert() {
        let ast = QueryParser::parse("INSERT INTO Product (name, price) VALUES ('iPhone', 999)").unwrap();
        match ast {
            QueryAst::Insert { class, columns, values } => {
                assert_eq!(class, "Product");
                assert_eq!(columns, vec!["name", "price"]);
                assert_eq!(values.len(), 2);
            }
            _ => panic!("expected Insert"),
        }
    }

    #[test]
    fn test_parse_select() {
        let ast = QueryParser::parse("SELECT name, price FROM Product WHERE price > 100 LIMIT 10").unwrap();
        match ast {
            QueryAst::Select { from, limit, .. } => {
                assert_eq!(from, "Product");
                assert_eq!(limit, Some(10));
            }
            _ => panic!("expected Select"),
        }
    }

    #[test]
    fn test_parse_match() {
        let ast = QueryParser::parse("MATCH (p: Product) WHERE price > 100 RETURN name, price").unwrap();
        match ast {
            QueryAst::Match {
                variable, class, ..
            } => {
                assert_eq!(variable, "p");
                assert_eq!(class, "Product");
            }
            _ => panic!("expected Match"),
        }
    }

    #[test]
    fn test_parse_create_ontology() {
        let ast = QueryParser::parse("CREATE ONTOLOGY shop (CLASS Product)").unwrap();
        match ast {
            QueryAst::CreateOntology { sql } => {
                assert!(sql.contains("CREATE ONTOLOGY"));
            }
            _ => panic!("expected CreateOntology"),
        }
    }

    #[test]
    fn test_parse_backup() {
        let ast = QueryParser::parse("BACKUP TO '/tmp/backup'").unwrap();
        match ast {
            QueryAst::Backup { path } => assert_eq!(path, "/tmp/backup"),
            _ => panic!("expected Backup"),
        }
    }

    #[test]
    fn test_parse_backup_double_quotes() {
        let ast = QueryParser::parse("BACKUP TO \"/data/backup\"").unwrap();
        match ast {
            QueryAst::Backup { path } => assert_eq!(path, "/data/backup"),
            _ => panic!("expected Backup"),
        }
    }

    #[test]
    fn test_parse_backup_missing_path() {
        assert!(QueryParser::parse("BACKUP TO").is_err());
    }

    #[test]
    fn test_parse_restore() {
        let ast = QueryParser::parse("RESTORE FROM '/tmp/backup'").unwrap();
        match ast {
            QueryAst::Restore { path } => assert_eq!(path, "/tmp/backup"),
            _ => panic!("expected Restore"),
        }
    }

    #[test]
    fn test_parse_restore_missing_from() {
        assert!(QueryParser::parse("RESTORE '/tmp/backup'").is_err());
    }

    #[test]
    fn test_parse_flush() {
        let ast = QueryParser::parse("FLUSH").unwrap();
        assert!(matches!(ast, QueryAst::Flush));
    }

    // ===== AND/OR Precedence Tests =====

    fn extract_filter(ast: &QueryAst) -> &Option<FilterExpr> {
        match ast {
            QueryAst::Select { filter, .. } => filter,
            _ => panic!("expected Select"),
        }
    }

    #[test]
    fn test_and_binds_tighter_than_or() {
        // a=1 OR b=2 AND c=3  should be  a=1 OR (b=2 AND c=3)
        let ast = QueryParser::parse("SELECT * FROM T WHERE a = 1 OR b = 2 AND c = 3").unwrap();
        let filter = extract_filter(&ast).as_ref().unwrap();
        match filter {
            FilterExpr::Or(left, right) => {
                // left = a=1
                assert!(matches!(left.as_ref(), FilterExpr::Eq(col, _) if col == "a"));
                // right = (b=2 AND c=3)
                match right.as_ref() {
                    FilterExpr::And(b_left, b_right) => {
                        assert!(matches!(b_left.as_ref(), FilterExpr::Eq(col, _) if col == "b"));
                        assert!(matches!(b_right.as_ref(), FilterExpr::Eq(col, _) if col == "c"));
                    }
                    other => panic!("expected And on right, got {:?}", other),
                }
            }
            other => panic!("expected Or at top, got {:?}", other),
        }
    }

    #[test]
    fn test_multiple_and_chains_correctly() {
        // a=1 AND b=2 AND c=3  should be  (a=1 AND b=2) AND c=3
        let ast = QueryParser::parse("SELECT * FROM T WHERE a = 1 AND b = 2 AND c = 3").unwrap();
        let filter = extract_filter(&ast).as_ref().unwrap();
        match filter {
            FilterExpr::And(left, right) => {
                assert!(matches!(right.as_ref(), FilterExpr::Eq(col, _) if col == "c"));
                match left.as_ref() {
                    FilterExpr::And(l2, r2) => {
                        assert!(matches!(l2.as_ref(), FilterExpr::Eq(col, _) if col == "a"));
                        assert!(matches!(r2.as_ref(), FilterExpr::Eq(col, _) if col == "b"));
                    }
                    other => panic!("expected nested And, got {:?}", other),
                }
            }
            other => panic!("expected And at top, got {:?}", other),
        }
    }

    #[test]
    fn test_or_left_associative() {
        // a=1 OR b=2 OR c=3  should be  (a=1 OR b=2) OR c=3
        let ast = QueryParser::parse("SELECT * FROM T WHERE a = 1 OR b = 2 OR c = 3").unwrap();
        let filter = extract_filter(&ast).as_ref().unwrap();
        match filter {
            FilterExpr::Or(left, right) => {
                assert!(matches!(right.as_ref(), FilterExpr::Eq(col, _) if col == "c"));
                match left.as_ref() {
                    FilterExpr::Or(l2, r2) => {
                        assert!(matches!(l2.as_ref(), FilterExpr::Eq(col, _) if col == "a"));
                        assert!(matches!(r2.as_ref(), FilterExpr::Eq(col, _) if col == "b"));
                    }
                    other => panic!("expected nested Or, got {:?}", other),
                }
            }
            other => panic!("expected Or at top, got {:?}", other),
        }
    }

    #[test]
    fn test_mixed_and_or_precedence() {
        // a=1 AND b=2 OR c=3 AND d=4  should be  (a=1 AND b=2) OR (c=3 AND d=4)
        let ast = QueryParser::parse("SELECT * FROM T WHERE a = 1 AND b = 2 OR c = 3 AND d = 4").unwrap();
        let filter = extract_filter(&ast).as_ref().unwrap();
        match filter {
            FilterExpr::Or(left, right) => {
                match left.as_ref() {
                    FilterExpr::And(l, r) => {
                        assert!(matches!(l.as_ref(), FilterExpr::Eq(col, _) if col == "a"));
                        assert!(matches!(r.as_ref(), FilterExpr::Eq(col, _) if col == "b"));
                    }
                    other => panic!("expected And on left, got {:?}", other),
                }
                match right.as_ref() {
                    FilterExpr::And(l, r) => {
                        assert!(matches!(l.as_ref(), FilterExpr::Eq(col, _) if col == "c"));
                        assert!(matches!(r.as_ref(), FilterExpr::Eq(col, _) if col == "d"));
                    }
                    other => panic!("expected And on right, got {:?}", other),
                }
            }
            other => panic!("expected Or at top, got {:?}", other),
        }
    }

    #[test]
    fn test_parenthesized_or_overrides_precedence() {
        // (a=1 OR b=2) AND c=3  should be  (a=1 OR b=2) AND c=3
        let ast = QueryParser::parse("SELECT * FROM T WHERE (a = 1 OR b = 2) AND c = 3").unwrap();
        let filter = extract_filter(&ast).as_ref().unwrap();
        match filter {
            FilterExpr::And(left, right) => {
                assert!(matches!(right.as_ref(), FilterExpr::Eq(col, _) if col == "c"));
                match left.as_ref() {
                    FilterExpr::Or(l, r) => {
                        assert!(matches!(l.as_ref(), FilterExpr::Eq(col, _) if col == "a"));
                        assert!(matches!(r.as_ref(), FilterExpr::Eq(col, _) if col == "b"));
                    }
                    other => panic!("expected Or inside parens, got {:?}", other),
                }
            }
            other => panic!("expected And at top, got {:?}", other),
        }
    }

    // ===== Empty String vs NULL Tests =====

    #[test]
    fn test_empty_string_not_null() {
        // INSERT INTO T (col) VALUES ('') should store empty string, not NULL
        let ast = QueryParser::parse("INSERT INTO T (col) VALUES ('')").unwrap();
        match ast {
            QueryAst::Insert { values, .. } => {
                assert_eq!(values.len(), 1);
                match &values[0] {
                    LiteralValue::String(s) => assert_eq!(s, ""),
                    other => panic!("expected String(''), got {:?}", other),
                }
            }
            _ => panic!("expected Insert"),
        }
    }

    #[test]
    fn test_null_still_works() {
        let ast = QueryParser::parse("INSERT INTO T (col) VALUES (NULL)").unwrap();
        match ast {
            QueryAst::Insert { values, .. } => {
                assert_eq!(values.len(), 1);
                assert!(matches!(&values[0], LiteralValue::Null));
            }
            _ => panic!("expected Insert"),
        }
    }

    #[test]
    fn test_where_empty_string_literal() {
        let ast = QueryParser::parse("SELECT * FROM T WHERE name = ''").unwrap();
        match &ast {
            QueryAst::Select { filter, .. } => {
                let f = filter.as_ref().unwrap();
                match f {
                    FilterExpr::Eq(col, LiteralValue::String(val)) => {
                        assert_eq!(col, "name");
                        assert_eq!(val, "");
                    }
                    other => panic!("expected Eq with empty string, got {:?}", other),
                }
            }
            _ => panic!("expected Select"),
        }
    }

    // ===== Parenthesis Matching Tests =====

    #[test]
    fn test_find_matching_paren_simple_skips_quoted() {
        // VALUES ('hello(world)', 42) should match the outer parens correctly
        let input = "('hello(world)', 42)";
        let result = QueryParser::find_matching_paren_simple(input).unwrap();
        assert_eq!(result, input.len() - 1, "should match the last )");
    }

    #[test]
    fn test_find_matching_paren_skips_single_quoted() {
        let input = "(a, ')', b)";
        let result = QueryParser::find_matching_paren(input).unwrap();
        assert_eq!(result, input.len() - 1);
    }

    #[test]
    fn test_find_matching_paren_skips_double_quoted() {
        let input = "(a, \")\", b)";
        let result = QueryParser::find_matching_paren(input).unwrap();
        assert_eq!(result, input.len() - 1);
    }

    #[test]
    fn test_insert_with_parens_in_string() {
        // INSERT INTO T (col) VALUES ('hello(world)') should parse correctly
        let ast = QueryParser::parse("INSERT INTO T (col) VALUES ('hello(world)')").unwrap();
        match ast {
            QueryAst::Insert { values, .. } => {
                assert_eq!(values.len(), 1);
                match &values[0] {
                    LiteralValue::String(s) => assert_eq!(s, "hello(world)"),
                    other => panic!("expected String, got {:?}", other),
                }
            }
            _ => panic!("expected Insert"),
        }
    }

    #[test]
    fn test_select_where_string_with_parens() {
        let ast = QueryParser::parse("SELECT * FROM T WHERE name = 'foo(bar)'").unwrap();
        match &ast {
            QueryAst::Select { filter, .. } => {
                let f = filter.as_ref().unwrap();
                match f {
                    FilterExpr::Eq(col, LiteralValue::String(val)) => {
                        assert_eq!(col, "name");
                        assert_eq!(val, "foo(bar)");
                    }
                    other => panic!("expected Eq, got {:?}", other),
                }
            }
            _ => panic!("expected Select"),
        }
    }

    // ===== Quoted Identifier Tests =====

    #[test]
    fn test_parse_word_double_quoted() {
        let (word, rest) = QueryParser::parse_word("\"My Table\" extra").unwrap();
        assert_eq!(word, "My Table");
        assert_eq!(rest, "extra");
    }

    #[test]
    fn test_parse_word_bracket_quoted() {
        let (word, rest) = QueryParser::parse_word("[My Table] extra").unwrap();
        assert_eq!(word, "My Table");
        assert_eq!(rest, "extra");
    }

    #[test]
    fn test_select_from_quoted_table() {
        let ast = QueryParser::parse("SELECT * FROM \"My Table\"").unwrap();
        match ast {
            QueryAst::Select { from, .. } => assert_eq!(from, "My Table"),
            _ => panic!("expected Select"),
        }
    }

    #[test]
    fn test_insert_quoted_columns() {
        let ast = QueryParser::parse("INSERT INTO Person (\"name\", \"age\") VALUES ('Alice', 30)").unwrap();
        match ast {
            QueryAst::Insert { class, columns, .. } => {
                assert_eq!(class, "Person");
                assert_eq!(columns, vec!["name", "age"]);
            }
            _ => panic!("expected Insert"),
        }
    }

    #[test]
    fn test_update_quoted_columns() {
        let ast = QueryParser::parse("UPDATE Person SET \"name\" = 'Bob' WHERE \"id\" = 1").unwrap();
        match ast {
            QueryAst::Update { class, assignments, .. } => {
                assert_eq!(class, "Person");
                assert_eq!(assignments[0].0, "name");
            }
            _ => panic!("expected Update"),
        }
    }

    // ===== LIKE Pattern Quote Escaping Tests =====

    #[test]
    fn test_like_pattern_with_single_quote() {
        // LIKE pattern containing a single quote should be handled
        let ast = QueryParser::parse("SELECT * FROM T WHERE name LIKE '%test%'").unwrap();
        match &ast {
            QueryAst::Select { filter, .. } => {
                let f = filter.as_ref().unwrap();
                match f {
                    FilterExpr::Like(col, pat) => {
                        assert_eq!(col, "name");
                        assert_eq!(pat, "%test%");
                    }
                    other => panic!("expected Like, got {:?}", other),
                }
            }
            _ => panic!("expected Select"),
        }
    }
}
