//! onto-query: Query engine for OntoDB.
//!
//! Provides SQL parsing, semantic extensions, query optimization, and execution.

pub mod cache;
pub mod executor;
pub mod optimizer;
pub mod parser;
pub mod sparql;

#[cfg(test)]
mod concurrent_bench;
#[cfg(test)]
mod binary_row_bench;

pub use cache::{PlanCache, QueryCache, CacheStats};
pub use executor::{QueryExecutor, QueryResult};
pub use optimizer::{CostEstimate, CostModel, ExecutionPlan, PlanNode, QueryPlanner};
pub use parser::{QueryAst, QueryParser};
pub use sparql::{SparqlParser, SparqlQuery, SparqlResult};
