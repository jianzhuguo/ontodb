//! SPARQL query parser and translator for OntoDB.
//!
//! Supports a subset of SPARQL 1.1:
//! - SELECT / SELECT DISTINCT with variables (?x, ?y) or *
//! - WHERE with triple patterns
//! - FILTER with comparisons, regex, bound, EXISTS, NOT EXISTS, AND/OR/NOT
//! - OPTIONAL (document-model NULL semantics)
//! - UNION (translated to SQL UNION ALL)
//! - ORDER BY, LIMIT, OFFSET
//! - CONSTRUCT, ASK
//!
//! Translates SPARQL queries to SQL for execution against the OntoDB document engine.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A parsed SPARQL query.
#[derive(Debug, Clone)]
pub struct SparqlQuery {
    /// SELECT clause: list of variables or * for all.
    pub select: SparqlSelect,
    /// WHERE clause: list of triple patterns.
    pub where_patterns: Vec<TriplePattern>,
    /// Optional FILTER expressions.
    pub filters: Vec<SparqlFilter>,
    /// OPTIONAL blocks (left join).
    pub optional: Vec<OptionalBlock>,
    /// UNION alternatives.
    pub unions: Vec<Vec<TriplePattern>>,
    /// Whether SELECT DISTINCT is used.
    pub distinct: bool,
    /// Optional ORDER BY.
    pub order_by: Option<SparqlOrderBy>,
    /// Optional LIMIT.
    pub limit: Option<usize>,
    /// Optional OFFSET.
    pub offset: Option<usize>,
    /// Whether this is a CONSTRUCT query.
    pub construct: Option<Vec<TriplePattern>>,
}

/// An OPTIONAL block in SPARQL (translates to LEFT JOIN).
#[derive(Debug, Clone)]
pub struct OptionalBlock {
    pub patterns: Vec<TriplePattern>,
    pub filters: Vec<SparqlFilter>,
}

/// SELECT clause.
#[derive(Debug, Clone)]
pub enum SparqlSelect {
    /// SELECT * - all variables
    All,
    /// SELECT ?x ?y - specific variables
    Variables(Vec<String>),
}

/// A triple pattern: ?s ?p ?o with any position being a variable, IRI, or literal.
#[derive(Debug, Clone)]
pub struct TriplePattern {
    pub subject: PatternTerm,
    pub predicate: PatternTerm,
    pub object: PatternTerm,
}

/// A term in a triple pattern.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PatternTerm {
    /// A variable like ?x
    Variable(String),
    /// An IRI like <http://example.org/Person> or prefixed name like ex:Person
    Iri(String),
    /// A literal value
    Literal(String),
    /// A blank node
    BlankNode(String),
}

/// A FILTER expression.
#[derive(Debug, Clone)]
pub enum SparqlFilter {
    /// ?x = "value"
    Eq(String, PatternTerm),
    /// ?x != "value"
    Ne(String, PatternTerm),
    /// ?x > value
    Gt(String, PatternTerm),
    /// ?x < value
    Lt(String, PatternTerm),
    /// ?x >= value
    Gte(String, PatternTerm),
    /// ?x <= value
    Lte(String, PatternTerm),
    /// regex(?x, "pattern")
    Regex(String, String),
    /// bound(?x)
    Bound(String),
    /// !expr
    Not(Box<SparqlFilter>),
    /// expr1 && expr2
    And(Box<SparqlFilter>, Box<SparqlFilter>),
    /// expr1 || expr2
    Or(Box<SparqlFilter>, Box<SparqlFilter>),
    /// EXISTS { ... }
    Exists(Vec<TriplePattern>),
    /// NOT EXISTS { ... }
    NotExists(Vec<TriplePattern>),
}

/// ORDER BY clause.
#[derive(Debug, Clone)]
pub struct SparqlOrderBy {
    pub terms: Vec<OrderByTerm>,
}

/// A single ORDER BY term.
#[derive(Debug, Clone)]
pub struct OrderByTerm {
    pub variable: String,
    pub ascending: bool,
}

/// SPARQL query result in JSON format (W3C SPARQL Results JSON Format).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SparqlResult {
    pub head: SparqlHead,
    pub results: SparqlResultSet,
}

/// Head of SPARQL result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SparqlHead {
    pub vars: Vec<String>,
}

/// Result set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SparqlResultSet {
    pub bindings: Vec<HashMap<String, SparqlTerm>>,
}

/// A term in a SPARQL result binding.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SparqlTerm {
    #[serde(rename = "type")]
    pub term_type: String,
    pub value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub datatype: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "xml:lang")]
    pub language: Option<String>,
}

/// A segment of a WHERE clause body.
enum WhereSegment {
    /// Main triple patterns.
    Main(String),
    /// OPTIONAL block body.
    Optional(String),
    /// UNION block bodies (alternatives).
    Union(Vec<String>),
}

/// SPARQL parser.
pub struct SparqlParser {
    prefixes: HashMap<String, String>,
}

impl SparqlParser {
    /// Creates a new SPARQL parser.
    pub fn new() -> Self {
        Self {
            prefixes: HashMap::new(),
        }
    }

    /// Parses a SPARQL query string.
    pub fn parse(&mut self, input: &str) -> Result<SparqlQuery, String> {
        let input = input.trim();

        // Parse PREFIX declarations
        let mut remaining = input.to_string();
        while remaining.to_uppercase().starts_with("PREFIX") {
            let end = remaining.find('\n').unwrap_or(remaining.len());
            let prefix_line = &remaining[..end];
            self.parse_prefix(prefix_line)?;
            remaining = remaining[end..].trim().to_string();
        }

        // Determine query type
        let upper = remaining.to_uppercase();
        if upper.starts_with("SELECT") {
            self.parse_select(&remaining)
        } else if upper.starts_with("CONSTRUCT") {
            self.parse_construct(&remaining)
        } else if upper.starts_with("ASK") {
            self.parse_ask(&remaining)
        } else {
            Err(format!("Unsupported SPARQL query type: {}", remaining.split_whitespace().next().unwrap_or("")))
        }
    }

    /// Parses a PREFIX declaration.
    fn parse_prefix(&mut self, line: &str) -> Result<(), String> {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 3 && parts[0].to_uppercase() == "PREFIX" {
            let prefix = parts[1].trim_end_matches(':');
            let iri = parts[2].trim_start_matches('<').trim_end_matches('>');
            self.prefixes.insert(prefix.to_string(), iri.to_string());
        }
        Ok(())
    }

    /// Parses a SELECT query.
    fn parse_select(&self, input: &str) -> Result<SparqlQuery, String> {
        let input = input.trim();

        // Find WHERE clause
        let where_pos = input.to_uppercase().find("WHERE")
            .ok_or("Missing WHERE clause")?;

        let select_part = input[..where_pos].trim();
        let where_part = input[where_pos..].trim();

        // Parse SELECT clause
        let (select, select_distinct) = self.parse_select_clause(select_part)?;

        // Parse WHERE clause
        let (patterns, filters, optional, unions, remaining) = self.parse_where_clause(where_part)?;

        // Parse optional clauses (ORDER BY, LIMIT, OFFSET)
        let mut order_by = None;
        let mut limit = None;
        let mut offset = None;

        let mut remaining = remaining.trim().to_string();
        while !remaining.is_empty() {
            let upper = remaining.to_uppercase();
            if upper.starts_with("ORDER BY") {
                let (ob, rest) = self.parse_order_by(&remaining)?;
                order_by = Some(ob);
                remaining = rest.trim().to_string();
            } else if upper.starts_with("LIMIT") {
                let (lim, rest) = self.parse_limit(&remaining)?;
                limit = Some(lim);
                remaining = rest.trim().to_string();
            } else if upper.starts_with("OFFSET") {
                let (off, rest) = self.parse_offset(&remaining)?;
                offset = Some(off);
                remaining = rest.trim().to_string();
            } else {
                break;
            }
        }

        Ok(SparqlQuery {
            select,
            where_patterns: patterns,
            filters,
            optional,
            unions,
            distinct: select_distinct,
            order_by,
            limit,
            offset,
            construct: None,
        })
    }

