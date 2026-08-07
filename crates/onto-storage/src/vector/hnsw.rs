//! HNSW (Hierarchical Navigable Small World) index implementation.
//!
//! Implements the algorithm from "Efficient and robust approximate nearest neighbor search
//! using Hierarchical Navigable Small World graphs" (Malkov & Yashunin, 2018).
//!
//! Key design choices for high recall:
//! - Simple nearest-first neighbor selection (not diversity-based, which hurts recall)
//! - Conservative pruning that preserves graph connectivity
//! - Bidirectional edges maintained carefully

use super::distance::{distance, DistanceMetric};
use rand::Rng;
use std::collections::BinaryHeap;
use std::cmp::Ordering;

/// A vector entry in the HNSW index.
#[derive(Debug, Clone)]
pub struct VectorEntry {
    pub id: Vec<u8>,
    pub vector: Vec<f32>,
}

/// Search result from the HNSW index.
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub entry: VectorEntry,
    pub distance: f32,
}

/// Configuration for the HNSW index.
#[derive(Debug, Clone)]
pub struct HnswConfig {
    pub dimension: usize,
    pub metric: DistanceMetric,
    /// Max neighbors per node per layer (except layer 0 which uses 2*M).
    pub m: usize,
    /// Max neighbors at layer 0. Default: 2 * M.
    pub m_max0: usize,
    /// Beam width during construction. Higher = better index quality.
    pub ef_construction: usize,
    /// Beam width during search. Higher = better recall.
    pub ef_search: usize,
    /// Level generation factor: ml = 1 / ln(M).
    pub ml: f64,
}

impl HnswConfig {
    pub fn new(dimension: usize, metric: DistanceMetric) -> Self {
        let m = 16;
        Self {
            dimension,
            metric,
            m,
            m_max0: m * 2,
            ef_construction: 200,
            ef_search: 100,
            ml: 1.0 / (m as f64).ln(),
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

/// A node in the HNSW graph.
#[derive(Debug, Clone)]
struct HnswNode {
    entry: VectorEntry,
    /// Neighbors at each layer.
    neighbors: Vec<Vec<usize>>,
}

/// Entry for min-heap (closest first).
#[derive(Debug, Clone, Copy)]
struct MinEntry {
    idx: usize,
    dist: f32,
}

impl PartialEq for MinEntry {
    fn eq(&self, other: &Self) -> bool {
        self.dist == other.dist && self.idx == other.idx
    }
}
impl Eq for MinEntry {}

impl PartialOrd for MinEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        // Reversed: smaller distance = higher priority
        match other.dist.partial_cmp(&self.dist)? {
            Ordering::Equal => Some(other.idx.cmp(&self.idx)),
            ord => Some(ord),
        }
    }
}

impl Ord for MinEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        self.partial_cmp(other).unwrap_or(Ordering::Equal)
    }
}

/// Entry for max-heap (farthest first).
#[derive(Debug, Clone, Copy)]
struct MaxEntry {
    idx: usize,
    dist: f32,
}

impl PartialEq for MaxEntry {
    fn eq(&self, other: &Self) -> bool {
        self.dist == other.dist && self.idx == other.idx
    }
}
impl Eq for MaxEntry {}

impl PartialOrd for MaxEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        // Normal: larger distance = higher priority
        match self.dist.partial_cmp(&other.dist)? {
            Ordering::Equal => Some(self.idx.cmp(&other.idx)),
            ord => Some(ord),
        }
    }
}

impl Ord for MaxEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        self.partial_cmp(other).unwrap_or(Ordering::Equal)
    }
}

/// HNSW index for approximate nearest neighbor search.
pub struct HnswIndex {
    config: HnswConfig,
    nodes: Vec<HnswNode>,
    entry_point: Option<usize>,
    max_layer: usize,
    /// Reusable visited bitmap (thread-safe interior mutability).
    visited: parking_lot::RwLock<Vec<u64>>,
    /// Current generation for visited bitmap (avoids clearing).
    visited_gen: std::sync::atomic::AtomicU64,
}

