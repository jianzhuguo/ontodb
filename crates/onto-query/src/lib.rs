#![allow(clippy::too_many_arguments)]
#![allow(clippy::type_complexity)]
#![allow(clippy::manual_strip)]
#![allow(clippy::needless_range_loop)]
#![allow(clippy::new_without_default)]
#![allow(clippy::collapsible_match)]
#![allow(clippy::if_same_then_else)]
#![allow(clippy::manual_checked_ops)]
#![allow(clippy::ptr_arg)]
#![allow(clippy::non_canonical_partial_ord_impl)]
#![allow(clippy::should_implement_trait)]
#![allow(clippy::sliced_string_as_bytes)]
#![allow(clippy::len_without_is_empty)]
#![allow(clippy::lines_filter_map_ok)]
#![allow(clippy::vec_init_then_push)]
#![allow(clippy::unnecessary_find_map)]
#![allow(clippy::unnecessary_unwrap)]
#![allow(clippy::result_large_err)]
#![allow(clippy::doc_lazy_continuation)]

//! onto-query: Query engine for OntoDB.
//!
//! Provides SQL parsing, semantic extensions, query optimization, and execution.

pub mod cache;
pub mod executor;
pub mod ontoql;
pub mod optimizer;
pub mod parser;
pub mod parser_util;
pub mod sparql;

#[cfg(test)]
mod concurrent_bench;
#[cfg(test)]
mod binary_row_bench;
#[cfg(test)]
mod fuzz_tests;

pub use cache::{PlanCache, QueryCache, CacheStats};
pub use executor::{QueryExecutor, QueryResult};
pub use ontoql::{OntoQLAst, OntoQLParser};
pub use optimizer::{CostEstimate, CostModel, ExecutionPlan, PlanNode, QueryPlanner};
pub use parser::{QueryAst, QueryParser};
pub use sparql::{SparqlParser, SparqlQuery, SparqlResult};
