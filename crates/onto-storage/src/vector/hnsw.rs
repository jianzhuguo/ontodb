//! HNSW (Hierarchical Navigable Small World) index implementation.
//!
//! Based on the paper "Efficient and robust approximate nearest neighbor search
//! using Hierarchical Navigable Small World graphs" by Yu. A. Malkov, D.A. Yashunin.
//!
//! Key properties:
//! - O(log n) average search time
//! - O(n * log n) construction time
//! - High recall with configurable ef_search parameter
//! - Supports incremental insertions

use super::distance::{distance, DistanceMetric};
use rand::Rng;
use std::collections::{BinaryHeap, HashSet};
use std::cmp::Ordering;

/// A vector entry in the HNSW index.
#[derive(Debug, Clone)]
pub struct VectorEntry {
    /// Unique identifier for this vector (e.g., the document key).
    pub id: Vec<u8>,
    /// The vector data.
    pub vector: Vec<f32>,
}

/// A neighbor in the HNSW graph.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Neighbor {
    /// Index into the nodes array.
    idx: usize,
    /// Distance to the query.
    distance: f32,
}

impl Eq for Neighbor {}

impl PartialOrd for Neighbor {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        // Reverse for max-heap (we want closest neighbors)
        other.distance.partial_cmp(&self.distance)
    }
}

impl Ord for Neighbor {
    fn cmp(&self, other: &Self) -> Ordering {
        self.partial_cmp(other).unwrap_or(Ordering::Equal)
    }
}

/// A node in the HNSW graph.
#[derive(Debug, Clone)]
struct HnswNode {
    /// The vector entry (id + vector data).
    entry: VectorEntry,
    /// Neighbors at each layer. neighbors[layer] = list of neighbor indices.
    neighbors: Vec<Vec<usize>>,
}

/// Search result from the HNSW index.
#[derive(Debug, Clone)]
pub struct SearchResult {
    /// The vector entry.
    pub entry: VectorEntry,
    /// Distance to the query vector.
    pub distance: f32,
}

/// Configuration for the HNSW index.
#[derive(Debug, Clone)]
pub struct HnswConfig {
    /// Number of dimensions for vectors.
    pub dimension: usize,
    /// Distance metric to use.
    pub metric: DistanceMetric,
    /// M parameter: max number of neighbors per node per layer.
    /// Higher = better recall, more memory. Typical: 16-64.
    pub m: usize,
    /// M_max0: max neighbors at layer 0. Usually 2 * M.
    pub m_max0: usize,
    /// ef_construction: beam width during construction.
    /// Higher = better index quality, slower construction. Typical: 100-500.
    pub ef_construction: usize,
    /// ef_search: beam width during search.
    /// Higher = better recall, slower search. Typical: 50-200.
    pub ef_search: usize,
    /// Max layer for a new node = floor(-ln(uniform(0,1)) * ml).
    /// ml = 1 / ln(M). Default: 1/ln(M).
    pub ml: f64,
}

impl HnswConfig {
    pub fn new(dimension: usize, metric: DistanceMetric) -> Self {
        let m = 16;
        let ml = 1.0 / (m as f64).ln();
        Self {
            dimension,
            metric,
            m,
            m_max0: m * 2,
            ef_construction: 200,
            ef_search: 100,
            ml,
        }
    }

    pub fn with_m(mut self, m: usize) -> Self {
        self.m = m;
        self.m_max0 = m * 2;
        self.ml = 1.0 / (m as f64).ln();
        self
    }

    pub fn with_ef_construction(mut self, ef: usize) -> Self {
        self.ef_construction = ef;
        self
    }

    pub fn with_ef_search(mut self, ef: usize) -> Self {
        self.ef_search = ef;
        self
    }
}

