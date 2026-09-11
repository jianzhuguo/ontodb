//! Vector Database Enhancement - Design Document
//!
//! Enhancements:
//! 1. Cosine pre-normalization (zero-cost search)
//! 2. Incremental HNSW persistence
//! 3. K-Means vector clustering
//! 4. Multi-vector hybrid query support

// ═══════════════════════════════════════════════════════════════
// Enhancement 1: Cosine Pre-normalization
// ═══════════════════════════════════════════════════════════════
// File: crates/onto-storage/src/vector/normalize.rs (NEW)

//! Pre-normalize vectors on insert for zero-cost cosine distance.
//!
//! When cosine distance is used, we L2-normalize each vector on insert.
//! At search time, cosine_distance(a, b) = 1 - dot(a, b), which is
//! just a dot product — no sqrt, no norm computation.
//!
//! This gives ~2x speedup for cosine search on high-dimensional vectors.

use crate::vector::distance::DistanceMetric;

/// L2-normalize a vector in-place.
pub fn l2_normalize(v: &mut [f32]) {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 1e-12 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

/// Check if a metric benefits from pre-normalization.
pub fn should_normalize(metric: &DistanceMetric) -> bool {
    matches!(metric, DistanceMetric::Cosine)
}

/// Distance between pre-normalized vectors (cosine = 1 - dot).
pub fn normalized_cosine_distance(a: &[f32], b: &[f32]) -> f32 {
    1.0 - a.iter().zip(b.iter()).map(|(x, y)| x * y).sum::<f32>()
}

// ═══════════════════════════════════════════════════════════════
// Enhancement 2: Incremental HNSW Persistence
// ═══════════════════════════════════════════════════════════════
// File: crates/onto-storage/src/vector/incremental_persist.rs (NEW)

//! Incremental persistence for HNSW indexes.
//!
//! Instead of saving the entire graph on every flush, we track
//! dirty nodes and only persist the changes.
//!
//! Design:
//! - On insert: mark node as dirty
//! - On flush: save only dirty nodes + a version counter
//! - On load: replay base snapshot + incremental deltas

use std::collections::HashSet;

/// Tracks which HNSW nodes have been modified since last persist.
pub struct DirtyTracker {
    dirty_nodes: HashSet<usize>,
    base_version: u64,
    current_version: u64,
}

impl DirtyTracker {
    pub fn new() -> Self {
        Self {
            dirty_nodes: HashSet::new(),
            base_version: 0,
            current_version: 0,
        }
    }

    /// Mark a node as dirty (needs persistence).
    pub fn mark_dirty(&mut self, node_id: usize) {
        self.dirty_nodes.insert(node_id);
        self.current_version += 1;
    }

    /// Get all dirty node IDs.
    pub fn drain_dirty(&mut self) -> HashSet<usize> {
        let dirty = self.dirty_nodes.drain().collect();
        self.base_version = self.current_version;
        dirty
    }

    /// Check if there are dirty nodes.
    pub fn has_dirty(&self) -> bool {
        !self.dirty_nodes.is_empty()
    }

    /// Current version counter.
    pub fn version(&self) -> u64 {
        self.current_version
    }
}

/// Incremental persistence format:
/// [base_version: u64] [num_deltas: u32] [delta_1] [delta_2] ...
/// where each delta is: [node_id: u32] [neighbors_per_layer: u8]
///                       [layer_0_neighbors: Vec<u32>] ...
#[derive(Debug, Clone)]
pub struct HnswDelta {
    pub node_id: usize,
    pub neighbors_per_layer: Vec<Vec<usize>>,
}

// ═══════════════════════════════════════════════════════════════
// Enhancement 3: K-Means Vector Clustering
// ═══════════════════════════════════════════════════════════════
// File: crates/onto-storage/src/vector/cluster.rs (NEW)

//! K-Means clustering for vector data.
//!
//! Supports:
//! - Standard K-Means with k-means++ initialization
//! - Mini-batch K-Means for large datasets
//! - Cluster assignment as a virtual column

use rand::Rng;

/// A cluster centroid with its ID and member count.
#[derive(Debug, Clone)]
pub struct Cluster {
    pub id: usize,
    pub centroid: Vec<f32>,
    pub member_count: usize,
}

/// K-Means clustering result.
#[derive(Debug, Clone)]
pub struct ClusteringResult {
    pub clusters: Vec<Cluster>,
    pub assignments: Vec<usize>,  // vector_index -> cluster_id
    pub iterations: usize,
    pub converged: bool,
}

/// K-Means clusterer.
pub struct KMeans {
    k: usize,
    max_iterations: usize,
    tolerance: f32,
}

impl KMeans {
    pub fn new(k: usize) -> Self {
        Self {
            k,
            max_iterations: 100,
            tolerance: 1e-4,
        }
    }

    pub fn with_max_iterations(mut self, max: usize) -> Self {
        self.max_iterations = max;
        self
    }

    /// Run K-Means clustering on the given vectors.
    pub fn fit(&self, vectors: &[Vec<f32>]) -> ClusteringResult {
        if vectors.is_empty() || self.k == 0 {
            return ClusteringResult {
                clusters: Vec::new(),
                assignments: Vec::new(),
                iterations: 0,
                converged: true,
            };
        }

        let dim = vectors[0].len();
        let n = vectors.len();
        let k = self.k.min(n);

        // k-means++ initialization
        let mut centroids = self.kmeans_plus_plus_init(vectors, k);
        let mut assignments = vec![0usize; n];
        let mut converged = false;

        for iter in 0..self.max_iterations {
            // Assignment step
            for (i, v) in vectors.iter().enumerate() {
                assignments[i] = self.nearest_centroid(v, &centroids);
            }

            // Update step
            let mut new_centroids = vec![vec![0.0f32; dim]; k];
            let mut counts = vec![0usize; k];
            for (i, v) in vectors.iter().enumerate() {
                let c = assignments[i];
                counts[c] += 1;
                for j in 0..dim {
                    new_centroids[c][j] += v[j];
                }
            }
            for c in 0..k {
                if counts[c] > 0 {
                    for j in 0..dim {
                        new_centroids[c][j] /= counts[c] as f32;
                    }
                }
            }

            // Check convergence
            let mut max_shift = 0.0f32;
            for c in 0..k {
                let shift = l2_distance(&centroids[c], &new_centroids[c]);
                max_shift = max_shift.max(shift);
            }
            centroids = new_centroids;

            if max_shift < self.tolerance {
                converged = true;
                return ClusteringResult {
                    clusters: centroids.into_iter().enumerate().map(|(id, centroid)| {
                        Cluster { id, centroid, member_count: 0 }
                    }).collect(),
                    assignments,
                    iterations: iter + 1,
                    converged,
                };
            }
        }

        ClusteringResult {
            clusters: centroids.into_iter().enumerate().map(|(id, centroid)| {
                Cluster { id, centroid, member_count: 0 }
            }).collect(),
            assignments,
            iterations: self.max_iterations,
            converged,
        }
    }

    /// k-means++ initialization for better convergence.
    fn kmeans_plus_plus_init(&self, vectors: &[Vec<f32>], k: usize) -> Vec<Vec<f32>> {
        let mut rng = rand::thread_rng();
        let mut centroids = Vec::with_capacity(k);

        // First centroid: random
        let first = rng.gen_range(0..vectors.len());
        centroids.push(vectors[first].clone());

        // Remaining centroids: weighted by distance
        for _ in 1..k {
            let mut distances = Vec::with_capacity(vectors.len());
            let mut total = 0.0f32;
            for v in vectors {
                let min_dist = centroids.iter()
                    .map(|c| l2_distance(v, c))
                    .min_by(|a, b| a.partial_cmp(b).unwrap())
                    .unwrap_or(f32::MAX);
                distances.push(min_dist * min_dist);
                total += min_dist * min_dist;
            }

            // Weighted random selection
            let threshold = rng.gen::<f32>() * total;
            let mut cumulative = 0.0f32;
            for (i, d) in distances.iter().enumerate() {
                cumulative += d;
                if cumulative >= threshold {
                    centroids.push(vectors[i].clone());
                    break;
                }
            }
        }

        centroids
    }

    fn nearest_centroid(&self, v: &[f32], centroids: &[Vec<f32>]) -> usize {
        centroids.iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| {
                l2_distance(v, a).partial_cmp(&l2_distance(v, b)).unwrap()
            })
            .map(|(i, _)| i)
            .unwrap_or(0)
    }
}