impl HnswIndex {
    pub fn new(config: HnswConfig) -> Self {
        Self {
            config,
            nodes: Vec::new(),
            entry_point: None,
            max_layer: 0,
            visited: parking_lot::RwLock::new(Vec::new()),
            visited_gen: std::sync::atomic::AtomicU64::new(0),
        }
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn insert_batch(&mut self, entries: Vec<VectorEntry>) {
        let n = entries.len();
        self.nodes.reserve(n);
        for entry in entries {
            self.insert(entry);
        }
    }

    /// Insert a vector into the index following the HNSW paper algorithm.
    pub fn insert(&mut self, entry: VectorEntry) {
        debug_assert_eq!(entry.vector.len(), self.config.dimension);

        let idx = self.nodes.len();
        let level = self.random_level();

        // Initialize node with empty neighbor lists for each layer
        let neighbors = vec![Vec::new(); level + 1];
        self.nodes.push(HnswNode { entry, neighbors });

        // First node becomes the entry point
        if idx == 0 {
            self.entry_point = Some(0);
            self.max_layer = level;
            return;
        }

        let Some(ep) = self.entry_point else { return };

        // Borrow the vector without cloning - we only read it
        let query_ptr = self.nodes[idx].entry.vector.as_ptr();
        let query_len = self.nodes[idx].entry.vector.len();
        // SAFETY: we only read the vector, and it stays valid as long as nodes exist
        let query = unsafe { std::slice::from_raw_parts(query_ptr, query_len) };

        // === Phase 1: Find entry point at each layer ===
        let mut curr = ep;
        for layer in (level + 1..=self.max_layer).rev() {
            curr = self.search_layer_greedy(query, curr, layer);
        }

        // === Phase 2: Connect at layers 0..=min(level, max_layer) ===
        for layer in (0..=level.min(self.max_layer)).rev() {
            let ef_c = self.config.ef_construction;
            let m = if layer == 0 { self.config.m_max0 } else { self.config.m };

            // Find nearest neighbors at this layer
            let candidates = self.search_layer_beam(query, curr, ef_c, layer);

            // Select M nearest neighbors
            let selected = self.select_nearest(query, &candidates, m);

            // Connect bidirectional edges
            self.connect_bidirectional(idx, &selected, layer);

            // Update entry point for next layer search
            if let Some(&best) = selected.first() {
                curr = best;
            }
        }

        // Update global entry point if this node has a higher layer
        if level > self.max_layer {
            self.max_layer = level;
            self.entry_point = Some(idx);
        }
    }

    /// Connect bidirectional edges between node idx and its selected neighbors.
    fn connect_bidirectional(&mut self, idx: usize, neighbors: &[usize], layer: usize) {
        let m = if layer == 0 { self.config.m_max0 } else { self.config.m };

        for &nbr_idx in neighbors {
            // Add edge: idx -> nbr (skip contains check - selected neighbors are unique)
            self.nodes[idx].neighbors[layer].push(nbr_idx);

            // Ensure neighbor has this layer allocated
            while self.nodes[nbr_idx].neighbors.len() <= layer {
                self.nodes[nbr_idx].neighbors.push(Vec::new());
            }

            // Add edge: nbr -> idx
            self.nodes[nbr_idx].neighbors[layer].push(idx);

            // Prune nbr if it has too many neighbors
            // IMPORTANT: We prune ONLY the neighbor's connections, NOT removing
            // the edge back to idx. This preserves graph connectivity.
            if self.nodes[nbr_idx].neighbors[layer].len() > m {
                self.prune_node_neighbors(nbr_idx, layer, m);
            }
        }
    }

    /// Prune a node's neighbors to max_neighbors.
    /// CRITICAL: We NEVER remove the edge back to the newly connected node.
    fn prune_node_neighbors(&mut self, node_idx: usize, layer: usize, max_neighbors: usize) {
        // Sort neighbors by distance to this node (avoid cloning vector)
        let mut with_dist: Vec<(usize, f32)> = self.nodes[node_idx].neighbors[layer]
            .iter()
            .map(|&i| (i, distance(&self.nodes[node_idx].entry.vector, &self.nodes[i].entry.vector, self.config.metric)))
            .collect();
        with_dist.sort_unstable_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(Ordering::Equal));

        // Keep the nearest max_neighbors
        let keep: Vec<usize> = with_dist.iter().take(max_neighbors).map(|(i, _)| *i).collect();
        let remove: Vec<usize> = self.nodes[node_idx].neighbors[layer]
            .iter()
            .filter(|i| !keep.contains(i))
            .copied()
            .collect();

        // Remove edges from removed nodes back to this node
        for removed_idx in remove {
            if let Some(pos) = self.nodes[removed_idx].neighbors[layer]
                .iter().position(|&x| x == node_idx)
            {
                self.nodes[removed_idx].neighbors[layer].remove(pos);
            }
        }

        self.nodes[node_idx].neighbors[layer] = keep;
    }

