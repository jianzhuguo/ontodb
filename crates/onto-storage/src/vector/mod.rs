//! Vector index module for similarity search.
//!
//! Provides HNSW (Hierarchical Navigable Small World) indexing for
//! approximate nearest neighbor search, with support for
//! ontology-filtered vector retrieval.

pub mod distance;
pub mod hnsw;
pub mod manager;

pub use distance::DistanceMetric;
pub use hnsw::{HnswConfig, HnswIndex, SearchResult, VectorEntry};
pub use manager::VectorIndexManager;
