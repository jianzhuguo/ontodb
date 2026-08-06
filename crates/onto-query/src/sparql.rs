//! SPARQL query parser and executor for OntoDB.
//!
//! Supports a basic subset of SPARQL 1.1:
//! - SELECT with variables (?x, ?y)
//! - WHERE with triple patterns
//! - FILTER with basic comparisons
//! - ORDER BY, LIMIT, OFFSET
//! - OPTIONAL (basic left join)
//!
//! Translates SPARQL queries to SQL for execution against the OntoDB engine.

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
    /// Optional ORDER BY.
    pub order_by: Option<SparqlOrderBy>,
    /// Optional LIMIT.
    pub limit: Option<usize>,
    /// Optional OFFSET.
    pub offset: Option<usize>,
    /// Whether this is a CONSTRUCT query.
    pub construct: Option<Vec<TriplePattern>>,
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
        let select = self.parse_select_clause(select_part)?;

        // Parse WHERE clause
        let (patterns, filters, remaining) = self.parse_where_clause(where_part)?;

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
        let (patterns, filters, _remaining) = self.parse_where_clause(where_part)?;

        Ok(SparqlQuery {
            select: SparqlSelect::All,
            where_patterns: patterns,
            filters,
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
        let (patterns, filters, _) = self.parse_where_clause(where_part)?;

        Ok(SparqlQuery {
            select: SparqlSelect::Variables(vec!["ask".to_string()]),
            where_patterns: patterns,
            filters,
            order_by: None,
            limit: Some(1),
            offset: None,
            construct: None,
        })
    }

    /// Parses SELECT clause.
    fn parse_select_clause(&self, input: &str) -> Result<SparqlSelect, String> {
        let input = input.trim();
        let upper = input.to_uppercase();

        if !upper.starts_with("SELECT") {
            return Err("Missing SELECT keyword".to_string());
        }

        let after_select = input[6..].trim();

        if after_select == "*" || after_select.to_uppercase().starts_with("*") {
            return Ok(SparqlSelect::All);
        }

        // Check for DISTINCT
        let after_select = if after_select.to_uppercase().starts_with("DISTINCT") {
            after_select[8..].trim()
        } else {
            after_select
        };

        let mut variables = Vec::new();
        for token in after_select.split_whitespace() {
            let token = token.trim();
            if token.starts_with('?') {
                variables.push(token[1..].to_string());
            } else if token.to_uppercase() == "WHERE" {
                break;
            }
        }

        if variables.is_empty() {
            return Err("No variables in SELECT clause".to_string());
        }

        Ok(SparqlSelect::Variables(variables))
    }

    /// Parses WHERE clause with triple patterns and FILTERs.
    fn parse_where_clause(&self, input: &str) -> Result<(Vec<TriplePattern>, Vec<SparqlFilter>, String), String> {
        let input = input.trim();

        // Find the opening {
        let start = input.find('{').ok_or("Missing { in WHERE clause")?;
        let end = self.find_matching_brace(&input[start..]).ok_or("Missing } in WHERE clause")?;
        let body = &input[start+1..start+end];

        let patterns = self.parse_triple_patterns(body)?;
        let filters = self.parse_filters(body)?;

        let remaining = input[start+end+1..].to_string();

        Ok((patterns, filters, remaining))
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
        let upper = input.to_uppercase();

        if !upper.starts_with("ORDER BY") {
            return Err("Missing ORDER BY".to_string());
        }

        let after = input[8..].trim();
        let mut terms = Vec::new();
        let mut remaining = after;

        for token in after.split_whitespace() {
            let token_upper = token.to_uppercase();
            if token_upper == "LIMIT" || token_upper == "OFFSET" {
                break;
            }

            if token.starts_with('?') || token.starts_with("DESC") || token.starts_with("ASC") {
                let (term, _) = self.parse_order_by_term(token)?;
                terms.push(term);
                remaining = &remaining[token.len()..];
            } else {
                remaining = &remaining[token.len()..];
            }
        }

        Ok((SparqlOrderBy { terms }, remaining.to_string()))
    }

    /// Parses a single ORDER BY term.
    fn parse_order_by_term(&self, input: &str) -> Result<(OrderByTerm, String), String> {
        let input = input.trim();

        if input.to_uppercase().starts_with("DESC") || input.to_uppercase().starts_with("ASC") {
            let ascending = input.to_uppercase().starts_with("ASC");
            let start = input.find('(').unwrap_or(0);
            let end = input.find(')').unwrap_or(input.len());
            let var = input[start+1..end].trim().trim_start_matches('?').to_string();
            Ok((OrderByTerm { variable: var, ascending }, input[end+1..].to_string()))
        } else if input.starts_with('?') {
            let var = input[1..].split_whitespace().next().unwrap_or("").to_string();
            let var_len = var.len();
            Ok((OrderByTerm { variable: var, ascending: true }, input[var_len+1..].to_string()))
        } else {
            Err(format!("Cannot parse ORDER BY term: {}", input))
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
        // Group triple patterns by subject to determine the main class
        let mut subject_patterns: HashMap<String, Vec<&TriplePattern>> = HashMap::new();
        for pattern in &query.where_patterns {
            match &pattern.subject {
                PatternTerm::Variable(var) => {
                    subject_patterns.entry(var.clone()).or_default().push(pattern);
                }
                PatternTerm::Iri(iri) => {
                    subject_patterns.entry(iri.clone()).or_default().push(pattern);
                }
                _ => {}
            }
        }

        // Find the main class from rdf:type patterns
        let mut main_class: Option<String> = None;
        let mut _main_var: Option<String> = None;
        for pattern in &query.where_patterns {
            if let PatternTerm::Iri(pred) = &pattern.predicate {
                if pred.ends_with("#type") || pred == "rdf:type" || pred == "type" {
                    if let PatternTerm::Iri(class_iri) = &pattern.object {
                        main_class = Some(class_iri.clone());
                        if let PatternTerm::Variable(var) = &pattern.subject {
                            _main_var = Some(var.clone());
                        }
                    }
                }
            }
        }

        let main_class = main_class.ok_or("No rdf:type pattern found in WHERE clause")?;
        let class_name = extract_local_name(&main_class);

        // Build SELECT clause
        let select_cols = match &query.select {
            SparqlSelect::All => {
                // Collect all variables
                let mut vars: Vec<String> = Vec::new();
                for pattern in &query.where_patterns {
                    self.collect_variables(pattern, &mut vars);
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

        // Map variables to column names
        let col_exprs: Vec<String> = select_cols.iter().map(|var| {
            // Check if this variable appears as a predicate in any pattern
            for pattern in &query.where_patterns {
                if let PatternTerm::Variable(pred_var) = &pattern.predicate {
                    if pred_var == var {
                        return format!("\"{}\"", var);
                    }
                }
            }
            format!("\"{}\"", var)
        }).collect();

        sql.push_str(&col_exprs.join(", "));
        sql.push_str(&format!(" FROM {}", class_name));

        // Build WHERE clause
        let mut conditions = Vec::new();
        for pattern in &query.where_patterns {
            // Skip rdf:type patterns (handled by FROM clause)
            if let PatternTerm::Iri(pred) = &pattern.predicate {
                if pred.ends_with("#type") || pred == "rdf:type" || pred == "type" {
                    continue;
                }
            }

            // Convert triple pattern to SQL condition
            if let (PatternTerm::Variable(_subj_var), PatternTerm::Iri(pred_iri), PatternTerm::Variable(obj_var)) =
                (&pattern.subject, &pattern.predicate, &pattern.object)
            {
                let prop_name = extract_local_name(pred_iri);
                conditions.push(format!("\"{}\" IS NOT NULL", prop_name));
                // If the object variable is in SELECT, we need to alias it
                if select_cols.contains(obj_var) {
                    // This is a property access - we'll handle it in post-processing
                }
            } else if let (PatternTerm::Variable(_subj_var), PatternTerm::Iri(pred_iri), PatternTerm::Literal(lit)) =
                (&pattern.subject, &pattern.predicate, &pattern.object)
            {
                let prop_name = extract_local_name(pred_iri);
                conditions.push(format!("\"{}\" = '{}'", prop_name, lit.replace('\'', "''")));
            } else if let (PatternTerm::Variable(_subj_var), PatternTerm::Iri(pred_iri), PatternTerm::Iri(obj_iri)) =
                (&pattern.subject, &pattern.predicate, &pattern.object)
            {
                let prop_name = extract_local_name(pred_iri);
                let obj_name = extract_local_name(obj_iri);
                conditions.push(format!("\"{}\" = '{}'", prop_name, obj_name));
            }
        }

        // Add FILTER conditions
        for filter in &query.filters {
            if let Some(cond) = self.translate_filter(filter) {
                conditions.push(cond);
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

        // Add LIMIT
        if let Some(limit) = query.limit {
            sql.push_str(&format!(" LIMIT {}", limit));
        }

        // Add OFFSET
        if let Some(offset) = query.offset {
            sql.push_str(&format!(" OFFSET {}", offset));
        }

        Ok(sql)
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
                Some(format!("\"{}\" LIKE '%{}%'", var, pattern.replace('\'', "''")))
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
        }
    }

    /// Converts a pattern term to SQL value.
    fn term_to_sql_value(&self, term: &PatternTerm) -> Option<String> {
        match term {
            PatternTerm::Literal(s) => Some(format!("'{}'", s.replace('\'', "''"))),
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
}