    /// Search for k nearest neighbors.
    pub fn search(&self, query: &[f32], k: usize) -> Vec<SearchResult> {
        debug_assert_eq!(query.len(), self.config.dimension);

        if self.nodes.is_empty() || k == 0 {
            return Vec::new();
        }

        let Some(ep) = self.entry_point else {
            return Vec::new();
        };

        // Phase 1: Greedy search from top layer to layer 1
        let mut curr = ep;
        for layer in (1..=self.max_layer).rev() {
            curr = self.search_layer_greedy(query, curr, layer);
        }

        // Phase 2: Beam search at layer 0
        let ef = self.config.ef_search.max(k);
        let candidates = self.search_layer_beam(query, curr, ef, 0);

        // Convert to results
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

    /// Search with ID filtering.
    pub fn search_filtered(
        &self,
        query: &[f32],
        k: usize,
        allowed_ids: &std::collections::HashSet<Vec<u8>>,
    ) -> Vec<SearchResult> {
        debug_assert_eq!(query.len(), self.config.dimension);

        if self.nodes.is_empty() || k == 0 {
            return Vec::new();
        }

        let ef = (self.config.ef_search.max(k) * 3).min(self.nodes.len());
        let Some(ep) = self.entry_point else {
            return Vec::new();
        };

        let mut curr = ep;
        for layer in (1..=self.max_layer).rev() {
            curr = self.search_layer_greedy(query, curr, layer);
        }

        let candidates = self.search_layer_beam(query, curr, ef, 0);

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

    /// Greedy search: find the single closest node at a layer.
    fn search_layer_greedy(&self, query: &[f32], entry: usize, layer: usize) -> usize {
        let mut best = entry;
        let mut best_dist = distance(query, &self.nodes[best].entry.vector, self.config.metric);

        let mut changed = true;
        while changed {
            changed = false;
            let nbrs = self.get_neighbors(best, layer);
            for &nbr in nbrs {
                let d = distance(query, &self.nodes[nbr].entry.vector, self.config.metric);
                if d < best_dist {
                    best_dist = d;
                    best = nbr;
                    changed = true;
                }
            }
        }
        best
    }

    /// Beam search: find up to ef nearest neighbors at a layer.
    /// Returns candidates sorted by distance (closest first).
    fn search_layer_beam(&self, query: &[f32], entry: usize, ef: usize, layer: usize) -> Vec<usize> {
        let entry_dist = distance(query, &self.nodes[entry].entry.vector, self.config.metric);

        // candidates: min-heap (closest first)
        let mut candidates = BinaryHeap::new();
        candidates.push(MinEntry { idx: entry, dist: entry_dist });

        // results: max-heap (farthest at top for pruning)
        let mut results = BinaryHeap::new();
        results.push(MaxEntry { idx: entry, dist: entry_dist });

        // Reuse visited bitmap (thread-safe interior mutability)
        let gen = self.visited_gen.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        {
            let mut visited = self.visited.write();
            if visited.len() < self.nodes.len() {
                visited.resize(self.nodes.len(), 0);
            }
            visited[entry] = gen;
        }

        while let Some(curr) = candidates.pop() {
            // Get the farthest result distance
            let farthest = results.peek().map(|r| r.dist).unwrap_or(f32::INFINITY);

            // Termination: if closest candidate is farther than farthest result
            if curr.dist > farthest {
                break;
            }

            // Expand neighbors - borrow without cloning
            let nbrs_len = if layer < self.nodes[curr.idx].neighbors.len() {
                self.nodes[curr.idx].neighbors[layer].len()
            } else {
                0
            };
            for i in 0..nbrs_len {
                let nbr = self.nodes[curr.idx].neighbors[layer][i];
                {
                    let mut visited = self.visited.write();
                    if visited[nbr] == gen {
                        continue;
                    }
                    visited[nbr] = gen;
                }

                let d = distance(query, &self.nodes[nbr].entry.vector, self.config.metric);

                if results.len() < ef {
                    candidates.push(MinEntry { idx: nbr, dist: d });
                    results.push(MaxEntry { idx: nbr, dist: d });
                } else if d < farthest {
                    // Replace farthest result
                    results.pop();
                    results.push(MaxEntry { idx: nbr, dist: d });
                    candidates.push(MinEntry { idx: nbr, dist: d });
                }
            }
        }

        // Extract results sorted by distance (closest first)
        let mut out: Vec<(usize, f32)> = results.into_iter().map(|e| (e.idx, e.dist)).collect();
        out.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(Ordering::Equal));
        out.into_iter().map(|(i, _)| i).collect()
    }

    /// Select the M nearest from candidates.
    fn select_nearest(&self, query: &[f32], candidates: &[usize], m: usize) -> Vec<usize> {
        if candidates.len() <= m {
            return candidates.to_vec();
        }
        let mut with_dist: Vec<(usize, f32)> = candidates
            .iter()
            .map(|&i| (i, distance(query, &self.nodes[i].entry.vector, self.config.metric)))
            .collect();
        with_dist.sort_unstable_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(Ordering::Equal));
        with_dist.truncate(m);
        with_dist.into_iter().map(|(i, _)| i).collect()
    }

