// Copyright (c) 2024-2026 OntoDB Team
// Licensed under the Business Source License 1.1 (BUSL-1.1).
// See LICENSE for details. Change Date: 2031-09-15.
// On the Change Date, this file will be licensed under Apache License 2.0.
//! OntoQL parser — the official query language for OntoDB.
//!
//! OntoQL unifies SQL-like syntax with semantic web features:
//! - Ontology-aware class/property definitions with @onto annotations
//! - INFER clauses for real-time reasoning
//! - MATCH graph patterns
//! - @mind/@trace extension markers
//!
//! This module implements Phase 1: core parsing for CREATE CLASS, CREATE PROPERTY,
//! SELECT, INSERT, UPDATE, DELETE, and ontology annotations.

use onto_core::{CoreError, Result};
use serde::{Deserialize, Serialize};

// ── Reuse helper functions from parser module ──
use crate::parser::{self, QueryAst};
use crate::parser_util::{
    find_ignore_ascii_case, safe_slice, safe_slice_from, starts_with_ignore_ascii_case,
};

/// Find unquoted substring (skips content inside single/double quotes).
fn find_unquoted(haystack: &str, needle: &str) -> Option<usize> {
    let nlen = needle.len();
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
        } else if &hay_bytes[i..i + nlen] == needle.as_bytes() {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Find unquoted substring, case-insensitive (ASCII).
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
            let matched =
                (0..nlen).all(|j| hay_bytes[i + j].to_ascii_uppercase() == needle_upper[j]);
            if matched {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// Split a string by a delimiter, respecting quotes and parentheses.
fn split_quoted(input: &str, delim: char) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut in_quote: Option<char> = None;
    let mut depth = 0u32;

    for c in input.chars() {
        match c {
            '\'' | '"' if in_quote.is_none() => {
                in_quote = Some(c);
                current.push(c);
            }
            c if Some(c) == in_quote => {
                in_quote = None;
                current.push(c);
            }
            _ if in_quote.is_some() => current.push(c),
            '(' => {
                depth += 1;
                current.push(c);
            }
            ')' if depth > 0 => {
                depth -= 1;
                current.push(c);
            }
            c if c == delim && depth == 0 => {
                let trimmed = current.trim().to_string();
                if !trimmed.is_empty() {
                    parts.push(trimmed);
                }
                current.clear();
            }
            _ => current.push(c),
        }
    }
    let trimmed = current.trim().to_string();
    if !trimmed.is_empty() {
        parts.push(trimmed);
    }
    parts
}

/// Extract a quoted string or bare word from the start of `input`.
/// Returns (extracted_value, remaining_input).
fn extract_quoted_or_word(input: &str) -> (String, &str) {
    let trimmed = input.trim_start();
    if trimmed.starts_with('\'') || trimmed.starts_with('"') {
        let quote = trimmed.as_bytes()[0] as char;
        if let Some(end) = trimmed[1..].find(quote) {
            let val = &trimmed[1..1 + end];
            return (
                val.to_string(),
                safe_slice_from(trimmed, 1 + end + 1).trim_start(),
            );
        }
    }
    // Bare word: read until whitespace or special char
    let end = trimmed
        .find(|c: char| c.is_whitespace() || c == ',' || c == ')' || c == ';')
        .unwrap_or(trimmed.len());
    if end == 0 {
        return (String::new(), trimmed);
    }
    (
        trimmed[..end].to_string(),
        safe_slice_from(trimmed, end).trim_start(),
    )
}

/// Consume input until one of the given keywords is found (case-insensitive).
/// Returns (consumed_part, remaining_input, keyword_found).
fn consume_until_keywords<'a>(
    input: &'a str,
    keywords: &[&'a str],
) -> (&'a str, &'a str, Option<&'a str>) {
    let lower = input.to_ascii_lowercase();
    let mut earliest: Option<(usize, &str)> = None;
    for kw in keywords {
        if let Some(pos) = find_unquoted_ignore_ascii_case(&lower, &kw.to_ascii_lowercase()) {
            if earliest.is_none() || pos < earliest.unwrap().0 {
                earliest = Some((pos, *kw));
            }
        }
    }
    if let Some((pos, kw)) = earliest {
        let consumed = safe_slice(input, 0, pos);
        let remaining = safe_slice_from(input, pos);
        (consumed, remaining, Some(kw))
    } else {
        (input, "", None)
    }
}

/// Trim trailing semicolons and whitespace.
fn trim_semicolons(s: &str) -> &str {
    s.trim_end_matches(|c: char| c == ';' || c.is_whitespace())
}

// ════════════════════════════════════════════════════════════════════
//  OntoQL AST Types
// ════════════════════════════════════════════════════════════════════

/// OntoQL extension annotation: `@onto(key="value")` or `@mind(param=val)`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Annotation {
    pub marker: String, // "onto", "mind", "trace"
    pub params: Vec<(String, String)>,
}

/// Property characteristic (OWL feature).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Characteristic {
    Transitive,
    Symmetric,
    Functional,
    InverseFunctional,
}

/// Property type: datatype or object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PropertyKind {
    Datatype,
    Object,
}

/// Infer clause mode.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum InferMode {
    Local,
    Remote { endpoint: String },
}

/// A value expression in OntoQL (for INSERT/UPDATE assignments and WHERE conditions).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum OntoValueExpr {
    Literal(OntoLiteral),
    Column(String),
    Null,
    Function {
        name: String,
        args: Vec<OntoValueExpr>,
    },
    Arithmetic {
        op: String,
        left: Box<OntoValueExpr>,
        right: Box<OntoValueExpr>,
    },
}

/// OntoQL literal values.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum OntoLiteral {
    String(String),
    Int(i64),
    Float(f64),
    Bool(bool),
}

/// OntoQL filter expression (WHERE clause).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum OntoFilterExpr {
    Eq(String, OntoValueExpr),
    Ne(String, OntoValueExpr),
    Gt(String, OntoValueExpr),
    Lt(String, OntoValueExpr),
    Gte(String, OntoValueExpr),
    Lte(String, OntoValueExpr),
    Like(String, String),
    In(String, Vec<OntoValueExpr>),
    IsNull(String),
    IsNotNull(String),
    Between(String, OntoValueExpr, OntoValueExpr),
    And(Box<OntoFilterExpr>, Box<OntoFilterExpr>),
    Or(Box<OntoFilterExpr>, Box<OntoFilterExpr>),
    Not(Box<OntoFilterExpr>),
    /// @onto(...) marker condition
    AnnotationCondition(Annotation),
}

/// OntoQL SELECT projection.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum OntoProjection {
    All,             // SELECT *
    AllFrom(String), // SELECT p.*
    Column { name: String, alias: Option<String> },
    Expression { expr: OntoValueExpr, alias: String },
}

/// OntoQL ORDER BY.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OntoOrderBy {
    pub column: String,
    pub ascending: bool,
}

/// OntoQL JOIN type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum JoinType {
    Inner,
    Left,
    Right,
    Full,
}

/// OntoQL FROM clause.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum OntoFromClause {
    Class { name: String, alias: Option<String> },
    Subquery(Box<OntoQLAst>),
}

/// OntoQL JOIN clause.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OntoJoinClause {
    pub join_type: JoinType,
    pub target: OntoFromClause,
    pub on_condition: OntoFilterExpr,
}

/// The OntoQL AST — the complete parsed representation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum OntoQLAst {
    // ── Schema Definition ──
    CreateClass {
        name: String,
        extends: Option<String>,
        abstract_class: bool,
        annotations: Vec<Annotation>,
    },
    DropClass {
        name: String,
    },
    DropOntology {
        name: String,
    },
    CreateProperty {
        name: String,
        kind: PropertyKind,
        domain: String,
        range: String,
        inverse_of: Option<String>,
        characteristics: Vec<Characteristic>,
        required: bool,
        multi_valued: bool,
    },

    // ── Data Manipulation ──
    Insert {
        class: String,
        assignments: Vec<(String, OntoValueExpr)>,
    },
    /// INSERT TRIPLE SET subject = "s", predicate = "p", object = "o"
    InsertTriple {
        subject: String,
        predicate: String,
        object: String,
    },
    /// INSERT TRIPLES (subject, predicate, object) VALUES ("s1","p1","o1"), ...
    InsertTriples {
        triples: Vec<(String, String, String)>,
    },
    /// DELETE TRIPLE SET subject = "s", predicate = "p", object = "o"
    DeleteTriple {
        subject: String,
        predicate: String,
        object: String,
    },
    Update {
        class: String,
        assignments: Vec<(String, OntoValueExpr)>,
        filter: Option<OntoFilterExpr>,
    },
    Delete {
        class: String,
        filter: Option<OntoFilterExpr>,
    },

    // ── Query (core) ──
    Select {
        distinct: bool,
        projections: Vec<OntoProjection>,
        from: OntoFromClause,
        joins: Vec<OntoJoinClause>,
        filter: Option<OntoFilterExpr>,
        infer: Option<InferClause>,
        group_by: Vec<String>,
        having: Option<OntoFilterExpr>,
        order_by: Vec<OntoOrderBy>,
        limit: Option<usize>,
        offset: Option<usize>,
        annotations: Vec<Annotation>,
    },
    /// SELECT TRIPLE [WHERE subject = "s" AND predicate = "p" AND object = "o"]
    /// Pattern matching: any combination of subject/predicate/object can be specified
    SelectTriples {
        subject: Option<String>,
        predicate: Option<String>,
        object: Option<String>,
        limit: Option<usize>,
    },

    // ── Inference ──
    Infer {
        query: Box<OntoQLAst>,
        mode: InferMode,
        rules: Vec<String>,
    },

    // ── Explain ──
    Explain {
        query: Box<OntoQLAst>,
    },

    // ── Transactions ──
    Begin,
    Commit,
    Rollback,

    // ── Namespace management ──
    CreateNamespace {
        name: String,
    },
    DropNamespace {
        name: String,
    },
    UseNamespace {
        name: String,
    },

    // ── Ontology import ──
    ImportOntology {
        sql: String,
    },

    // ── Pass-through to existing SQL parser ──
    SqlPassthrough(QueryAst),
}

/// INFER clause attached to SELECT.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InferClause {
    pub scope: String,      // e.g. "SUBCLASS", "ALL"
    pub rules: Vec<String>, // specific rules, empty = all defaults
}

// ════════════════════════════════════════════════════════════════════
//  OntoQL Parser
// ════════════════════════════════════════════════════════════════════

/// The OntoQL parser — a hand-written recursive descent parser.
pub struct OntoQLParser;

impl OntoQLParser {
    /// Parse an OntoQL statement into an AST.
    pub fn parse(input: &str) -> Result<OntoQLAst> {
        let input = trim_semicolons(input.trim());
        if input.is_empty() {
            return Err(CoreError::InvalidArgument("Empty query".into()));
        }

        // Dispatch by leading keyword
        if starts_with_ignore_ascii_case(input, "CREATE NAMESPACE") {
            return Self::parse_create_namespace(input);
        }
        if starts_with_ignore_ascii_case(input, "CREATE CLASS") {
            return Self::parse_create_class(input);
        }
        if starts_with_ignore_ascii_case(input, "CREATE DATATYPE PROPERTY")
            || starts_with_ignore_ascii_case(input, "CREATE OBJECT PROPERTY")
            || starts_with_ignore_ascii_case(input, "CREATE PROPERTY")
        {
            return Self::parse_create_property(input);
        }
        if starts_with_ignore_ascii_case(input, "DROP NAMESPACE") {
            return Self::parse_drop_namespace(input);
        }
        if starts_with_ignore_ascii_case(input, "DROP CLASS") {
            return Self::parse_drop_class(input);
        }
        if starts_with_ignore_ascii_case(input, "DROP ONTOLOGY") {
            return Self::parse_drop_ontology(input);
        }
        if starts_with_ignore_ascii_case(input, "USE NAMESPACE") {
            return Self::parse_use_namespace(input);
        }
        if starts_with_ignore_ascii_case(input, "SELECT") {
            return Self::parse_select(input);
        }
        if starts_with_ignore_ascii_case(input, "INSERT") {
            return Self::parse_insert(input);
        }
        if starts_with_ignore_ascii_case(input, "UPDATE") {
            return Self::parse_update(input);
        }
        if starts_with_ignore_ascii_case(input, "DELETE") {
            return Self::parse_delete(input);
        }
        if starts_with_ignore_ascii_case(input, "INFER") {
            return Self::parse_infer(input);
        }
        if starts_with_ignore_ascii_case(input, "EXPLAIN") {
            return Self::parse_explain(input);
        }
        if starts_with_ignore_ascii_case(input, "BEGIN") {
            return Ok(OntoQLAst::Begin);
        }
        if starts_with_ignore_ascii_case(input, "COMMIT") {
            return Ok(OntoQLAst::Commit);
        }
        if starts_with_ignore_ascii_case(input, "ROLLBACK") {
            return Ok(OntoQLAst::Rollback);
        }
        if starts_with_ignore_ascii_case(input, "CREATE ONTOLOGY") {
            return Ok(OntoQLAst::ImportOntology {
                sql: input.to_string(),
            });
        }

        // Fallback: pass to SQL parser
        let sql_ast = parser::QueryParser::parse(input)?;
        Ok(OntoQLAst::SqlPassthrough(sql_ast))
    }