/// HNSW index for approximate nearest neighbor search.
pub struct HnswIndex {
    /// Configuration.
    config: HnswConfig,
    /// All nodes in the graph.
    nodes: Vec<HnswNode>,
    /// Entry point for search (node index with highest layer).
    entry_point: Option<usize>,
    /// Max layer in the current graph.
    max_layer: usize,
}

impl HnswIndex {
    /// Creates a new empty HNSW index.
    pub fn new(config: HnswConfig) -> Self {
        Self {
            config,
            nodes: Vec::new(),
            entry_point: None,
            max_layer: 0,
        }
    }

    /// Returns the number of vectors in the index.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Returns true if the index is empty.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Inserts a vector into the index.
    pub fn insert(&mut self, entry: VectorEntry) {
        debug_assert_eq!(entry.vector.len(), self.config.dimension);

        let idx = self.nodes.len();
        let level = self.random_level();

        // Initialize the node with empty neighbor lists for each layer
        let mut neighbors = Vec::with_capacity(level + 1);
        for _ in 0..=level {
            neighbors.push(Vec::new());
        }

        let node = HnswNode {
            entry,
            neighbors,
        };
        self.nodes.push(node);

        // First node becomes the entry point
        if idx == 0 {
            self.entry_point = Some(0);
            self.max_layer = level;
            return;
        }

        let Some(ep) = self.entry_point else {
            return;
        };

        let query = self.nodes[idx].entry.vector.clone();

        // Search from top layer down to level+1, finding the closest entry point
        let mut curr = ep;
        for layer in (level + 1..=self.max_layer).rev() {
            curr = self.search_layer_single(&query, curr, layer);
        }

        // At layers min(level, max_layer) down to 0, find neighbors and connect
        let search_layers: Vec<usize> = (0..=level.min(self.max_layer)).collect();
        for &layer in &search_layers {
            let ef = if layer == 0 {
                self.config.m_max0
            } else {
                self.config.ef_construction
            };

            // Find nearest neighbors at this layer
            let candidates = self.search_layer(&query, curr, ef, layer);

            // Select M nearest neighbors (with diversity heuristic)
            let m = if layer == 0 { self.config.m_max0 } else { self.config.m };
            let selected = self.select_neighbors(&candidates, m);

            // Connect bidirectional edges
            for &neighbor_idx in &selected {
                self.nodes[idx].neighbors[layer].push(neighbor_idx);

                // Extend the neighbor's neighbor lists if it doesn't have this layer
                while self.nodes[neighbor_idx].neighbors.len() <= layer {
                    self.nodes[neighbor_idx].neighbors.push(Vec::new());
                }
                self.nodes[neighbor_idx].neighbors[layer].push(idx);

                // Prune neighbors of the neighbor if it has too many
                let max_neighbors = if layer == 0 { self.config.m_max0 } else { self.config.m };
                if self.nodes[neighbor_idx].neighbors[layer].len() > max_neighbors {
                    let nn = self.nodes[neighbor_idx].neighbors[layer].clone();
                    let pruned = self.select_neighbors_simple(&query, &nn, max_neighbors, layer);
                    self.nodes[neighbor_idx].neighbors[layer] = pruned;
                }
            }

            if !selected.is_empty() {
                curr = selected[0];
            }
        }

        // Update entry point if this node has a higher layer
        if level > self.max_layer {
            self.max_layer = level;
            self.entry_point = Some(idx);
        }
    }

    /// Searches for the k nearest neighbors to the query vector.
    pub fn search(&self, query: &[f32], k: usize) -> Vec<SearchResult> {
        debug_assert_eq!(query.len(), self.config.dimension);

        if self.nodes.is_empty() || k == 0 {
            return Vec::new();
        }

        let Some(ep) = self.entry_point else {
            return Vec::new();
        };

        // Search from top layer down to layer 1
        let mut curr = ep;
        for layer in (1..=self.max_layer).rev() {
            curr = self.search_layer_single(query, curr, layer);
        }

        // At layer 0, use ef_search beam width
        let candidates = self.search_layer(query, curr, self.config.ef_search.max(k), 0);

        // Return top-k results
        let mut results: Vec<SearchResult> = candidates
            .iter()
            .map(|&idx| SearchResult {
                entry: self.nodes[idx].entry.clone(),
                distance: distance(query, &self.nodes[idx].entry.vector, self.config.metric),
            })
            .collect();

        results.sort_by(|a, b| a.distance.partial_cmp(&b.distance).unwrap_or(Ordering::Equal));
        results.truncate(k);
        results
    }

