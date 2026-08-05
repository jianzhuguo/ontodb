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

    /// SELECT ... FROM <class> [WHERE ...] [ORDER BY ...] [LIMIT ...]
    Select {
        columns: SelectColumns,
        from: String,
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

        // Parse FROM clause
        let (from, rest) = Self::parse_word(rest)?;

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
            filter,
            order_by,
            limit,
        })
    }

    fn parse_update(input: &str) -> Result<QueryAst> {
        let upper = input.to_uppercase();
        let set_pos = upper
            .find(" SET ")
            .ok_or_else(|| CoreError::InvalidArgument("expected 'SET'".to_string()))?;

        let class = input[6..set_pos].trim().to_string();
        let rest = &input[set_pos + 5..];

        // Parse SET assignments
        let where_upper = rest.to_uppercase();
        let (set_part, filter_str) = if let Some(wp) = where_upper.find(" WHERE ") {
            (&rest[..wp], Some(rest[wp + 7..].trim()))
        } else {
            (rest, None)
        };

        let assignments: Vec<(String, LiteralValue)> = set_part
            .split(',')
            .map(|s| {
                let s = s.trim();
                let eq_pos = s
                    .find('=')
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

    fn parse_where(input: &str) -> Result<(Option<FilterExpr>, String)> {
        // Simple single-condition parser: column op value
        let input = input.trim();

        // Find operator
        for (op_str, op_fn) in &[
            (">=", FilterExpr::Gte as fn(String, LiteralValue) -> FilterExpr),
            ("<=", FilterExpr::Lte),
            ("!=", FilterExpr::Ne),
            ("<>", FilterExpr::Ne),
            (">", FilterExpr::Gt),
            ("<", FilterExpr::Lt),
            ("=", FilterExpr::Eq),
        ] {
            if let Some(pos) = input.find(op_str) {
                let col = input[..pos].trim().to_string();
                let rest = input[pos + op_str.len()..].trim();

                // Find the end of the value (space, semicolon, or end of string)
                let val_end = rest
                    .find(|c: char| c.is_whitespace() || c == ';' || c == ',')
                    .unwrap_or(rest.len());
                let val_str = &rest[..val_end];
                let remaining = rest[val_end..].to_string();

                let val = Self::parse_literal(val_str)?;
                let remaining = remaining.trim().to_string();
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
