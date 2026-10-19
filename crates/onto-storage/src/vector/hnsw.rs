// Copyright (c) 2024-2026 OntoDB Team
// Licensed under the Business Source License 1.1 (BUSL-1.1).
// See LICENSE for details. Change Date: 2031-09-15.
// On the Change Date, this file will be licensed under Apache License 2.0.
//! HNSW (Hierarchical Navigable Small World) index implementation.
//!
//! Implements the algorithm from "Efficient and robust approximate nearest neighbor search
//! using Hierarchical Navigable Small World graphs" (Malkov & Yashunin, 2018).
//!
//! Key design choices for high recall:
//! - Simple nearest-first neighbor selection (not diversity-based, which hurts recall)
//! - Conservative pruning that preserves graph connectivity
//! - Bidirectional edges maintained carefully

use super::distance::{distance, hamming_packed, tanimoto_packed, DistanceMetric};
use rand::Rng;
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashSet};

/// Compact vector storage: bit-packed for binary fingerprints (128B for
/// 1024 bits) or dense f32 for general embeddings. Keeps the hot graph
/// structure small enough for millions of vectors.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum VecData {
    Dense(Vec<f32>),
    /// Bit-packed 0/1 vector, MSB-first within each byte.
    Packed(Vec<u8>),
}

impl VecData {
    /// Packs a 0/1 f32 vector; falls back to Dense for non-binary data.
    pub fn from_f32(vec: &[f32]) -> Self {
        let binary = vec.iter().all(|v| *v == 0.0 || *v == 1.0) && !vec.is_empty();
        if binary {
            let mut out = vec![0u8; vec.len().div_ceil(8)];
            for (i, v) in vec.iter().enumerate() {
                if *v != 0.0 {
                    out[i / 8] |= 1 << (7 - (i % 8));
                }
            }
            VecData::Packed(out)
        } else {
            VecData::Dense(vec.to_vec())
        }
    }

