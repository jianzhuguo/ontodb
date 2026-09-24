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
// Copyright (c) 2024-2026 OntoDB Team
// Licensed under the Business Source License 1.1 (BUSL-1.1).
// See LICENSE for details. Change Date: 2031-09-15.
// On the Change Date, this file will be licensed under Apache License 2.0.
//! OntoDB Graph Module - Property Graph data model with traversal support.
//!
//! # Community Edition (Open Source)
//! - Property Graph with CRUD operations
//! - Graph traversal (BFS/DFS)
//! - Basic graph algorithms (PageRank, connected components, degree distribution, clustering)
//! - Cache infrastructure with LRU eviction
//! - Index support
//! - Ontology parsing (OWL-lite)
//! - Graph visualization (DOT/D3/Cytoscape/Mermaid)
//!
//! # Enterprise Edition
//! - Distributed graph storage with consistent hashing
//! - Streaming graph computation with incremental algorithms
//! - Graph Neural Network (GNN) integration
//! - Knowledge graph reasoning engine
//! - Advanced algorithms (Dijkstra, betweenness centrality, subgraph extraction)
//! - Parallel BFS (Rayon)
//! - Graph pattern matching (subgraph isomorphism)
//! - Compressed graph storage (CSR)
//!
//! Enable enterprise features with: `cargo build --features enterprise`

// ══════════════════════════════════════════════════════════════════════════════
// Community Edition Modules (Always Available)
// ══════════════════════════════════════════════════════════════════════════════

pub mod error;
pub mod model;
pub mod store;
pub mod traversal;

// Community analytics: basic algorithms only
pub mod analytics;

// Community visualization: graph export to DOT/D3/Cytoscape/Mermaid
pub mod visualization;

// ══════════════════════════════════════════════════════════════════════════════
// Enterprise Edition Modules (Requires `enterprise` feature flag)
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(feature = "enterprise")]
pub mod compressed;

#[cfg(feature = "enterprise")]
pub mod distributed;

#[cfg(feature = "enterprise")]
pub mod gnn;

#[cfg(feature = "enterprise")]
pub mod pattern;

#[cfg(feature = "enterprise")]
pub mod reasoning;

#[cfg(feature = "enterprise")]
pub mod streaming;

#[cfg(feature = "enterprise")]
pub mod streaming_compute;

// ══════════════════════════════════════════════════════════════════════════════
// Public API Re-exports
// ══════════════════════════════════════════════════════════════════════════════

// Community exports
pub use analytics::{
    average_clustering_coefficient, connected_components, degree_distribution, pagerank,
    DegreeDistribution,
};
pub use error::GraphError;
pub use model::{Edge, GraphElement, PropValue, PropertyMap, Vertex};
pub use store::{CacheStats, EdgeChange, GraphStore};
pub use traversal::{Direction, TraversalEngine, TraversalPath, TraversalResult};

// Enterprise-only exports
#[cfg(feature = "enterprise")]
pub use analytics::{
    betweenness_centrality, dijkstra, dijkstra_path, subgraph_by_edge_label, subgraph_extraction,
    DijkstraResult,
};

#[cfg(feature = "enterprise")]
pub use distributed::{DistributedGraph, GlobalStats};

#[cfg(feature = "enterprise")]
pub use gnn::{GcnModel, NodeEmbeddings};

#[cfg(feature = "enterprise")]
pub use reasoning::{Reasoner, Triple};

#[cfg(feature = "enterprise")]
pub use streaming::GraphStream;

#[cfg(feature = "enterprise")]
pub use streaming_compute::StreamingComputeEngine;

// Community visualization exports
pub use visualization::{to_dot, to_d3_json, to_cytoscape_json, to_mermaid, VisualConfig};

// ══════════════════════════════════════════════════════════════════════════════
// Edition Information
// ══════════════════════════════════════════════════════════════════════════════

/// Returns true if enterprise features are enabled.
pub fn is_enterprise() -> bool {
    cfg!(feature = "enterprise")
}

/// Returns the edition name.
pub fn edition() -> &'static str {
    if cfg!(feature = "enterprise") {
        "Enterprise"
    } else {
        "Community"
    }
}

/// Returns a list of available features.
pub fn available_features() -> Vec<&'static str> {
    #[allow(unused_mut)]
    let mut features = vec![
        "property_graph",
        "traversal",
        "pagerank",
        "connected_components",
        "degree_distribution",
        "clustering_coefficient",
        "cache_lru",
        "index_support",
        "visualization",
    ];

    #[cfg(feature = "enterprise")]
    {
        features.extend([
            "distributed",
            "streaming",
            "gnn",
            "reasoning",
            "pattern_matching",
            "compressed_csr",
            "parallel_bfs",
            "dijkstra",
            "betweenness_centrality",
            "subgraph_extraction",
        ]);
    }

    features
}