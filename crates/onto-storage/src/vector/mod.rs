// Copyright (c) 2024-2026 OntoDB Team
// Licensed under the Business Source License 1.1 (BUSL-1.1).
// See LICENSE for details. Change Date: 2031-09-15.
// On the Change Date, this file will be licensed under Apache License 2.0.
//! Vector index module for similarity search.
//!
//! Provides HNSW (Hierarchical Navigable Small World) indexing for
//! approximate nearest neighbor search, with support for
//! ontology-filtered vector retrieval.

pub mod cluster;
pub mod compaction;
pub mod distance;
pub mod hnsw;
pub mod hybrid;
pub mod incremental_persist;
pub mod manager;
pub mod normalize;
pub mod warmup;

pub use cluster::{Cluster, ClusteringResult, KMeans};
pub use compaction::{collect_live_entries, compact_index, CompactionResult};
pub use distance::DistanceMetric;
pub use hnsw::{HnswConfig, HnswIndex, SearchResult, VectorEntry};
pub use hybrid::{HybridSearchResult, HybridSearcher, VectorSearchSpec};
pub use incremental_persist::{DirtyTracker, IncrementalSnapshot, NodeDelta};
pub use manager::VectorIndexManager;
pub use normalize::{l2_normalize, normalized_cosine_distance, should_normalize};
pub use warmup::{WarmupConfig, WarmupManager};