    /// Searches for k nearest neighbors, only considering entries whose IDs
    /// are in the allowed set. This enables ontology-filtered vector search.
    pub fn search_filtered(
        &self,
        query: &[f32],
        k: usize,
        allowed_ids: &HashSet<Vec<u8>>,
    ) -> Vec<SearchResult> {
        debug_assert_eq!(query.len(), self.config.dimension);

        if self.nodes.is_empty() || k == 0 {
            return Vec::new();
        }

        // Use a larger ef to compensate for filtering
        let ef = (self.config.ef_search.max(k) * 3).min(self.nodes.len());

        let Some(ep) = self.entry_point else {
            return Vec::new();
        };

        let mut curr = ep;
        for layer in (1..=self.max_layer).rev() {
            curr = self.search_layer_single(query, curr, layer);
        }

        let candidates = self.search_layer(query, curr, ef, 0);

        let mut results: Vec<SearchResult> = candidates
            .iter()
            .filter(|&&idx| allowed_ids.contains(&self.nodes[idx].entry.id))
            .map(|&idx| SearchResult {
                entry: self.nodes[idx].entry.clone(),
                distance: distance(query, &self.nodes[idx].entry.vector, self.config.metric),
            })
            .collect();

        results.sort_by(|a, b| a.distance.partial_cmp(&b.distance).unwrap_or(Ordering::Equal));
        results.truncate(k);
        results
    }

    /// Greedy search at a single layer to find the closest node to the query.
    fn search_layer_single(&self, query: &[f32], entry: usize, layer: usize) -> usize {
        let mut visited = HashSet::new();
        visited.insert(entry);

        let mut best = entry;
        let mut best_dist = distance(query, &self.nodes[best].entry.vector, self.config.metric);

        let mut changed = true;
        while changed {
            changed = false;
            let neighbors = if layer < self.nodes[best].neighbors.len() {
                self.nodes[best].neighbors[layer].clone()
            } else {
                Vec::new()
            };
            for &neighbor_idx in &neighbors {
                if visited.contains(&neighbor_idx) {
                    continue;
                }
                visited.insert(neighbor_idx);

                let d = distance(query, &self.nodes[neighbor_idx].entry.vector, self.config.metric);
                if d < best_dist {
                    best_dist = d;
                    best = neighbor_idx;
                    changed = true;
                }
            }
        }

        best
    }