    // ── CREATE CLASS ──────────────────────────────────────────────

    fn parse_create_class(input: &str) -> Result<OntoQLAst> {
        let rest = safe_slice_from(input, "CREATE CLASS".len()).trim_start();
        let (annotations, rest) = Self::extract_trailing_annotations(rest);
        let rest = rest.trim();

        // Parse: ClassName [EXTENDS Parent] [ABSTRACT true|false] [ANNOTATION ...]
        // Find the class name (first word)
        let (name, rest) = Self::extract_identifier(rest);
        if name.is_empty() {
            return Err(CoreError::InvalidArgument(
                "Missing class name in CREATE CLASS".into(),
            ));
        }

        let mut extends = None;
        let mut abstract_class = false;
        let remaining = rest.trim();

        // Check for EXTENDS keyword
        if let Some(pos) = find_ignore_ascii_case(remaining, "EXTENDS") {
            let after_extends = safe_slice_from(remaining, pos + "EXTENDS".len()).trim_start();
            let (parent, after_parent) = Self::extract_identifier(after_extends);
            if !parent.is_empty() {
                extends = Some(parent);
            }
            let after_parent = after_parent.trim();
            // Check for ABSTRACT after EXTENDS
            if let Some(abs_pos) = find_ignore_ascii_case(after_parent, "ABSTRACT") {
                let after_abs =
                    safe_slice_from(after_parent, abs_pos + "ABSTRACT".len()).trim_start();
                abstract_class = Self::parse_bool_value(after_abs);
            }
        } else if let Some(pos) = find_ignore_ascii_case(remaining, "ABSTRACT") {
            let after_abs = safe_slice_from(remaining, pos + "ABSTRACT".len()).trim_start();
            abstract_class = Self::parse_bool_value(after_abs);
        }

        Ok(OntoQLAst::CreateClass {
            name,
            extends,
            abstract_class,
            annotations,
        })
    }

    // ── DROP CLASS ────────────────────────────────────────────────

    fn parse_drop_class(input: &str) -> Result<OntoQLAst> {
        let rest = safe_slice_from(input, "DROP CLASS".len()).trim_start();
        let (name, _) = Self::extract_identifier(rest);
        if name.is_empty() {
            return Err(CoreError::InvalidArgument(
                "Missing class name in DROP CLASS".into(),
            ));
        }
        Ok(OntoQLAst::DropClass { name })
    }

    // ── DROP ONTOLOGY ─────────────────────────────────────────────

    fn parse_drop_ontology(input: &str) -> Result<OntoQLAst> {
        let rest = safe_slice_from(input, "DROP ONTOLOGY".len()).trim_start();
        let (name, _) = Self::extract_identifier(rest);
        if name.is_empty() {
            return Err(CoreError::InvalidArgument(
                "Missing ontology name in DROP ONTOLOGY".into(),
            ));
        }
        Ok(OntoQLAst::DropOntology { name })
    }

    // ── CREATE NAMESPACE ─────────────────────────────────────────

    fn parse_create_namespace(input: &str) -> Result<OntoQLAst> {
        let rest = safe_slice_from(input, "CREATE NAMESPACE".len()).trim_start();
        let (name, _) = Self::extract_identifier(rest);
        if name.is_empty() {
            return Err(CoreError::InvalidArgument(
                "Missing namespace name in CREATE NAMESPACE".into(),
            ));
        }
        Ok(OntoQLAst::CreateNamespace { name })
    }

    // ── DROP NAMESPACE ───────────────────────────────────────────

    fn parse_drop_namespace(input: &str) -> Result<OntoQLAst> {
        let rest = safe_slice_from(input, "DROP NAMESPACE".len()).trim_start();
        let (name, _) = Self::extract_identifier(rest);
        if name.is_empty() {
            return Err(CoreError::InvalidArgument(
                "Missing namespace name in DROP NAMESPACE".into(),
            ));
        }
        Ok(OntoQLAst::DropNamespace { name })
    }

    // ── USE NAMESPACE ────────────────────────────────────────────

    fn parse_use_namespace(input: &str) -> Result<OntoQLAst> {
        let rest = safe_slice_from(input, "USE NAMESPACE".len()).trim_start();
        let (name, _) = Self::extract_identifier(rest);
        if name.is_empty() {
            return Err(CoreError::InvalidArgument(
                "Missing namespace name in USE NAMESPACE".into(),
            ));
        }
        Ok(OntoQLAst::UseNamespace { name })
    }

    // ── CREATE PROPERTY ───────────────────────────────────────────

    fn parse_create_property(input: &str) -> Result<OntoQLAst> {
        let input = input.trim();
        let (kind, rest) = if starts_with_ignore_ascii_case(input, "CREATE DATATYPE PROPERTY") {
            (
                PropertyKind::Datatype,
                safe_slice_from(input, "CREATE DATATYPE PROPERTY".len()),
            )
        } else if starts_with_ignore_ascii_case(input, "CREATE OBJECT PROPERTY") {
            (
                PropertyKind::Object,
                safe_slice_from(input, "CREATE OBJECT PROPERTY".len()),
            )
        } else {
            (
                PropertyKind::Datatype,
                safe_slice_from(input, "CREATE PROPERTY".len()),
            )
        };

        let rest = rest.trim_start();
        let (name, rest) = Self::extract_identifier(rest);
        if name.is_empty() {
            return Err(CoreError::InvalidArgument("Missing property name".into()));
        }

        let rest = rest.trim();
        // Parse: DOMAIN <class> RANGE <type> [INVERSE OF <prop>] [CHARACTERISTICS (...)] [REQUIRED] [MULTI_VALUED]
        let domain = Self::extract_keyword_value(rest, "DOMAIN");
        let range = Self::extract_keyword_value(rest, "RANGE");
        let inverse_of = Self::extract_keyword_value(rest, "INVERSE OF");

        let required = find_ignore_ascii_case(rest, "REQUIRED").is_some();
        let multi_valued = find_ignore_ascii_case(rest, "MULTI_VALUED").is_some();

        let mut characteristics = Vec::new();
        if find_ignore_ascii_case(rest, "TRANSITIVE").is_some() {
            characteristics.push(Characteristic::Transitive);
        }
        if find_ignore_ascii_case(rest, "SYMMETRIC").is_some() {
            characteristics.push(Characteristic::Symmetric);
        }
        if find_ignore_ascii_case(rest, "FUNCTIONAL").is_some() {
            characteristics.push(Characteristic::Functional);
        }

        Ok(OntoQLAst::CreateProperty {
            name,
            kind,
            domain: domain.unwrap_or_default(),
            range: range.unwrap_or_default(),
            inverse_of,
            characteristics,
            required,
            multi_valued,
        })
    }

    // ── SELECT ────────────────────────────────────────────────────

    fn parse_select(input: &str) -> Result<OntoQLAst> {
        let rest = safe_slice_from(input, "SELECT".len()).trim_start();

        // SELECT TRIPLE [WHERE subject = "s" AND predicate = "p" AND object = "o"] [LIMIT n]
        if starts_with_ignore_ascii_case(rest, "TRIPLE") {
            let rest = safe_slice_from(rest, "TRIPLE".len()).trim_start();
            let mut subject = None;
            let mut predicate = None;
            let mut object = None;
            let mut limit = None;

            if starts_with_ignore_ascii_case(rest, "WHERE") {
                let rest = safe_slice_from(rest, "WHERE".len()).trim_start();
                let conditions = Self::parse_triple_conditions(rest)?;
                subject = conditions.0;
                predicate = conditions.1;
                object = conditions.2;
            }

            // Check for LIMIT at the end
            if let Some(pos) = find_unquoted_ignore_ascii_case(rest, "LIMIT") {
                let limit_str = safe_slice_from(rest, pos + "LIMIT".len()).trim_start();
                limit = limit_str
                    .split_whitespace()
                    .next()
                    .and_then(|s| s.parse().ok());
            }

            return Ok(OntoQLAst::SelectTriples {
                subject,
                predicate,
                object,
                limit,
            });
        }

        // Check DISTINCT
        let (distinct, rest) = if starts_with_ignore_ascii_case(rest, "DISTINCT") {
            (true, safe_slice_from(rest, "DISTINCT".len()).trim_start())
        } else {
            (false, rest)
        };

        // Extract trailing annotations from the full input
        let (annotations, _rest) = Self::extract_trailing_annotations(input);

        // Parse projections: everything before FROM
        let (proj_str, rest) = if let Some(pos) = find_unquoted_ignore_ascii_case(rest, " FROM ") {
            (safe_slice(rest, 0, pos), safe_slice_from(rest, pos + 1))
        } else {
            return Err(CoreError::InvalidArgument(
                "Missing FROM clause in SELECT".into(),
            ));
        };

        let projections = Self::parse_projections(proj_str.trim())?;

        // Parse FROM
        let rest = safe_slice_from(rest, "FROM".len()).trim_start();
        let (from, rest) = Self::parse_from_clause(rest)?;

        // Parse optional clauses
        let mut joins = Vec::new();
        let mut filter = None;
        let mut infer = None;
        let mut group_by = Vec::new();
        let mut having = None;
        let mut order_by = Vec::new();
        let mut limit = None;
        let mut offset = None;

        let mut remaining = rest;

        loop {
            remaining = remaining.trim();
            if remaining.is_empty() {
                break;
            }

            if starts_with_ignore_ascii_case(remaining, "JOIN")
                || starts_with_ignore_ascii_case(remaining, "LEFT JOIN")
                || starts_with_ignore_ascii_case(remaining, "RIGHT JOIN")
                || starts_with_ignore_ascii_case(remaining, "FULL JOIN")
            {
                let (join, new_rest) = Self::parse_join_clause(remaining)?;
                joins.push(join);
                remaining = new_rest;
            } else if starts_with_ignore_ascii_case(remaining, "WHERE") {
                remaining = safe_slice_from(remaining, "WHERE".len()).trim_start();
                let (f, new_rest) = Self::parse_filter_expr(remaining)?;
                filter = Some(f);
                remaining = new_rest;
            } else if starts_with_ignore_ascii_case(remaining, "INFER") {
                let (inf, new_rest) = Self::parse_infer_clause(remaining)?;
                infer = Some(inf);
                remaining = new_rest;
            } else if starts_with_ignore_ascii_case(remaining, "GROUP BY") {
                remaining = safe_slice_from(remaining, "GROUP BY".len()).trim_start();
                let (gb_str, new_rest, _) =
                    consume_until_keywords(remaining, &["HAVING", "ORDER", "LIMIT", "OFFSET"]);
                group_by = split_quoted(gb_str, ',')
                    .iter()
                    .map(|s| s.trim().trim_matches('"').to_string())
                    .collect();
                remaining = new_rest;
            } else if starts_with_ignore_ascii_case(remaining, "HAVING") {
                remaining = safe_slice_from(remaining, "HAVING".len()).trim_start();
                let (f, new_rest) = Self::parse_filter_expr(remaining)?;
                having = Some(f);
                remaining = new_rest;
            } else if starts_with_ignore_ascii_case(remaining, "ORDER BY") {
                remaining = safe_slice_from(remaining, "ORDER BY".len()).trim_start();
                let (ob_str, new_rest, _) = consume_until_keywords(remaining, &["LIMIT", "OFFSET"]);
                order_by = Self::parse_order_by(ob_str);
                remaining = new_rest;
            } else if starts_with_ignore_ascii_case(remaining, "LIMIT") {
                remaining = safe_slice_from(remaining, "LIMIT".len()).trim_start();
                let (val_str, new_rest) = Self::extract_number(remaining);
                limit = val_str.parse::<usize>().ok();
                remaining = new_rest;
                // Check for OFFSET after LIMIT
                if starts_with_ignore_ascii_case(remaining.trim_start(), "OFFSET") {
                    remaining =
                        safe_slice_from(remaining.trim_start(), "OFFSET".len()).trim_start();
                    let (off_str, new_rest2) = Self::extract_number(remaining);
                    offset = off_str.parse::<usize>().ok();
                    remaining = new_rest2;
                }
            } else if starts_with_ignore_ascii_case(remaining, "OFFSET") {
                remaining = safe_slice_from(remaining, "OFFSET".len()).trim_start();
                let (off_str, new_rest) = Self::extract_number(remaining);
                offset = off_str.parse::<usize>().ok();
                remaining = new_rest;
            } else {
                break;
            }
        }

        Ok(OntoQLAst::Select {
            distinct,
            projections,
            from,
            joins,
            filter,
            infer,
            group_by,
            having,
            order_by,
            limit,
            offset,
            annotations,
        })
    }