fn l2_distance(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| (x - y) * (x - y)).sum::<f32>().sqrt()
}

// ═══════════════════════════════════════════════════════════════
// Enhancement 4: Multi-vector Hybrid Query
// ═══════════════════════════════════════════════════════════════
// File: crates/onto-storage/src/vector/hybrid.rs (NEW)

//! Multi-vector hybrid query support.
//!
//! Supports searching multiple vector columns simultaneously and
//! combining results with configurable weights.
//!
//! Example:
//!   SELECT * FROM Product
//!   VECTOR SEARCH (title_embedding, image_embedding)
//!   QUERY ([0.1, 0.2, ...], [0.3, 0.4, ...])
//!   WEIGHTS (0.7, 0.3)
//!   TOP 10;

use std::collections::HashMap;

/// A single vector search request for one column.
#[derive(Debug, Clone)]
pub struct VectorSearchSpec {
    pub column: String,
    pub query_vector: Vec<f32>,
    pub top_k: usize,
    pub weight: f32,  // weight for combining scores
}

/// Result from a multi-vector search.
#[derive(Debug, Clone)]
pub struct HybridSearchResult {
    pub doc_key: Vec<u8>,
    pub combined_score: f32,
    pub per_column_scores: HashMap<String, f32>,
}

/// Multi-vector search combiner.
pub struct HybridSearcher {
    specs: Vec<VectorSearchSpec>,
}