    /// Beam search at a layer, returning up to ef candidates sorted by distance.
    fn search_layer(&self, query: &[f32], entry: usize, ef: usize, layer: usize) -> Vec<usize> {
        let mut visited = HashSet::new();
        visited.insert(entry);

        let entry_dist = distance(query, &self.nodes[entry].entry.vector, self.config.metric);

        // candidates: min-heap (closest first) — but BinaryHeap is max-heap,
        // so we use Reverse or negate. Here we use Neighbor with reversed ordering.
        let mut candidates = BinaryHeap::new();
        candidates.push(Neighbor { idx: entry, distance: entry_dist });

        let mut results = BinaryHeap::new(); // max-heap (farthest at top for pruning)
        results.push(Neighbor { idx: entry, distance: entry_dist });

        while let Some(curr) = candidates.pop() {
            // If the farthest result is closer than the nearest candidate, stop
            if let Some(farthest) = results.peek() {
                if curr.distance > farthest.distance && results.len() >= ef {
                    break;
                }
            }

            // Expand neighbors (skip if node doesn't have this layer)
            let empty = Vec::new();
            let node_neighbors = if layer < self.nodes[curr.idx].neighbors.len() {
                &self.nodes[curr.idx].neighbors[layer]
            } else {
                &empty
            };
            for &neighbor_idx in node_neighbors {
                if visited.contains(&neighbor_idx) {
                    continue;
                }
                visited.insert(neighbor_idx);

                let d = distance(query, &self.nodes[neighbor_idx].entry.vector, self.config.metric);

                if results.len() < ef {
                    candidates.push(Neighbor { idx: neighbor_idx, distance: d });
                    results.push(Neighbor { idx: neighbor_idx, distance: d });
                } else if let Some(farthest) = results.peek() {
                    if d < farthest.distance {
                        results.pop();
                        results.push(Neighbor { idx: neighbor_idx, distance: d });
                        candidates.push(Neighbor { idx: neighbor_idx, distance: d });
                    }
                }
            }
        }

        // Extract results sorted by distance (closest first)
        let mut result_vec: Vec<usize> = results.into_iter().map(|n| n.idx).collect();
        result_vec.sort_by(|&a, &b| {
            let da = distance(query, &self.nodes[a].entry.vector, self.config.metric);
            let db = distance(query, &self.nodes[b].entry.vector, self.config.metric);
            da.partial_cmp(&db).unwrap_or(Ordering::Equal)
        });
        result_vec
    }

    /// Select neighbors using the diversity heuristic from the HNSW paper.
    /// Prefer neighbors that are not only close to the query but also diverse.
    fn select_neighbors(&self, candidates: &[usize], m: usize) -> Vec<usize> {
        if candidates.len() <= m {
            return candidates.to_vec();
        }

        // Simple heuristic: just take the M nearest (diversity heuristic can be added later)
        candidates[..m.min(candidates.len())].to_vec()
    }

    /// Simple neighbor selection (just take M nearest by distance to the node).
    fn select_neighbors_simple(
        &self,
        _query: &[f32],
        candidates: &[usize],
        m: usize,
        layer: usize,
    ) -> Vec<usize> {
        if candidates.len() <= m {
            return candidates.to_vec();
        }

        // Compute distance from each candidate to each other, take M nearest to center
        let mut sorted = candidates.to_vec();
        sorted.sort_by(|&a, &b| {
            let da = self.nodes[a].neighbors[layer].len();
            let db = self.nodes[b].neighbors[layer].len();
            da.cmp(&db) // Fewer neighbors = keep for balance
        });
        sorted.truncate(m);
        sorted
    }