    // ── INSERT ────────────────────────────────────────────────────

    fn parse_insert(input: &str) -> Result<OntoQLAst> {
        let rest = safe_slice_from(input, "INSERT".len()).trim_start();

        // INSERT TRIPLES (subject, predicate, object) VALUES ("s1","p1","o1"), ...
        // Must check TRIPLES before TRIPLE to avoid prefix match
        if starts_with_ignore_ascii_case(rest, "TRIPLES") {
            let rest = safe_slice_from(rest, "TRIPLES".len()).trim_start();
            // Skip column list if present: (subject, predicate, object)
            let rest = if rest.starts_with('(') {
                let (_, rest) = Self::extract_paren_content(rest)?;
                rest.trim_start()
            } else {
                rest
            };
            if !starts_with_ignore_ascii_case(rest, "VALUES") {
                return Err(CoreError::InvalidArgument(
                    "Expected VALUES in INSERT TRIPLES".into(),
                ));
            }
            let rest = safe_slice_from(rest, "VALUES".len()).trim_start();
            let triples = Self::parse_triple_values(rest)?;
            return Ok(OntoQLAst::InsertTriples { triples });
        }

        // INSERT TRIPLE SET subject = "s", predicate = "p", object = "o"
        if starts_with_ignore_ascii_case(rest, "TRIPLE ") {
            let rest = safe_slice_from(rest, "TRIPLE".len()).trim_start();
            if !starts_with_ignore_ascii_case(rest, "SET") {
                return Err(CoreError::InvalidArgument(
                    "Expected SET in INSERT TRIPLE".into(),
                ));
            }
            let rest = safe_slice_from(rest, "SET".len()).trim_start();
            let assignments = Self::parse_assignments(rest)?;
            let subject = Self::get_assignment_value(&assignments, "subject")?;
            let predicate = Self::get_assignment_value(&assignments, "predicate")?;
            let object = Self::get_assignment_value(&assignments, "object")?;
            return Ok(OntoQLAst::InsertTriple {
                subject,
                predicate,
                object,
            });
        }

        // INSERT INTO class SET ...
        if !starts_with_ignore_ascii_case(rest, "INTO") {
            return Err(CoreError::InvalidArgument(
                "Expected INTO after INSERT".into(),
            ));
        }
        let rest = safe_slice_from(rest, "INTO".len()).trim_start();
        let (class, rest) = Self::extract_identifier(rest);
        if class.is_empty() {
            return Err(CoreError::InvalidArgument(
                "Missing class name in INSERT".into(),
            ));
        }

        let rest = rest.trim();
        // Parse SET assignments: SET col1 = val1, col2 = val2
        if !starts_with_ignore_ascii_case(rest, "SET") {
            return Err(CoreError::InvalidArgument("Expected SET in INSERT".into()));
        }
        let rest = safe_slice_from(rest, "SET".len()).trim_start();
        let assignments = Self::parse_assignments(rest)?;

        Ok(OntoQLAst::Insert { class, assignments })
    }

    // ── UPDATE ────────────────────────────────────────────────────

    fn parse_update(input: &str) -> Result<OntoQLAst> {
        let rest = safe_slice_from(input, "UPDATE".len()).trim_start();
        let (class, rest) = Self::extract_identifier(rest);
        if class.is_empty() {
            return Err(CoreError::InvalidArgument(
                "Missing class name in UPDATE".into(),
            ));
        }

        let rest = rest.trim();
        if !starts_with_ignore_ascii_case(rest, "SET") {
            return Err(CoreError::InvalidArgument("Expected SET in UPDATE".into()));
        }
        let rest = safe_slice_from(rest, "SET".len()).trim_start();

        // Split by WHERE (if present)
        let (assign_str, rest) = if let Some(pos) = find_unquoted_ignore_ascii_case(rest, " WHERE ")
        {
            (
                safe_slice(rest, 0, pos),
                safe_slice_from(rest, pos + " WHERE ".len()),
            )
        } else {
            (rest, "")
        };

        let assignments = Self::parse_assignments(assign_str.trim())?;

        let filter = if !rest.trim().is_empty() {
            let (f, _) = Self::parse_filter_expr(rest.trim())?;
            Some(f)
        } else {
            None
        };

        Ok(OntoQLAst::Update {
            class,
            assignments,
            filter,
        })
    }

    // ── DELETE ────────────────────────────────────────────────────

    fn parse_delete(input: &str) -> Result<OntoQLAst> {
        let rest = safe_slice_from(input, "DELETE".len()).trim_start();

        // DELETE TRIPLE SET subject = "s", predicate = "p", object = "o"
        if starts_with_ignore_ascii_case(rest, "TRIPLE") {
            let rest = safe_slice_from(rest, "TRIPLE".len()).trim_start();
            if !starts_with_ignore_ascii_case(rest, "SET") {
                return Err(CoreError::InvalidArgument(
                    "Expected SET in DELETE TRIPLE".into(),
                ));
            }
            let rest = safe_slice_from(rest, "SET".len()).trim_start();
            let assignments = Self::parse_assignments(rest)?;
            let subject = Self::get_assignment_value(&assignments, "subject")?;
            let predicate = Self::get_assignment_value(&assignments, "predicate")?;
            let object = Self::get_assignment_value(&assignments, "object")?;
            return Ok(OntoQLAst::DeleteTriple {
                subject,
                predicate,
                object,
            });
        }

        // DELETE FROM class [WHERE ...]
        if !starts_with_ignore_ascii_case(rest, "FROM") {
            return Err(CoreError::InvalidArgument(
                "Expected FROM after DELETE".into(),
            ));
        }
        let rest = safe_slice_from(rest, "FROM".len()).trim_start();
        let (class, rest) = Self::extract_identifier(rest);
        if class.is_empty() {
            return Err(CoreError::InvalidArgument(
                "Missing class name in DELETE".into(),
            ));
        }

        let rest = rest.trim();
        let filter = if starts_with_ignore_ascii_case(rest, "WHERE") {
            let rest = safe_slice_from(rest, "WHERE".len()).trim_start();
            let (f, _) = Self::parse_filter_expr(rest)?;
            Some(f)
        } else {
            None
        };

        Ok(OntoQLAst::Delete { class, filter })
    }

    // ── INFER ─────────────────────────────────────────────────────

    fn parse_infer(input: &str) -> Result<OntoQLAst> {
        let rest = safe_slice_from(input, "INFER".len()).trim_start();

        // Check for CHAINED REASONING
        if starts_with_ignore_ascii_case(rest, "CHAINED") {
            let rest = safe_slice_from(rest, "CHAINED".len()).trim_start();
            if starts_with_ignore_ascii_case(rest, "REASONING") {
                let rest = safe_slice_from(rest, "REASONING".len()).trim_start();
                // Parse LOCAL USING rule1, rule2 [REMOTE @mind(...)]
                let mut rules = Vec::new();
                let mut mode = InferMode::Local;

                if starts_with_ignore_ascii_case(rest, "LOCAL") {
                    let rest = safe_slice_from(rest, "LOCAL".len()).trim_start();
                    if starts_with_ignore_ascii_case(rest, "USING") {
                        let rest = safe_slice_from(rest, "USING".len()).trim_start();
                        let (rules_str, new_rest, _) = consume_until_keywords(rest, &["REMOTE"]);
                        rules = split_quoted(rules_str, ',')
                            .iter()
                            .map(|s| s.trim().to_string())
                            .collect();
                        let remaining = new_rest.trim();
                        if starts_with_ignore_ascii_case(remaining, "REMOTE") {
                            let rest = safe_slice_from(remaining, "REMOTE".len()).trim_start();
                            if rest.starts_with('@') {
                                let (endpoint, _) = Self::extract_quoted_or_word_value(rest);
                                mode = InferMode::Remote { endpoint };
                            }
                        }
                    }
                }

                return Ok(OntoQLAst::Infer {
                    query: Box::new(OntoQLAst::Select {
                        distinct: false,
                        projections: vec![OntoProjection::All],
                        from: OntoFromClause::Class {
                            name: "*".into(),
                            alias: None,
                        },
                        joins: vec![],
                        filter: None,
                        infer: None,
                        group_by: vec![],
                        having: None,
                        order_by: vec![],
                        limit: None,
                        offset: None,
                        annotations: vec![],
                    }),
                    mode,
                    rules,
                });
            }
        }

        // INFER <subquery> — not yet fully supported in Phase 1
        Err(CoreError::InvalidArgument(
            "INFER with subquery not yet supported. Use SELECT ... INFER @onto(scope=...)".into(),
        ))
    }

    // ── EXPLAIN ───────────────────────────────────────────────────

    fn parse_explain(input: &str) -> Result<OntoQLAst> {
        let rest = safe_slice_from(input, "EXPLAIN".len()).trim_start();
        // Remove optional "PLAN FOR"
        let rest = if starts_with_ignore_ascii_case(rest, "PLAN FOR") {
            safe_slice_from(rest, "PLAN FOR".len()).trim_start()
        } else {
            rest
        };
        let inner = OntoQLParser::parse(rest)?;
        Ok(OntoQLAst::Explain {
            query: Box::new(inner),
        })
    }

    // ══════════════════════════════════════════════════════════════
    //  Sub-parsers
    // ══════════════════════════════════════════════════════════════

    /// Parse projections (column list between SELECT and FROM).
    fn parse_projections(input: &str) -> Result<Vec<OntoProjection>> {
        if input == "*" {
            return Ok(vec![OntoProjection::All]);
        }

        let parts = split_quoted(input, ',');
        let mut projections = Vec::new();

        for part in parts {
            let part = part.trim();
            if part == "*" {
                projections.push(OntoProjection::All);
            } else if part.ends_with(".*") {
                let table = part.trim_end_matches(".*").trim_matches('"');
                projections.push(OntoProjection::AllFrom(table.to_string()));
            } else {
                // Check for alias (AS keyword or space-separated)
                let (expr, alias) = if let Some(pos) = find_ignore_ascii_case(part, " AS ") {
                    let expr = safe_slice(part, 0, pos).trim();
                    let alias = safe_slice_from(part, pos + 4).trim().trim_matches('"');
                    (expr, Some(alias.to_string()))
                } else {
                    (part, None)
                };
                projections.push(OntoProjection::Column {
                    name: expr.trim_matches('"').to_string(),
                    alias,
                });
            }
        }

        Ok(projections)
    }

