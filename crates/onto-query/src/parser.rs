//! SQL parser with semantic extensions for OntoDB.
//!
//! Supports standard SQL (SELECT, INSERT, UPDATE, DELETE)
//! plus OntoDB extensions:
//! - CREATE ONTOLOGY
//! - MATCH (p: ClassName) - semantic class-based queries

use onto_core::{CoreError, Result};
use serde::{Deserialize, Serialize};

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

    /// SELECT [DISTINCT] ... FROM <class> [JOIN ...] [WHERE ...] [GROUP BY ...] [HAVING ...] [ORDER BY ...] [LIMIT ...]
    Select {
        distinct: bool,
        columns: SelectColumns,
        from: String,
        from_alias: Option<String>,
        joins: Vec<JoinClause>,
        filter: Option<FilterExpr>,
        group_by: Option<GroupByClause>,
        having: Option<FilterExpr>,
        order_by: Option<OrderBy>,
        limit: Option<usize>,
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

    /// DROP INDEX <name> ON <class> (<column>)
    DropIndex {
        class: String,
        column: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SelectColumns {
    All,
    Columns(Vec<SelectItem>),
}

/// A single item in SELECT: either a column reference or an aggregate function.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SelectItem {
    /// A regular column: `name` or `p.name`
    Column(String),
    /// An aggregate: `COUNT(*)`, `SUM(price) as total`
    Aggregate(AggregateExpr),
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
    And(Box<FilterExpr>, Box<FilterExpr>),
    Or(Box<FilterExpr>, Box<FilterExpr>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderBy {
    pub column: String,
    pub ascending: bool,
}

/// A JOIN clause: JOIN <table> [AS <alias>] ON <left_col> = <right_col>
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinClause {
    pub table: String,
    pub alias: Option<String>,
    pub on: JoinOn,
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

        if upper.starts_with("CREATE ONTOLOGY") {
            Ok(QueryAst::CreateOntology {
                sql: input.to_string(),
            })
        } else if upper.starts_with("CREATE INDEX") {
            Self::parse_create_index(input)
        } else if upper.starts_with("DROP INDEX") {
            Self::parse_drop_index(input)
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

    fn parse_insert(input: &str) -> Result<QueryAst> {
        // INSERT INTO <class> (<cols>) VALUES (<vals>)
        let upper = input.to_uppercase();

        let into_pos = upper
            .find("INTO")
            .ok_or_else(|| CoreError::InvalidArgument("expected 'INTO'".to_string()))?;
        let values_pos = upper
            .find("VALUES")
            .ok_or_else(|| CoreError::InvalidArgument("expected 'VALUES'".to_string()))?;

        let header = input[into_pos + 4..values_pos].trim();
        let values_str = input[values_pos + 6..].trim();

        // Parse header: class (col1, col2, ...)
        let paren_start = header
            .find('(')
            .ok_or_else(|| CoreError::InvalidArgument("expected '(' in INSERT".to_string()))?;
        let class = header[..paren_start].trim().to_string();
        let cols_str = &header[paren_start + 1..header.len() - 1]; // Remove parens

        let columns: Vec<String> = cols_str
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        // Parse values: (val1, val2, ...)
        let values_str = values_str.trim();
        if !values_str.starts_with('(') || !values_str.ends_with(')') {
            return Err(CoreError::InvalidArgument(
                "expected '(values)'".to_string(),
            ));
        }
        let vals_inner = &values_str[1..values_str.len() - 1];

        let values: Vec<LiteralValue> = vals_inner
            .split(',')
            .map(|s| Self::parse_literal(s.trim()))
            .collect::<Result<Vec<_>>>()?;

        Ok(QueryAst::Insert {
            class,
            columns,
            values,
        })
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

        // Parse optional ORDER BY
        let rest_upper = rest.to_uppercase();
        let rest = rest; // reborrow
        let (order_by, rest) = if rest_upper.trim_start().starts_with("ORDER BY") {
            let start = Self::find_unquoted(&rest_upper, "ORDER BY")
                .ok_or_else(|| CoreError::InvalidArgument("expected 'ORDER BY'".to_string()))?;
            let rest = rest[start + 8..].trim();
            let (col, rest) = Self::parse_word(rest)?;
            let rest_upper = rest.to_uppercase();
            let ascending = if rest_upper.trim_start().starts_with("DESC") {
                let pos = Self::find_unquoted(&rest_upper, "DESC")
                    .ok_or_else(|| CoreError::InvalidArgument("expected 'DESC'".to_string()))?;
                let rest = rest[pos + 4..].trim();
                (false, rest.to_string())
            } else if rest_upper.trim_start().starts_with("ASC") {
                let pos = Self::find_unquoted(&rest_upper, "ASC")
                    .ok_or_else(|| CoreError::InvalidArgument("expected 'ASC'".to_string()))?;
                let rest = rest[pos + 3..].trim();
                (true, rest.to_string())
            } else {
                (true, rest.to_string())
            };
            (
                Some(OrderBy {
                    column: col,
                    ascending: ascending.0,
                }),
                ascending.1,
            )
        } else {
            (None, rest)
        };

        // Parse optional LIMIT
        let rest_upper = rest.to_uppercase();
        let limit = if rest_upper.trim_start().starts_with("LIMIT") {
            let start = Self::find_unquoted(&rest_upper, "LIMIT")
                .ok_or_else(|| CoreError::InvalidArgument("expected 'LIMIT'".to_string()))?;
            let num_str = rest[start + 5..].trim();
            Some(
                num_str
                    .parse::<usize>()
                    .map_err(|_| CoreError::InvalidArgument("invalid LIMIT".to_string()))?,
            )
        } else {
            None
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
            if !upper.starts_with("JOIN") {
                break;
            }

            // Skip "JOIN"
            let after_join = rest[4..].trim();

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
                &["WHERE", "JOIN", "ORDER BY", "LIMIT"],
            );

            joins.push(JoinClause {
                table,
                alias,
                on: JoinOn { left, right },
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

    /// Parses: CREATE INDEX ON <class> (<column>)
    fn parse_create_index(input: &str) -> Result<QueryAst> {
        // CREATE INDEX ON Product (price)
        let upper = input.to_uppercase();
        let on_pos = upper
            .find(" ON ")
            .ok_or_else(|| CoreError::InvalidArgument("expected 'ON' after CREATE INDEX".to_string()))?;
        let rest = input[on_pos + 4..].trim();

        // Find the parenthesized column
        let paren_open = rest
            .find('(')
            .ok_or_else(|| CoreError::InvalidArgument("expected '(column)' in CREATE INDEX".to_string()))?;
        let class = rest[..paren_open].trim().to_string();
        let paren_close = rest
            .find(')')
            .ok_or_else(|| CoreError::InvalidArgument("expected ')' in CREATE INDEX".to_string()))?;
        let column = rest[paren_open + 1..paren_close].trim().to_string();

        if class.is_empty() || column.is_empty() {
            return Err(CoreError::InvalidArgument(
                "class and column cannot be empty in CREATE INDEX".to_string(),
            ));
        }

        Ok(QueryAst::CreateIndex { class, column })
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

        for c in input.chars() {
            if let Some(q) = in_quote {
                current.push(c);
                if c == q {
                    in_quote = None;
                }
            } else if c == '\'' || c == '"' {
                in_quote = Some(c);
                current.push(c);
            } else if c == delim {
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

        // Try keyword operators first: LIKE, BETWEEN, IN
        let upper = input.to_uppercase();

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