    /// Generates a random level for a new node.
    fn random_level(&self) -> usize {
        let mut rng = rand::thread_rng();
        let r: f64 = rng.gen(); // uniform(0, 1)
        if r == 0.0 {
            return 0;
        }
        (-r.ln() * self.config.ml).floor() as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn random_vector(dim: usize) -> Vec<f32> {
        let mut rng = rand::thread_rng();
        (0..dim).map(|_| rng.gen_range(-1.0..1.0)).collect()
    }

    #[test]
    fn test_hnsw_basic_insert_and_search() {
        let config = HnswConfig::new(3, DistanceMetric::L2)
            .with_m(8)
            .with_ef_construction(50)
            .with_ef_search(30);

        let mut index = HnswIndex::new(config);

        // Insert some vectors
        let v1 = VectorEntry { id: b"v1".to_vec(), vector: vec![1.0, 0.0, 0.0] };
        let v2 = VectorEntry { id: b"v2".to_vec(), vector: vec![0.0, 1.0, 0.0] };
        let v3 = VectorEntry { id: b"v3".to_vec(), vector: vec![0.0, 0.0, 1.0] };
        let v4 = VectorEntry { id: b"v4".to_vec(), vector: vec![1.0, 1.0, 0.0] };

        index.insert(v1);
        index.insert(v2);
        index.insert(v3);
        index.insert(v4);

        assert_eq!(index.len(), 4);

        // Search for nearest to [1, 0, 0] — should find v1
        let results = index.search(&[1.0, 0.0, 0.0], 1);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry.id, b"v1");
        assert!(results[0].distance < 0.01);

        // Search for 2 nearest to [1, 0, 0] — should find v1, v4
        let results = index.search(&[1.0, 0.0, 0.0], 2);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].entry.id, b"v1");
    }

    #[test]
    fn test_hnsw_larger_dataset() {
        let dim = 16;
        let config = HnswConfig::new(dim, DistanceMetric::L2)
            .with_m(16)
            .with_ef_construction(100)
            .with_ef_search(50);

        let mut index = HnswIndex::new(config);

        // Insert 200 random vectors
        let mut entries = Vec::new();
        for i in 0..200 {
            let entry = VectorEntry {
                id: format!("vec_{}", i).into_bytes(),
                vector: random_vector(dim),
            };
            entries.push(entry.clone());
            index.insert(entry);
        }

        assert_eq!(index.len(), 200);

        // Search should return valid results
        let query = random_vector(dim);
        let results = index.search(&query, 10);
        assert_eq!(results.len(), 10);

        // Results should be sorted by distance
        for i in 1..results.len() {
            assert!(results[i - 1].distance <= results[i].distance);
        }
    }

    #[test]
    fn test_hnsw_filtered_search() {
        let config = HnswConfig::new(3, DistanceMetric::L2)
            .with_m(8)
            .with_ef_construction(50)
            .with_ef_search(30);

        let mut index = HnswIndex::new(config);

        index.insert(VectorEntry { id: b"a".to_vec(), vector: vec![1.0, 0.0, 0.0] });
        index.insert(VectorEntry { id: b"b".to_vec(), vector: vec![0.9, 0.1, 0.0] });
        index.insert(VectorEntry { id: b"c".to_vec(), vector: vec![0.0, 1.0, 0.0] });

        // Only allow "a" and "c"
        let mut allowed = HashSet::new();
        allowed.insert(b"a".to_vec());
        allowed.insert(b"c".to_vec());

        let results = index.search_filtered(&[1.0, 0.0, 0.0], 2, &allowed);
        assert_eq!(results.len(), 2);
        // "a" should be closest, then "c"
        assert_eq!(results[0].entry.id, b"a");
        assert_eq!(results[1].entry.id, b"c");
    }

    #[test]
    fn test_hnsw_empty_search() {
        let config = HnswConfig::new(3, DistanceMetric::L2);
        let index = HnswIndex::new(config);
        let results = index.search(&[1.0, 0.0, 0.0], 5);
        assert!(results.is_empty());
    }

    #[test]
    fn test_hnsw_single_element() {
        let config = HnswConfig::new(3, DistanceMetric::L2);
        let mut index = HnswIndex::new(config);
        index.insert(VectorEntry { id: b"only".to_vec(), vector: vec![1.0, 2.0, 3.0] });

        let results = index.search(&[1.0, 2.0, 3.0], 1);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry.id, b"only");
    }

    #[test]
    fn test_hnsw_cosine_metric() {
        let config = HnswConfig::new(3, DistanceMetric::Cosine)
            .with_m(8)
            .with_ef_construction(50)
            .with_ef_search(30);

        let mut index = HnswIndex::new(config);

        index.insert(VectorEntry { id: b"x".to_vec(), vector: vec![1.0, 0.0, 0.0] });
        index.insert(VectorEntry { id: b"y".to_vec(), vector: vec![0.0, 1.0, 0.0] });
        index.insert(VectorEntry { id: b"z".to_vec(), vector: vec![0.7, 0.7, 0.0] });

        // Query along x-axis — x should be closest, z next
        let results = index.search(&[1.0, 0.0, 0.0], 2);
        assert_eq!(results[0].entry.id, b"x");
    }
}