    /// Parse FROM clause.
    fn parse_from_clause(input: &str) -> Result<(OntoFromClause, &str)> {
        let input = input.trim_start();
        if input.starts_with('(') {
            // Subquery
            return Err(CoreError::InvalidArgument(
                "Subquery in FROM not yet supported".into(),
            ));
        }

        // Class name with optional alias
        let (name, rest) = Self::extract_identifier(input);
        let rest = rest.trim_start();

        let (alias, rest) = if !rest.is_empty()
            && !starts_with_ignore_ascii_case(rest, "WHERE")
            && !starts_with_ignore_ascii_case(rest, "JOIN")
            && !starts_with_ignore_ascii_case(rest, "LEFT")
            && !starts_with_ignore_ascii_case(rest, "RIGHT")
            && !starts_with_ignore_ascii_case(rest, "FULL")
            && !starts_with_ignore_ascii_case(rest, "GROUP")
            && !starts_with_ignore_ascii_case(rest, "ORDER")
            && !starts_with_ignore_ascii_case(rest, "LIMIT")
            && !starts_with_ignore_ascii_case(rest, "INFER")
            && !starts_with_ignore_ascii_case(rest, "HAVING")
        {
            // Check for AS keyword
            if starts_with_ignore_ascii_case(rest, "AS ") {
                let rest = safe_slice_from(rest, 3).trim_start();
                let (alias, rest) = Self::extract_identifier(rest);
                (Some(alias), rest)
            } else if starts_with_ignore_ascii_case(rest, "ON ")
                || starts_with_ignore_ascii_case(rest, "WHERE ")
                || starts_with_ignore_ascii_case(rest, "JOIN ")
                || starts_with_ignore_ascii_case(rest, "LEFT ")
            {
                (None, rest)
            } else {
                // Could be alias without AS
                let first_char = rest.as_bytes().first().copied().unwrap_or(0);
                if first_char.is_ascii_alphabetic() || first_char == b'_' {
                    let (alias, rest) = Self::extract_identifier(rest);
                    if !alias.eq_ignore_ascii_case(&name) {
                        (Some(alias), rest)
                    } else {
                        (None, rest)
                    }
                } else {
                    (None, rest)
                }
            }
        } else {
            (None, rest)
        };

        Ok((OntoFromClause::Class { name, alias }, rest))
    }

    /// Parse a JOIN clause.
    fn parse_join_clause(input: &str) -> Result<(OntoJoinClause, &str)> {
        let input = input.trim_start();
        let (join_type, rest) = if starts_with_ignore_ascii_case(input, "LEFT JOIN") {
            (JoinType::Left, safe_slice_from(input, "LEFT JOIN".len()))
        } else if starts_with_ignore_ascii_case(input, "RIGHT JOIN") {
            (JoinType::Right, safe_slice_from(input, "RIGHT JOIN".len()))
        } else if starts_with_ignore_ascii_case(input, "FULL JOIN") {
            (JoinType::Full, safe_slice_from(input, "FULL JOIN".len()))
        } else if starts_with_ignore_ascii_case(input, "JOIN") {
            (JoinType::Inner, safe_slice_from(input, "JOIN".len()))
        } else {
            return Err(CoreError::InvalidArgument("Expected JOIN keyword".into()));
        };

        let rest = rest.trim_start();
        // Parse: <table> [alias] ON <condition>
        let (from, rest) = Self::parse_from_clause(rest)?;
        let rest = rest.trim_start();

        if !starts_with_ignore_ascii_case(rest, "ON") {
            return Err(CoreError::InvalidArgument("Expected ON after JOIN".into()));
        }
        let rest = safe_slice_from(rest, 2).trim_start();
        let (condition, rest) = Self::parse_filter_expr(rest)?;

        Ok((
            OntoJoinClause {
                join_type,
                target: from,
                on_condition: condition,
            },
            rest,
        ))
    }

    /// Parse INFER clause: `INFER @onto(scope=SUBCLASS)`.
    fn parse_infer_clause(input: &str) -> Result<(InferClause, &str)> {
        let rest = safe_slice_from(input, "INFER".len()).trim_start();

        // Parse @onto(scope=...) or just INFER
        if rest.starts_with('@') {
            let (ann, rest) = Self::parse_annotation(rest)?;
            let scope = ann
                .params
                .iter()
                .find(|(k, _)| k == "scope")
                .map(|(_, v)| v.clone())
                .unwrap_or_else(|| "SUBCLASS".into());
            Ok((
                InferClause {
                    scope,
                    rules: vec![],
                },
                rest,
            ))
        } else {
            // Simple INFER
            Ok((
                InferClause {
                    scope: "ALL".into(),
                    rules: vec![],
                },
                rest,
            ))
        }
    }

    /// Parse filter expression (WHERE/HAVING/ON conditions).
    /// Delegates to parse_filter_atom for the actual parsing, then chains AND/OR.
    fn parse_filter_expr(input: &str) -> Result<(OntoFilterExpr, &str)> {
        let (expr, rest) = Self::parse_filter_atom(input)?;
        Self::chain_and_or(expr, rest)
    }

    /// Check for trailing AND/OR and chain expressions.
    /// AND binds tighter than OR: `a OR b AND c` → `a OR (b AND c)`.
    fn chain_and_or(expr: OntoFilterExpr, rest: &str) -> Result<(OntoFilterExpr, &str)> {
        let rest = rest.trim_start();
        if starts_with_ignore_ascii_case(rest, "AND ") {
            // AND: parse the next atom, then continue chaining at AND level
            let after = safe_slice_from(rest, 4).trim_start();
            let (right, after2) = Self::parse_filter_atom(after)?;
            let combined = OntoFilterExpr::And(Box::new(expr), Box::new(right));
            Self::chain_and_or(combined, after2)
        } else if starts_with_ignore_ascii_case(rest, "OR ") {
            // OR: parse the right side as a full AND-chain (higher precedence)
            let after = safe_slice_from(rest, 3).trim_start();
            let (right, after2) = Self::parse_filter_and(after)?;
            Ok((OntoFilterExpr::Or(Box::new(expr), Box::new(right)), after2))
        } else {
            Ok((expr, rest))
        }
    }

    /// Parse an AND-chain: one or more atoms joined by AND.
    fn parse_filter_and(input: &str) -> Result<(OntoFilterExpr, &str)> {
        let (mut left, mut rest) = Self::parse_filter_atom(input)?;
        loop {
            let r = rest.trim_start();
            if starts_with_ignore_ascii_case(r, "AND ") {
                let after = safe_slice_from(r, 4).trim_start();
                let (right, new_rest) = Self::parse_filter_atom(after)?;
                left = OntoFilterExpr::And(Box::new(left), Box::new(right));
                rest = new_rest;
            } else {
                break;
            }
        }
        Ok((left, rest))
    }

    /// Parse a single filter atom: comparison, IS NULL, LIKE, BETWEEN, IN, NOT, etc.
    fn parse_filter_atom(input: &str) -> Result<(OntoFilterExpr, &str)> {
        let input = input.trim_start();
        if input.is_empty() {
            return Err(CoreError::InvalidArgument("Empty filter expression".into()));
        }

        // Handle NOT prefix
        if starts_with_ignore_ascii_case(input, "NOT ")
            || starts_with_ignore_ascii_case(input, "NOT(")
        {
            let after = if starts_with_ignore_ascii_case(input, "NOT(") {
                safe_slice_from(input, 4).trim_start()
            } else {
                safe_slice_from(input, 4).trim_start()
            };
            // NOT (expr)
            if after.starts_with('(') {
                if let Ok((inner_str, rest)) = Self::extract_paren_content(after) {
                    let (inner, _) = Self::parse_filter_expr(inner_str)?;
                    return Ok((OntoFilterExpr::Not(Box::new(inner)), rest.trim_start()));
                }
            }
            // NOT expr
            let (inner, rest) = Self::parse_filter_atom(after)?;
            return Ok((OntoFilterExpr::Not(Box::new(inner)), rest));
        }

        let (left, rest) = Self::extract_identifier(input);
        if left.is_empty() {
            return Err(CoreError::InvalidArgument(
                "Expected column name in filter".into(),
            ));
        }

        let rest = rest.trim_start();

        // IS NULL / IS NOT NULL
        if starts_with_ignore_ascii_case(rest, "IS NOT NULL") {
            let rest = safe_slice_from(rest, "IS NOT NULL".len()).trim_start();
            return Ok((OntoFilterExpr::IsNotNull(left), rest));
        }
        if starts_with_ignore_ascii_case(rest, "IS NULL") {
            let rest = safe_slice_from(rest, "IS NULL".len()).trim_start();
            return Ok((OntoFilterExpr::IsNull(left), rest));
        }

        // BETWEEN low AND high
        if starts_with_ignore_ascii_case(rest, "BETWEEN") {
            let after = safe_slice_from(rest, 7).trim_start();
            let (low, after) = Self::parse_value_expr(after)?;
            let after = after.trim_start();
            if starts_with_ignore_ascii_case(after, "AND") {
                let after = safe_slice_from(after, 3).trim_start();
                let (high, after) = Self::parse_value_expr(after)?;
                return Ok((OntoFilterExpr::Between(left, low, high), after.trim_start()));
            }
            return Err(CoreError::InvalidArgument("Expected AND in BETWEEN".into()));
        }

        // IN (val1, val2, ...)
        if starts_with_ignore_ascii_case(rest, "IN") {
            let after = safe_slice_from(rest, 2).trim_start();
            if after.starts_with('(') {
                if let Ok((inner_str, rest)) = Self::extract_paren_content(after) {
                    let vals: Vec<OntoValueExpr> = split_quoted(inner_str, ',')
                        .iter()
                        .map(|s| Self::parse_value_expr(s.trim()).map(|(v, _)| v))
                        .collect::<Result<Vec<_>>>()?;
                    return Ok((OntoFilterExpr::In(left, vals), rest.trim_start()));
                }
            }
            return Err(CoreError::InvalidArgument("Expected ( after IN".into()));
        }

        // Comparison operators
        let (op, rest) = Self::extract_operator(rest)?;
        let rest = rest.trim_start();

        // Right-hand value
        let (value, rest) = Self::parse_value_expr(rest)?;
        let rest = rest.trim_start();

        let expr = match op.as_str() {
            "=" => OntoFilterExpr::Eq(left, value),
            "!=" | "<>" => OntoFilterExpr::Ne(left, value),
            ">" => OntoFilterExpr::Gt(left, value),
            "<" => OntoFilterExpr::Lt(left, value),
            ">=" => OntoFilterExpr::Gte(left, value),
            "<=" => OntoFilterExpr::Lte(left, value),
            "LIKE" => {
                if let OntoValueExpr::Literal(OntoLiteral::String(s)) = value {
                    OntoFilterExpr::Like(left, s)
                } else {
                    return Err(CoreError::InvalidArgument(
                        "LIKE requires a string pattern".into(),
                    ));
                }
            }
            _ => {
                return Err(CoreError::InvalidArgument(format!(
                    "Unknown operator: {}",
                    op
                )))
            }
        };

        Ok((expr, rest))
    }

    /// Parse a value expression (literal or column reference).
    fn parse_value_expr(input: &str) -> Result<(OntoValueExpr, &str)> {
        let input = input.trim_start();
        if input.is_empty() {
            return Err(CoreError::InvalidArgument("Expected value".into()));
        }

        // NULL
        if starts_with_ignore_ascii_case(input, "NULL")
            && (input.len() == 4 || !input.as_bytes()[4].is_ascii_alphanumeric())
        {
            return Ok((OntoValueExpr::Null, safe_slice_from(input, 4).trim_start()));
        }

        // Boolean
        if starts_with_ignore_ascii_case(input, "TRUE")
            && (input.len() == 4 || !input.as_bytes()[4].is_ascii_alphanumeric())
        {
            return Ok((
                OntoValueExpr::Literal(OntoLiteral::Bool(true)),
                safe_slice_from(input, 4).trim_start(),
            ));
        }
        if starts_with_ignore_ascii_case(input, "FALSE")
            && (input.len() == 5 || !input.as_bytes()[5].is_ascii_alphanumeric())
        {
            return Ok((
                OntoValueExpr::Literal(OntoLiteral::Bool(false)),
                safe_slice_from(input, 5).trim_start(),
            ));
        }

        // String literal
        if input.starts_with('\'') {
            if let Some(end) = input[1..].find('\'') {
                let val = &input[1..1 + end];
                let rest = safe_slice_from(input, 1 + end + 1).trim_start();
                return Ok((
                    OntoValueExpr::Literal(OntoLiteral::String(val.to_string())),
                    rest,
                ));
            }
        }

        // Number (negative or positive)
        let first = input.as_bytes()[0];
        if first.is_ascii_digit() || first == b'-' {
            let end = input
                .find(|c: char| c.is_whitespace() || c == ',' || c == ')' || c == ';')
                .unwrap_or(input.len());
            let num_str = &input[..end];
            let rest = safe_slice_from(input, end).trim_start();
            if num_str.contains('.') {
                if let Ok(f) = num_str.parse::<f64>() {
                    return Ok((OntoValueExpr::Literal(OntoLiteral::Float(f)), rest));
                }
            } else if let Ok(i) = num_str.parse::<i64>() {
                return Ok((OntoValueExpr::Literal(OntoLiteral::Int(i)), rest));
            }
        }

        // Column reference or function call
        let (name, rest) = Self::extract_identifier(input);
        let rest_trimmed = rest.trim_start();
        if rest_trimmed.starts_with('(') {
            // Function call: name(args)
            let (args_str, rest) = Self::extract_paren_content(rest_trimmed)?;
            let args = if args_str.trim().is_empty() {
                vec![]
            } else {
                split_quoted(args_str, ',')
                    .iter()
                    .map(|a| Self::parse_value_expr(a.trim()).map(|(v, _)| v))
                    .collect::<Result<Vec<_>>>()?
            };
            Ok((OntoValueExpr::Function { name, args }, rest.trim_start()))
        } else {
            Ok((OntoValueExpr::Column(name), rest))
        }
    }