    /// Parses a CONSTRUCT query.
    fn parse_construct(&self, input: &str) -> Result<SparqlQuery, String> {
        let input = input.trim();

        // Find WHERE clause
        let where_pos = input.to_uppercase().find("WHERE")
            .ok_or("Missing WHERE clause in CONSTRUCT")?;

        let construct_part = input[..where_pos].trim();
        let where_part = input[where_pos..].trim();

        // Parse CONSTRUCT template (simplified: just parse triple patterns in { })
        let construct_start = construct_part.find('{').ok_or("Missing { in CONSTRUCT")?;
        let construct_end = construct_part.rfind('}').ok_or("Missing } in CONSTRUCT")?;
        let construct_body = &construct_part[construct_start+1..construct_end];
        let construct_patterns = self.parse_triple_patterns(construct_body)?;

        // Parse WHERE clause
        let (patterns, filters, _optional, _unions, _remaining) = self.parse_where_clause(where_part)?;

        Ok(SparqlQuery {
            select: SparqlSelect::All,
            where_patterns: patterns,
            filters,
            optional: Vec::new(),
            unions: Vec::new(),
            distinct: false,
            order_by: None,
            limit: None,
            offset: None,
            construct: Some(construct_patterns),
        })
    }

    /// Parses an ASK query.
    fn parse_ask(&self, input: &str) -> Result<SparqlQuery, String> {
        let input = input.trim();

        // Find WHERE clause
        let where_pos = input.to_uppercase().find("WHERE")
            .ok_or("Missing WHERE clause in ASK")?;

        let where_part = input[where_pos..].trim();
        let (patterns, filters, _optional, _unions, _) = self.parse_where_clause(where_part)?;

        Ok(SparqlQuery {
            select: SparqlSelect::Variables(vec!["ask".to_string()]),
            where_patterns: patterns,
            filters,
            optional: Vec::new(),
            unions: Vec::new(),
            distinct: false,
            order_by: None,
            limit: Some(1),
            offset: None,
            construct: None,
        })
    }

    /// Parses SELECT clause. Returns (select, distinct).
    fn parse_select_clause(&self, input: &str) -> Result<(SparqlSelect, bool), String> {
        let input = input.trim();
        let upper = input.to_uppercase();

        if !upper.starts_with("SELECT") {
            return Err("Missing SELECT keyword".to_string());
        }

        let after_select = input[6..].trim();

        if after_select == "*" || after_select.to_uppercase().starts_with("*") {
            return Ok((SparqlSelect::All, false));
        }

        // Check for DISTINCT
        let (after_select, distinct) = if after_select.to_uppercase().starts_with("DISTINCT") {
            (after_select[8..].trim(), true)
        } else {
            (after_select, false)
        };

        let mut variables = Vec::new();
        for token in after_select.split_whitespace() {
            let token = token.trim();
            if token.starts_with('?') || token.starts_with('$') {
                variables.push(token[1..].to_string());
            } else if token.to_uppercase() == "WHERE" {
                break;
            }
        }

        if variables.is_empty() {
            return Err("No variables in SELECT clause".to_string());
        }

        Ok((SparqlSelect::Variables(variables), distinct))
    }

    /// Parses WHERE clause with triple patterns and FILTERs.
    fn parse_where_clause(&self, input: &str) -> Result<(Vec<TriplePattern>, Vec<SparqlFilter>, Vec<OptionalBlock>, Vec<Vec<TriplePattern>>, String), String> {
        let input = input.trim();

        // Find the opening {
        let start = input.find('{').ok_or("Missing { in WHERE clause")?;
        let end = self.find_matching_brace(&input[start..]).ok_or("Missing } in WHERE clause")?;
        let body = &input[start+1..start+end];

        let (main_patterns, filters, optional_blocks, union_blocks) = self.parse_where_body(body)?;

        let remaining = input[start+end+1..].to_string();

        Ok((main_patterns, filters, optional_blocks, union_blocks, remaining))
    }

    /// Parses the body of a WHERE clause, handling OPTIONAL and UNION blocks.
    fn parse_where_body(&self, body: &str) -> Result<(Vec<TriplePattern>, Vec<SparqlFilter>, Vec<OptionalBlock>, Vec<Vec<TriplePattern>>), String> {
        let mut main_patterns = Vec::new();
        let mut filters = Vec::new();
        let mut optional_blocks = Vec::new();
        let mut union_blocks = Vec::new();

        // Split body into segments: main patterns, OPTIONAL blocks, UNION blocks
        let segments = self.split_where_segments(body);

        for segment in &segments {
            match segment {
                WhereSegment::Main(body) => {
                    let pats = self.parse_triple_patterns(body)?;
                    let filts = self.parse_filters(body)?;
                    main_patterns.extend(pats);
                    filters.extend(filts);
                }
                WhereSegment::Optional(body) => {
                    let pats = self.parse_triple_patterns(body)?;
                    let filts = self.parse_filters(body)?;
                    optional_blocks.push(OptionalBlock { patterns: pats, filters: filts });
                }
                WhereSegment::Union(bodies) => {
                    for body in bodies {
                        let pats = self.parse_triple_patterns(body)?;
                        union_blocks.push(pats);
                    }
                }
            }
        }

        Ok((main_patterns, filters, optional_blocks, union_blocks))
    }