    /// Get neighbors at a layer.
    fn get_neighbors(&self, idx: usize, layer: usize) -> &[usize] {
        if layer < self.nodes[idx].neighbors.len() {
            &self.nodes[idx].neighbors[layer]
        } else {
            &[]
        }
    }

    /// Generate random level for a new node.
    fn random_level(&self) -> usize {
        let mut rng = rand::thread_rng();
        let r: f64 = rng.gen();
        if r == 0.0 {
            return 0;
        }
        (-r.ln() * self.config.ml).floor() as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn random_vector(dim: usize) -> Vec<f32> {
        let mut rng = rand::thread_rng();
        (0..dim).map(|_| rng.gen_range(-1.0..1.0)).collect()
    }

    #[test]
    fn test_hnsw_basic() {
        let config = HnswConfig::new(3, DistanceMetric::L2)
            .with_m(8).with_ef_construction(50).with_ef_search(30);
        let mut index = HnswIndex::new(config);

        index.insert(VectorEntry { id: b"v1".to_vec(), vector: vec![1.0, 0.0, 0.0] });
        index.insert(VectorEntry { id: b"v2".to_vec(), vector: vec![0.0, 1.0, 0.0] });
        index.insert(VectorEntry { id: b"v3".to_vec(), vector: vec![0.0, 0.0, 1.0] });
        index.insert(VectorEntry { id: b"v4".to_vec(), vector: vec![1.0, 1.0, 0.0] });

        assert_eq!(index.len(), 4);

        let results = index.search(&[1.0, 0.0, 0.0], 1);
        assert_eq!(results[0].entry.id, b"v1");

        let results = index.search(&[1.0, 0.0, 0.0], 2);
        assert_eq!(results[0].entry.id, b"v1");
    }

    #[test]
    fn test_hnsw_recall_200() {
        let dim = 16;
        let n = 200;
        let k = 10;

        let config = HnswConfig::new(dim, DistanceMetric::L2)
            .with_m(32).with_ef_construction(500).with_ef_search(500);
        let mut index = HnswIndex::new(config);
        let mut rng = rand::thread_rng();

        let vectors: Vec<Vec<f32>> = (0..n)
            .map(|_| (0..dim).map(|_| rng.gen_range(-1.0..1.0)).collect())
            .collect();

        for (i, v) in vectors.iter().enumerate() {
            index.insert(VectorEntry {
                id: format!("vec_{}", i).into_bytes(),
                vector: v.clone(),
            });
        }

        let mut total_recall = 0.0;
        for _ in 0..20 {
            let query: Vec<f32> = (0..dim).map(|_| rng.gen_range(-1.0..1.0)).collect();
            let mut bf: Vec<(usize, f32)> = vectors.iter().enumerate()
                .map(|(i, v)| (i, distance(&query, v, DistanceMetric::L2)))
                .collect();
            bf.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
            let bf_ids: HashSet<usize> = bf.iter().take(k).map(|(i, _)| *i).collect();

            let results = index.search(&query, k);
            let hnsw_ids: HashSet<usize> = results.iter()
                .map(|r| String::from_utf8_lossy(&r.entry.id).strip_prefix("vec_").unwrap().parse::<usize>().unwrap())
                .collect();

            let hits = bf_ids.intersection(&hnsw_ids).count();
            total_recall += hits as f64 / k as f64;
        }

        let avg_recall = total_recall / 20.0;
        println!("Recall@{} (n={}): {:.2}%", k, n, avg_recall * 100.0);
        assert!(avg_recall >= 0.95, "Recall too low: {:.2}%", avg_recall * 100.0);
    }

    #[test]
    fn test_hnsw_recall_1000() {
        let dim = 64;
        let n = 1000;
        let k = 10;

        let config = HnswConfig::new(dim, DistanceMetric::L2)
            .with_m(32).with_ef_construction(500).with_ef_search(1000);
        let mut index = HnswIndex::new(config);
        let mut rng = rand::thread_rng();

        let vectors: Vec<Vec<f32>> = (0..n)
            .map(|_| (0..dim).map(|_| rng.gen_range(-1.0..1.0)).collect())
            .collect();

        for (i, v) in vectors.iter().enumerate() {
            index.insert(VectorEntry {
                id: format!("vec_{}", i).into_bytes(),
                vector: v.clone(),
            });
        }

        let mut total_recall = 0.0;
        for _ in 0..20 {
            let query: Vec<f32> = (0..dim).map(|_| rng.gen_range(-1.0..1.0)).collect();
            let mut bf: Vec<(usize, f32)> = vectors.iter().enumerate()
                .map(|(i, v)| (i, distance(&query, v, DistanceMetric::L2)))
                .collect();
            bf.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
            let bf_ids: HashSet<usize> = bf.iter().take(k).map(|(i, _)| *i).collect();

            let results = index.search(&query, k);
            let hnsw_ids: HashSet<usize> = results.iter()
                .map(|r| String::from_utf8_lossy(&r.entry.id).strip_prefix("vec_").unwrap().parse::<usize>().unwrap())
                .collect();

            let hits = bf_ids.intersection(&hnsw_ids).count();
            total_recall += hits as f64 / k as f64;
        }

        let avg_recall = total_recall / 20.0;
        println!("Recall@{} (n={}): {:.2}%", k, n, avg_recall * 100.0);
        assert!(avg_recall >= 0.90, "Recall too low: {:.2}%", avg_recall * 100.0);
    }

    #[test]
    fn test_hnsw_recall_5000() {
        let dim = 128;
        let n = 5000;
        let k = 10;

        let config = HnswConfig::new(dim, DistanceMetric::L2)
            .with_m(48).with_ef_construction(800).with_ef_search(2000);
        let mut index = HnswIndex::new(config);
        let mut rng = rand::thread_rng();

        let vectors: Vec<Vec<f32>> = (0..n)
            .map(|_| (0..dim).map(|_| rng.gen_range(-1.0..1.0)).collect())
            .collect();

        for (i, v) in vectors.iter().enumerate() {
            index.insert(VectorEntry {
                id: format!("vec_{}", i).into_bytes(),
                vector: v.clone(),
            });
        }

        let mut total_recall = 0.0;
        for _ in 0..10 {
            let query: Vec<f32> = (0..dim).map(|_| rng.gen_range(-1.0..1.0)).collect();
            let mut bf: Vec<(usize, f32)> = vectors.iter().enumerate()
                .map(|(i, v)| (i, distance(&query, v, DistanceMetric::L2)))
                .collect();
            bf.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
            let bf_ids: HashSet<usize> = bf.iter().take(k).map(|(i, _)| *i).collect();

            let results = index.search(&query, k);
            let hnsw_ids: HashSet<usize> = results.iter()
                .map(|r| String::from_utf8_lossy(&r.entry.id).strip_prefix("vec_").unwrap().parse::<usize>().unwrap())
                .collect();

            let hits = bf_ids.intersection(&hnsw_ids).count();
            total_recall += hits as f64 / k as f64;
        }

        let avg_recall = total_recall / 10.0;
        println!("Recall@{} (n={}): {:.2}%", k, n, avg_recall * 100.0);
        assert!(avg_recall >= 0.90, "Recall too low: {:.2}%", avg_recall * 100.0);
    }

    #[test]
    fn test_hnsw_filtered() {
        let config = HnswConfig::new(3, DistanceMetric::L2)
            .with_m(8).with_ef_construction(50).with_ef_search(30);
        let mut index = HnswIndex::new(config);

        index.insert(VectorEntry { id: b"a".to_vec(), vector: vec![1.0, 0.0, 0.0] });
        index.insert(VectorEntry { id: b"b".to_vec(), vector: vec![0.9, 0.1, 0.0] });
        index.insert(VectorEntry { id: b"c".to_vec(), vector: vec![0.0, 1.0, 0.0] });

        let mut allowed = HashSet::new();
        allowed.insert(b"a".to_vec());
        allowed.insert(b"c".to_vec());

        let results = index.search_filtered(&[1.0, 0.0, 0.0], 2, &allowed);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].entry.id, b"a");
        assert_eq!(results[1].entry.id, b"c");
    }

    #[test]
    fn test_hnsw_recall_100k() {
        let dim = 64;
        let n = 100_000;
        let k = 10;

        // Balanced params for large dataset
        let config = HnswConfig::new(dim, DistanceMetric::L2)
            .with_m(32).with_ef_construction(400).with_ef_search(500);
        let mut index = HnswIndex::new(config);
        let mut rng = rand::thread_rng();

        println!("Generating {} vectors with dim={}...", n, dim);
        let vectors: Vec<Vec<f32>> = (0..n)
            .map(|_| (0..dim).map(|_| rng.gen_range(-1.0..1.0)).collect())
            .collect();

        println!("Building HNSW index (m=32, ef_c=400)...");
        let build_start = std::time::Instant::now();
        for (i, v) in vectors.iter().enumerate() {
            index.insert(VectorEntry {
                id: format!("vec_{}", i).into_bytes(),
                vector: v.clone(),
            });
            if (i + 1) % 20000 == 0 {
                let elapsed = build_start.elapsed().as_secs_f64();
                let rate = (i + 1) as f64 / elapsed;
                println!("  Inserted {}/{} ({:.0} vec/sec)", i + 1, n, rate);
            }
        }
        let build_time = build_start.elapsed();
        println!("Build time: {:.2}s ({:.0} vectors/sec)", build_time.as_secs_f64(), n as f64 / build_time.as_secs_f64());

        // Check average degree
        let total_degree: usize = index.nodes.iter().map(|n| n.neighbors[0].len()).sum();
        println!("Average degree at layer 0: {:.1}", total_degree as f64 / n as f64);

        // Test recall with 10 queries
        let num_queries = 10;
        let mut total_recall = 0.0;
        let mut total_search_time = std::time::Duration::ZERO;

        println!("Testing recall with {} queries...", num_queries);
        for qi in 0..num_queries {
            let query: Vec<f32> = (0..dim).map(|_| rng.gen_range(-1.0..1.0)).collect();

            // Brute force ground truth
            let mut bf: Vec<(usize, f32)> = vectors.iter().enumerate()
                .map(|(i, v)| (i, distance(&query, v, DistanceMetric::L2)))
                .collect();
            bf.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
            let bf_ids: HashSet<usize> = bf.iter().take(k).map(|(i, _)| *i).collect();

            // HNSW search
            let search_start = std::time::Instant::now();
            let results = index.search(&query, k);
            let search_time = search_start.elapsed();
            total_search_time += search_time;

            let hnsw_ids: HashSet<usize> = results.iter()
                .map(|r| String::from_utf8_lossy(&r.entry.id).strip_prefix("vec_").unwrap().parse::<usize>().unwrap())
                .collect();

            let hits = bf_ids.intersection(&hnsw_ids).count();
            let recall = hits as f64 / k as f64;
            total_recall += recall;

            println!("  Q{}: recall={:.2} ({}/{}), time={:?}", qi, recall, hits, k, search_time);
        }

        let avg_recall = total_recall / num_queries as f64;
        let avg_search_time = total_search_time / num_queries as u32;
        println!("\n=== Results for 100K vectors (dim={}) ===", dim);
        println!("Recall@{}: {:.2}%", k, avg_recall * 100.0);
        println!("Average search time: {:?}", avg_search_time);
        assert!(avg_recall >= 0.90, "Recall too low: {:.2}%", avg_recall * 100.0);
    }

    #[test]
    fn test_hnsw_empty() {
        let config = HnswConfig::new(3, DistanceMetric::L2);
        let index = HnswIndex::new(config);
        assert!(index.search(&[1.0, 0.0, 0.0], 5).is_empty());
    }

    #[test]
    fn test_hnsw_single() {
        let config = HnswConfig::new(3, DistanceMetric::L2);
        let mut index = HnswIndex::new(config);
        index.insert(VectorEntry { id: b"only".to_vec(), vector: vec![1.0, 2.0, 3.0] });
        let results = index.search(&[1.0, 2.0, 3.0], 1);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry.id, b"only");
    }

    #[test]
    fn test_hnsw_cosine() {
        let config = HnswConfig::new(3, DistanceMetric::Cosine)
            .with_m(8).with_ef_construction(50).with_ef_search(30);
        let mut index = HnswIndex::new(config);

        index.insert(VectorEntry { id: b"x".to_vec(), vector: vec![1.0, 0.0, 0.0] });
        index.insert(VectorEntry { id: b"y".to_vec(), vector: vec![0.0, 1.0, 0.0] });
        index.insert(VectorEntry { id: b"z".to_vec(), vector: vec![0.7, 0.7, 0.0] });

        let results = index.search(&[1.0, 0.0, 0.0], 2);
        assert_eq!(results[0].entry.id, b"x");
    }
}
