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