    /// Splits WHERE body into segments (main, OPTIONAL, UNION).
    fn split_where_segments(&self, body: &str) -> Vec<WhereSegment> {
        let mut segments = Vec::new();
        let mut current_main = String::new();
        let chars: Vec<char> = body.chars().collect();
        let mut i = 0;

        while i < chars.len() {
            // Check for OPTIONAL keyword
            if i + 8 <= chars.len() {
                let word: String = chars[i..i+8].iter().collect();
                if word.to_uppercase() == "OPTIONAL" {
                    // Flush current main
                    let trimmed = current_main.trim().to_string();
                    if !trimmed.is_empty() {
                        segments.push(WhereSegment::Main(trimmed));
                    }
                    current_main.clear();

                    // Find the { } block after OPTIONAL
                    let after = &chars[i+8..];
                    let mut j = 0;
                    while j < after.len() && after[j] != '{' { j += 1; }
                    if j < after.len() {
                        let block_start = i + 8 + j;
                        let mut depth = 0;
                        let mut k = block_start;
                        while k < chars.len() {
                            if chars[k] == '{' { depth += 1; }
                            if chars[k] == '}' { depth -= 1; if depth == 0 { break; } }
                            k += 1;
                        }
                        let block_body: String = chars[block_start+1..k].iter().collect();
                        segments.push(WhereSegment::Optional(block_body));
                        i = k + 1;
                        continue;
                    }
                }
            }

            // Check for '{' at top level — could be a UNION block
            if chars[i] == '{' {
                // Find matching '}'
                let mut depth = 0;
                let mut k = i;
                while k < chars.len() {
                    if chars[k] == '{' { depth += 1; }
                    if chars[k] == '}' { depth -= 1; if depth == 0 { break; } }
                    k += 1;
                }
                if k < chars.len() {
                    // Check what comes after '}' — is it UNION?
                    let mut after = k + 1;
                    while after < chars.len() && chars[after].is_whitespace() { after += 1; }
                    if after + 5 <= chars.len() {
                        let next_word: String = chars[after..after+5].iter().collect();
                        if next_word.to_uppercase() == "UNION" {
                            // This is a UNION! Collect all alternatives.
                            let trimmed = current_main.trim().to_string();
                            if !trimmed.is_empty() {
                                segments.push(WhereSegment::Main(trimmed));
                            }
                            current_main.clear();

                            let mut alternatives = Vec::new();
                            // First alternative: content between { and }
                            let block_body: String = chars[i+1..k].iter().collect();
                            alternatives.push(block_body);
                            i = after + 5;

                            // Collect more UNION alternatives
                            loop {
                                while i < chars.len() && chars[i].is_whitespace() { i += 1; }
                                if i < chars.len() && chars[i] == '{' {
                                    let mut d = 0;
                                    let mut m = i;
                                    while m < chars.len() {
                                        if chars[m] == '{' { d += 1; }
                                        if chars[m] == '}' { d -= 1; if d == 0 { break; } }
                                        m += 1;
                                    }
                                    let body: String = chars[i+1..m].iter().collect();
                                    alternatives.push(body);
                                    i = m + 1;
                                    // Check for another UNION
                                    let mut j = i;
                                    while j < chars.len() && chars[j].is_whitespace() { j += 1; }
                                    if j + 5 <= chars.len() {
                                        let w: String = chars[j..j+5].iter().collect();
                                        if w.to_uppercase() == "UNION" {
                                            i = j + 5;
                                            continue;
                                        }
                                    }
                                }
                                break;
                            }
                            segments.push(WhereSegment::Union(alternatives));
                            continue;
                        }
                    }
                }
                // Not a UNION block — treat '{' as regular content
                current_main.push(chars[i]);
                i += 1;
                continue;
            }

            current_main.push(chars[i]);
            i += 1;
        }

        // Flush remaining main
        let trimmed = current_main.trim().to_string();
        if !trimmed.is_empty() {
            segments.push(WhereSegment::Main(trimmed));
        }

        segments
    }

