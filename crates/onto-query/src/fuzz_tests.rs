//! Fuzz-style stress tests for the SQL and SPARQL parsers.
//!
//! These tests generate random inputs and verify that the parsers
//! never panic — they must always return Ok or Err gracefully.

use rand::Rng;

/// Characters commonly found in SQL queries.
const SQL_CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789 _=<>(),.'\";*+-/\n\t";

/// Generate a random string from the given character set.
fn random_string(rng: &mut impl Rng, max_len: usize) -> String {
    let len = rng.gen_range(0..max_len);
    (0..len)
        .map(|_| {
            let idx = rng.gen_range(0..SQL_CHARS.len());
            SQL_CHARS[idx] as char
        })
        .collect()
}

/// Generate a random SQL-like query (biased toward valid tokens).
fn random_sql_biased(rng: &mut impl Rng) -> String {
    let keywords = [
        "SELECT", "INSERT", "UPDATE", "DELETE", "CREATE", "DROP",
        "FROM", "WHERE", "AND", "OR", "NOT", "IN", "LIKE",
        "ORDER BY", "GROUP BY", "LIMIT", "OFFSET", "JOIN",
        "VALUES", "SET", "INTO", "TABLE", "INDEX", "CLASS",
        "BETWEEN", "NULL", "IS", "AS", "ON", "DISTINCT",
        "COUNT", "SUM", "AVG", "MIN", "MAX",
        "VECTOR SEARCH", "MATCH", "SPARQL",
    ];
    let operators = ["=", "!=", ">", "<", ">=", "<=", "AND", "OR"];
    let mut query = String::new();
    let num_tokens = rng.gen_range(1..8);

    for i in 0..num_tokens {
        if i > 0 {
            query.push(' ');
        }
        if rng.gen_bool(0.6) {
            // Use a keyword
            query.push_str(keywords[rng.gen_range(0..keywords.len())]);
        } else if rng.gen_bool(0.5) {
            // Use an operator
            query.push_str(operators[rng.gen_range(0..operators.len())]);
        } else {
            // Use a random identifier/literal
            let kind = rng.gen_range(0..4);
            match kind {
                0 => {
                    // Table/column name
                    let name = random_string(rng, 10);
                    query.push_str(&name);
                }
                1 => {
                    // Number
                    query.push_str(&rng.gen_range(0..10000).to_string());
                }
                2 => {
                    // String literal
                    query.push('\'');
                    query.push_str(&random_string(rng, 20));
                    query.push('\'');
                }
                _ => {
                    // Wildcard or parens
                    query.push('*', );
                }
            }
        }
    }
    query
}

#[test]
fn fuzz_sql_parser_random_strings() {
    let mut rng = rand::thread_rng();
    let iterations = 5000;

    for _ in 0..iterations {
        let input = random_string(&mut rng, 200);
        // Parser must never panic — Ok or Err is fine
        let _ = crate::QueryParser::parse(&input);
    }
}

#[test]
fn fuzz_sql_parser_biased_queries() {
    let mut rng = rand::thread_rng();
    let iterations = 5000;

    for _ in 0..iterations {
        let input = random_sql_biased(&mut rng);
        // Parser must never panic
        let _ = crate::QueryParser::parse(&input);
    }
}

#[test]
fn fuzz_sql_parser_edge_cases() {
    let edge_cases = [
        "",
        " ",
        "\n",
        "\t",
        ";",
        ";;;",
        "'",
        "''",
        "\"",
        "\"\"",
        "(",
        "()",
        "SELECT",
        "SELECT *",
        "SELECT * FROM",
        "SELECT * FROM t WHERE",
        "SELECT * FROM t WHERE 1=1; DROP TABLE t;--",
        "SELECT * FROM t WHERE x = 'unclosed",
        "SELECT * FROM t WHERE x = ''''''",
        &"A".repeat(10000),
        &"SELECT ".repeat(100),
        "\0\0\0",
        "🎉🎊",
        "%s%s%s%s",
        "NULL NULL NULL",
    ];

    for input in &edge_cases {
        let _ = crate::QueryParser::parse(input);
    }
}

#[test]
fn fuzz_sparql_parser_random_strings() {
    let mut rng = rand::thread_rng();
    let iterations = 3000;

    for _ in 0..iterations {
        let input = random_string(&mut rng, 200);
        let mut parser = crate::SparqlParser::new();
        let _ = parser.parse(&input);
    }
}

#[test]
fn fuzz_sparql_parser_biased_queries() {
    let mut rng = rand::thread_rng();
    let iterations = 3000;

    let sparql_keywords = [
        "SELECT", "WHERE", "PREFIX", "CONSTRUCT", "ASK", "DESCRIBE",
        "FILTER", "OPTIONAL", "UNION", "GRAPH", "LIMIT", "OFFSET",
        "ORDER BY", "DISTINCT", "?x", "?y", "?z", "<http://example.org>",
        "a", ".", "{", "}", "*",
    ];

    for _ in 0..iterations {
        let num_tokens = rng.gen_range(1..10);
        let input: String = (0..num_tokens)
            .map(|_| sparql_keywords[rng.gen_range(0..sparql_keywords.len())])
            .collect::<Vec<_>>()
            .join(" ");

        let mut parser = crate::SparqlParser::new();
        let _ = parser.parse(&input);
    }
}