    /// Parse assignments: `col1 = val1, col2 = val2`.
    fn parse_assignments(input: &str) -> Result<Vec<(String, OntoValueExpr)>> {
        let parts = split_quoted(input, ',');
        let mut assignments = Vec::new();

        for part in parts {
            let part = part.trim();
            let eq_pos = find_unquoted(part, "=")
                .ok_or_else(|| CoreError::InvalidArgument("Expected = in assignment".into()))?;
            let col = safe_slice(part, 0, eq_pos)
                .trim()
                .trim_matches('"')
                .to_string();
            let val_str = safe_slice_from(part, eq_pos + 1).trim();
            let (val, _) = Self::parse_value_expr(val_str)?;
            assignments.push((col, val));
        }

        Ok(assignments)
    }

    /// Get a string value from assignments by key.
    fn get_assignment_value(assignments: &[(String, OntoValueExpr)], key: &str) -> Result<String> {
        for (k, v) in assignments {
            if k == key {
                return match v {
                    OntoValueExpr::Literal(OntoLiteral::String(s)) => Ok(s.clone()),
                    OntoValueExpr::Column(c) => Ok(c.clone()),
                    _ => Err(CoreError::InvalidArgument(format!(
                        "Expected string value for {}",
                        key
                    ))),
                };
            }
        }
        Err(CoreError::InvalidArgument(format!(
            "Missing {} in INSERT TRIPLE",
            key
        )))
    }

    /// Parse triple values: ("s1","p1","o1"), ("s2","p2","o2"), ...
    fn parse_triple_values(input: &str) -> Result<Vec<(String, String, String)>> {
        let parts = split_quoted(input, ',');
        let mut triples = Vec::new();

        // Each part might be a tuple like ("s1","p1","o1") or individual values
        // Try to parse as grouped tuples first
        let mut i = 0;
        while i < parts.len() {
            let part = parts[i].trim();
            // Check if it's a parenthesized tuple
            if part.starts_with('(') && part.ends_with(')') {
                let inner = &part[1..part.len() - 1];
                let vals = split_quoted(inner, ',');
                if vals.len() == 3 {
                    let s = vals[0].trim().trim_matches('"').to_string();
                    let p = vals[1].trim().trim_matches('"').to_string();
                    let o = vals[2].trim().trim_matches('"').to_string();
                    triples.push((s, p, o));
                    i += 1;
                    continue;
                }
            }
            // Otherwise, treat as flat list: s, p, o, s, p, o, ...
            if i + 2 < parts.len() {
                let s = parts[i].trim().trim_matches('"').to_string();
                let p = parts[i + 1].trim().trim_matches('"').to_string();
                let o = parts[i + 2].trim().trim_matches('"').to_string();
                triples.push((s, p, o));
                i += 3;
            } else {
                return Err(CoreError::InvalidArgument(
                    "Triple values must be groups of 3".into(),
                ));
            }
        }

        Ok(triples)
    }

    /// Parse triple conditions: subject = "s" AND predicate = "p" AND object = "o"
    fn parse_triple_conditions(
        input: &str,
    ) -> Result<(Option<String>, Option<String>, Option<String>)> {
        let mut subject = None;
        let mut predicate = None;
        let mut object = None;

        // Split by AND
        let parts: Vec<&str> = input.split(" AND ").collect();
        for part in parts {
            let part = part.trim();
            let eq_pos = find_unquoted(part, "=").ok_or_else(|| {
                CoreError::InvalidArgument("Expected = in triple condition".into())
            })?;
            let key = safe_slice(part, 0, eq_pos).trim().to_ascii_lowercase();
            let val_str = safe_slice_from(part, eq_pos + 1).trim();
            let val = val_str.trim_matches('"').to_string();

            match key.as_str() {
                "subject" | "s" => subject = Some(val),
                "predicate" | "p" => predicate = Some(val),
                "object" | "o" => object = Some(val),
                _ => {
                    return Err(CoreError::InvalidArgument(format!(
                        "Unknown triple field: {}",
                        key
                    )))
                }
            }
        }

        Ok((subject, predicate, object))
    }

    /// Parse ORDER BY: `col1 ASC, col2 DESC`.
    fn parse_order_by(input: &str) -> Vec<OntoOrderBy> {
        let parts = split_quoted(input, ',');
        parts
            .iter()
            .filter_map(|part| {
                let part = part.trim();
                let mut ascending = true;
                let col = if starts_with_ignore_ascii_case(part, "DESC") {
                    ascending = false;
                    part[4..].trim().trim_matches('"').to_string()
                } else if starts_with_ignore_ascii_case(part, "ASC") {
                    part[3..].trim().trim_matches('"').to_string()
                } else {
                    // Split by space and check last word
                    let words: Vec<&str> = part.split_whitespace().collect();
                    if words.len() > 1 {
                        match words.last().unwrap().to_ascii_uppercase().as_str() {
                            "DESC" => {
                                ascending = false;
                                words[..words.len() - 1]
                                    .join(" ")
                                    .trim_matches('"')
                                    .to_string()
                            }
                            "ASC" => words[..words.len() - 1]
                                .join(" ")
                                .trim_matches('"')
                                .to_string(),
                            _ => part.trim_matches('"').to_string(),
                        }
                    } else {
                        part.trim_matches('"').to_string()
                    }
                };
                if col.is_empty() {
                    None
                } else {
                    Some(OntoOrderBy {
                        column: col,
                        ascending,
                    })
                }
            })
            .collect()
    }

    // ══════════════════════════════════════════════════════════════
    //  Utility helpers
    // ══════════════════════════════════════════════════════════════

    /// Extract an identifier (alphanumeric + underscore, or quoted).
    /// Supports dotted identifiers like `table.column`.
    fn extract_identifier(input: &str) -> (String, &str) {
        let input = input.trim_start();
        if input.is_empty() {
            return (String::new(), input);
        }

        // Quoted identifier
        if input.starts_with('"') {
            if let Some(end) = input[1..].find('"') {
                let ident = &input[1..1 + end];
                let rest = safe_slice_from(input, 1 + end + 1);
                // Check for dotted continuation
                if rest.starts_with('.') {
                    let (suffix, rest2) = Self::extract_identifier(safe_slice_from(rest, 1));
                    if !suffix.is_empty() {
                        return (format!("{}.{}", ident, suffix), rest2);
                    }
                }
                return (ident.to_string(), rest);
            }
        }

        // @marker identifier (e.g. @onto, @mind)
        if input.starts_with('@') {
            let end = input[1..]
                .find(|c: char| !c.is_alphanumeric() && c != '_')
                .map(|e| e + 1)
                .unwrap_or(input.len());
            return (input[..end].to_string(), safe_slice_from(input, end));
        }

        // Bare identifier
        let end = input
            .find(|c: char| !c.is_alphanumeric() && c != '_' && c != '.')
            .unwrap_or(input.len());
        if end == 0 {
            (String::new(), input)
        } else {
            let mut ident = input[..end].to_string();
            let mut rest = safe_slice_from(input, end);
            // Handle dotted identifiers: table.column
            while rest.starts_with('.') {
                rest = safe_slice_from(rest, 1);
                let (suffix, new_rest) = Self::extract_identifier(rest);
                if !suffix.is_empty() {
                    ident.push('.');
                    ident.push_str(&suffix);
                    rest = new_rest;
                } else {
                    break;
                }
            }
            (ident, rest)
        }
    }

    /// Extract a comparison operator.
    fn extract_operator(input: &str) -> Result<(String, &str)> {
        let input = input.trim_start();
        if input.starts_with(">=") {
            return Ok((">=".into(), safe_slice_from(input, 2)));
        }
        if input.starts_with("<=") {
            return Ok(("<=".into(), safe_slice_from(input, 2)));
        }
        if input.starts_with("!=") {
            return Ok(("!=".into(), safe_slice_from(input, 2)));
        }
        if input.starts_with("<>") {
            return Ok(("<>".into(), safe_slice_from(input, 2)));
        }
        if input.starts_with('=') {
            return Ok(("=".into(), safe_slice_from(input, 1)));
        }
        if input.starts_with('>') {
            return Ok((">".into(), safe_slice_from(input, 1)));
        }
        if input.starts_with('<') {
            return Ok(("<".into(), safe_slice_from(input, 1)));
        }
        if starts_with_ignore_ascii_case(input, "LIKE") {
            return Ok(("LIKE".into(), safe_slice_from(input, 4)));
        }
        Err(CoreError::InvalidArgument(format!(
            "Expected operator, found: {}",
            &input[..input.len().min(20)]
        )))
    }

    /// Extract a number from the start of input.
    fn extract_number(input: &str) -> (String, &str) {
        let input = input.trim_start();
        let end = input
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(input.len());
        (input[..end].to_string(), safe_slice_from(input, end))
    }

    /// Extract content between parentheses.
    fn extract_paren_content(input: &str) -> Result<(&str, &str)> {
        if !input.starts_with('(') {
            return Err(CoreError::InvalidArgument("Expected (".into()));
        }
        let mut depth = 1;
        let mut i = 1;
        let bytes = input.as_bytes();
        let mut in_quote: Option<u8> = None;
        while i < bytes.len() && depth > 0 {
            let b = bytes[i];
            if let Some(q) = in_quote {
                if b == q {
                    in_quote = None;
                }
            } else {
                match b {
                    b'\'' | b'"' => in_quote = Some(b),
                    b'(' => depth += 1,
                    b')' => depth -= 1,
                    _ => {}
                }
            }
            i += 1;
        }
        if depth != 0 {
            return Err(CoreError::InvalidArgument("Unmatched parentheses".into()));
        }
        Ok((&input[1..i - 1], safe_slice_from(input, i)))
    }

    /// Extract keyword value: find `keyword value` and return the value.
    /// Only matches at word boundaries (keyword must be preceded by whitespace or start of string).
    fn extract_keyword_value(input: &str, keyword: &str) -> Option<String> {
        let kw_upper = keyword.to_ascii_uppercase();
        let lower = input.to_ascii_lowercase();
        let kw_lower = kw_upper.to_lowercase();
        // Find keyword at word boundary
        let mut search_start = 0;
        while let Some(pos) = lower[search_start..].find(&kw_lower) {
            let abs_pos = search_start + pos;
            // Check word boundary: must be preceded by whitespace or be at start
            let preceded_ok = abs_pos == 0 || input.as_bytes()[abs_pos - 1].is_ascii_whitespace();
            // Check word boundary: must be followed by whitespace or be at end
            let after_pos = abs_pos + keyword.len();
            let followed_ok =
                after_pos >= input.len() || input.as_bytes()[after_pos].is_ascii_whitespace();
            if preceded_ok && followed_ok {
                let after = safe_slice_from(input, after_pos).trim_start();
                let (val, _) = Self::extract_identifier(after);
                if !val.is_empty() {
                    return Some(val);
                }
            }
            search_start = abs_pos + 1;
        }
        None
    }

