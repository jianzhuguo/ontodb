//! SQL parser with semantic extensions for OntoDB.
//!
//! Supports standard SQL (SELECT, INSERT, UPDATE, DELETE)
//! plus OntoDB extensions:
//! - CREATE ONTOLOGY
//! - MATCH (p: ClassName) - semantic class-based queries

use onto_core::{CoreError, Result};
use serde::{Deserialize, Serialize};

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

            // Writes
            QueryAst::Insert { .. } => false,
            QueryAst::BatchInsert { .. } => false,
            QueryAst::InsertSelect { .. } => false,
            QueryAst::Upsert { .. } => false,
            QueryAst::Update { .. } => false,
            QueryAst::Delete { .. } => false,
            QueryAst::Import { .. } => false,

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

#[derive(Debug, Clone, Serialize, Deserialize)]
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

        let upper = input.to_uppercase();

        if upper.starts_with("EXPLAIN") {
            Self::parse_explain(input)
        } else if upper.starts_with("WITH") {
            Self::parse_with(input)
        } else if upper.starts_with("CREATE ONTOLOGY") {
            Ok(QueryAst::CreateOntology {
                sql: input.to_string(),
            })
        } else if upper.starts_with("CREATE VECTOR INDEX") {
            Self::parse_create_vector_index(input)
        } else if upper.starts_with("DROP VECTOR INDEX") {
            Self::parse_drop_vector_index(input)
        } else if upper.starts_with("VECTOR SEARCH") {
            Self::parse_vector_search(input)
        } else if upper.starts_with("CREATE MATERIALIZED VIEW") {
            Self::parse_create_materialized_view(input)
        } else if upper.starts_with("DROP MATERIALIZED VIEW") {
            Self::parse_drop_materialized_view(input)
        } else if upper.starts_with("REFRESH MATERIALIZED VIEW") {
            Self::parse_refresh_materialized_view(input)
        } else if upper.starts_with("CREATE INDEX") {
            Self::parse_create_index(input)
        } else if upper.starts_with("DROP INDEX") {
            Self::parse_drop_index(input)
        } else if upper.starts_with("ANALYZE") {
            Self::parse_analyze(input)
        } else if upper.starts_with("BEGIN") {
            Ok(QueryAst::Begin)
        } else if upper.starts_with("COMMIT") {
            Ok(QueryAst::Commit)
        } else if upper.starts_with("ROLLBACK") {
            Ok(QueryAst::Rollback)
        } else if upper.starts_with("IMPORT") {
            Self::parse_import(input)
        } else if upper.starts_with("INSERT") {
            Self::parse_insert(input)
        } else if upper.starts_with("SELECT") {
            let ast = Self::parse_select(input)?;
            Self::try_wrap_union(input, ast)
        } else if upper.starts_with("UPDATE") {
            Self::parse_update(input)
        } else if upper.starts_with("DELETE") {
            Self::parse_delete(input)
        } else if upper.starts_with("MATCH") {
            Self::parse_match(input)
        } else {
            Err(CoreError::InvalidArgument(format!(
                "unsupported query: {}",
                input
            )))
        }
    }

    /// Parses EXPLAIN <query>
    fn parse_explain(input: &str) -> Result<QueryAst> {
        let upper = input.to_uppercase();
        let query_start = if upper.starts_with("EXPLAIN ANALYZE") {
            14
        } else if upper.starts_with("EXPLAIN") {
            7
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
        Ok(QueryAst::Explain {
            query: Box::new(inner_ast),
        })
    }

    /// Parses WITH [RECURSIVE] <cte_name> AS (<query>) <main_query>
    fn parse_with(input: &str) -> Result<QueryAst> {
        let upper = input.to_uppercase();
        if !upper.starts_with("WITH") {
            return Err(CoreError::InvalidArgument("expected WITH".to_string()));
        }

        let mut remaining = input[4..].trim();
        let mut recursive = false;

        // Check for RECURSIVE keyword
        if remaining.to_uppercase().starts_with("RECURSIVE") {
            recursive = true;
            remaining = remaining[9..].trim();
        }

        let mut ctes = Vec::new();

        loop {
            let remaining_upper = remaining.to_uppercase();

            // Parse CTE name
            let as_pos = remaining_upper.find(" AS ")
                .ok_or_else(|| CoreError::InvalidArgument("expected 'AS' after CTE name".to_string()))?;

            let name_part = remaining[..as_pos].trim();
            remaining = remaining[as_pos + 4..].trim();

            // Parse optional column aliases
            let (name, columns) = if name_part.starts_with('(') {
                let close = name_part.find(')')
                    .ok_or_else(|| CoreError::InvalidArgument("expected ')' in CTE column list".to_string()));
                let close = close?;
                let cte_name = name_part[1..close].trim().to_string();
                let cols_str = &name_part[1..close];
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

            remaining = remaining[close + 1..].trim();

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
        let upper = input.to_uppercase();
        // Find UNION that's not inside parentheses
        let mut depth = 0i32;
        let mut in_quote: Option<char> = None;
        let bytes = upper.as_bytes();
        let mut i = 0;

        while i + 5 <= bytes.len() {
            let c = bytes[i] as char;
            if let Some(q) = in_quote {
                if c == q { in_quote = None; }
            } else if c == '\'' || c == '"' {
                in_quote = Some(c);
            } else if c == '(' {
                depth += 1;
            } else if c == ')' {
                depth -= 1;
            } else if depth == 0 && bytes[i..].starts_with(b"UNION") {
                let after = &input[i + 5..].trim_start();
                let (all, rest) = if after.to_uppercase().starts_with("ALL") {
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
        let upper = input.to_uppercase();

        // IMPORT INTO <class> FROM <format> '<path>'
        let into_pos = upper.find("INTO")
            .ok_or_else(|| CoreError::InvalidArgument("expected 'INTO' after IMPORT".to_string()))?;

        let from_pos = upper.find(" FROM ")
            .ok_or_else(|| CoreError::InvalidArgument("expected 'FROM' in IMPORT".to_string()))?;

        let class = input[into_pos + 4..from_pos].trim().to_string();
        if class.is_empty() {
            return Err(CoreError::InvalidArgument("missing class name in IMPORT".to_string()));
        }

        let after_from = input[from_pos + 6..].trim();
        let after_from_upper = after_from.to_uppercase();

        let format = if after_from_upper.starts_with("CSV") {
            ImportFormat::Csv
        } else if after_from_upper.starts_with("JSON") {
            ImportFormat::Json
        } else {
            return Err(CoreError::InvalidArgument("expected CSV or JSON after FROM".to_string()));
        };

        // Extract file path (between quotes)
        let path_start = after_from.find('\'')
            .or_else(|| after_from.find('"'))
            .ok_or_else(|| CoreError::InvalidArgument("expected quoted file path in IMPORT".to_string()))?;
        let quote_char = after_from.as_bytes()[path_start] as char;
        let path_end = after_from[path_start + 1..].find(quote_char)
            .ok_or_else(|| CoreError::InvalidArgument("unterminated file path in IMPORT".to_string()))?;
        let file_path = after_from[path_start + 1..path_start + 1 + path_end].to_string();

        Ok(QueryAst::Import { class, file_path, format })
    }

    fn parse_insert(input: &str) -> Result<QueryAst> {
        let upper = input.to_uppercase();

        let into_pos = upper
            .find("INTO")
            .ok_or_else(|| CoreError::InvalidArgument("expected 'INTO'".to_string()))?;

        // Check if this is INSERT INTO ... SELECT
        let select_pos = Self::find_unquoted(&upper, " SELECT ");
        if let Some(sp) = select_pos {
            let header = input[into_pos + 4..sp].trim();
            let (class, columns) = Self::parse_insert_header(header)?;
            let query_str = input[sp + 1..].trim();
            let query = Self::parse(query_str)?;
            return Ok(QueryAst::InsertSelect { class, columns, query: Box::new(query) });
        }

        let values_pos = upper
            .find("VALUES")
            .ok_or_else(|| CoreError::InvalidArgument("expected 'VALUES'".to_string()))?;

        let header = input[into_pos + 4..values_pos].trim();
        let values_str = input[values_pos + 6..].trim();

        let (class, columns) = Self::parse_insert_header(header)?;

        // Check for ON CONFLICT (UPSERT)
        let on_conflict_upper = values_str.to_uppercase();
        let (values_str, upsert_info) = if let Some(oc_pos) = Self::find_unquoted(&on_conflict_upper, " ON CONFLICT ") {
            let vals_part = &values_str[..oc_pos];
            let conflict_part = &values_str[oc_pos + 13..].trim();
            // Parse: (col) DO UPDATE SET col1 = val1, col2 = val2
            let conflict_upper = conflict_part.to_uppercase();
            let do_update_pos = conflict_upper.find(" DO UPDATE SET ")
                .ok_or_else(|| CoreError::InvalidArgument("expected 'DO UPDATE SET' after ON CONFLICT".to_string()))?;
            let conflict_col = conflict_part[..do_update_pos].trim();
            let conflict_col = conflict_col.trim_start_matches('(').trim_end_matches(')').trim().to_string();
            let set_part = conflict_part[do_update_pos + 15..].trim();
            let assignments = Self::parse_set_assignments(set_part)?;
            (vals_part, Some((conflict_col, assignments)))
        } else {
            (values_str, None)
        };

        // Parse values: (val1, val2, ...) or (v1, v2), (v3, v4), ...
        let values_str = values_str.trim();

        // Count opening parens to detect batch insert
        let open_count = values_str.matches('(').count();
        if open_count > 1 && values_str.contains("),") {
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
                remaining = remaining[close + 1..].trim();
                if remaining.starts_with(',') {
                    remaining = remaining[1..].trim();
                }
            }
            if let Some((conflict_col, assignments)) = upsert_info {
                // Upsert with batch - use first row for now
                Ok(QueryAst::Upsert {
                    class,
                    columns,
                    values: rows.into_iter().next().unwrap_or_default(),
                    conflict_column: conflict_col,
                    assignments,
                })
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
        let class = header[..paren_start].trim().to_string();
        let cols_str = &header[paren_start + 1..header.len() - 1];
        let columns: Vec<String> = cols_str
            .split(',')
            .map(|s| s.trim().to_string())
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
                let col = s[..eq_pos].trim().to_string();
                let val = Self::parse_literal(s[eq_pos + 1..].trim())?;
                Ok((col, val))
            })
            .collect::<Result<Vec<_>>>()
    }

    /// Simple matching paren finder (no quote handling needed for values).
    fn find_matching_paren_simple(input: &str) -> Result<usize> {
        let mut depth = 0;
        for (i, c) in input.char_indices() {
            if c == '(' { depth += 1; }
            if c == ')' {
                depth -= 1;
                if depth == 0 {
                    return Ok(i);
                }
            }
        }
        Err(CoreError::InvalidArgument("unmatched parenthesis".to_string()))
    }

    fn parse_select(input: &str) -> Result<QueryAst> {
        let upper = input.to_uppercase();

        let from_pos = upper
            .find(" FROM ")
            .ok_or_else(|| CoreError::InvalidArgument("expected 'FROM'".to_string()))?;

        let after_select = input[6..from_pos].trim(); // After "SELECT"

        // Check for DISTINCT keyword
        let (distinct, cols_str) = if after_select.to_uppercase().starts_with("DISTINCT") {
            (true, after_select[8..].trim())
        } else {
            (false, after_select)
        };

        let rest = input[from_pos + 6..].trim();

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
        let (filter, rest) = if rest.to_uppercase().starts_with("WHERE") {
            let rest = rest[5..].trim();
            Self::parse_where(rest)?
        } else {
            (None, rest.to_string())
        };

        // Parse optional GROUP BY
        let rest_upper = rest.to_uppercase();
        let (group_by, rest) = if rest_upper.trim_start().starts_with("GROUP BY") {
            let pos = Self::find_unquoted(&rest_upper, "GROUP BY")
                .ok_or_else(|| CoreError::InvalidArgument("expected 'GROUP BY'".to_string()))?;
            let rest = rest[pos + 8..].trim();
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
        let rest_upper = rest.to_uppercase();
        let (having, rest) = if rest_upper.trim_start().starts_with("HAVING") {
            let pos = Self::find_unquoted(&rest_upper, "HAVING")
                .ok_or_else(|| CoreError::InvalidArgument("expected 'HAVING'".to_string()))?;
            let rest = rest[pos + 6..].trim();
            Self::parse_where(rest)?
        } else {
            (None, rest)
        };

        // Parse optional ORDER BY (multi-column)
        let rest_upper = rest.to_uppercase();
        let rest = rest; // reborrow
        let (order_by, rest) = if rest_upper.trim_start().starts_with("ORDER BY") {
            let start = Self::find_unquoted(&rest_upper, "ORDER BY")
                .ok_or_else(|| CoreError::InvalidArgument("expected 'ORDER BY'".to_string()))?;
            let (ob_str, rest) = Self::consume_until_keywords(rest[start + 8..].trim(), &["LIMIT", "OFFSET"]);
            let mut order_cols = Vec::new();
            for part in Self::split_quoted(ob_str.trim(), ',') {
                let part = part.trim();
                let part_upper = part.to_uppercase();
                let (col, ascending) = if part_upper.ends_with(" DESC") {
                    (part[..part.len() - 5].trim().to_string(), false)
                } else if part_upper.ends_with(" ASC") {
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
        let rest_upper = rest.to_uppercase();
        let (limit, offset, _rest) = if rest_upper.trim_start().starts_with("LIMIT") {
            let start = Self::find_unquoted(&rest_upper, "LIMIT")
                .ok_or_else(|| CoreError::InvalidArgument("expected 'LIMIT'".to_string()))?;
            let after_limit = rest[start + 5..].trim();
            // Parse limit number (may be followed by OFFSET or end)
            let (num_str, after_num) = Self::parse_word(after_limit)?;
            let limit_val = num_str
                .parse::<usize>()
                .map_err(|_| CoreError::InvalidArgument("invalid LIMIT".to_string()))?;
            // Check for OFFSET
            let after_upper = after_num.to_uppercase();
            if after_upper.trim_start().starts_with("OFFSET") {
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
        let rest_upper = rest.to_uppercase();

        if rest_upper.starts_with("AS") {
            let rest = rest[2..].trim();
            let (alias, rest) = Self::parse_word(rest)?;
            Ok((table, Some(alias), rest))
        } else if !rest.is_empty()
            && !rest_upper.starts_with("WHERE")
            && !rest_upper.starts_with("JOIN")
            && !rest_upper.starts_with("LEFT")
            && !rest_upper.starts_with("RIGHT")
            && !rest_upper.starts_with("FULL")
            && !rest_upper.starts_with("INNER")
            && !rest_upper.starts_with("ON")
            && !rest_upper.starts_with("GROUP")
            && !rest_upper.starts_with("HAVING")
            && !rest_upper.starts_with("ORDER")
            && !rest_upper.starts_with("LIMIT")
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
            let upper = rest.to_uppercase().trim().to_string();

            // Detect join type
            let (join_type, skip_len) = if upper.starts_with("LEFT JOIN") || upper.starts_with("LEFT OUTER JOIN") {
                let len = if upper.starts_with("LEFT OUTER JOIN") { 15 } else { 10 };
                (JoinType::Left, len)
            } else if upper.starts_with("RIGHT JOIN") || upper.starts_with("RIGHT OUTER JOIN") {
                let len = if upper.starts_with("RIGHT OUTER JOIN") { 16 } else { 11 };
                (JoinType::Right, len)
            } else if upper.starts_with("FULL JOIN") || upper.starts_with("FULL OUTER JOIN") {
                let len = if upper.starts_with("FULL OUTER JOIN") { 15 } else { 10 };
                (JoinType::Full, len)
            } else if upper.starts_with("INNER JOIN") {
                (JoinType::Inner, 11)
            } else if upper.starts_with("JOIN") {
                (JoinType::Inner, 4)
            } else {
                break;
            };

            // Skip join keyword
            let after_join = rest[skip_len..].trim();

            // Parse table name and optional alias
            let (table, alias, after_table) = Self::parse_from_clause(after_join)?;

            // Parse ON
            let upper_after = after_table.to_uppercase();
            if !upper_after.starts_with("ON") {
                return Err(CoreError::InvalidArgument(
                    "expected 'ON' after JOIN table".to_string(),
                ));
            }
            let on_input = after_table[2..].trim();

            // Parse <left> = <right>
            let eq_pos = Self::find_unquoted(on_input, "=")
                .ok_or_else(|| CoreError::InvalidArgument("expected '=' in ON clause".to_string()))?;
            let left = on_input[..eq_pos].trim().to_string();
            let right_on = on_input[eq_pos + 1..].trim();

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
            let upper = part.to_uppercase();

            // Check for CASE WHEN expression
            if upper.starts_with("CASE ") || upper.starts_with("CASE\n") {
                let case_expr = Self::parse_case_when(part)?;
                // Check for alias after END
                let end_upper = part.to_uppercase();
                let end_pos = end_upper.find(" END").unwrap_or(part.len());
                let after_end = part[end_pos + 4..].trim();
                let expr = if after_end.to_uppercase().starts_with("AS ") {
                    let _alias = after_end[3..].trim().to_string();
                    // Store with alias info — we'll wrap in a named expression
                    SelectItem::Expression(case_expr)
                } else {
                    SelectItem::Expression(case_expr)
                };
                items.push(expr);
                continue;
            }

            // Check for window functions: ROW_NUMBER(), RANK(), etc. with OVER
            if Self::contains_window_function(&upper) {
                let window_expr = Self::parse_window_function(part)?;
                items.push(SelectItem::WindowFunction(window_expr));
                continue;
            }

            // Check for built-in functions: COALESCE, NULLIF, CONCAT, etc.
            let builtin_funcs = ["COALESCE", "NULLIF", "CONCAT", "SUBSTRING", "UPPER", "LOWER", "NOW", "LENGTH", "TRIM", "ABS", "ROUND"];
            let mut is_builtin = false;
            for func_name in &builtin_funcs {
                if upper.starts_with(func_name) && part.len() > func_name.len() && part.as_bytes()[func_name.len()] == b'(' {
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
            let func = if upper.starts_with("COUNT(") {
                Some(AggregateFunc::Count)
            } else if upper.starts_with("SUM(") {
                Some(AggregateFunc::Sum)
            } else if upper.starts_with("AVG(") {
                Some(AggregateFunc::Avg)
            } else if upper.starts_with("MIN(") {
                Some(AggregateFunc::Min)
            } else if upper.starts_with("MAX(") {
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
                let arg = part[open + 1..close].trim().to_string();

                // Check for alias: ... AS alias
                let after = part[close + 1..].trim();
                let alias = if after.to_uppercase().starts_with("AS") {
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
                    Self::find_unquoted(&upper, " AS ")
                {
                    (part[..as_pos].trim(), Some(part[as_pos + 4..].trim()))
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
    fn contains_window_function(upper: &str) -> bool {
        // Window functions: ROW_NUMBER, RANK, DENSE_RANK, LAG, LEAD, etc.
        // Must have OVER keyword after the function call
        let window_funcs = ["ROW_NUMBER", "RANK", "DENSE_RANK", "LAG", "LEAD", 
                           "FIRST_VALUE", "LAST_VALUE", "NTH_VALUE"];
        for func in &window_funcs {
            if upper.contains(func) && upper.contains(" OVER ") {
                return true;
            }
        }
        // Also check for aggregate OVER (e.g., SUM(x) OVER (...))
        if upper.contains(") OVER ") || upper.contains(")OVER ") {
            return true;
        }
        false
    }

    /// Parse a window function expression.
    fn parse_window_function(input: &str) -> Result<WindowExpr> {
        let upper = input.to_uppercase();
        
        // Find OVER keyword
        let over_pos = Self::find_unquoted(&upper, " OVER ")
            .ok_or_else(|| CoreError::InvalidArgument("expected 'OVER' in window function".to_string()))?;
        
        let func_part = input[..over_pos].trim();
        let over_part = input[over_pos + 6..].trim();

        // Parse function name and argument
        let (func, arg) = Self::parse_window_func_name(func_part)?;

        // Parse OVER clause
        let over = Self::parse_window_spec(over_part)?;

        // Check for alias after OVER clause
        let after_over = &input[over_pos + 6..];
        let close_paren = Self::find_matching_paren(after_over)?;
        let after = after_over[close_paren + 1..].trim();
        let alias = if after.to_uppercase().starts_with("AS") {
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
        let upper = input.to_uppercase();
        
        // Find the opening parenthesis
        let open = input.find('(')
            .ok_or_else(|| CoreError::InvalidArgument("expected '(' in window function".to_string()))?;
        let close = input.rfind(')')
            .ok_or_else(|| CoreError::InvalidArgument("expected ')' in window function".to_string()))?;
        
        let func_name = &upper[..open];
        let arg_str = input[open + 1..close].trim();
        let arg = if arg_str.is_empty() || arg_str == "*" {
            None
        } else {
            Some(arg_str.to_string())
        };

        let func = match func_name {
            "ROW_NUMBER" => WindowFunc::RowNumber,
            "RANK" => WindowFunc::Rank,
            "DENSE_RANK" => WindowFunc::DenseRank,
            "LAG" => WindowFunc::Lag,
            "LEAD" => WindowFunc::Lead,
            "FIRST_VALUE" => WindowFunc::FirstValue,
            "LAST_VALUE" => WindowFunc::LastValue,
            "NTH_VALUE" => WindowFunc::NthValue,
            "SUM" => WindowFunc::Sum,
            "AVG" => WindowFunc::Avg,
            "MIN" => WindowFunc::Min,
            "MAX" => WindowFunc::Max,
            "COUNT" => WindowFunc::Count,
            _ => return Err(CoreError::InvalidArgument(format!("unknown window function: {}", func_name))),
        };

        Ok((func, arg))
    }

    /// Parse window specification (OVER clause).
    fn parse_window_spec(input: &str) -> Result<WindowSpec> {
        let input = input.trim();
        
        // Remove outer parentheses
        let input = if input.starts_with('(') && input.ends_with(')') {
            &input[1..input.len() - 1].trim()
        } else {
            input
        };

        let mut partition_by = Vec::new();
        let mut order_by = Vec::new();
        let mut frame = None;

        let upper = input.to_uppercase();

        // Parse PARTITION BY
        if let Some(pb_pos) = Self::find_unquoted(&upper, "PARTITION BY") {
            let pb_str = &input[pb_pos + 12..].trim();
            let (pb_cols, remaining) = Self::consume_until_keywords(pb_str, &["ORDER BY", "ROWS", "RANGE"]);
            partition_by = pb_cols.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
            
            // Parse ORDER BY from remaining
            let remaining_upper = remaining.to_uppercase();
            if remaining_upper.starts_with("ORDER BY") {
                let ob_str = &remaining[8..].trim();
                order_by = Self::parse_window_order_by(ob_str)?;
            }
        } else if upper.starts_with("ORDER BY") {
            let ob_str = &input[8..].trim();
            order_by = Self::parse_window_order_by(ob_str)?;
        }

        // Parse frame specification
        if let Some(rows_pos) = Self::find_unquoted(&upper, "ROWS") {
            frame = Some(Self::parse_window_frame(&input[rows_pos..], WindowFrameType::Rows)?);
        } else if let Some(range_pos) = Self::find_unquoted(&upper, "RANGE") {
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
            let upper = part.to_uppercase();
            let (col, ascending) = if upper.ends_with(" DESC") {
                (part[..part.len() - 5].trim().to_string(), false)
            } else if upper.ends_with(" ASC") {
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
        let upper = input.to_uppercase();
        
        // Parse: ROWS BETWEEN <start> AND <end>
        let between_pos = Self::find_unquoted(&upper, "BETWEEN")
            .ok_or_else(|| CoreError::InvalidArgument("expected 'BETWEEN' in window frame".to_string()))?;
        let frame_str = &input[between_pos + 7..].trim();

        // Find AND separator
        let and_pos = Self::find_unquoted(&frame_str.to_uppercase(), " AND ")
            .ok_or_else(|| CoreError::InvalidArgument("expected 'AND' in window frame".to_string()))?;

        let start_str = frame_str[..and_pos].trim();
        let end_str = frame_str[and_pos + 5..].trim();

        let start = Self::parse_frame_bound(start_str)?;
        let end = Some(Self::parse_frame_bound(end_str)?);

        Ok(WindowFrame { frame_type, start, end })
    }

    /// Parse a window frame bound.
    fn parse_frame_bound(input: &str) -> Result<WindowFrameBound> {
        let upper = input.trim().to_uppercase();
        match upper.as_str() {
            "UNBOUNDED PRECEDING" => Ok(WindowFrameBound::UnboundedPreceding),
            "UNBOUNDED FOLLOWING" => Ok(WindowFrameBound::UnboundedFollowing),
            "CURRENT ROW" => Ok(WindowFrameBound::CurrentRow),
            _ => {
                if upper.ends_with("PRECEDING") {
                    let n_str = upper[..upper.len() - 9].trim();
                    let n = n_str.parse::<u64>()
                        .map_err(|_| CoreError::InvalidArgument("invalid frame bound".to_string()))?;
                    Ok(WindowFrameBound::Preceding(n))
                } else if upper.ends_with("FOLLOWING") {
                    let n_str = upper[..upper.len() - 9].trim();
                    let n = n_str.parse::<u64>()
                        .map_err(|_| CoreError::InvalidArgument("invalid frame bound".to_string()))?;
                    Ok(WindowFrameBound::Following(n))
                } else {
                    Err(CoreError::InvalidArgument(format!("invalid frame bound: {}", input)))
                }
            }
        }
    }

    /// Find matching closing parenthesis.
    fn find_matching_paren(input: &str) -> Result<usize> {
        let mut depth = 0;
        for (i, c) in input.char_indices() {
            if c == '(' { depth += 1; }
            if c == ')' {
                depth -= 1;
                if depth == 0 {
                    return Ok(i);
                }
            }
        }
        Err(CoreError::InvalidArgument("unmatched parenthesis".to_string()))
    }

    /// Consumes input until a keyword is found. Returns (consumed, remaining).
    fn consume_until_keywords<'a>(input: &'a str, keywords: &[&str]) -> (String, &'a str) {
        let upper = input.to_uppercase();
        let mut earliest = input.len();

        for kw in keywords {
            if let Some(pos) = Self::find_unquoted(&upper, kw) {
                if pos < earliest {
                    earliest = pos;
                }
            }
        }

        let consumed = input[..earliest].trim().to_string();
        let remaining = &input[earliest..];
        (consumed, remaining)
    }

    /// Parses: CREATE INDEX ON <class> (<column>) or CREATE INDEX ON <class> (<col1>, <col2>)
    fn parse_create_index(input: &str) -> Result<QueryAst> {
        let upper = input.to_uppercase();
        let on_pos = upper
            .find(" ON ")
            .ok_or_else(|| CoreError::InvalidArgument("expected 'ON' after CREATE INDEX".to_string()))?;
        let rest = input[on_pos + 4..].trim();

        // Find the parenthesized column(s)
        let paren_open = rest
            .find('(')
            .ok_or_else(|| CoreError::InvalidArgument("expected '(column)' in CREATE INDEX".to_string()))?;
        let class = rest[..paren_open].trim().to_string();
        let paren_close = rest
            .find(')')
            .ok_or_else(|| CoreError::InvalidArgument("expected ')' in CREATE INDEX".to_string()))?;
        let columns_str = rest[paren_open + 1..paren_close].trim().to_string();

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
            Ok(QueryAst::CreateIndex { class, column: columns.into_iter().next().unwrap() })
        }
    }

    /// Parses: DROP INDEX ON <class> (<column>)
    fn parse_drop_index(input: &str) -> Result<QueryAst> {
        let upper = input.to_uppercase();
        let on_pos = upper
            .find(" ON ")
            .ok_or_else(|| CoreError::InvalidArgument("expected 'ON' after DROP INDEX".to_string()))?;
        let rest = input[on_pos + 4..].trim();

        let paren_open = rest
            .find('(')
            .ok_or_else(|| CoreError::InvalidArgument("expected '(column)' in DROP INDEX".to_string()))?;
        let class = rest[..paren_open].trim().to_string();
        let paren_close = rest
            .find(')')
            .ok_or_else(|| CoreError::InvalidArgument("expected ')' in DROP INDEX".to_string()))?;
        let column = rest[paren_open + 1..paren_close].trim().to_string();

        if class.is_empty() || column.is_empty() {
            return Err(CoreError::InvalidArgument(
                "class and column cannot be empty in DROP INDEX".to_string(),
            ));
        }

        Ok(QueryAst::DropIndex { class, column })
    }

    /// Parses: CREATE VECTOR INDEX ON <class> (<column>) METRIC <metric> DIMENSION <dim> [M <m>] [EF_CONSTRUCTION <ef>] [EF_SEARCH <ef>]
    fn parse_create_vector_index(input: &str) -> Result<QueryAst> {
        let upper = input.to_uppercase();
        let on_pos = upper
            .find(" ON ")
            .ok_or_else(|| CoreError::InvalidArgument("expected 'ON' after CREATE VECTOR INDEX".to_string()))?;
        let rest = input[on_pos + 4..].trim();

        // Find the parenthesized column
        let paren_open = rest
            .find('(')
            .ok_or_else(|| CoreError::InvalidArgument("expected '(column)' in CREATE VECTOR INDEX".to_string()))?;
        let class = rest[..paren_open].trim().to_string();
        let paren_close = rest
            .find(')')
            .ok_or_else(|| CoreError::InvalidArgument("expected ')' in CREATE VECTOR INDEX".to_string()))?;
        let column = rest[paren_open + 1..paren_close].trim().to_string();

        if class.is_empty() || column.is_empty() {
            return Err(CoreError::InvalidArgument(
                "class and column cannot be empty in CREATE VECTOR INDEX".to_string(),
            ));
        }

        let after_paren = rest[paren_close + 1..].trim().to_uppercase();

        // Parse METRIC
        let metric = if let Some(pos) = after_paren.find("METRIC") {
            let metric_str = after_paren[pos + 6..].trim();
            let end = metric_str.find(|c: char| c.is_whitespace()).unwrap_or(metric_str.len());
            metric_str[..end].to_lowercase()
        } else {
            "cosine".to_string()
        };

        // Parse DIMENSION
        let dimension = if let Some(pos) = after_paren.find("DIMENSION") {
            let dim_str = after_paren[pos + 9..].trim();
            let end = dim_str.find(|c: char| !c.is_ascii_digit()).unwrap_or(dim_str.len());
            dim_str[..end].parse::<usize>()
                .map_err(|_| CoreError::InvalidArgument("invalid DIMENSION value".to_string()))?
        } else {
            return Err(CoreError::InvalidArgument("DIMENSION is required in CREATE VECTOR INDEX".to_string()));
        };

        // Parse optional M
        let m = if let Some(pos) = after_paren.find(" M ") {
            let m_str = after_paren[pos + 3..].trim();
            let end = m_str.find(|c: char| !c.is_ascii_digit()).unwrap_or(m_str.len());
            m_str[..end].parse::<usize>().unwrap_or(16)
        } else {
            16
        };

        // Parse optional EF_CONSTRUCTION
        let ef_construction = if let Some(pos) = after_paren.find("EF_CONSTRUCTION") {
            let ef_str = after_paren[pos + 15..].trim();
            let end = ef_str.find(|c: char| !c.is_ascii_digit()).unwrap_or(ef_str.len());
            ef_str[..end].parse::<usize>().unwrap_or(200)
        } else {
            200
        };

        // Parse optional EF_SEARCH
        let ef_search = if let Some(pos) = after_paren.find("EF_SEARCH") {
            let ef_str = after_paren[pos + 9..].trim();
            let end = ef_str.find(|c: char| !c.is_ascii_digit()).unwrap_or(ef_str.len());
            ef_str[..end].parse::<usize>().unwrap_or(100)
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
        let upper = input.to_uppercase();
        let on_pos = upper
            .find(" ON ")
            .ok_or_else(|| CoreError::InvalidArgument("expected 'ON' after DROP VECTOR INDEX".to_string()))?;
        let rest = input[on_pos + 4..].trim();

        let paren_open = rest
            .find('(')
            .ok_or_else(|| CoreError::InvalidArgument("expected '(column)' in DROP VECTOR INDEX".to_string()))?;
        let class = rest[..paren_open].trim().to_string();
        let paren_close = rest
            .find(')')
            .ok_or_else(|| CoreError::InvalidArgument("expected ')' in DROP VECTOR INDEX".to_string()))?;
        let column = rest[paren_open + 1..paren_close].trim().to_string();

        if class.is_empty() || column.is_empty() {
            return Err(CoreError::InvalidArgument(
                "class and column cannot be empty in DROP VECTOR INDEX".to_string(),
            ));
        }

        Ok(QueryAst::DropVectorIndex { class, column })
    }

    /// Parses: VECTOR SEARCH ON <class> (<column>) QUERY [v1, v2, ...] TOP <k> [WHERE ...]
    fn parse_vector_search(input: &str) -> Result<QueryAst> {
        let upper = input.to_uppercase();
        let on_pos = upper
            .find(" ON ")
            .ok_or_else(|| CoreError::InvalidArgument("expected 'ON' after VECTOR SEARCH".to_string()))?;
        let rest = input[on_pos + 4..].trim();

        // Find the parenthesized column
        let paren_open = rest
            .find('(')
            .ok_or_else(|| CoreError::InvalidArgument("expected '(column)' in VECTOR SEARCH".to_string()))?;
        let class = rest[..paren_open].trim().to_string();
        let paren_close = rest
            .find(')')
            .ok_or_else(|| CoreError::InvalidArgument("expected ')' in VECTOR SEARCH".to_string()))?;
        let column = rest[paren_open + 1..paren_close].trim().to_string();

        if class.is_empty() || column.is_empty() {
            return Err(CoreError::InvalidArgument(
                "class and column cannot be empty in VECTOR SEARCH".to_string(),
            ));
        }

        let after_paren = rest[paren_close + 1..].trim();

        // Parse QUERY keyword
        let upper_after = after_paren.to_uppercase();
        let query_pos = upper_after
            .find("QUERY")
            .ok_or_else(|| CoreError::InvalidArgument("expected 'QUERY' in VECTOR SEARCH".to_string()))?;
        let after_query = after_paren[query_pos + 5..].trim();

        // Parse the vector: [v1, v2, ...]
        let bracket_open = after_query
            .find('[')
            .ok_or_else(|| CoreError::InvalidArgument("expected '[' for query vector".to_string()))?;
        let bracket_close = after_query
            .find(']')
            .ok_or_else(|| CoreError::InvalidArgument("expected ']' for query vector".to_string()))?;
        let vec_str = &after_query[bracket_open + 1..bracket_close];
        let query_vector: Vec<f32> = vec_str
            .split(',')
            .map(|s| s.trim().parse::<f32>())
            .collect::<std::result::Result<Vec<f32>, _>>()
            .map_err(|_| CoreError::InvalidArgument("invalid vector element".to_string()))?;

        let after_vec = after_query[bracket_close + 1..].trim();
        let upper_after_vec = after_vec.to_uppercase();

        // Parse TOP <k>
        let top_pos = upper_after_vec
            .find("TOP")
            .ok_or_else(|| CoreError::InvalidArgument("expected 'TOP' in VECTOR SEARCH".to_string()))?;
        let after_top = after_vec[top_pos + 3..].trim();
        let (top_str, remaining) = Self::parse_word(after_top)?;
        let top_k = top_str
            .parse::<usize>()
            .map_err(|_| CoreError::InvalidArgument("invalid TOP value".to_string()))?;

        // Parse optional WHERE
        let filter = if remaining.to_uppercase().starts_with("WHERE") {
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
        let upper = input.to_uppercase();
        let set_pos = upper
            .find(" SET ")
            .ok_or_else(|| CoreError::InvalidArgument("expected 'SET'".to_string()))?;

        let class = input[6..set_pos].trim().to_string();
        let rest = &input[set_pos + 5..];

        // Find WHERE position, respecting quotes
        let (set_part, filter_str) = if let Some(wp) = Self::find_unquoted(rest, " WHERE ") {
            (&rest[..wp], Some(rest[wp + 7..].trim()))
        } else {
            (rest, None)
        };

        // Split assignments by comma, respecting quotes
        let assignments: Vec<(String, LiteralValue)> = Self::split_quoted(set_part, ',')
            .iter()
            .map(|s| {
                let s = s.trim();
                let eq_pos = Self::find_unquoted(s, "=")
                    .ok_or_else(|| CoreError::InvalidArgument("expected '=' in SET".to_string()))?;
                let col = s[..eq_pos].trim().to_string();
                let val = Self::parse_literal(s[eq_pos + 1..].trim())?;
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
        let upper = input.to_uppercase();
        let from_pos = upper
            .find("FROM")
            .ok_or_else(|| CoreError::InvalidArgument("expected 'FROM'".to_string()))?;

        let rest = input[from_pos + 4..].trim();
        let (class, rest) = Self::parse_word(rest)?;

        let filter = if rest.to_uppercase().starts_with("WHERE") {
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

        let pattern = rest[open + 1..close].trim();

        // Parse pattern: <var>: <Class>
        let colon_pos = pattern
            .find(':')
            .ok_or_else(|| CoreError::InvalidArgument("expected ':' in MATCH pattern".to_string()))?;

        let variable = pattern[..colon_pos].trim().to_string();
        let class = pattern[colon_pos + 1..].trim().to_string();

        let after = rest[close + 1..].trim();

        // Parse optional WHERE
        let (filter, after) = if after.to_uppercase().starts_with("WHERE") {
            Self::parse_where(after[5..].trim())?
        } else {
            (None, after.to_string())
        };

        // Parse RETURN
        let after_upper = after.to_uppercase();
        if !after_upper.starts_with("RETURN") {
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

    fn parse_where(input: &str) -> Result<(Option<FilterExpr>, String)> {
        let input = input.trim();

        // Try keyword operators first: EXISTS, LIKE, BETWEEN, IN, IS NULL, NOT
        let upper = input.to_uppercase();

        // Check for NOT (expr)
        if upper.starts_with("NOT ") || upper.starts_with("NOT(") {
            let not_len = if upper.starts_with("NOT(") { 4 } else { 4 };
            let rest = input[not_len..].trim();
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
                    let inner = rest[1..close].trim();
                    let remaining = rest[close + 1..].trim();
                    let (inner_expr, _) = Self::parse_where(inner)?;
                    if let Some(expr) = inner_expr {
                        let expr = FilterExpr::Not(Box::new(expr));
                        return Self::wrap_chain(expr, remaining);
                    }
                }
            }
        }

        // Check for EXISTS (SELECT ...)
        if upper.starts_with("EXISTS") || upper.starts_with("NOT EXISTS") {
            let (exists_start, is_not) = if upper.starts_with("NOT EXISTS") {
                (10, true)
            } else {
                (6, false)
            };
            let rest = input[exists_start..].trim();
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
                    let inner = rest[1..close].trim();
                    let remaining = rest[close + 1..].trim();
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
        if let Some(is_pos) = Self::find_unquoted(&upper, " IS ") {
            let col = input[..is_pos].trim().to_string();
            let rest = input[is_pos + 4..].trim();
            let rest_upper = rest.to_uppercase();
            if rest_upper.starts_with("NOT NULL") {
                let remaining = rest[8..].trim();
                let expr = FilterExpr::IsNotNull(col);
                return Self::wrap_chain(expr, remaining);
            } else if rest_upper.starts_with("NULL") {
                let remaining = rest[4..].trim();
                let expr = FilterExpr::IsNull(col);
                return Self::wrap_chain(expr, remaining);
            }
        }

        // Check for LIKE: column LIKE 'pattern'
        if let Some(like_pos) = Self::find_unquoted(&upper, " LIKE ") {
            let col = input[..like_pos].trim().to_string();
            let rest = input[like_pos + 6..].trim();
            let (pattern, remaining) = Self::extract_quoted_or_word(rest);
            let expr = FilterExpr::Like(col, pattern);
            return Self::wrap_chain(expr, &remaining);
        }

        // Check for BETWEEN: column BETWEEN low AND high
        if let Some(between_pos) = Self::find_unquoted(&upper, " BETWEEN ") {
            let col = input[..between_pos].trim().to_string();
            let rest = input[between_pos + 9..].trim();
            let rest_upper = rest.to_uppercase();

            if let Some(and_pos) = Self::find_unquoted(&rest_upper, " AND ") {
                let low_str = rest[..and_pos].trim();
                let high_rest = rest[and_pos + 5..].trim();
                let (high_str, remaining) = Self::extract_quoted_or_word(high_rest);
                let low = Self::parse_literal(low_str)?;
                let high = Self::parse_literal(&high_str)?;
                let expr = FilterExpr::Between(col, low, high);
                return Self::wrap_chain(expr, &remaining);
            }
        }

        // Check for IN: column IN (val1, val2, ...) or column IN (SELECT ...)
        if let Some(in_pos) = Self::find_unquoted(&upper, " IN ") {
            let col = input[..in_pos].trim().to_string();
            let rest = input[in_pos + 4..].trim();
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
                    let inner = rest[1..close].trim();
                    let remaining = rest[close + 1..].trim();

                    // Check if it's a subquery
                    if inner.to_uppercase().starts_with("SELECT") {
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

        // Symbol operators: >=, <=, !=, <>, >, <, =
        for (op_str, op_fn) in &[
            (">=", FilterExpr::Gte as fn(String, LiteralValue) -> FilterExpr),
            ("<=", FilterExpr::Lte),
            ("!=", FilterExpr::Ne),
            ("<>", FilterExpr::Ne),
            (">", FilterExpr::Gt),
            ("<", FilterExpr::Lt),
            ("=", FilterExpr::Eq),
        ] {
            if let Some(pos) = Self::find_unquoted(input, op_str) {
                let col = input[..pos].trim().to_string();
                let rest = input[pos + op_str.len()..].trim();

                let (val_str, remaining) = Self::extract_quoted_or_word(rest);
                let val = Self::parse_literal(&val_str)?;
                let expr = op_fn(col, val);
                return Self::wrap_chain(expr, &remaining);
            }
        }

        Err(CoreError::InvalidArgument(format!(
            "invalid WHERE clause: {}",
            input
        )))
    }

    /// Wraps an expression with AND/OR if the remaining text starts with AND/OR.
    fn wrap_chain(expr: FilterExpr, remaining: &str) -> Result<(Option<FilterExpr>, String)> {
        let remaining = remaining.trim();
        let rem_upper = remaining.to_uppercase();
        if rem_upper.starts_with("AND ") {
            let rest_after = &remaining[4..];
            let (right, final_rest) = Self::parse_where(rest_after)?;
            if let Some(right_expr) = right {
                return Ok((Some(FilterExpr::And(Box::new(expr), Box::new(right_expr))), final_rest));
            }
            return Ok((Some(expr), final_rest));
        } else if rem_upper.starts_with("OR ") {
            let rest_after = &remaining[3..];
            let (right, final_rest) = Self::parse_where(rest_after)?;
            if let Some(right_expr) = right {
                return Ok((Some(FilterExpr::Or(Box::new(expr), Box::new(right_expr))), final_rest));
            }
            return Ok((Some(expr), final_rest));
        }
        Ok((Some(expr), remaining.to_string()))
    }

    /// Extracts a value from the start of input. Handles quoted strings and bare words.
    /// Returns (value_string, remaining_input).
    fn extract_quoted_or_word(input: &str) -> (String, String) {
        let input = input.trim();
        if input.starts_with('\'') || input.starts_with('"') {
            let quote = input.as_bytes()[0] as char;
            if let Some(end) = input[1..].find(quote) {
                return (
                    input[1..1 + end].to_string(),
                    input[1 + end + 1..].trim().to_string(),
                );
            }
        }
        let end = input
            .find(|c: char| c.is_whitespace() || c == ';' || c == ',')
            .unwrap_or(input.len());
        (input[..end].to_string(), input[end..].trim().to_string())
    }

    /// Parse CREATE MATERIALIZED VIEW <name> AS <query>
    fn parse_create_materialized_view(input: &str) -> Result<QueryAst> {
        let upper = input.to_uppercase();
        if !upper.starts_with("CREATE MATERIALIZED VIEW") {
            return Err(CoreError::InvalidArgument("expected CREATE MATERIALIZED VIEW".to_string()));
        }

        let rest = input[24..].trim();
        let as_pos = rest.to_uppercase().find(" AS ")
            .ok_or_else(|| CoreError::InvalidArgument("expected 'AS' in CREATE MATERIALIZED VIEW".to_string()))?;

        let name = rest[..as_pos].trim().to_string();
        let query_str = rest[as_pos + 4..].trim();
        let query = Self::parse(query_str)?;

        Ok(QueryAst::CreateMaterializedView {
            name,
            query: Box::new(query),
        })
    }

    /// Parse DROP MATERIALIZED VIEW <name>
    fn parse_drop_materialized_view(input: &str) -> Result<QueryAst> {
        let upper = input.to_uppercase();
        if !upper.starts_with("DROP MATERIALIZED VIEW") {
            return Err(CoreError::InvalidArgument("expected DROP MATERIALIZED VIEW".to_string()));
        }

        let name = input[22..].trim().to_string();
        Ok(QueryAst::DropMaterializedView { name })
    }

    /// Parse REFRESH MATERIALIZED VIEW <name>
    fn parse_refresh_materialized_view(input: &str) -> Result<QueryAst> {
        let upper = input.to_uppercase();
        if !upper.starts_with("REFRESH MATERIALIZED VIEW") {
            return Err(CoreError::InvalidArgument("expected REFRESH MATERIALIZED VIEW".to_string()));
        }

        let name = input[25..].trim().to_string();
        Ok(QueryAst::RefreshMaterializedView { name })
    }

    /// Parses a CASE WHEN ... THEN ... ELSE ... END expression.
    fn parse_case_when(input: &str) -> Result<ValueExpr> {
        let upper = input.to_uppercase();
        if !upper.starts_with("CASE ") && !upper.starts_with("CASE\n") {
            return Err(CoreError::InvalidArgument("expected CASE".to_string()));
        }

        let mut remaining = input[5..].trim();
        let mut when_branches = Vec::new();
        let mut else_expr: Option<Box<ValueExpr>> = None;

        loop {
            let rem_upper = remaining.to_uppercase().trim().to_string();

            if rem_upper.starts_with("WHEN ") || rem_upper.starts_with("WHEN\n") {
                // Find THEN keyword
                let then_pos = Self::find_unquoted(&rem_upper, " THEN ")
                    .ok_or_else(|| CoreError::InvalidArgument("expected THEN after WHEN".to_string()))?;
                let cond_str = remaining[5..then_pos].trim();
                remaining = remaining[then_pos + 6..].trim();

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
            } else if rem_upper.starts_with("ELSE ") || rem_upper.starts_with("ELSE\n") {
                remaining = remaining[5..].trim();
                let (value_str, rest) = Self::consume_until_keywords(remaining, &["END"]);
                else_expr = Some(Box::new(Self::parse_value_expr(&value_str)?));
                remaining = rest;
            } else if rem_upper.starts_with("END") {
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
        let upper = input.to_uppercase();

        // CASE WHEN
        if upper.starts_with("CASE ") {
            return Self::parse_case_when(input);
        }

        // Scalar subquery: (SELECT ...)
        if input.starts_with('(') && upper[1..].trim_start().starts_with("SELECT") {
            let subquery = Self::parse_subquery(input)?;
            return Ok(ValueExpr::ScalarSubquery(Box::new(subquery)));
        }

        // Built-in function: FUNC(args...)
        let func_names = ["COALESCE", "NULLIF", "CONCAT", "SUBSTRING", "UPPER", "LOWER", "NOW", "LENGTH", "TRIM", "ABS", "ROUND"];
        for func_name in &func_names {
            if upper.starts_with(func_name) && input.len() > func_name.len() && input.as_bytes()[func_name.len()] == b'(' {
                // Find matching closing paren
                let open = func_name.len();
                let close = Self::find_matching_paren_simple(&input[open..])? + open;
                let args_str = &input[open + 1..close];
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
            let left = Self::parse_value_expr(&input[..pos])?;
            let right = Self::parse_value_expr(&input[pos + 1..])?;
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
                '/' if depth == 0 => {
                    if last_op.is_none() { last_op = Some((ArithmeticOp::Div, i)); }
                }
                _ => {}
            }
        }
        last_op
    }

    /// Parses ANALYZE <table>
    fn parse_analyze(input: &str) -> Result<QueryAst> {
        let upper = input.to_uppercase();
        if !upper.starts_with("ANALYZE") {
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

        if s.is_empty() || s.eq_ignore_ascii_case("NULL") {
            return Ok(LiteralValue::Null);
        }
        if s.eq_ignore_ascii_case("TRUE") {
            return Ok(LiteralValue::Bool(true));
        }
        if s.eq_ignore_ascii_case("FALSE") {
            return Ok(LiteralValue::Bool(false));
        }

        // String literal
        if (s.starts_with('\'') && s.ends_with('\''))
            || (s.starts_with('"') && s.ends_with('"'))
        {
            return Ok(LiteralValue::String(s[1..s.len() - 1].to_string()));
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
        let end = input
            .find(|c: char| c.is_whitespace() || c == ';' || c == ',')
            .unwrap_or(input.len());
        Ok((input[..end].to_string(), input[end..].trim_start().to_string()))
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
}