    /// Builds vector storage straight from a JSON column value.
    /// Base64 Bytes (bit-packed fingerprints) decode directly to Packed —
    /// no 1024-float round-trip.  Float arrays go through `from_f32`.
    pub fn from_json(val: &serde_json::Value, dim: usize) -> Option<Self> {
        match val {
            serde_json::Value::String(s) => {
                let packed = onto_core::binary_fp::b64_to_bytes(s)?;
                if packed.len() == dim.div_ceil(8) {
                    Some(VecData::Packed(packed))
                } else {
                    None
                }
            }
            serde_json::Value::Array(arr) => {
                let vec: Vec<f32> = arr
                    .iter()
                    .filter_map(|v| v.as_f64().map(|f| f as f32))
                    .collect();
                if vec.len() == dim {
                    Some(VecData::from_f32(&vec))
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    pub fn to_f32(&self, dim: usize) -> Vec<f32> {
        match self {
            VecData::Dense(v) => v.clone(),
            VecData::Packed(p) => {
                let mut out = Vec::with_capacity(dim);
                for i in 0..dim {
                    let bit = (p[i / 8] >> (7 - (i % 8))) & 1;
                    out.push(if bit == 1 { 1.0 } else { 0.0 });
                }
                out
            }
        }
    }

    /// Distance between two vectors; Packed pairs use bit ops (fast path).
    pub fn distance_to(&self, other: &VecData, metric: DistanceMetric) -> f32 {
        match (self, other) {
            (VecData::Packed(a), VecData::Packed(b)) => {
                if metric == DistanceMetric::Tanimoto {
                    tanimoto_packed(a, b)
                } else {
                    hamming_packed(a, b)
                }
            }
            (a, b) => {
                let dim = match (a, b) {
                    (VecData::Dense(x), _) => x.len(),
                    (_, VecData::Dense(y)) => y.len(),
                    (VecData::Packed(p), _) => p.len() * 8,
                };
                distance(&a.to_f32(dim), &b.to_f32(dim), metric)
            }
        }
    }

    /// True if this storage holds the same values as the f32 slice
    /// (used for stale-entry detection after updates).
    pub fn matches_f32(&self, vec: &[f32]) -> bool {
        match self {
            VecData::Dense(v) => {
                v.len() == vec.len() && v.iter().zip(vec.iter()).all(|(a, b)| (a - b).abs() <= 1e-6)
            }
            VecData::Packed(_) => {
                let other = VecData::from_f32(vec);
                match &other {
                    VecData::Packed(op) => match self {
                        VecData::Packed(p) => p == op,
                        VecData::Dense(_) => false,
                    },
                    VecData::Dense(o) => self.to_f32(vec.len()) == *o,
                }
            }
        }
    }
}

/// A vector entry in the HNSW index.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
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
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
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
        let m = 8;
        Self {
            dimension,
            metric,
            m,
            m_max0: m * 2,
            ef_construction: 128,
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
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct HnswNode {
    id: Vec<u8>,
    /// Compact vector storage (packed bits for binary fingerprints).
    data: VecData,
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

/// Serializable snapshot of an HNSW index.
#[derive(serde::Serialize, serde::Deserialize)]
struct HnswSnapshot {
    config: HnswConfig,
    nodes: Vec<HnswNode>,
    entry_point: Option<usize>,
    max_layer: usize,
}

/// HNSW index for approximate nearest neighbor search.
pub struct HnswIndex {
    config: HnswConfig,
    nodes: Vec<HnswNode>,
    entry_point: Option<usize>,
    max_layer: usize,
    /// Reusable visited bitmap (thread-safe interior mutability).
    #[allow(dead_code)]
    visited: parking_lot::RwLock<Vec<u64>>,
    /// Current generation for visited bitmap (avoids clearing).
    #[allow(dead_code)]
    visited_gen: std::sync::atomic::AtomicU64,
    /// Nodes modified since the last incremental persist.
    dirty: super::incremental_persist::DirtyTracker,
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
            dirty: super::incremental_persist::DirtyTracker::new(),
        }
    }

    /// Save the HNSW index to bytes (for persistence).
    ///
    /// Captures the full graph structure (nodes, neighbors, entry point)
    /// so the index can be restored without re-inserting all vectors.
    pub fn save_to_bytes(&self) -> Result<Vec<u8>, String> {
        let snapshot = HnswSnapshot {
            config: self.config.clone(),
            nodes: self.nodes.clone(),
            entry_point: self.entry_point,
            max_layer: self.max_layer,
        };
        bincode::serialize(&snapshot).map_err(|e| format!("Failed to serialize HNSW index: {}", e))
    }

    /// Load an HNSW index from bytes.
    ///
    /// Restores the full graph structure, avoiding the need to re-insert vectors.
    pub fn load_from_bytes(data: &[u8]) -> Result<Self, String> {
        let snapshot: HnswSnapshot = bincode::deserialize(data)
            .map_err(|e| format!("Failed to deserialize HNSW index: {}", e))?;

        Ok(Self {
            config: snapshot.config,
            nodes: snapshot.nodes,
            entry_point: snapshot.entry_point,
            max_layer: snapshot.max_layer,
            visited: parking_lot::RwLock::new(Vec::new()),
            visited_gen: std::sync::atomic::AtomicU64::new(0),
            dirty: super::incremental_persist::DirtyTracker::new(),
        })
    }

    pub fn config(&self) -> &HnswConfig {
        &self.config
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Collects every node id. Used by snapshot-restore backfill to skip
    /// vectors already present in the graph (`insert_data` does not dedup).
    pub fn id_set(&self) -> std::collections::HashSet<Vec<u8>> {
        self.nodes.iter().map(|n| n.id.clone()).collect()
    }

    /// Removes every node whose id is NOT in `keep`, in place.
    ///
    /// `insert_data` does not dedup, so one id can appear on several nodes
    /// (each fp UPDATE leaves the previous node behind as a stale leftover).
    /// Only **one** node per id survives:
    /// - if `prefer[id]` is given (the live `doc_vectors` entry), the node
    ///   whose `data` matches it wins
    /// - otherwise the **last** occurrence wins (highest node index =
    ///   most recently inserted)
    ///
    /// Guarantees:
    /// - exactly one node per surviving id (`nodes.len() == keep.len()`
    ///   when every kept id is present in the graph)
    /// - no neighbour list on **any** layer holds an index of a removed node
    /// - the bidirectional invariant holds: if `A.neighbors[l]` contains `B`
    ///   then `B.neighbors[l]` contains `A`
    /// - `entry_point` / `max_layer` are recomputed
    ///
    /// Returns the number of nodes removed.
    pub fn prune_ids(
        &mut self,
        keep: &std::collections::HashSet<Vec<u8>>,
        prefer: &std::collections::HashMap<Vec<u8>, VecData>,
    ) -> usize {
        use std::collections::HashMap;
        let n_before = self.nodes.len();

        // ---- Pass 1: choose at most one survivor per id ----
        // chosen[id] = (node_idx, rank) where higher rank wins on ties.
        // rank: 2 = exact data match with prefer, 1 = no prefer entry,
        // 0 = prefer given but data differs (fall back to last insert).
        let mut chosen: HashMap<&[u8], (usize, u8)> = HashMap::new();
        for (old_i, node) in self.nodes.iter().enumerate() {
            if !keep.contains(&node.id) {
                continue;
            }
            let rank = match prefer.get(&node.id) {
                Some(want) if *want == node.data => 2u8,
                Some(_) => 0u8,
                None => 1u8,
            };
            match chosen.get(node.id.as_slice()) {
                Some((_, best_rank)) if *best_rank > rank => {}
                Some((best_i, best_rank)) if *best_rank == rank && *best_i > old_i => {}
                _ => {
                    chosen.insert(node.id.as_slice(), (old_i, rank));
                }
            }
        }

        let mut remap: Vec<Option<usize>> = vec![None; n_before];
        // Emit in ascending old index so remap is built in one deterministic walk.
        let mut picks: Vec<usize> = chosen.values().map(|(old_i, _)| *old_i).collect();
        picks.sort_unstable();
        let mut survivors: Vec<HnswNode> = Vec::with_capacity(picks.len());
        for &old_i in &picks {
            remap[old_i] = Some(survivors.len());
            survivors.push(self.nodes[old_i].clone());
        }

        // ---- Pass 2: rewrite every layer's neighbour list (no dangling idx) ----
        for (my_new, node) in survivors.iter_mut().enumerate() {
            for layer in node.neighbors.iter_mut() {
                let mut out = Vec::with_capacity(layer.len());
                for &nb in layer.iter() {
                    match remap.get(nb).copied().flatten() {
                        Some(new_nb) if new_nb != my_new => out.push(new_nb),
                        _ => {}
                    }
                }
                out.sort_unstable();
                out.dedup();
                *layer = out;
            }
        }

        // ---- Pass 3: enforce bidirectionality on every layer ----
        {
            let mut lists: Vec<Vec<Vec<usize>>> = survivors
                .iter()
                .map(|nd| nd.neighbors.clone())
                .collect();

            let mut pending: Vec<(usize, usize, usize)> = Vec::new();
            for (a, layers) in lists.iter().enumerate() {
                for (l, nbs) in layers.iter().enumerate() {
                    for &b in nbs {
                        pending.push((a, l, b));
                    }
                }
            }
            for (a, l, b) in pending {
                if b >= lists.len() {
                    continue;
                }
                if l >= lists[b].len() {
                    lists[b].resize(l + 1, Vec::new());
                }
                if !lists[b][l].contains(&a) {
                    lists[b][l].push(a);
                }
            }

            for (node, mut layers) in survivors.iter_mut().zip(lists) {
                for layer in layers.iter_mut() {
                    layer.sort_unstable();
                    layer.dedup();
                }
                while layers.len() > 1 && layers.last().map(|l| l.is_empty()).unwrap_or(false) {
                    layers.pop();
                }
                if layers.is_empty() {
                    layers.push(Vec::new());
                }
                node.neighbors = layers;
            }
        }

        // ---- Pass 4: entry_point + max_layer ----
        let mut new_ep: Option<usize> =
            self.entry_point.and_then(|ep| remap.get(ep).copied().flatten());
        let mut new_max = 0usize;
        for nd in survivors.iter() {
            let lvl = nd.neighbors.len().saturating_sub(1);
            if lvl > new_max {
                new_max = lvl;
            }
        }
        if new_ep.is_none() {
            let mut best_lvl = 0usize;
            for (i, nd) in survivors.iter().enumerate() {
                let lvl = nd.neighbors.len().saturating_sub(1);
                if lvl >= best_lvl {
                    best_lvl = lvl;
                    new_ep = Some(i);
                }
            }
        }

        // ---- Swap in ----
        self.nodes = survivors;
        self.entry_point = new_ep;
        self.max_layer = new_max;
        // Indices all changed; drop stale dirty marks. Caller must do a full
        // `save_to_bytes` persist after prune (it does).
        self.dirty = super::incremental_persist::DirtyTracker::new();

        n_before.saturating_sub(self.nodes.len())
    }

    pub fn insert_batch(&mut self, entries: Vec<VectorEntry>) {
        let n = entries.len();
        self.nodes.reserve(n);
        for entry in entries {
            self.insert(entry);
        }
    }

    /// Insert with pre-converted vector data (avoids re-packing on bulk paths).
    pub fn insert_data(&mut self, id: Vec<u8>, data: VecData) {
        let idx = self.nodes.len();
        let level = self.random_level();
        let neighbors = vec![Vec::new(); level + 1];
        self.nodes.push(HnswNode {
            id,
            data,
            neighbors,
        });
        self.dirty.mark_dirty(idx);

        if idx == 0 {
            self.entry_point = Some(0);
            self.max_layer = level;
            return;
        }

        let Some(ep) = self.entry_point else { return };

        // Reuse the stored node data as the query (Packed clone is 128B).
        let query = self.nodes[idx].data.clone();

        // === Phase 1: Find entry point at each layer ===
        let mut curr = ep;
        for layer in (level + 1..=self.max_layer).rev() {
            curr = self.search_layer_greedy(&query, curr, layer);
        }

        // === Phase 2: Connect at layers 0..=min(level, max_layer) ===
        for layer in (0..=level.min(self.max_layer)).rev() {
            let ef_c = self.config.ef_construction;
            let m = if layer == 0 {
                self.config.m_max0
            } else {
                self.config.m
            };

            let candidates = self.search_layer_beam(&query, curr, ef_c, layer);
            let selected = self.select_nearest(&query, &candidates, m);
            self.connect_bidirectional(idx, &selected, layer);

            if let Some(&best) = selected.first() {
                curr = best;
            }
        }

        if level > self.max_layer {
            self.max_layer = level;
            self.entry_point = Some(idx);
        }
    }

    /// True when nodes have been modified since the last incremental persist.
    pub fn has_dirty_nodes(&self) -> bool {
        self.dirty.has_dirty()
    }

    /// Number of dirty (unpersisted) nodes.
    pub fn dirty_count(&self) -> usize {
        self.dirty.dirty_count()
    }

    /// Drop dirty marks without exporting (after a full snapshot that already
    /// captured every node). Without this, bulk-insert dirty sets never clear
    /// and persist_vector_deltas warns forever.
    pub fn clear_dirty(&mut self) {
        self.dirty = super::incremental_persist::DirtyTracker::new();
    }

    /// Serializes every dirty node (id + data + neighbors) and clears the
    /// dirty set.  The output feeds `IncrementalSnapshot` for delta persistence.
    pub fn export_dirty_nodes(&mut self) -> Vec<(u32, Vec<u8>)> {
        let ids = self.dirty.drain_dirty();
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(node) = self.nodes.get(id) {
                if let Ok(blob) = bincode::serialize(node) {
                    out.push((id as u32, blob));
                }
            }
        }
        out
    }

    /// Applies persisted node records (from `export_dirty_nodes` / snapshot
    /// deltas).  Overwrites existing nodes at that id or extends the node
    /// table for brand-new inserts.  Restores `entry_point`/`max_layer` when
    /// a node carries a higher layer.
    pub fn import_nodes(&mut self, nodes: &[(u32, Vec<u8>)]) {
        for (nid, blob) in nodes {
            let Ok(node) = bincode::deserialize::<HnswNode>(blob) else {
                continue;
            };
            let id = *nid as usize;
            // Keep the graph contiguous: grow with placeholder rows if needed.
            while self.nodes.len() <= id {
                self.nodes.push(HnswNode {
                    id: Vec::new(),
                    data: VecData::Packed(Vec::new()),
                    neighbors: Vec::new(),
                });
            }
            let level = node.neighbors.len().saturating_sub(1);
            self.nodes[id] = node;
            if self.entry_point.is_none() {
                self.entry_point = Some(id);
                self.max_layer = level;
            } else if level > self.max_layer {
                self.max_layer = level;
                self.entry_point = Some(id);
            }
        }
    }

    /// Insert a vector into the index following the HNSW paper algorithm.
    pub fn insert(&mut self, entry: VectorEntry) {
        debug_assert_eq!(entry.vector.len(), self.config.dimension);
        let data = VecData::from_f32(&entry.vector);
        self.insert_data(entry.id, data);
    }

    /// Connect bidirectional edges between node idx and its selected neighbors.
    fn connect_bidirectional(&mut self, idx: usize, neighbors: &[usize], layer: usize) {
        let m = if layer == 0 {
            self.config.m_max0
        } else {
            self.config.m
        };

        for &nbr_idx in neighbors {
            // Add edge: idx -> nbr (skip contains check - selected neighbors are unique)
            self.nodes[idx].neighbors[layer].push(nbr_idx);

            // Ensure neighbor has this layer allocated
            while self.nodes[nbr_idx].neighbors.len() <= layer {
                self.nodes[nbr_idx].neighbors.push(Vec::new());
            }

            // Add edge: nbr -> idx
            self.nodes[nbr_idx].neighbors[layer].push(idx);

            // Prune nbr if it has too many neighbors.
            // `idx` is protected: the reciprocal edge must survive this prune,
            // otherwise a new node attached to an already-saturated graph is
            // immediately evicted and becomes unreachable (self-recall 0).
            if self.nodes[nbr_idx].neighbors[layer].len() > m {
                self.prune_node_neighbors(nbr_idx, layer, m, Some(idx));
            }
        }
    }

    /// Prune a node's neighbors to max_neighbors using the diversity
    /// heuristic (same as select_nearest). Keeps a neighbor only if it is
    /// closer to this node than to any already-kept neighbor — preserves
    /// long-range cross-cluster edges that pure nearest-N would discard.
    ///
    /// `protected` (when given) is never dropped — it is the node we just
    /// connected and losing it would orphan that node on a saturated graph.
    ///
    /// The diversity test is `<=` not `<`: binary fingerprints produce many
    /// exact distance ties, and strict `<` systematically evicted newly
    /// inserted nodes during incremental backfill.
    fn prune_node_neighbors(
        &mut self,
        node_idx: usize,
        layer: usize,
        max_neighbors: usize,
        protected: Option<usize>,
    ) {
        // Sort neighbors by distance to this node
        let mut with_dist: Vec<(usize, f32)> = self.nodes[node_idx].neighbors[layer]
            .iter()
            .map(|&i| {
                (
                    i,
                    self.nodes[node_idx]
                        .data
                        .distance_to(&self.nodes[i].data, self.config.metric),
                )
            })
            .collect();
        with_dist.sort_unstable_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(Ordering::Equal));

        // Diversity heuristic: keep e only if d(e, node_idx) <= d(e, r) for
        // every already-kept r.  Falls back to nearest-remaining to fill up.
        let mut keep: Vec<usize> = Vec::with_capacity(max_neighbors);
        let mut rest: Vec<usize> = Vec::new();
        // Keep the protected neighbor first so it occupies a slot.
        if let Some(p) = protected {
            if with_dist.iter().any(|(i, _)| *i == p) {
                keep.push(p);
            }
        }
        for &(i, dist_self) in &with_dist {
            if Some(i) == protected {
                continue;
            }
            if keep.len() >= max_neighbors {
                rest.push(i);
                continue;
            }
            let diverse = keep.iter().all(|&k| {
                let d_ik = self.nodes[i].data.distance_to(&self.nodes[k].data, self.config.metric);
                dist_self <= d_ik
            });
            if diverse {
                keep.push(i);
            } else {
                rest.push(i);
            }
        }
        for i in rest {
            if keep.len() >= max_neighbors {
                break;
            }
            keep.push(i);
        }

        let remove: Vec<usize> = self.nodes[node_idx].neighbors[layer]
            .iter()
            .filter(|i| !keep.contains(i))
            .copied()
            .collect();

        // Remove edges from removed nodes back to this node
        for removed_idx in remove {
            if let Some(pos) = self.nodes[removed_idx].neighbors[layer]
                .iter()
                .position(|&x| x == node_idx)
            {
                self.nodes[removed_idx].neighbors[layer].remove(pos);
            }
        }

        self.nodes[node_idx].neighbors[layer] = keep;
    }

    /// Search for k nearest neighbors.
    pub fn search(&self, query: &[f32], k: usize) -> Vec<SearchResult> {
        let ef = self.config.ef_search;
        self.search_with_ef(query, k, ef)
    }

    /// Search with a caller-supplied ef_search (recall/latency knob at query
    /// time; no index rebuild required).
    pub fn search_with_ef(&self, query: &[f32], k: usize, ef_search: usize) -> Vec<SearchResult> {
        debug_assert_eq!(query.len(), self.config.dimension);

        if self.nodes.is_empty() || k == 0 {
            return Vec::new();
        }

        let Some(ep) = self.entry_point else {
            return Vec::new();
        };

        let qd = VecData::from_f32(query);

        // Phase 1: Greedy search from top layer to layer 1
        let mut curr = ep;
        for layer in (1..=self.max_layer).rev() {
            curr = self.search_layer_greedy(&qd, curr, layer);
        }

        // Phase 2: Beam search at layer 0
        let ef = ef_search.max(k);
        let candidates = self.search_layer_beam(&qd, curr, ef, 0);

        // Convert to results (expand packed→f32 only for the top-k)
        let dim = self.config.dimension;
        let mut results: Vec<SearchResult> = candidates
            .iter()
            .map(|&idx| SearchResult {
                entry: VectorEntry {
                    id: self.nodes[idx].id.clone(),
                    vector: self.nodes[idx].data.to_f32(dim),
                },
                distance: qd.distance_to(&self.nodes[idx].data, self.config.metric),
            })
            .collect();
        results.sort_by(|a, b| {
            a.distance
                .partial_cmp(&b.distance)
                .unwrap_or(Ordering::Equal)
        });
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

        let qd = VecData::from_f32(query);
        let dim = self.config.dimension;

        let mut curr = ep;
        for layer in (1..=self.max_layer).rev() {
            curr = self.search_layer_greedy(&qd, curr, layer);
        }

        let candidates = self.search_layer_beam(&qd, curr, ef, 0);

        let mut results: Vec<SearchResult> = candidates
            .iter()
            .filter(|&&idx| allowed_ids.contains(&self.nodes[idx].id))
            .map(|&idx| SearchResult {
                entry: VectorEntry {
                    id: self.nodes[idx].id.clone(),
                    vector: self.nodes[idx].data.to_f32(dim),
                },
                distance: qd.distance_to(&self.nodes[idx].data, self.config.metric),
            })
            .collect();

        results.sort_by(|a, b| {
            a.distance
                .partial_cmp(&b.distance)
                .unwrap_or(Ordering::Equal)
        });
        results.truncate(k);
        results
    }

    /// Greedy search: find the single closest node at a layer.
    fn search_layer_greedy(&self, query: &VecData, entry: usize, layer: usize) -> usize {
        let mut best = entry;
        let mut best_dist = query.distance_to(&self.nodes[best].data, self.config.metric);

        let mut changed = true;
        while changed {
            changed = false;
            let nbrs = self.get_neighbors(best, layer);
            for &nbr in nbrs {
                let d = query.distance_to(&self.nodes[nbr].data, self.config.metric);
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
    fn search_layer_beam(
        &self,
        query: &VecData,
        entry: usize,
        ef: usize,
        layer: usize,
    ) -> Vec<usize> {
        let entry_dist = query.distance_to(&self.nodes[entry].data, self.config.metric);

        // candidates: min-heap (closest first)
        let mut candidates = BinaryHeap::new();
        candidates.push(MinEntry {
            idx: entry,
            dist: entry_dist,
        });

        // results: max-heap (farthest at top for pruning)
        let mut results = BinaryHeap::new();
        results.push(MaxEntry {
            idx: entry,
            dist: entry_dist,
        });

        // Use local HashSet for visited tracking (avoids lock contention in hot loop)
        let mut visited = HashSet::new();
        visited.insert(entry);

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
                if !visited.insert(nbr) {
                    continue;
                }

                let d = query.distance_to(&self.nodes[nbr].data, self.config.metric);

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
    /// Select up to `m` neighbors from candidates using the HNSW diversity
    /// heuristic (paper Algorithm 4). A candidate is kept only if it is closer
    /// to the query than to any already-selected neighbor — this spreads edges
    /// across clusters and keeps the graph connected for clustered data.
    /// Falls back to nearest-remaining when the diversity filter is too strict.
    fn select_nearest(&self, query: &VecData, candidates: &[usize], m: usize) -> Vec<usize> {
        if candidates.len() <= m {
            return candidates.to_vec();
        }
        let mut with_dist: Vec<(usize, f32)> = candidates
            .iter()
            .map(|&i| (i, query.distance_to(&self.nodes[i].data, self.config.metric)))
            .collect();
        with_dist.sort_unstable_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(Ordering::Equal));

        // Diversity heuristic: keep candidate e only if d(e,q) <= d(e,r) for
        // every already-selected r.  This prevents the neighbor list from
        // collapsing into a single cluster. `<=` (not `<`) because binary
        // fingerprints produce many exact distance ties.
        let mut selected: Vec<usize> = Vec::with_capacity(m);
        let mut rest: Vec<usize> = Vec::new();
        for &(i, dist_q) in &with_dist {
            if selected.len() >= m {
                rest.push(i);
                continue;
            }
            let diverse = selected.iter().all(|&s| {
                let d_er = self.nodes[i].data.distance_to(&self.nodes[s].data, self.config.metric);
                dist_q <= d_er
            });
            if diverse {
                selected.push(i);
            } else {
                rest.push(i);
            }
        }
        // Fill remaining slots with nearest non-selected candidates.
        for i in rest {
            if selected.len() >= m {
                break;
            }
            selected.push(i);
        }
        selected
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

    #[allow(dead_code)]
    fn random_vector(dim: usize) -> Vec<f32> {
        let mut rng = rand::thread_rng();
        (0..dim).map(|_| rng.gen_range(-1.0..1.0)).collect()
    }

    #[test]
    fn test_hnsw_basic() {
        let config = HnswConfig::new(3, DistanceMetric::L2)
            .with_m(8)
            .with_ef_construction(50)
            .with_ef_search(30);
        let mut index = HnswIndex::new(config);

        index.insert(VectorEntry {
            id: b"v1".to_vec(),
            vector: vec![1.0, 0.0, 0.0],
        });
        index.insert(VectorEntry {
            id: b"v2".to_vec(),
            vector: vec![0.0, 1.0, 0.0],
        });
        index.insert(VectorEntry {
            id: b"v3".to_vec(),
            vector: vec![0.0, 0.0, 1.0],
        });
        index.insert(VectorEntry {
            id: b"v4".to_vec(),
            vector: vec![1.0, 1.0, 0.0],
        });

        assert_eq!(index.len(), 4);

        let results = index.search(&[1.0, 0.0, 0.0], 1);
        assert_eq!(results[0].entry.id, b"v1");

        let results = index.search(&[1.0, 0.0, 0.0], 2);
        assert_eq!(results[0].entry.id, b"v1");
    }

    #[test]
    fn test_hnsw_filtered() {
        let config = HnswConfig::new(3, DistanceMetric::L2)
            .with_m(8)
            .with_ef_construction(50)
            .with_ef_search(30);
        let mut index = HnswIndex::new(config);

        index.insert(VectorEntry {
            id: b"a".to_vec(),
            vector: vec![1.0, 0.0, 0.0],
        });
        index.insert(VectorEntry {
            id: b"b".to_vec(),
            vector: vec![0.9, 0.1, 0.0],
        });
        index.insert(VectorEntry {
            id: b"c".to_vec(),
            vector: vec![0.0, 1.0, 0.0],
        });

        let mut allowed = HashSet::new();
        allowed.insert(b"a".to_vec());
        allowed.insert(b"c".to_vec());

        let results = index.search_filtered(&[1.0, 0.0, 0.0], 2, &allowed);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].entry.id, b"a");
        assert_eq!(results[1].entry.id, b"c");
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
        index.insert(VectorEntry {
            id: b"only".to_vec(),
            vector: vec![1.0, 2.0, 3.0],
        });
        let results = index.search(&[1.0, 2.0, 3.0], 1);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry.id, b"only");
    }

    #[test]
    fn test_hnsw_cosine() {
        let config = HnswConfig::new(3, DistanceMetric::Cosine)
            .with_m(8)
            .with_ef_construction(50)
            .with_ef_search(30);
        let mut index = HnswIndex::new(config);

        index.insert(VectorEntry {
            id: b"x".to_vec(),
            vector: vec![1.0, 0.0, 0.0],
        });
        index.insert(VectorEntry {
            id: b"y".to_vec(),
            vector: vec![0.0, 1.0, 0.0],
        });
        index.insert(VectorEntry {
            id: b"z".to_vec(),
            vector: vec![0.7, 0.7, 0.0],
        });

        let results = index.search(&[1.0, 0.0, 0.0], 2);
        assert_eq!(results[0].entry.id, b"x");
    }

    // ── prune_ids: 6 boundary cases ─────────────────────────────────
    //
    // Guarantees under test:
    //   * no neighbour list on ANY layer holds a removed index (no dangling)
    //   * bidirectional: A→B at layer l implies B→A at layer l
    //   * entry_point / max_layer recomputed
    //   * idempotent

    /// Asserts the two structural invariants prune must maintain.
    fn assert_graph_invariants(index: &HnswIndex) {
        let n = index.nodes.len();
        for (i, nd) in index.nodes.iter().enumerate() {
            for (l, nbs) in nd.neighbors.iter().enumerate() {
                for &b in nbs {
                    assert!(b < n, "dangling index {b} (len {n}) in node {i} layer {l}");
                    assert_ne!(b, i, "self-loop at node {i} layer {l}");
                    // bidirectional
                    let b_layers = &index.nodes[b].neighbors;
                    assert!(
                        l < b_layers.len() && b_layers[l].contains(&i),
                        "missing reverse edge {b} -> {i} at layer {l}"
                    );
                }
            }
        }
        if let Some(ep) = index.entry_point {
            assert!(ep < n, "entry_point {ep} out of range (len {n})");
        }
    }

    fn make_index(ids: &[&str]) -> HnswIndex {
        let config = HnswConfig::new(2, DistanceMetric::L2)
            .with_m(4)
            .with_ef_construction(32)
            .with_ef_search(16);
        let mut index = HnswIndex::new(config);
        for (i, id) in ids.iter().enumerate() {
            let v = (i as f32) * 0.01;
            index.insert(VectorEntry {
                id: id.as_bytes().to_vec(),
                vector: vec![v, 1.0 - v],
            });
        }
        index
    }

    /// Case 1: keep = everything → node count and identity preserved.
    #[test]
    fn test_prune_keep_all_is_identity() {
        let mut index = make_index(&["a", "b", "c", "d", "e"]);
        let before = index.len();
        let keep: HashSet<Vec<u8>> = index.id_set();
        let removed = index.prune_ids(&keep, &std::collections::HashMap::new());
        assert_eq!(removed, 0);
        assert_eq!(index.len(), before);
        assert_graph_invariants(&index);
    }

    /// Case 2: keep = empty → empty graph, entry_point None, no panic.
    #[test]
    fn test_prune_keep_none_yields_empty() {
        let mut index = make_index(&["a", "b", "c"]);
        let removed = index.prune_ids(&HashSet::new(), &std::collections::HashMap::new());
        assert_eq!(removed, 3);
        assert_eq!(index.len(), 0);
        assert!(index.entry_point.is_none());
        assert_graph_invariants(&index);
    }

    /// Case 3: prune the entry_point node → entry migrates, max_layer shrinks.
    #[test]
    fn test_prune_entry_point_migrates() {
        let mut index = make_index(&["a", "b", "c", "d", "e", "f", "g", "h"]);
        let ep = index.entry_point.expect("built graph has an entry");
        let ep_id = index.nodes[ep].id.clone();
        let ep_level = index.nodes[ep].neighbors.len() - 1;
        assert_eq!(index.max_layer, ep_level, "entry should be at max_layer");

        let mut keep = index.id_set();
        keep.remove(&ep_id);
        let removed = index.prune_ids(&keep, &std::collections::HashMap::new());
        assert_eq!(removed, 1);

        let new_ep = index.entry_point.expect("survivors remain");
        assert_ne!(index.nodes[new_ep].id, ep_id, "old entry is gone");
        let new_level = index.nodes[new_ep].neighbors.len() - 1;
        assert!(
            new_level <= ep_level,
            "max_layer must shrink or stay ({} -> {})",
            ep_level,
            new_level
        );
        assert!(index.max_layer <= ep_level);
        assert_graph_invariants(&index);
    }

    /// Case 4: prune a bridge node between two clusters → no panic; both
    /// clusters stay internally reachable (search finds a same-cluster hit).
    #[test]
    fn test_prune_bridge_node_no_panic() {
        // Cluster L around (0,1), cluster R around (1,0), bridge around (0.5,0.5).
        let config = HnswConfig::new(2, DistanceMetric::L2)
            .with_m(4)
            .with_ef_construction(32)
            .with_ef_search(16);
        let mut index = HnswIndex::new(config);
        index.insert(VectorEntry { id: b"L0".to_vec(), vector: vec![0.0, 1.0] });
        index.insert(VectorEntry { id: b"L1".to_vec(), vector: vec![0.05, 0.95] });
        index.insert(VectorEntry { id: b"BR".to_vec(), vector: vec![0.5, 0.5] });
        index.insert(VectorEntry { id: b"R0".to_vec(), vector: vec![1.0, 0.0] });
        index.insert(VectorEntry { id: b"R1".to_vec(), vector: vec![0.95, 0.05] });

        let mut keep = index.id_set();
        keep.remove(b"BR".to_vec().as_slice());
        let removed = index.prune_ids(&keep, &std::collections::HashMap::new());
        assert_eq!(removed, 1);
        assert_eq!(index.len(), 4);
        assert_graph_invariants(&index);

        // Left cluster still finds its own points.
        let left = index.search(&[0.0, 1.0], 2);
        assert!(!left.is_empty());
        assert!(left.iter().any(|r| r.entry.id.starts_with(b"L")));
        // Right cluster still finds its own points.
        let right = index.search(&[1.0, 0.0], 2);
        assert!(!right.is_empty());
        assert!(right.iter().any(|r| r.entry.id.starts_with(b"R")));
    }

    /// Case 5: mixed-level deletion → EVERY layer is free of dangling indices.
    #[test]
    fn test_prune_cross_layer_no_dangling() {
        let mut index = make_index(&[
            "n0", "n1", "n2", "n3", "n4", "n5", "n6", "n7", "n8", "n9", "n10", "n11",
        ]);
        // Drop a spread: a high-layer candidate (early insert = often higher
        // level by chance is not guaranteed, so just drop a mix of ids) plus
        // pure layer-0 ones.
        let mut keep = index.id_set();
        for drop in ["n0", "n3", "n5", "n11"] {
            keep.remove(drop.as_bytes().to_vec().as_slice());
        }
        let removed = index.prune_ids(&keep, &std::collections::HashMap::new());
        assert_eq!(removed, 4);
        assert_eq!(index.len(), 8);
        assert_graph_invariants(&index);
        // Every node has at least one layer list.
        for nd in &index.nodes {
            assert!(!nd.neighbors.is_empty());
        }
    }

    /// Case 6: bidirectionality after prune + idempotent re-prune.
    #[test]
    fn test_prune_bidirectional_and_idempotent() {
        let mut index = make_index(&["a", "b", "c", "d", "e", "f", "g", "h", "i", "j"]);
        let mut keep = index.id_set();
        for drop in ["a", "e", "i"] {
            keep.remove(drop.as_bytes().to_vec().as_slice());
        }
        let removed1 = index.prune_ids(&keep, &std::collections::HashMap::new());
        assert_eq!(removed1, 3);
        assert_graph_invariants(&index);
        let len_after = index.len();

        // Second prune with the same keep set must be a no-op.
        let removed2 = index.prune_ids(&keep, &std::collections::HashMap::new());
        assert_eq!(removed2, 0);
        assert_eq!(index.len(), len_after);
        assert_graph_invariants(&index);

        // And the surviving ids are exactly `keep`.
        let ids = index.id_set();
        assert_eq!(ids, keep);
    }

    /// Case 7: duplicate ids (stale leftovers from `insert_data` not deduping)
    /// collapse to exactly one node per id — the one matching `prefer`.
    #[test]
    fn test_prune_duplicate_ids_collapse_to_prefer() {
        let config = HnswConfig::new(2, DistanceMetric::L2)
            .with_m(4)
            .with_ef_construction(32)
            .with_ef_search(16);
        let mut index = HnswIndex::new(config);

        // id "dup" gets inserted three times with three different vectors
        // (simulating fp v0 -> v1 -> v2 updates). "other" is a normal node.
        index.insert(VectorEntry { id: b"dup".to_vec(), vector: vec![0.0, 1.0] }); // v0 stale
        index.insert(VectorEntry { id: b"other".to_vec(), vector: vec![1.0, 0.0] });
        index.insert(VectorEntry { id: b"dup".to_vec(), vector: vec![0.3, 0.7] }); // v1 stale
        index.insert(VectorEntry { id: b"dup".to_vec(), vector: vec![0.6, 0.4] }); // v2 live
        assert_eq!(index.len(), 4, "three dup nodes + one other");

        let mut keep: HashSet<Vec<u8>> = HashSet::new();
        keep.insert(b"dup".to_vec());
        keep.insert(b"other".to_vec());

        // prefer = the live doc_vectors entry for "dup" (v2).
        let mut prefer = std::collections::HashMap::new();
        prefer.insert(b"dup".to_vec(), VecData::from_f32(&[0.6, 0.4]));

        let removed = index.prune_ids(&keep, &prefer);
        assert_eq!(removed, 2, "two stale dup nodes dropped");
        assert_eq!(index.len(), 2, "one node per kept id");
        assert_graph_invariants(&index);

        // The surviving dup node must carry the preferred (v2) vector.
        let survivors: Vec<&HnswNode> = index.nodes.iter().filter(|n| n.id == b"dup").collect();
        assert_eq!(survivors.len(), 1);
        assert_eq!(survivors[0].data, VecData::from_f32(&[0.6, 0.4]));

        // Without `prefer`, the LAST insert wins (v2 anyway here) — but if we
        // point prefer at v0, the first node must win instead.
        let mut index2 = make_index(&["x"]);
        index2.insert(VectorEntry { id: b"dup".to_vec(), vector: vec![0.0, 1.0] });
        index2.insert(VectorEntry { id: b"dup".to_vec(), vector: vec![0.6, 0.4] });
        let mut keep2: HashSet<Vec<u8>> = HashSet::new();
        keep2.insert(b"dup".to_vec());
        keep2.insert(b"x".to_vec());
        let mut prefer2 = std::collections::HashMap::new();
        prefer2.insert(b"dup".to_vec(), VecData::from_f32(&[0.0, 1.0])); // the OLD v0
        let removed2 = index2.prune_ids(&keep2, &prefer2);
        assert_eq!(removed2, 1);
        let s2: Vec<&HnswNode> = index2.nodes.iter().filter(|n| n.id == b"dup").collect();
        assert_eq!(s2.len(), 1);
        assert_eq!(s2[0].data, VecData::from_f32(&[0.0, 1.0]), "prefer wins over last-insert");
    }
}