    /// Finds the matching closing brace.
    fn find_matching_brace(&self, input: &str) -> Option<usize> {
        let mut depth = 0;
        for (i, c) in input.chars().enumerate() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// Parses triple patterns from a WHERE clause body.
    fn parse_triple_patterns(&self, body: &str) -> Result<Vec<TriplePattern>, String> {
        let mut patterns = Vec::new();

        // Split by . to get individual triple patterns
        // But be careful with IRIs that contain .
        let statements = self.split_statements(body);

        for stmt in statements {
            let stmt = stmt.trim();
            if stmt.is_empty() || stmt.starts_with("FILTER") || stmt.starts_with("filter") {
                continue;
            }

            // Parse subject predicate object
            let parts: Vec<&str> = stmt.splitn(3, |c: char| c.is_whitespace()).collect();
            if parts.len() < 3 {
                continue;
            }

            let subject = self.parse_term(parts[0].trim())?;
            let predicate = self.parse_term(parts[1].trim())?;
            let object_str = parts[2].trim();

            // Handle object lists (, separated)
            for obj in object_str.split(',') {
                let object = self.parse_term(obj.trim())?;
                patterns.push(TriplePattern {
                    subject: subject.clone(),
                    predicate: predicate.clone(),
                    object,
                });
            }
        }

        Ok(patterns)
    }

    /// Splits statements by . while respecting IRIs.
    fn split_statements(&self, body: &str) -> Vec<String> {
        let mut statements = Vec::new();
        let mut current = String::new();
        let mut in_iri = false;
        let mut in_string = false;

        for c in body.chars() {
            match c {
                '<' => { in_iri = true; current.push(c); }
                '>' => { in_iri = false; current.push(c); }
                '"' => { in_string = !in_string; current.push(c); }
                '.' if !in_iri && !in_string => {
                    if !current.trim().is_empty() {
                        statements.push(current.clone());
                    }
                    current.clear();
                }
                _ => { current.push(c); }
            }
        }
        if !current.trim().is_empty() {
            statements.push(current);
        }

        statements
    }

    /// Parses a term (variable, IRI, literal, or blank node).
    fn parse_term(&self, input: &str) -> Result<PatternTerm, String> {
        let input = input.trim();

        if input.starts_with('?') || input.starts_with('$') {
            Ok(PatternTerm::Variable(input[1..].to_string()))
        } else if input.starts_with('<') && input.ends_with('>') {
            Ok(PatternTerm::Iri(input[1..input.len()-1].to_string()))
        } else if input.starts_with('"') {
            let end = input.rfind('"').unwrap_or(input.len());
            Ok(PatternTerm::Literal(input[1..end].to_string()))
        } else if input.starts_with('_') {
            Ok(PatternTerm::BlankNode(input.to_string()))
        } else if input.contains(':') {
            // Prefixed name
            let expanded = self.expand_prefixed_name(input);
            Ok(PatternTerm::Iri(expanded))
        } else {
            // Treat as IRI
            Ok(PatternTerm::Iri(input.to_string()))
        }
    }

    /// Expands a prefixed name.
    fn expand_prefixed_name(&self, name: &str) -> String {
        if let Some(colon_pos) = name.find(':') {
            let prefix = &name[..colon_pos];
            let local = &name[colon_pos + 1..];
            if let Some(iri) = self.prefixes.get(prefix) {
                return format!("{}{}", iri, local);
            }
        }
        name.to_string()
    }

    /// Parses FILTER expressions from a WHERE clause body.
    fn parse_filters(&self, body: &str) -> Result<Vec<SparqlFilter>, String> {
        let mut filters = Vec::new();
        let body_upper = body.to_uppercase();

        let mut pos = 0;
        while let Some(filter_pos) = body_upper[pos..].find("FILTER") {
            let abs_pos = pos + filter_pos;
            let after_filter = body[abs_pos + 6..].trim();

            if after_filter.starts_with('(') {
                // Find matching )
                if let Some(end) = self.find_matching_paren(after_filter) {
                    let expr = &after_filter[1..end];
                    if let Ok(filter) = self.parse_filter_expr(expr) {
                        filters.push(filter);
                    }
                    pos = abs_pos + 6 + end + 1;
                } else {
                    break;
                }
            } else {
                pos = abs_pos + 6;
            }
        }

        Ok(filters)
    }

    /// Finds matching closing parenthesis.
    fn find_matching_paren(&self, input: &str) -> Option<usize> {
        let mut depth = 0;
        for (i, c) in input.chars().enumerate() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// Parses a filter expression.
    fn parse_filter_expr(&self, expr: &str) -> Result<SparqlFilter, String> {
        let expr = expr.trim();

        // Check for regex
        if expr.to_lowercase().starts_with("regex") {
            let inner = expr[5..].trim();
            let inner = inner.trim_start_matches('(').trim_end_matches(')').trim();
            let parts: Vec<&str> = inner.splitn(2, ',').collect();
            if parts.len() == 2 {
                let var = parts[0].trim().trim_start_matches('?').to_string();
                let pattern = parts[1].trim().trim_matches('"').to_string();
                return Ok(SparqlFilter::Regex(var, pattern));
            }
        }

        // Check for bound
        if expr.to_lowercase().starts_with("bound") {
            let inner = expr[5..].trim();
            let inner = inner.trim_start_matches('(').trim_end_matches(')').trim();
            let var = inner.trim_start_matches('?').to_string();
            return Ok(SparqlFilter::Bound(var));
        }

        // Check for NOT EXISTS
        let expr_upper = expr.to_uppercase();
        if expr_upper.starts_with("NOT EXISTS") || expr_upper.starts_with("NOTEXIST") {
            let after = if expr_upper.starts_with("NOT EXISTS") {
                expr[10..].trim()
            } else {
                expr[9..].trim()
            };
            if after.starts_with('{') {
                if let Some(end) = self.find_matching_brace(after) {
                    let body = &after[1..end];
                    let patterns = self.parse_triple_patterns(body)?;
                    return Ok(SparqlFilter::NotExists(patterns));
                }
            }
        }

        // Check for EXISTS
        if expr_upper.starts_with("EXISTS") {
            let after = expr[6..].trim();
            if after.starts_with('{') {
                if let Some(end) = self.find_matching_brace(after) {
                    let body = &after[1..end];
                    let patterns = self.parse_triple_patterns(body)?;
                    return Ok(SparqlFilter::Exists(patterns));
                }
            }
        }

        // Check for NOT
        if expr.starts_with('!') {
            let inner = self.parse_filter_expr(&expr[1..])?;
            return Ok(SparqlFilter::Not(Box::new(inner)));
        }

        // Check for AND/OR
        if let Some(pos) = self.find_operator(expr, "&&") {
            let left = self.parse_filter_expr(&expr[..pos])?;
            let right = self.parse_filter_expr(&expr[pos+2..])?;
            return Ok(SparqlFilter::And(Box::new(left), Box::new(right)));
        }
        if let Some(pos) = self.find_operator(expr, "||") {
            let left = self.parse_filter_expr(&expr[..pos])?;
            let right = self.parse_filter_expr(&expr[pos+2..])?;
            return Ok(SparqlFilter::Or(Box::new(left), Box::new(right)));
        }

        // Parse comparison operators
        let operators = ["!=", ">=", "<=", "=", ">", "<"];
        for op in &operators {
            if let Some(pos) = self.find_operator(expr, op) {
                let left = expr[..pos].trim();
                let right = expr[pos+op.len()..].trim();

                let var = left.trim_start_matches('?').to_string();
                let term = self.parse_term(right)?;
                return match *op {
                    "!=" => Ok(SparqlFilter::Ne(var, term)),
                    ">=" => Ok(SparqlFilter::Gte(var, term)),
                    "<=" => Ok(SparqlFilter::Lte(var, term)),
                    "=" => Ok(SparqlFilter::Eq(var, term)),
                    ">" => Ok(SparqlFilter::Gt(var, term)),
                    "<" => Ok(SparqlFilter::Lt(var, term)),
                    _ => unreachable!(),
                };
            }
        }

        Err(format!("Cannot parse filter expression: {}", expr))
    }

    /// Finds an operator outside of parentheses and strings.
    fn find_operator(&self, expr: &str, op: &str) -> Option<usize> {
        let mut depth = 0;
        let mut in_string = false;
        let bytes = expr.as_bytes();
        let op_bytes = op.as_bytes();

        for i in 0..bytes.len() {
            match bytes[i] as char {
                '(' => depth += 1,
                ')' => depth -= 1,
                '"' => in_string = !in_string,
                _ if depth == 0 && !in_string => {
                    if bytes[i..].starts_with(op_bytes) {
                        // Make sure >= and <= don't match > and <
                        if (op == ">" || op == "<") && i + 1 < bytes.len() && bytes[i + 1] == b'=' {
                            continue;
                        }
                        return Some(i);
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// Parses ORDER BY clause.
    fn parse_order_by(&self, input: &str) -> Result<(SparqlOrderBy, String), String> {
        let input = input.trim();

        if !input.len() >= 8 || !input[..8].eq_ignore_ascii_case("ORDER BY") {
            return Err("Missing ORDER BY".to_string());
        }

        let after = input[8..].trim();
        let mut terms = Vec::new();
        let mut pos = 0;

        while pos < after.len() {
            let remaining = after[pos..].trim_start();
            let skip_ws = after.len() - pos - remaining.len();
            pos += skip_ws;

            if remaining.is_empty() {
                break;
            }

            // Stop at LIMIT/OFFSET keywords (case-insensitive, ASCII only, word boundary)
            if (remaining.len() >= 5 && remaining[..5].eq_ignore_ascii_case("LIMIT")
                && (remaining.len() == 5 || !remaining.as_bytes()[5].is_ascii_alphanumeric()))
                || (remaining.len() >= 6 && remaining[..6].eq_ignore_ascii_case("OFFSET")
                    && (remaining.len() == 6 || !remaining.as_bytes()[6].is_ascii_alphanumeric()))
            {
                break;
            }

            // Parse the next ORDER BY term
            if remaining.starts_with('?') {
                // Simple variable: ?x
                let end = remaining[1..].find(|c: char| !c.is_alphanumeric() && c != '_')
                    .map(|i| i + 1)
                    .unwrap_or(remaining.len());
                let var = remaining[1..end].trim().to_string();
                terms.push(OrderByTerm { variable: var, ascending: true });
                pos += end;
            } else if (remaining.len() >= 4 && remaining[..4].eq_ignore_ascii_case("DESC"))
                || (remaining.len() >= 3 && remaining[..3].eq_ignore_ascii_case("ASC")
                    && (remaining.len() == 3 || !remaining.as_bytes()[3].is_ascii_alphanumeric()))
            {
                let (term, consumed) = self.parse_order_by_term(remaining)?;
                terms.push(term);
                pos += consumed;
            } else if remaining.starts_with('(') {
                // Parenthesized expression: (expression) — extract inner as variable
                let mut depth = 0;
                let mut end = remaining.len();
                for (i, c) in remaining.char_indices() {
                    match c {
                        '(' => depth += 1,
                        ')' => {
                            depth -= 1;
                            if depth == 0 {
                                end = i;
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                let expr = remaining[1..end].trim().to_string();
                terms.push(OrderByTerm { variable: expr, ascending: true });
                pos += end + 1;
            } else {
                // Function call or bare expression: take until whitespace
                let end = remaining.find(char::is_whitespace).unwrap_or(remaining.len());
                let token = remaining[..end].to_string();
                terms.push(OrderByTerm { variable: token, ascending: true });
                pos += end;
            }
        }

        Ok((SparqlOrderBy { terms }, after[pos..].to_string()))
    }

    /// Parses a single ORDER BY term.
    /// Returns (term, consumed_bytes) where consumed_bytes is the number of bytes consumed from the original input.
    fn parse_order_by_term(&self, input: &str) -> Result<(OrderByTerm, usize), String> {
        let trimmed = input.trim();
        let leading_ws = input.len() - input.trim_start().len();

        let is_desc = trimmed.len() >= 4 && trimmed[..4].eq_ignore_ascii_case("DESC")
            && (trimmed.len() == 4 || !trimmed.as_bytes()[4].is_ascii_alphanumeric());
        let is_asc = trimmed.len() >= 3 && trimmed[..3].eq_ignore_ascii_case("ASC")
            && (trimmed.len() == 3 || !trimmed.as_bytes()[3].is_ascii_alphanumeric());

        if is_desc || is_asc {
            let ascending = is_asc;
            let kw_len = if is_asc { 3 } else { 4 };
            // Find matching parenthesis pair for expressions like DESC(?x)
            if let Some(start) = trimmed.find('(') {
                // Track parenthesis depth to find matching close
                let mut depth = 0;
                let mut end = trimmed.len();
                for (i, c) in trimmed[start..].char_indices() {
                    match c {
                        '(' => depth += 1,
                        ')' => {
                            depth -= 1;
                            if depth == 0 {
                                end = start + i;
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                let var = trimmed[start+1..end].trim().trim_start_matches('?').to_string();
                let consumed = if end + 1 < trimmed.len() {
                    leading_ws + end + 1
                } else {
                    leading_ws + trimmed.len()
                };
                Ok((OrderByTerm { variable: var, ascending }, consumed))
            } else {
                // Bare ASC/DESC without parens — treat the next word as the variable
                let after_kw = &trimmed[kw_len..];
                let after_trimmed = after_kw.trim_start();
                let ws_after_kw = after_kw.len() - after_trimmed.len();
                let var_start = if after_trimmed.starts_with('?') { 1 } else { 0 };
                let var_end = after_trimmed[var_start..]
                    .find(char::is_whitespace)
                    .unwrap_or(after_trimmed[var_start..].len());
                let var = after_trimmed[var_start..var_start + var_end].to_string();
                let consumed = leading_ws + kw_len + ws_after_kw + var_start + var_end;
                Ok((OrderByTerm { variable: var, ascending }, consumed))
            }
        } else if trimmed.starts_with('?') {
            let var = trimmed[1..].split_whitespace().next().unwrap_or("").to_string();
            let consumed = leading_ws + 1 + var.len();
            Ok((OrderByTerm { variable: var, ascending: true }, consumed))
        } else if trimmed.starts_with('(') {
            // Parenthesized expression
            let mut depth = 0;
            let mut end = trimmed.len();
            for (i, c) in trimmed.char_indices() {
                match c {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            end = i;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let expr = trimmed[1..end].trim().to_string();
            let consumed = leading_ws + end + 1;
            Ok((OrderByTerm { variable: expr, ascending: true }, consumed))
        } else {
            // Function call or bare expression
            let end = trimmed.find(char::is_whitespace).unwrap_or(trimmed.len());
            let token = trimmed[..end].to_string();
            let consumed = leading_ws + end;
            Ok((OrderByTerm { variable: token, ascending: true }, consumed))
        }
    }

    /// Parses LIMIT clause.
    fn parse_limit(&self, input: &str) -> Result<(usize, String), String> {
        let input = input.trim();
        let upper = input.to_uppercase();

        if !upper.starts_with("LIMIT") {
            return Err("Missing LIMIT".to_string());
        }

        let after = input[5..].trim();
        let num_str = after.split_whitespace().next().unwrap_or("");
        let limit: usize = num_str.parse().map_err(|_| format!("Invalid LIMIT: {}", num_str))?;

        Ok((limit, after[num_str.len()..].to_string()))
    }

    /// Parses OFFSET clause.
    fn parse_offset(&self, input: &str) -> Result<(usize, String), String> {
        let input = input.trim();
        let upper = input.to_uppercase();

        if !upper.starts_with("OFFSET") {
            return Err("Missing OFFSET".to_string());
        }

        let after = input[6..].trim();
        let num_str = after.split_whitespace().next().unwrap_or("");
        let offset: usize = num_str.parse().map_err(|_| format!("Invalid OFFSET: {}", num_str))?;

        Ok((offset, after[num_str.len()..].to_string()))
    }

    /// Translates a SPARQL query to SQL.
    pub fn translate_to_sql(&self, query: &SparqlQuery) -> Result<String, String> {
        // Handle UNION: each alternative is a separate SELECT with UNION ALL
        if !query.unions.is_empty() {
            return self.translate_union(query);
        }

        // Find the main class from rdf:type patterns
        let main_class = self.find_main_class(&query.where_patterns)?;
        let class_name = extract_local_name(&main_class);

        // Build SELECT clause
        let select_cols = match &query.select {
            SparqlSelect::All => {
                let mut vars: Vec<String> = Vec::new();
                for pattern in &query.where_patterns {
                    self.collect_variables(pattern, &mut vars);
                }
                // Also collect from OPTIONAL blocks
                for opt in &query.optional {
                    for pattern in &opt.patterns {
                        self.collect_variables(pattern, &mut vars);
                    }
                }
                vars.sort();
                vars.dedup();
                vars
            }
            SparqlSelect::Variables(vars) => vars.clone(),
        };

        if select_cols.is_empty() {
            return Err("No variables in SELECT clause".to_string());
        }

        // Build SQL
        let mut sql = String::new();
        sql.push_str("SELECT ");
        if query.distinct {
            sql.push_str("DISTINCT ");
        }

        // Sanitize column names to prevent SQL injection
        let col_exprs: Vec<String> = select_cols.iter().map(|var| {
            let sanitized: String = var.chars().filter(|c| c.is_alphanumeric() || *c == '_').collect();
            format!("\"{}\"", sanitized)
        }).collect();
        sql.push_str(&col_exprs.join(", "));
        // Sanitize class name to prevent SQL injection
        let safe_class: String = class_name.chars().filter(|c| c.is_alphanumeric() || *c == '_').collect();
        sql.push_str(&format!(" FROM {}", safe_class));

        // Build WHERE clause — main patterns only (not OPTIONAL)
        let mut conditions = Vec::new();
        for pattern in &query.where_patterns {
            if let PatternTerm::Iri(pred) = &pattern.predicate {
                if pred.ends_with("#type") || pred == "rdf:type" || pred == "type" {
                    continue;
                }
            }
            if let Some(cond) = self.pattern_to_condition(pattern) {
                conditions.push(cond);
            }
        }

        // Add FILTER conditions (including from main block)
        for filter in &query.filters {
            if let Some(cond) = self.translate_filter(filter) {
                conditions.push(cond);
            }
        }
        // Add OPTIONAL block filters (those inside OPTIONAL { ... FILTER(...) })
        for opt in &query.optional {
            for filter in &opt.filters {
                if let Some(cond) = self.translate_filter(filter) {
                    conditions.push(cond);
                }
            }
        }

        if !conditions.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&conditions.join(" AND "));
        }

        // Add ORDER BY
        if let Some(ref order_by) = query.order_by {
            if !order_by.terms.is_empty() {
                sql.push_str(" ORDER BY ");
                let order_terms: Vec<String> = order_by.terms.iter().map(|t| {
                    format!("\"{}\" {}", t.variable, if t.ascending { "ASC" } else { "DESC" })
                }).collect();
                sql.push_str(&order_terms.join(", "));
            }
        }

        if let Some(limit) = query.limit {
            sql.push_str(&format!(" LIMIT {}", limit));
        }

        if let Some(offset) = query.offset {
            sql.push_str(&format!(" OFFSET {}", offset));
        }

        Ok(sql)
    }

    /// Translates a UNION query: each alternative becomes a SELECT ... UNION ALL ...
    fn translate_union(&self, query: &SparqlQuery) -> Result<String, String> {
        let mut parts = Vec::new();

        // Each union alternative is a Vec<TriplePattern>
        for alt_patterns in &query.unions {
            let main_class = self.find_main_class(alt_patterns)?;
            let class_name = extract_local_name(&main_class);

            let select_cols = match &query.select {
                SparqlSelect::All => {
                    let mut vars: Vec<String> = Vec::new();
                    for pattern in alt_patterns {
                        self.collect_variables(pattern, &mut vars);
                    }
                    vars.sort();
                    vars.dedup();
                    vars
                }
                SparqlSelect::Variables(vars) => vars.clone(),
            };

            let mut sql = String::new();
            sql.push_str("SELECT ");
            if query.distinct {
                sql.push_str("DISTINCT ");
            }
            let col_exprs: Vec<String> = select_cols.iter().map(|v| format!("\"{}\"", v)).collect();
            sql.push_str(&col_exprs.join(", "));
            sql.push_str(&format!(" FROM {}", class_name));

            let mut conditions = Vec::new();
            for pattern in alt_patterns {
                if let PatternTerm::Iri(pred) = &pattern.predicate {
                    if pred.ends_with("#type") || pred == "rdf:type" || pred == "type" {
                        continue;
                    }
                }
                if let Some(cond) = self.pattern_to_condition(pattern) {
                    conditions.push(cond);
                }
            }
            // Add global filters to each alternative
            for filter in &query.filters {
                if let Some(cond) = self.translate_filter(filter) {
                    conditions.push(cond);
                }
            }
            if !conditions.is_empty() {
                sql.push_str(" WHERE ");
                sql.push_str(&conditions.join(" AND "));
            }
            parts.push(sql);
        }

        Ok(parts.join(" UNION ALL "))
    }

    /// Finds the main class IRI from rdf:type patterns.
    fn find_main_class(&self, patterns: &[TriplePattern]) -> Result<String, String> {
        for pattern in patterns {
            if let PatternTerm::Iri(pred) = &pattern.predicate {
                if pred.ends_with("#type") || pred == "rdf:type" || pred == "type" {
                    if let PatternTerm::Iri(class_iri) = &pattern.object {
                        return Ok(class_iri.clone());
                    }
                }
            }
        }
        Err("No rdf:type pattern found in WHERE clause".to_string())
    }

    /// Converts a triple pattern to a SQL condition. Returns None for rdf:type patterns.
    fn pattern_to_condition(&self, pattern: &TriplePattern) -> Option<String> {
        match (&pattern.subject, &pattern.predicate, &pattern.object) {
            (PatternTerm::Variable(_), PatternTerm::Iri(pred_iri), PatternTerm::Variable(_)) => {
                let prop_name = extract_local_name(pred_iri);
                Some(format!("\"{}\" IS NOT NULL", prop_name))
            }
            (PatternTerm::Variable(_), PatternTerm::Iri(pred_iri), PatternTerm::Literal(lit)) => {
                let prop_name = extract_local_name(pred_iri);
                Some(format!("\"{}\" = '{}'", prop_name, escape_sql_string(lit)))
            }
            (PatternTerm::Variable(_), PatternTerm::Iri(pred_iri), PatternTerm::Iri(obj_iri)) => {
                let prop_name = extract_local_name(pred_iri);
                let obj_name = extract_local_name(obj_iri);
                Some(format!("\"{}\" = '{}'", prop_name, obj_name))
            }
            _ => None,
        }
    }

    /// Collects all variables from a triple pattern.
    fn collect_variables(&self, pattern: &TriplePattern, vars: &mut Vec<String>) {
        if let PatternTerm::Variable(v) = &pattern.subject {
            if !vars.contains(v) { vars.push(v.clone()); }
        }
        if let PatternTerm::Variable(v) = &pattern.predicate {
            if !vars.contains(v) { vars.push(v.clone()); }
        }
        if let PatternTerm::Variable(v) = &pattern.object {
            if !vars.contains(v) { vars.push(v.clone()); }
        }
    }

    /// Translates a SPARQL filter to SQL condition.
    fn translate_filter(&self, filter: &SparqlFilter) -> Option<String> {
        match filter {
            SparqlFilter::Eq(var, term) => {
                let val = self.term_to_sql_value(term)?;
                Some(format!("\"{}\" = {}", var, val))
            }
            SparqlFilter::Ne(var, term) => {
                let val = self.term_to_sql_value(term)?;
                Some(format!("\"{}\" != {}", var, val))
            }
            SparqlFilter::Gt(var, term) => {
                let val = self.term_to_sql_value(term)?;
                Some(format!("\"{}\" > {}", var, val))
            }
            SparqlFilter::Lt(var, term) => {
                let val = self.term_to_sql_value(term)?;
                Some(format!("\"{}\" < {}", var, val))
            }
            SparqlFilter::Gte(var, term) => {
                let val = self.term_to_sql_value(term)?;
                Some(format!("\"{}\" >= {}", var, val))
            }
            SparqlFilter::Lte(var, term) => {
                let val = self.term_to_sql_value(term)?;
                Some(format!("\"{}\" <= {}", var, val))
            }
            SparqlFilter::Regex(var, pattern) => {
                let like_patterns = regex_to_like(pattern);
                if like_patterns.is_empty() {
                    return None;
                }
                // Handle alternation: generate OR conditions for each branch
                let conds: Vec<String> = like_patterns.iter()
                    .map(|p| format!("\"{}\" LIKE '{}' ESCAPE '\\'", var, escape_sql_string(p)))
                    .collect();
                if conds.len() == 1 {
                    Some(conds.into_iter().next().unwrap())
                } else {
                    Some(format!("({})", conds.join(" OR ")))
                }
            }
            SparqlFilter::Bound(var) => {
                Some(format!("\"{}\" IS NOT NULL", var))
            }
            SparqlFilter::Not(inner) => {
                self.translate_filter(inner).map(|c| format!("NOT ({})", c))
            }
            SparqlFilter::And(left, right) => {
                let l = self.translate_filter(left)?;
                let r = self.translate_filter(right)?;
                Some(format!("({}) AND ({})", l, r))
            }
            SparqlFilter::Or(left, right) => {
                let l = self.translate_filter(left)?;
                let r = self.translate_filter(right)?;
                Some(format!("({}) OR ({})", l, r))
            }
            SparqlFilter::Exists(patterns) => {
                // In document model: EXISTS { ?x ex:prop ?y } → "prop" IS NOT NULL
                let conds: Vec<String> = patterns.iter()
                    .filter_map(|p| {
                        if let PatternTerm::Iri(pred) = &p.predicate {
                            let prop = extract_local_name(pred);
                            Some(format!("\"{}\" IS NOT NULL", prop))
                        } else {
                            None
                        }
                    })
                    .collect();
                if conds.is_empty() {
                    None
                } else if conds.len() == 1 {
                    Some(conds.into_iter().next().unwrap())
                } else {
                    Some(format!("({})", conds.join(" AND ")))
                }
            }
            SparqlFilter::NotExists(patterns) => {
                // NOT EXISTS { ?x ex:prop ?y } → "prop" IS NULL
                let conds: Vec<String> = patterns.iter()
                    .filter_map(|p| {
                        if let PatternTerm::Iri(pred) = &p.predicate {
                            let prop = extract_local_name(pred);
                            Some(format!("\"{}\" IS NULL", prop))
                        } else {
                            None
                        }
                    })
                    .collect();
                if conds.is_empty() {
                    None
                } else if conds.len() == 1 {
                    Some(conds.into_iter().next().unwrap())
                } else {
                    Some(format!("({})", conds.join(" AND ")))
                }
            }
        }
    }

    /// Converts a pattern term to SQL value.
    fn term_to_sql_value(&self, term: &PatternTerm) -> Option<String> {
        match term {
            PatternTerm::Literal(s) => Some(format!("'{}'", escape_sql_string(s))),
            PatternTerm::Iri(i) => Some(format!("'{}'", extract_local_name(i))),
            _ => None,
        }
    }
}

/// Extracts local name from an IRI.
fn extract_local_name(iri: &str) -> String {
    if let Some(pos) = iri.rfind('#') {
        iri[pos + 1..].to_string()
    } else if let Some(pos) = iri.rfind('/') {
        iri[pos + 1..].to_string()
    } else if let Some(pos) = iri.rfind(':') {
        iri[pos + 1..].to_string()
    } else {
        iri.to_string()
    }
}

/// Escape a string for safe inclusion in a SQL string literal.
/// Handles single quotes, backslashes, null bytes, and other special characters.
fn escape_sql_string(s: &str) -> String {
    let mut escaped = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\'' => escaped.push_str("''"),
            '\\' => escaped.push_str("\\\\"),
            '\0' => escaped.push_str("\\0"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            _ => escaped.push(c),
        }
    }
    escaped
}

/// Converts a SPARQL regex pattern to a SQL LIKE pattern.
/// Handles common regex constructs: ^ → prefix match, $ → suffix match,
/// .* → % wildcard, . → _ single char.
/// Returns a vector of LIKE patterns (one per alternation branch).
fn regex_to_like(pattern: &str) -> Vec<String> {
    let has_start_anchor = pattern.starts_with('^');
    let has_end_anchor = pattern.ends_with('$');
    let inner = pattern
        .trim_start_matches('^')
        .trim_end_matches('$');

    // Split on top-level alternation (|) to handle OR patterns
    let branches = split_alternation(inner);

    branches.iter().map(|branch| {
        // Check for complex regex metacharacters that can't be expressed in LIKE
        let has_complex = branch.contains("\\") || branch.contains("[") || branch.contains("{")
            || branch.contains("(") || branch.contains("+")
            || branch.contains("?");

        if has_complex {
            // Fall back to substring match for complex patterns
            return format!("%{}%", branch);
        }

        // Convert simple regex to LIKE pattern
        let mut like = String::new();
        if !has_start_anchor {
            like.push('%');
        }

        let mut chars = branch.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '.' => {
                    if chars.peek() == Some(&'*') {
                        chars.next();
                        like.push('%');
                    } else {
                        like.push('_');
                    }
                }
                '%' | '_' => {
                    // Escape LIKE wildcards in the literal parts
                    like.push('\\');
                    like.push(c);
                }
                _ => like.push(c),
            }
        }

        if !has_end_anchor {
            like.push('%');
        }

        like
    }).collect()
}

/// Split a regex pattern on top-level alternation (|) operators.
/// Respects parentheses — only splits at the top level.
fn split_alternation(pattern: &str) -> Vec<String> {
    let mut branches = Vec::new();
    let mut current = String::new();
    let mut depth = 0;

    for c in pattern.chars() {
        match c {
            '(' | '[' | '{' => {
                depth += 1;
                current.push(c);
            }
            ')' | ']' | '}' => {
                depth -= 1;
                current.push(c);
            }
            '|' if depth == 0 => {
                branches.push(current.clone());
                current.clear();
            }
            _ => {
                current.push(c);
            }
        }
    }
    if !current.is_empty() {
        branches.push(current);
    }

    if branches.is_empty() {
        branches.push(pattern.to_string());
    }

    branches
}

/// Formats query results as SPARQL JSON.
pub fn format_sparql_json(
    query: &SparqlQuery,
    rows: &[serde_json::Map<String, serde_json::Value>],
) -> SparqlResult {
    let vars = match &query.select {
        SparqlSelect::All => {
            let mut vars: Vec<String> = Vec::new();
            for pattern in &query.where_patterns {
                if let PatternTerm::Variable(v) = &pattern.subject {
                    if !vars.contains(v) { vars.push(v.clone()); }
                }
                if let PatternTerm::Variable(v) = &pattern.predicate {
                    if !vars.contains(v) { vars.push(v.clone()); }
                }
                if let PatternTerm::Variable(v) = &pattern.object {
                    if !vars.contains(v) { vars.push(v.clone()); }
                }
            }
            vars
        }
        SparqlSelect::Variables(vars) => vars.clone(),
    };

    let mut bindings = Vec::new();
    for row in rows {
        let mut binding = HashMap::new();
        for var in &vars {
            if let Some(val) = row.get(var) {
                let term = match val {
                    serde_json::Value::String(s) => SparqlTerm {
                        term_type: "literal".to_string(),
                        value: s.clone(),
                        datatype: None,
                        language: None,
                    },
                    serde_json::Value::Number(n) => SparqlTerm {
                        term_type: "literal".to_string(),
                        value: n.to_string(),
                        datatype: Some("http://www.w3.org/2001/XMLSchema#decimal".to_string()),
                        language: None,
                    },
                    serde_json::Value::Bool(b) => SparqlTerm {
                        term_type: "literal".to_string(),
                        value: b.to_string(),
                        datatype: Some("http://www.w3.org/2001/XMLSchema#boolean".to_string()),
                        language: None,
                    },
                    _ => SparqlTerm {
                        term_type: "literal".to_string(),
                        value: val.to_string(),
                        datatype: None,
                        language: None,
                    },
                };
                binding.insert(format!("?{}", var), term);
            }
        }
        bindings.push(binding);
    }

    SparqlResult {
        head: SparqlHead { vars: vars.iter().map(|v| format!("?{}", v)).collect() },
        results: SparqlResultSet { bindings },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_basic_select() {
        let mut parser = SparqlParser::new();
        let query = r#"
PREFIX ex: <http://example.org/>
SELECT ?name WHERE {
    ?x rdf:type ex:Person .
    ?x ex:name ?name .
}
"#;
        let result = parser.parse(query);
        assert!(result.is_ok(), "Failed to parse: {:?}", result.err());

        let q = result.unwrap();
        assert_eq!(q.where_patterns.len(), 2);
        assert!(matches!(q.select, SparqlSelect::Variables(vars) if vars.contains(&"name".to_string())));
    }

    #[test]
    fn test_parse_with_filter() {
        let mut parser = SparqlParser::new();
        let query = r#"
PREFIX ex: <http://example.org/>
SELECT ?name WHERE {
    ?x rdf:type ex:Person .
    ?x ex:name ?name .
    FILTER(?name = "Alice")
}
"#;
        let result = parser.parse(query);
        assert!(result.is_ok(), "Failed to parse: {:?}", result.err());

        let q = result.unwrap();
        assert_eq!(q.filters.len(), 1);
    }

    #[test]
    fn test_parse_with_limit() {
        let mut parser = SparqlParser::new();
        let query = r#"
SELECT ?x ?y WHERE {
    ?x rdf:type <http://example.org/Person> .
    ?x <http://example.org/name> ?y .
}
LIMIT 10
"#;
        let result = parser.parse(query);
        assert!(result.is_ok(), "Failed to parse: {:?}", result.err());

        let q = result.unwrap();
        assert_eq!(q.limit, Some(10));
    }

    #[test]
    fn test_translate_to_sql() {
        let mut parser = SparqlParser::new();
        let query = r#"
PREFIX ex: <http://example.org/>
SELECT ?name WHERE {
    ?x rdf:type ex:Person .
    ?x ex:name ?name .
}
"#;
        let q = parser.parse(query).unwrap();
        let sql = parser.translate_to_sql(&q);
        assert!(sql.is_ok(), "Failed to translate: {:?}", sql.err());

        let sql = sql.unwrap();
        assert!(sql.contains("Person"), "SQL should reference Person class: {}", sql);
    }

    #[test]
    fn test_parse_distinct() {
        let mut parser = SparqlParser::new();
        let query = r#"
PREFIX ex: <http://example.org/>
SELECT DISTINCT ?name WHERE {
    ?x rdf:type ex:Person .
    ?x ex:name ?name .
}
"#;
        let q = parser.parse(query).unwrap();
        assert!(q.distinct, "DISTINCT should be parsed");
    }

    #[test]
    fn test_translate_distinct() {
        let mut parser = SparqlParser::new();
        let query = r#"
PREFIX ex: <http://example.org/>
SELECT DISTINCT ?name WHERE {
    ?x rdf:type ex:Person .
    ?x ex:name ?name .
}
"#;
        let q = parser.parse(query).unwrap();
        let sql = parser.translate_to_sql(&q).unwrap();
        assert!(sql.contains("SELECT DISTINCT"), "SQL should contain SELECT DISTINCT: {}", sql);
    }

    #[test]
    fn test_parse_optional() {
        let mut parser = SparqlParser::new();
        let query = r#"
PREFIX ex: <http://example.org/>
SELECT ?name ?email WHERE {
    ?x rdf:type ex:Person .
    ?x ex:name ?name .
    OPTIONAL { ?x ex:email ?email }
}
"#;
        let q = parser.parse(query).unwrap();
        assert_eq!(q.optional.len(), 1, "Should have 1 OPTIONAL block");
        assert_eq!(q.optional[0].patterns.len(), 1, "OPTIONAL should have 1 pattern");
    }

    #[test]
    fn test_translate_optional() {
        let mut parser = SparqlParser::new();
        let query = r#"
PREFIX ex: <http://example.org/>
SELECT ?name ?email WHERE {
    ?x rdf:type ex:Person .
    ?x ex:name ?name .
    OPTIONAL { ?x ex:email ?email }
}
"#;
        let q = parser.parse(query).unwrap();
        let sql = parser.translate_to_sql(&q).unwrap();
        // Optional column should be in SELECT but not require IS NOT NULL in WHERE
        assert!(sql.contains("\"email\""), "SQL should include optional email column: {}", sql);
        // Main pattern should still have IS NOT NULL
        assert!(sql.contains("\"name\" IS NOT NULL"), "SQL should require name IS NOT NULL: {}", sql);
        // Should NOT have email IS NOT NULL (it's optional)
        assert!(!sql.contains("\"email\" IS NOT NULL"), "SQL should NOT require email IS NOT NULL: {}", sql);
    }

    #[test]
    fn test_parse_union() {
        let mut parser = SparqlParser::new();
        let query = r#"
PREFIX ex: <http://example.org/>
SELECT ?name WHERE {
    {
        ?x rdf:type ex:Person .
        ?x ex:name ?name .
    }
    UNION
    {
        ?x rdf:type ex:Organization .
        ?x ex:name ?name .
    }
}
"#;
        let q = parser.parse(query).unwrap();
        assert_eq!(q.unions.len(), 2, "Should have 2 UNION alternatives");
    }

    #[test]
    fn test_translate_union() {
        let mut parser = SparqlParser::new();
        let query = r#"
PREFIX ex: <http://example.org/>
SELECT ?name WHERE {
    {
        ?x rdf:type ex:Person .
        ?x ex:name ?name .
    }
    UNION
    {
        ?x rdf:type ex:Organization .
        ?x ex:name ?name .
    }
}
"#;
        let q = parser.parse(query).unwrap();
        let sql = parser.translate_to_sql(&q).unwrap();
        assert!(sql.contains("UNION ALL"), "SQL should contain UNION ALL: {}", sql);
        assert!(sql.contains("Person"), "SQL should reference Person: {}", sql);
        assert!(sql.contains("Organization"), "SQL should reference Organization: {}", sql);
    }

    #[test]
    fn test_parse_filter_exists() {
        let mut parser = SparqlParser::new();
        let query = r#"
PREFIX ex: <http://example.org/>
SELECT ?name WHERE {
    ?x rdf:type ex:Person .
    ?x ex:name ?name .
    FILTER (EXISTS { ?x ex:email ?email })
}
"#;
        let q = parser.parse(query).unwrap();
        assert_eq!(q.filters.len(), 1, "Should have 1 filter");
        assert!(matches!(q.filters[0], SparqlFilter::Exists(_)), "Should be Exists filter");
    }

    #[test]
    fn test_parse_filter_not_exists() {
        let mut parser = SparqlParser::new();
        let query = r#"
PREFIX ex: <http://example.org/>
SELECT ?name WHERE {
    ?x rdf:type ex:Person .
    ?x ex:name ?name .
    FILTER (NOT EXISTS { ?x ex:email ?email })
}
"#;
        let q = parser.parse(query).unwrap();
        assert_eq!(q.filters.len(), 1, "Should have 1 filter");
        assert!(matches!(q.filters[0], SparqlFilter::NotExists(_)), "Should be NotExists filter");
    }

    #[test]
    fn test_translate_filter_exists() {
        let mut parser = SparqlParser::new();
        let query = r#"
PREFIX ex: <http://example.org/>
SELECT ?name WHERE {
    ?x rdf:type ex:Person .
    ?x ex:name ?name .
    FILTER (EXISTS { ?x ex:email ?email })
}
"#;
        let q = parser.parse(query).unwrap();
        let sql = parser.translate_to_sql(&q).unwrap();
        assert!(sql.contains("\"email\" IS NOT NULL"), "EXISTS should translate to IS NOT NULL: {}", sql);
    }

    #[test]
    fn test_translate_filter_not_exists() {
        let mut parser = SparqlParser::new();
        let query = r#"
PREFIX ex: <http://example.org/>
SELECT ?name WHERE {
    ?x rdf:type ex:Person .
    ?x ex:name ?name .
    FILTER (NOT EXISTS { ?x ex:email ?email })
}
"#;
        let q = parser.parse(query).unwrap();
        let sql = parser.translate_to_sql(&q).unwrap();
        assert!(sql.contains("\"email\" IS NULL"), "NOT EXISTS should translate to IS NULL: {}", sql);
    }

    #[test]
    fn test_optional_with_filter() {
        let mut parser = SparqlParser::new();
        let query = r#"
PREFIX ex: <http://example.org/>
SELECT ?name ?email WHERE {
    ?x rdf:type ex:Person .
    ?x ex:name ?name .
    OPTIONAL {
        ?x ex:email ?email .
        FILTER(?email = "alice@example.org")
    }
}
"#;
        let q = parser.parse(query).unwrap();
        assert_eq!(q.optional.len(), 1);
        assert_eq!(q.optional[0].filters.len(), 1, "OPTIONAL block should have 1 filter");
    }
}
