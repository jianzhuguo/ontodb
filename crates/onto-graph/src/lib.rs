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

//! OntoDB Graph Module - Property Graph data model with traversal support.
//!
//! Supports:
//! - Vertex/Edge CRUD operations
//! - Single-hop and multi-hop traversal (BFS/DFS)
//! - Property filtering on vertices and edges
//! - Integration with vector search for hybrid graph+vector queries

pub mod model;
pub mod store;
pub mod traversal;
pub mod error;

pub use model::{Vertex, Edge, GraphElement, PropertyMap, PropValue};
pub use store::GraphStore;
pub use traversal::{TraversalEngine, TraversalResult, TraversalPath, Direction};
pub use error::GraphError;