impl HybridSearcher {
    pub fn new(specs: Vec<VectorSearchSpec>) -> Self {
        Self { specs }
    }

    /// Combine results from multiple vector searches.
    /// Uses weighted reciprocal rank fusion (RRF).
    pub fn combine_results(
        &self,
        per_column_results: HashMap<String, Vec<(Vec<u8>, f32)>>,
    ) -> Vec<HybridSearchResult> {
        let mut doc_scores: HashMap<Vec<u8>, HashMap<String, f32>> = HashMap::new();

        // Collect per-column scores
        for spec in &self.specs {
            if let Some(results) = per_column_results.get(&spec.column) {
                for (rank, (doc_key, score)) in results.iter().enumerate() {
                    let rrf_score = 1.0 / (60.0 + rank as f32) * spec.weight;
                    doc_scores
                        .entry(doc_key.clone())
                        .or_default()
                        .insert(spec.column.clone(), rrf_score);
                }
            }
        }

        // Compute combined scores
        let mut results: Vec<HybridSearchResult> = doc_scores
            .into_iter()
            .map(|(doc_key, scores)| {
                let combined_score = scores.values().sum::<f32>();
                HybridSearchResult {
                    doc_key,
                    combined_score,
                    per_column_scores: scores,
                }
            })
            .collect();

        results.sort_by(|a, b| b.combined_score.partial_cmp(&a.combined_score).unwrap());
        results
    }
}

// ═══════════════════════════════════════════════════════════════
// Integration: Enhanced VectorIndexManager API
// ═══════════════════════════════════════════════════════════════

/// New methods to add to VectorIndexManager:

/// 1. Cluster a vector column
/// ```rust
/// pub fn cluster_vectors(
///     &self,
///     class: &str,
///     column: &str,
///     k: usize,
/// ) -> Result<ClusteringResult, String> {
///     let key = IndexKey { class: class.to_string(), column: column.to_string() };
///     let index = self.indexes.get(&key).ok_or("Index not found")?;
///     let vectors: Vec<Vec<f32>> = index.all_vectors().collect();
///     let clusterer = KMeans::new(k);
///     Ok(clusterer.fit(&vectors))
/// }
/// ```

/// 2. Multi-vector search
/// ```rust
/// pub fn search_multi(
///     &self,
///     class: &str,
///     specs: Vec<VectorSearchSpec>,
/// ) -> Result<Vec<HybridSearchResult>, String> {
///     let mut per_column_results = HashMap::new();
///     for spec in &specs {
///         let results = self.search(class, &spec.column, &spec.query_vector, spec.top_k * 3)?;
///         let scored: Vec<(Vec<u8>, f32)> = results.into_iter()
///             .map(|r| (r.entry.id.clone(), r.distance))
///             .collect();
///         per_column_results.insert(spec.column.clone(), scored);
///     }
///     let searcher = HybridSearcher::new(specs);
///     Ok(searcher.combine_results(per_column_results))
/// }
/// ```

/// 3. Pre-normalize vectors on insert (when metric is Cosine)
/// ```rust
/// pub fn index_vector_normalized(
///     &mut self,
///     doc_key: Vec<u8>,
///     class: &str,
///     column: &str,
///     mut vector: Vec<f32>,
///     metric: &DistanceMetric,
/// ) -> Result<(), String> {
///     if should_normalize(metric) {
///         l2_normalize(&mut vector);
///     }
///     self.index_vector(doc_key, class, column, vector)
/// }
/// ```
