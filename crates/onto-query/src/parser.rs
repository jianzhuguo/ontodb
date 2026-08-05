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

    /// SELECT ... FROM <class> [JOIN ...] [WHERE ...] [ORDER BY ...] [LIMIT ...]
    Select {
        columns: SelectColumns,
        from: String,
        from_alias: Option<String>,
        joins: Vec<JoinClause>,
        filter: Option<FilterExpr>,
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SelectColumns {
    All,
    Columns(Vec<String>),
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
        } else if upper.starts_with("INSERT") {
            Self::parse_insert(input)
        } else if upper.starts_with("SELECT") {
            Self::parse_select(input)
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

        let cols_str = input[6..from_pos].trim(); // After "SELECT"
        let rest = input[from_pos + 6..].trim();

        let columns = if cols_str == "*" {
            SelectColumns::All
        } else {
            SelectColumns::Columns(
                cols_str
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .collect(),
            )
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

        // Parse optional ORDER BY
        let rest_upper = rest.to_uppercase();
        let (order_by, rest) = if rest_upper.starts_with("ORDER BY") {
            let rest = rest[8..].trim();
            let (col, rest) = Self::parse_word(rest)?;
            let ascending = if rest.to_uppercase().starts_with("DESC") {
                false
            } else {
                true
            };
            (
                Some(OrderBy {
                    column: col,
                    ascending,
                }),
                rest.to_string(),
            )
        } else {
            (None, rest)
        };

        // Parse optional LIMIT
        let rest_upper = rest.to_uppercase();
        let limit = if rest_upper.starts_with("LIMIT") {
            let num_str = rest[5..].trim();
            Some(
                num_str
                    .parse::<usize>()
                    .map_err(|_| CoreError::InvalidArgument("invalid LIMIT".to_string()))?,
            )
        } else {
            None
        };

        Ok(QueryAst::Select {
            columns,
            from,
            from_alias,
            joins,
            filter,
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
        // Simple single-condition parser: column op value
        let input = input.trim();

        // Find operator (skip quoted strings)
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

                // Find the end of the value, handling quoted strings
                let val_str;
                let remaining;
                if rest.starts_with('\'') || rest.starts_with('"') {
                    let quote = rest.as_bytes()[0] as char;
                    if let Some(end_quote) = rest[1..].find(quote) {
                        val_str = &rest[1..1 + end_quote];
                        remaining = rest[1 + end_quote + 1..].trim().to_string();
                    } else {
                        val_str = rest;
                        remaining = String::new();
                    }
                } else {
                    let val_end = rest
                        .find(|c: char| c.is_whitespace() || c == ';' || c == ',')
                        .unwrap_or(rest.len());
                    val_str = &rest[..val_end];
                    remaining = rest[val_end..].trim().to_string();
                }

                let val = Self::parse_literal(val_str)?;
                return Ok((Some(op_fn(col, val)), remaining));
            }
        }

        Err(CoreError::InvalidArgument(format!(
            "invalid WHERE clause: {}",
            input
        )))
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