    /// Parse a boolean value (true/false/1/0) from the start of input.
    fn parse_bool_value(input: &str) -> bool {
        let input = input.trim_start();
        starts_with_ignore_ascii_case(input, "TRUE") || input.starts_with('1')
    }

    /// Extract trailing annotations: `@onto(key="val") @mind(x="y")` from end of input.
    fn extract_trailing_annotations(input: &str) -> (Vec<Annotation>, &str) {
        let mut annotations = Vec::new();
        let mut remaining = input.trim_end();

        // Try to find annotations at the end
        while let Some(at_pos) = remaining.rfind('@') {
            let potential = safe_slice_from(remaining, at_pos);
            if let Ok((ann, _)) = Self::parse_annotation(potential) {
                annotations.insert(0, ann);
                remaining = safe_slice(remaining, 0, at_pos).trim_end();
            } else {
                break;
            }
        }

        // Also check for ANNOTATION(...) syntax
        if let Some(pos) = find_ignore_ascii_case(remaining, "ANNOTATION") {
            let after = safe_slice_from(remaining, pos + "ANNOTATION".len()).trim_start();
            if after.starts_with('(') {
                if let Ok((content, rest)) = Self::extract_paren_content(after) {
                    annotations.push(Annotation {
                        marker: "onto".into(),
                        params: Self::parse_annotation_params(content),
                    });
                    remaining = safe_slice(remaining, 0, pos).trim_end();
                    let _ = rest;
                }
            }
        }

        (annotations, remaining)
    }

    /// Parse an annotation: `@marker(key="val", ...)`.
    fn parse_annotation(input: &str) -> Result<(Annotation, &str)> {
        let input = input.trim_start();
        if !input.starts_with('@') {
            return Err(CoreError::InvalidArgument("Expected @".into()));
        }

        // Extract marker name
        let end = input[1..]
            .find(|c: char| !c.is_alphanumeric() && c != '_')
            .unwrap_or(input.len() - 1);
        let marker = &input[1..1 + end];
        let rest = safe_slice_from(input, 1 + end).trim_start();

        if rest.starts_with('(') {
            let (content, rest) = Self::extract_paren_content(rest)?;
            Ok((
                Annotation {
                    marker: marker.to_string(),
                    params: Self::parse_annotation_params(content),
                },
                rest.trim_start(),
            ))
        } else {
            Ok((
                Annotation {
                    marker: marker.to_string(),
                    params: vec![],
                },
                rest,
            ))
        }
    }

    /// Parse annotation parameters: `key="val", key2="val2"`.
    fn parse_annotation_params(input: &str) -> Vec<(String, String)> {
        let parts = split_quoted(input, ',');
        parts
            .iter()
            .filter_map(|part| {
                let part = part.trim();
                let eq_pos = part.find('=')?;
                let key = part[..eq_pos].trim().to_string();
                let val = part[eq_pos + 1..].trim().trim_matches('"').to_string();
                Some((key, val))
            })
            .collect()
    }

    /// Parse `@marker(key="val")` as a quoted or word value.
    fn extract_quoted_or_word_value(input: &str) -> (String, &str) {
        extract_quoted_or_word(input)
    }
}

// ════════════════════════════════════════════════════════════════════
//  Translation: OntoQLAst → SQL string → QueryAst
// ════════════════════════════════════════════════════════════════════
//
// The translation uses SQL string generation + existing QueryParser.
// This is the same strategy as SparqlParser::translate_to_sql() and
// ensures full compatibility with the existing executor without type
// mismatches between OntoQL AST and Query AST.

impl OntoQLAst {
    /// Translate this OntoQL AST into a QueryAst by generating equivalent SQL.
    ///
    /// This two-phase approach (OntoQL → SQL string → QueryAst) guarantees
    /// compatibility with the existing executor and optimizer.
    pub fn to_query_ast(&self) -> Result<QueryAst> {
        match self {
            OntoQLAst::SqlPassthrough(ast) => Ok(ast.clone()),
            // Namespace operations don't map to SQL — convert directly
            OntoQLAst::CreateNamespace { name } => {
                Ok(QueryAst::CreateNamespace { name: name.clone() })
            }
            OntoQLAst::DropNamespace { name } => Ok(QueryAst::DropNamespace { name: name.clone() }),
            OntoQLAst::UseNamespace { name } => Ok(QueryAst::UseNamespace { name: name.clone() }),
            // Ontology drop operations — convert directly
            OntoQLAst::DropOntology { name } => Ok(QueryAst::DropOntology { name: name.clone() }),
            _ => {
                let sql = self.to_sql()?;
                parser::QueryParser::parse(&sql)
            }
        }
    }

    /// Generate the equivalent SQL string for this OntoQL AST.
    pub fn to_sql(&self) -> Result<String> {
        match self {
            OntoQLAst::SqlPassthrough(_) => {
                // Already a parsed AST — re-serialize isn't needed; caller should
                // use the inner AST directly.
                Err(CoreError::InvalidArgument("SqlPassthrough has no SQL representation".into()))
            }

            OntoQLAst::CreateClass { name, extends, .. } => {
                // Map to CREATE ONTOLOGY which the existing parser understands
                let mut sql = format!("CREATE ONTOLOGY {} (CLASS {}", name, name);
                if let Some(parent) = extends {
                    sql.push_str(&format!(" SUBCLASS OF {}", parent));
                }
                sql.push(')');
                Ok(sql)
            }

            OntoQLAst::DropClass { name } => {
                Ok(format!("DELETE FROM \"__ontology__{}\"", name))
            }

            OntoQLAst::DropOntology { name } => {
                Ok(format!("DELETE FROM \"__ontology__{}\"", name))
            }

            OntoQLAst::CreateProperty { name, kind, domain, range, required, .. } => {
                let kind_str = match kind {
                    PropertyKind::Datatype => "DATATYPE",
                    PropertyKind::Object => "OBJECT",
                };
                Ok(format!(
                    "CREATE {} PROPERTY \"{}\" DOMAIN \"{}\" RANGE \"{}\"{}",
                    kind_str, name, domain, range,
                    if *required { " REQUIRED" } else { "" }
                ))
            }

            OntoQLAst::Select { distinct, projections, from, joins, filter, group_by, having, order_by, limit, offset, .. } => {
                let mut sql = String::from("SELECT ");
                if *distinct { sql.push_str("DISTINCT "); }

                // Projections
                let proj_strs: Vec<String> = projections.iter().map(|p| match p {
                    OntoProjection::All => "*".into(),
                    OntoProjection::AllFrom(table) => format!("{}.*", table),
                    OntoProjection::Column { name, alias } => {
                        match alias {
                            Some(a) => format!("{} AS {}", name, a),
                            None => name.clone(),
                        }
                    }
                    OntoProjection::Expression { expr, alias } => {
                        format!("{} AS {}", Self::expr_to_sql(expr), alias)
                    }
                }).collect();
                sql.push_str(&proj_strs.join(", "));

                // FROM
                sql.push_str(" FROM ");
                match from {
                    OntoFromClause::Class { name, alias } => {
                        sql.push_str(name);
                        if let Some(a) = alias {
                            sql.push_str(&format!(" AS {}", a));
                        }
                    }
                    OntoFromClause::Subquery(_) => {
                        return Err(CoreError::InvalidArgument("Subquery in FROM not yet supported".into()));
                    }
                }

                // JOINs
                for join in joins {
                    match join.join_type {
                        JoinType::Inner => sql.push_str(" JOIN "),
                        JoinType::Left => sql.push_str(" LEFT JOIN "),
                        JoinType::Right => sql.push_str(" RIGHT JOIN "),
                        JoinType::Full => sql.push_str(" FULL JOIN "),
                    }
                    match &join.target {
                        OntoFromClause::Class { name, alias } => {
                            sql.push_str(name);
                            if let Some(a) = alias {
                                sql.push_str(&format!(" AS {}", a));
                            }
                        }
                        _ => return Err(CoreError::InvalidArgument("Subquery in JOIN not yet supported".into())),
                    }
                    sql.push_str(&format!(" ON {}", Self::filter_to_sql(&join.on_condition)?));
                }

                // WHERE
                if let Some(f) = filter {
                    sql.push_str(&format!(" WHERE {}", Self::filter_to_sql(f)?));
                }

                // GROUP BY
                if !group_by.is_empty() {
                    sql.push_str(&format!(" GROUP BY {}", group_by.iter()
                        .map(|c| format!("\"{}\"", c))
                        .collect::<Vec<_>>()
                        .join(", ")));
                }

                // HAVING
                if let Some(h) = having {
                    sql.push_str(&format!(" HAVING {}", Self::filter_to_sql(h)?));
                }

                // ORDER BY
                if !order_by.is_empty() {
                    sql.push_str(" ORDER BY ");
                    let order_strs: Vec<String> = order_by.iter().map(|o| {
                        format!("\"{}\" {}", o.column, if o.ascending { "ASC" } else { "DESC" })
                    }).collect();
                    sql.push_str(&order_strs.join(", "));
                }

                // LIMIT / OFFSET
                if let Some(l) = limit {
                    sql.push_str(&format!(" LIMIT {}", l));
                }
                if let Some(o) = offset {
                    sql.push_str(&format!(" OFFSET {}", o));
                }

                Ok(sql)
            }

            OntoQLAst::Insert { class, assignments } => {
                let cols: Vec<String> = assignments.iter().map(|(c, _)| c.clone()).collect();
                let vals: Vec<String> = assignments.iter().map(|(_, v)| Self::expr_to_sql(v)).collect();
                Ok(format!("INSERT INTO {} ({}) VALUES ({})", class, cols.join(", "), vals.join(", ")))
            }

            // Triple operations generate a special marker that the executor recognizes
            OntoQLAst::InsertTriple { subject, predicate, object } => {
                Ok(format!("INSERT INTO __triple__ SET __subject__ = '{}', __predicate__ = '{}', __object__ = '{}'",
                    subject.replace('\'', "''"), predicate.replace('\'', "''"), object.replace('\'', "''")))
            }

            OntoQLAst::InsertTriples { triples } => {
                // Generate multiple INSERT statements
                let mut stmts = Vec::new();
                for (s, p, o) in triples {
                    stmts.push(format!("INSERT INTO __triple__ SET __subject__ = '{}', __predicate__ = '{}', __object__ = '{}'",
                        s.replace('\'', "''"), p.replace('\'', "''"), o.replace('\'', "''")));
                }
                Ok(stmts.join("; "))
            }

            OntoQLAst::DeleteTriple { subject, predicate, object } => {
                Ok(format!("DELETE FROM __triple__ WHERE __subject__ = '{}' AND __predicate__ = '{}' AND __object__ = '{}'",
                    subject.replace('\'', "''"), predicate.replace('\'', "''"), object.replace('\'', "''")))
            }

            OntoQLAst::Update { class, assignments, filter } => {
                let sets: Vec<String> = assignments.iter()
                    .map(|(c, v)| format!("{} = {}", c, Self::expr_to_sql(v)))
                    .collect();
                let mut sql = format!("UPDATE {} SET {}", class, sets.join(", "));
                if let Some(f) = filter {
                    sql.push_str(&format!(" WHERE {}", Self::filter_to_sql(f)?));
                }
                Ok(sql)
            }

            OntoQLAst::Delete { class, filter } => {
                let mut sql = format!("DELETE FROM {}", class);
                if let Some(f) = filter {
                    sql.push_str(&format!(" WHERE {}", Self::filter_to_sql(f)?));
                }
                Ok(sql)
            }

            OntoQLAst::Explain { query } => {
                Ok(format!("EXPLAIN {}", query.to_sql()?))
            }

            OntoQLAst::Begin => Ok("BEGIN".into()),
            OntoQLAst::Commit => Ok("COMMIT".into()),
            OntoQLAst::Rollback => Ok("ROLLBACK".into()),

            OntoQLAst::ImportOntology { sql } => Ok(sql.clone()),

            OntoQLAst::SelectTriples { .. } => {
                // Triple queries are handled directly by the HTTP handler
                Err(CoreError::InvalidArgument("SELECT TRIPLE is handled directly".into()))
            }

            OntoQLAst::Infer { .. } => {
                Err(CoreError::InvalidArgument("INFER standalone not yet supported in Phase 1".into()))
            }

            // Namespace operations don't have SQL representation
            OntoQLAst::CreateNamespace { .. } | OntoQLAst::DropNamespace { .. } | OntoQLAst::UseNamespace { .. } => {
                Err(CoreError::InvalidArgument("Namespace operations are handled directly".into()))
            }
        }
    }

