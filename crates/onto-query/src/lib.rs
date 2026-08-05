//! onto-query: Query engine for OntoDB.
//!
//! Provides SQL parsing, semantic extensions, and query execution.

pub mod executor;
pub mod parser;

pub use executor::{QueryExecutor, QueryResult};
pub use parser::{QueryAst, QueryParser};