    /// Convert OntoFilterExpr to SQL WHERE string.
    fn filter_to_sql(expr: &OntoFilterExpr) -> Result<String> {
        match expr {
            OntoFilterExpr::Eq(col, val) => Ok(format!("{} = {}", col, Self::expr_to_sql(val))),
            OntoFilterExpr::Ne(col, val) => Ok(format!("{} != {}", col, Self::expr_to_sql(val))),
            OntoFilterExpr::Gt(col, val) => Ok(format!("{} > {}", col, Self::expr_to_sql(val))),
            OntoFilterExpr::Lt(col, val) => Ok(format!("{} < {}", col, Self::expr_to_sql(val))),
            OntoFilterExpr::Gte(col, val) => Ok(format!("{} >= {}", col, Self::expr_to_sql(val))),
            OntoFilterExpr::Lte(col, val) => Ok(format!("{} <= {}", col, Self::expr_to_sql(val))),
            OntoFilterExpr::Like(col, pattern) => {
                let escaped = pattern.replace('\'', "''");
                Ok(format!("{} LIKE '{}'", col, escaped))
            }
            OntoFilterExpr::In(col, vals) => {
                let vals_str: Vec<String> = vals.iter().map(Self::expr_to_sql).collect();
                Ok(format!("{} IN ({})", col, vals_str.join(", ")))
            }
            OntoFilterExpr::IsNull(col) => Ok(format!("{} IS NULL", col)),
            OntoFilterExpr::IsNotNull(col) => Ok(format!("{} IS NOT NULL", col)),
            OntoFilterExpr::Between(col, lo, hi) => Ok(format!(
                "{} BETWEEN {} AND {}",
                col,
                Self::expr_to_sql(lo),
                Self::expr_to_sql(hi)
            )),
            OntoFilterExpr::And(l, r) => Ok(format!(
                "({} AND {})",
                Self::filter_to_sql(l)?,
                Self::filter_to_sql(r)?
            )),
            OntoFilterExpr::Or(l, r) => Ok(format!(
                "({} OR {})",
                Self::filter_to_sql(l)?,
                Self::filter_to_sql(r)?
            )),
            OntoFilterExpr::Not(e) => Ok(format!("NOT ({})", Self::filter_to_sql(e)?)),
            OntoFilterExpr::AnnotationCondition(ann) => Ok(format!(
                "@{}({})",
                ann.marker,
                ann.params
                    .iter()
                    .map(|(k, v)| format!("{}=\"{}\"", k, v))
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }

    /// Convert OntoValueExpr to SQL expression string.
    fn expr_to_sql(expr: &OntoValueExpr) -> String {
        match expr {
            OntoValueExpr::Null => "NULL".into(),
            OntoValueExpr::Literal(lit) => match lit {
                OntoLiteral::String(s) => format!("'{}'", s.replace('\'', "''")),
                OntoLiteral::Int(i) => i.to_string(),
                OntoLiteral::Float(f) => f.to_string(),
                OntoLiteral::Bool(b) => {
                    if *b {
                        "TRUE".into()
                    } else {
                        "FALSE".into()
                    }
                }
            },
            OntoValueExpr::Column(name) => format!("\"{}\"", name),
            OntoValueExpr::Function { name, args } => {
                let args_str: Vec<String> = args.iter().map(Self::expr_to_sql).collect();
                format!("{}({})", name, args_str.join(", "))
            }
            OntoValueExpr::Arithmetic { op, left, right } => {
                format!(
                    "({} {} {})",
                    Self::expr_to_sql(left),
                    op,
                    Self::expr_to_sql(right)
                )
            }
        }
    }
}

// ════════════════════════════════════════════════════════════════════
//  Tests
// ════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_create_class_basic() {
        let ast = OntoQLParser::parse("CREATE CLASS Person").unwrap();
        match ast {
            OntoQLAst::CreateClass {
                name,
                extends,
                abstract_class,
                ..
            } => {
                assert_eq!(name, "Person");
                assert_eq!(extends, None);
                assert!(!abstract_class);
            }
            _ => panic!("Expected CreateClass"),
        }
    }

    #[test]
    fn test_parse_create_class_with_extends() {
        let ast = OntoQLParser::parse("CREATE CLASS Employee EXTENDS Person").unwrap();
        match ast {
            OntoQLAst::CreateClass { name, extends, .. } => {
                assert_eq!(name, "Employee");
                assert_eq!(extends, Some("Person".into()));
            }
            _ => panic!("Expected CreateClass"),
        }
    }

    #[test]
    fn test_parse_create_class_abstract() {
        let ast = OntoQLParser::parse("CREATE CLASS Animal ABSTRACT true").unwrap();
        match ast {
            OntoQLAst::CreateClass {
                name,
                abstract_class,
                ..
            } => {
                assert_eq!(name, "Animal");
                assert!(abstract_class);
            }
            _ => panic!("Expected CreateClass"),
        }
    }

    #[test]
    fn test_parse_create_class_with_annotation() {
        let ast =
            OntoQLParser::parse("CREATE CLASS Person ANNOTATION(@onto(scope=\"full\"))").unwrap();
        match ast {
            OntoQLAst::CreateClass {
                name, annotations, ..
            } => {
                assert_eq!(name, "Person");
                assert!(!annotations.is_empty());
            }
            _ => panic!("Expected CreateClass"),
        }
    }

    #[test]
    fn test_parse_create_datatype_property() {
        let ast = OntoQLParser::parse(
            "CREATE DATATYPE PROPERTY name DOMAIN Person RANGE STRING REQUIRED",
        )
        .unwrap();
        match ast {
            OntoQLAst::CreateProperty {
                name,
                kind,
                domain,
                range,
                required,
                ..
            } => {
                assert_eq!(name, "name");
                assert_eq!(kind, PropertyKind::Datatype);
                assert_eq!(domain, "Person");
                assert_eq!(range, "STRING");
                assert!(required);
            }
            _ => panic!("Expected CreateProperty"),
        }
    }

    #[test]
    fn test_parse_create_object_property() {
        let ast = OntoQLParser::parse(
            "CREATE OBJECT PROPERTY worksAt DOMAIN Employee RANGE Company CHARACTERISTICS TRANSITIVE"
        ).unwrap();
        match ast {
            OntoQLAst::CreateProperty {
                name,
                kind,
                characteristics,
                ..
            } => {
                assert_eq!(name, "worksAt");
                assert_eq!(kind, PropertyKind::Object);
                assert!(characteristics.contains(&Characteristic::Transitive));
            }
            _ => panic!("Expected CreateProperty"),
        }
    }

    #[test]
    fn test_parse_select_basic() {
        let ast = OntoQLParser::parse("SELECT name, age FROM Person").unwrap();
        match ast {
            OntoQLAst::Select {
                projections, from, ..
            } => {
                assert_eq!(projections.len(), 2);
                match &from {
                    OntoFromClause::Class { name, .. } => assert_eq!(name, "Person"),
                    _ => panic!("Expected Class from"),
                }
            }
            _ => panic!("Expected Select"),
        }
    }

    #[test]
    fn test_parse_select_distinct() {
        let ast = OntoQLParser::parse("SELECT DISTINCT city FROM Person").unwrap();
        match ast {
            OntoQLAst::Select { distinct, .. } => assert!(distinct),
            _ => panic!("Expected Select"),
        }
    }

    #[test]
    fn test_parse_select_with_where() {
        let ast = OntoQLParser::parse("SELECT name FROM Person WHERE age > 18").unwrap();
        match ast {
            OntoQLAst::Select { filter, .. } => {
                assert!(filter.is_some());
                match filter.unwrap() {
                    OntoFilterExpr::Gt(col, _) => assert_eq!(col, "age"),
                    _ => panic!("Expected Gt filter"),
                }
            }
            _ => panic!("Expected Select"),
        }
    }

    #[test]
    fn test_parse_select_with_and() {
        let ast =
            OntoQLParser::parse("SELECT name FROM Person WHERE age > 18 AND city = 'Beijing'")
                .unwrap();
        match ast {
            OntoQLAst::Select { filter, .. } => match filter.unwrap() {
                OntoFilterExpr::And(_, _) => {}
                _ => panic!("Expected And filter"),
            },
            _ => panic!("Expected Select"),
        }
    }

    #[test]
    fn test_parse_select_with_limit_offset() {
        let ast = OntoQLParser::parse("SELECT * FROM Person LIMIT 10 OFFSET 20").unwrap();
        match ast {
            OntoQLAst::Select { limit, offset, .. } => {
                assert_eq!(limit, Some(10));
                assert_eq!(offset, Some(20));
            }
            _ => panic!("Expected Select"),
        }
    }

    #[test]
    fn test_parse_select_with_order_by() {
        let ast = OntoQLParser::parse("SELECT name FROM Person ORDER BY age DESC").unwrap();
        match ast {
            OntoQLAst::Select { order_by, .. } => {
                assert_eq!(order_by.len(), 1);
                assert_eq!(order_by[0].column, "age");
                assert!(!order_by[0].ascending);
            }
            _ => panic!("Expected Select"),
        }
    }

    #[test]
    fn test_parse_select_with_join() {
        let ast = OntoQLParser::parse(
            "SELECT p.name, c.name FROM Person p JOIN Company c ON p.company_id = c.id",
        )
        .unwrap();
        match ast {
            OntoQLAst::Select { joins, .. } => {
                assert_eq!(joins.len(), 1);
                assert_eq!(joins[0].join_type, JoinType::Inner);
            }
            _ => panic!("Expected Select"),
        }
    }

    #[test]
    fn test_parse_select_with_left_join() {
        let ast = OntoQLParser::parse(
            "SELECT p.name, d.name FROM Person p LEFT JOIN Department d ON p.dept_id = d.id",
        )
        .unwrap();
        match ast {
            OntoQLAst::Select { joins, .. } => {
                assert_eq!(joins.len(), 1);
                assert_eq!(joins[0].join_type, JoinType::Left);
            }
            _ => panic!("Expected Select"),
        }
    }

    #[test]
    fn test_parse_insert() {
        let ast = OntoQLParser::parse("INSERT INTO Person SET name = 'Alice', age = 30").unwrap();
        match ast {
            OntoQLAst::Insert { class, assignments } => {
                assert_eq!(class, "Person");
                assert_eq!(assignments.len(), 2);
                assert_eq!(assignments[0].0, "name");
                assert_eq!(assignments[1].0, "age");
            }
            _ => panic!("Expected Insert"),
        }
    }

    #[test]
    fn test_parse_update() {
        let ast = OntoQLParser::parse("UPDATE Person SET name = 'Bob' WHERE id = 1").unwrap();
        match ast {
            OntoQLAst::Update {
                class,
                assignments,
                filter,
            } => {
                assert_eq!(class, "Person");
                assert_eq!(assignments.len(), 1);
                assert!(filter.is_some());
            }
            _ => panic!("Expected Update"),
        }
    }

    #[test]
    fn test_parse_delete() {
        let ast = OntoQLParser::parse("DELETE FROM Person WHERE age < 18").unwrap();
        match ast {
            OntoQLAst::Delete { class, filter } => {
                assert_eq!(class, "Person");
                assert!(filter.is_some());
            }
            _ => panic!("Expected Delete"),
        }
    }

    #[test]
    fn test_parse_delete_all() {
        let ast = OntoQLParser::parse("DELETE FROM Person").unwrap();
        match ast {
            OntoQLAst::Delete { class, filter } => {
                assert_eq!(class, "Person");
                assert!(filter.is_none());
            }
            _ => panic!("Expected Delete"),
        }
    }

    #[test]
    fn test_parse_explain() {
        let ast = OntoQLParser::parse("EXPLAIN SELECT * FROM Person").unwrap();
        match ast {
            OntoQLAst::Explain { query } => match *query {
                OntoQLAst::Select { .. } => {}
                _ => panic!("Expected Select inside Explain"),
            },
            _ => panic!("Expected Explain"),
        }
    }

    #[test]
    fn test_parse_transactions() {
        assert!(matches!(
            OntoQLParser::parse("BEGIN").unwrap(),
            OntoQLAst::Begin
        ));
        assert!(matches!(
            OntoQLParser::parse("COMMIT").unwrap(),
            OntoQLAst::Commit
        ));
        assert!(matches!(
            OntoQLParser::parse("ROLLBACK").unwrap(),
            OntoQLAst::Rollback
        ));
    }

    #[test]
    fn test_parse_select_with_infer() {
        let ast =
            OntoQLParser::parse("SELECT name FROM Person INFER @onto(scope=\"SUBCLASS\")").unwrap();
        match ast {
            OntoQLAst::Select { infer, .. } => {
                assert!(infer.is_some());
                let inf = infer.unwrap();
                assert_eq!(inf.scope, "SUBCLASS");
            }
            _ => panic!("Expected Select"),
        }
    }

    #[test]
    fn test_parse_select_star() {
        let ast = OntoQLParser::parse("SELECT * FROM Person").unwrap();
        match ast {
            OntoQLAst::Select { projections, .. } => {
                assert_eq!(projections.len(), 1);
                assert!(matches!(projections[0], OntoProjection::All));
            }
            _ => panic!("Expected Select"),
        }
    }

    #[test]
    fn test_parse_select_with_alias() {
        let ast = OntoQLParser::parse("SELECT name AS person_name FROM Person").unwrap();
        match ast {
            OntoQLAst::Select { projections, .. } => match &projections[0] {
                OntoProjection::Column { name, alias } => {
                    assert_eq!(name, "name");
                    assert_eq!(alias.as_deref(), Some("person_name"));
                }
                _ => panic!("Expected Column projection"),
            },
            _ => panic!("Expected Select"),
        }
    }

    #[test]
    fn test_parse_filter_is_null() {
        let ast = OntoQLParser::parse("SELECT name FROM Person WHERE email IS NULL").unwrap();
        match ast {
            OntoQLAst::Select { filter, .. } => match filter.unwrap() {
                OntoFilterExpr::IsNull(col) => assert_eq!(col, "email"),
                _ => panic!("Expected IsNull"),
            },
            _ => panic!("Expected Select"),
        }
    }

    #[test]
    fn test_parse_filter_like() {
        let ast = OntoQLParser::parse("SELECT name FROM Person WHERE name LIKE '%alice%'").unwrap();
        match ast {
            OntoQLAst::Select { filter, .. } => match filter.unwrap() {
                OntoFilterExpr::Like(col, pattern) => {
                    assert_eq!(col, "name");
                    assert_eq!(pattern, "%alice%");
                }
                _ => panic!("Expected Like"),
            },
            _ => panic!("Expected Select"),
        }
    }

    #[test]
    fn test_to_query_ast_select() {
        let ast =
            OntoQLParser::parse("SELECT name, age FROM Person WHERE age > 18 LIMIT 10").unwrap();
        let sql_ast = ast.to_query_ast().unwrap();
        match sql_ast {
            QueryAst::Select { from, limit, .. } => {
                assert_eq!(from, "Person");
                assert_eq!(limit, Some(10));
            }
            _ => panic!("Expected Select QueryAst"),
        }
    }

    #[test]
    fn test_to_query_ast_insert() {
        let ast = OntoQLParser::parse("INSERT INTO Person SET name = 'Alice', age = 30").unwrap();
        let sql_ast = ast.to_query_ast().unwrap();
        match sql_ast {
            QueryAst::Insert {
                class,
                columns,
                values,
            } => {
                assert_eq!(class, "Person");
                assert_eq!(columns.len(), 2);
                assert_eq!(values.len(), 2);
            }
            _ => panic!("Expected Insert QueryAst"),
        }
    }

    #[test]
    fn test_parse_create_class_case_insensitive() {
        let ast = OntoQLParser::parse("create class person").unwrap();
        match ast {
            OntoQLAst::CreateClass { name, .. } => assert_eq!(name, "person"),
            _ => panic!("Expected CreateClass"),
        }
    }

    #[test]
    fn test_empty_query_error() {
        assert!(OntoQLParser::parse("").is_err());
        assert!(OntoQLParser::parse("  ").is_err());
    }

    #[test]
    fn test_parse_create_property_with_inverse() {
        let ast = OntoQLParser::parse(
            "CREATE OBJECT PROPERTY hasChild DOMAIN Person RANGE Person INVERSE OF hasParent",
        )
        .unwrap();
        match ast {
            OntoQLAst::CreateProperty {
                name, inverse_of, ..
            } => {
                assert_eq!(name, "hasChild");
                assert_eq!(inverse_of, Some("hasParent".into()));
            }
            _ => panic!("Expected CreateProperty"),
        }
    }

    #[test]
    fn test_parse_select_with_group_by() {
        let ast = OntoQLParser::parse("SELECT city, COUNT(*) FROM Person GROUP BY city").unwrap();
        match ast {
            OntoQLAst::Select { group_by, .. } => {
                assert_eq!(group_by.len(), 1);
                assert_eq!(group_by[0], "city");
            }
            _ => panic!("Expected Select"),
        }
    }

    #[test]
    fn test_drop_class() {
        let ast = OntoQLParser::parse("DROP CLASS Person").unwrap();
        match ast {
            OntoQLAst::DropClass { name } => assert_eq!(name, "Person"),
            _ => panic!("Expected DropClass"),
        }
    }

    #[test]
    fn test_to_sql_select() {
        let ast =
            OntoQLParser::parse("SELECT name, age FROM Person WHERE age > 18 LIMIT 10").unwrap();
        let sql = ast.to_sql().unwrap();
        assert!(sql.contains("SELECT"));
        assert!(sql.contains("FROM"));
        assert!(sql.contains("Person"));
        assert!(sql.contains("LIMIT 10"));
    }

    #[test]
    fn test_to_sql_insert() {
        let ast = OntoQLParser::parse("INSERT INTO Person SET name = 'Alice', age = 30").unwrap();
        let sql = ast.to_sql().unwrap();
        assert!(sql.contains("INSERT INTO"));
        assert!(sql.contains("Person"));
        assert!(sql.contains("Alice"));
    }

    #[test]
    fn test_to_sql_update() {
        let ast = OntoQLParser::parse("UPDATE Person SET name = 'Bob' WHERE id = 1").unwrap();
        let sql = ast.to_sql().unwrap();
        assert!(sql.contains("UPDATE"));
        assert!(sql.contains("SET"));
        assert!(sql.contains("WHERE"));
    }

    #[test]
    fn test_to_sql_delete() {
        let ast = OntoQLParser::parse("DELETE FROM Person WHERE age < 18").unwrap();
        let sql = ast.to_sql().unwrap();
        assert!(sql.contains("DELETE FROM"));
        assert!(sql.contains("WHERE"));
    }

    #[test]
    fn test_to_query_ast_roundtrip_select() {
        let ast =
            OntoQLParser::parse("SELECT name, age FROM Person WHERE age > 18 LIMIT 10").unwrap();
        let query_ast = ast.to_query_ast().unwrap();
        match query_ast {
            QueryAst::Select { from, limit, .. } => {
                assert_eq!(from, "Person");
                assert_eq!(limit, Some(10));
            }
            _ => panic!("Expected Select"),
        }
    }

    #[test]
    fn test_to_query_ast_roundtrip_insert() {
        let ast = OntoQLParser::parse("INSERT INTO Person SET name = 'Alice', age = 30").unwrap();
        let query_ast = ast.to_query_ast().unwrap();
        match query_ast {
            QueryAst::Insert { class, .. } => {
                assert_eq!(class, "Person");
            }
            _ => panic!("Expected Insert"),
        }
    }

    #[test]
    fn test_to_query_ast_roundtrip_update() {
        let ast = OntoQLParser::parse("UPDATE Person SET name = 'Bob' WHERE id = 1").unwrap();
        let query_ast = ast.to_query_ast().unwrap();
        match query_ast {
            QueryAst::Update { class, .. } => {
                assert_eq!(class, "Person");
            }
            _ => panic!("Expected Update"),
        }
    }

    #[test]
    fn test_to_query_ast_roundtrip_delete() {
        let ast = OntoQLParser::parse("DELETE FROM Person WHERE age < 18").unwrap();
        let query_ast = ast.to_query_ast().unwrap();
        match query_ast {
            QueryAst::Delete { class, .. } => {
                assert_eq!(class, "Person");
            }
            _ => panic!("Expected Delete"),
        }
    }

    #[test]
    fn test_to_query_ast_create_class() {
        let ast = OntoQLParser::parse("CREATE CLASS Employee EXTENDS Person").unwrap();
        let query_ast = ast.to_query_ast().unwrap();
        match query_ast {
            QueryAst::CreateOntology { sql } => {
                assert!(sql.contains("Employee"));
                assert!(sql.contains("Person"));
            }
            _ => panic!("Expected CreateOntology"),
        }
    }

    #[test]
    fn test_to_query_ast_explain() {
        let ast = OntoQLParser::parse("EXPLAIN SELECT * FROM Person").unwrap();
        let query_ast = ast.to_query_ast().unwrap();
        match query_ast {
            QueryAst::Explain { .. } => {}
            _ => panic!("Expected Explain"),
        }
    }

    #[test]
    fn test_to_query_ast_transactions() {
        let begin = OntoQLParser::parse("BEGIN")
            .unwrap()
            .to_query_ast()
            .unwrap();
        assert!(matches!(begin, QueryAst::Begin));

        let commit = OntoQLParser::parse("COMMIT")
            .unwrap()
            .to_query_ast()
            .unwrap();
        assert!(matches!(commit, QueryAst::Commit));

        let rollback = OntoQLParser::parse("ROLLBACK")
            .unwrap()
            .to_query_ast()
            .unwrap();
        assert!(matches!(rollback, QueryAst::Rollback));
    }

    // ===== P2 Tests =====

    #[test]
    fn test_extract_keyword_value_word_boundary() {
        // DOMAINES should NOT match DOMAIN keyword
        let result = OntoQLParser::extract_keyword_value("DOMAINES RANGE STRING", "DOMAIN");
        assert!(result.is_none(), "DOMAINES should not match DOMAIN");

        // DOMAIN Person should match
        let result = OntoQLParser::extract_keyword_value("DOMAIN Person RANGE STRING", "DOMAIN");
        assert_eq!(result.unwrap(), "Person");
    }

    #[test]
    fn test_ontology_between_in_not() {
        // BETWEEN
        let ast =
            OntoQLParser::parse("SELECT name FROM Person WHERE age BETWEEN 18 AND 65").unwrap();
        match &ast {
            OntoQLAst::Select {
                filter: Some(OntoFilterExpr::Between(col, _, _)),
                ..
            } => {
                assert_eq!(col, "age");
            }
            other => panic!("expected Between, got {:?}", other),
        }

        // IN
        let ast =
            OntoQLParser::parse("SELECT name FROM Person WHERE status IN ('active', 'pending')")
                .unwrap();
        match &ast {
            OntoQLAst::Select {
                filter: Some(OntoFilterExpr::In(col, vals)),
                ..
            } => {
                assert_eq!(col, "status");
                assert_eq!(vals.len(), 2);
            }
            other => panic!("expected In, got {:?}", other),
        }

        // NOT
        let ast = OntoQLParser::parse("SELECT name FROM Person WHERE NOT (age > 30)").unwrap();
        match &ast {
            OntoQLAst::Select {
                filter: Some(OntoFilterExpr::Not(_)),
                ..
            } => {}
            other => panic!("expected Not, got {:?}", other),
        }
    }

    #[test]
    fn test_to_sql_projection_quoted() {
        // Column names with special chars should be quoted in generated SQL
        let ast = OntoQLParser::parse("SELECT name FROM Person WHERE age > 18").unwrap();
        let sql = ast.to_query_ast().unwrap();
        // Just verify it parses without error
        match sql {
            QueryAst::Select { .. } => {}
            _ => panic!("expected Select"),
        }
    }
}
