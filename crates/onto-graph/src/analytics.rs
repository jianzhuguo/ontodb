// Copyright (c) 2024-2026 OntoDB Team
// Licensed under the Business Source License 1.1 (BUSL-1.1).
// See LICENSE for details. Change Date: 2031-09-15.
// On the Change Date, this file will be licensed under Apache License 2.0.
//! Graph analytics primitives: connected components, degree distribution,
//! PageRank, clustering coefficient, betweenness centrality, and subgraph extraction.

use std::collections::{HashMap, HashSet, VecDeque};

#[cfg(feature = "enterprise")]
use std::collections::BinaryHeap;
#[cfg(feature = "enterprise")]
use std::cmp::{Ordering, Reverse};

/// Wrapper for f64 that implements Ord (for use in BinaryHeap).
/// Treats NaN as positive infinity.
#[cfg(feature = "enterprise")]
#[derive(Debug, Clone, Copy, PartialEq)]
struct OrdF64(f64);

#[cfg(feature = "enterprise")]
impl Eq for OrdF64 {}

#[cfg(feature = "enterprise")]
impl PartialOrd for OrdF64 {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[cfg(feature = "enterprise")]
impl Ord for OrdF64 {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.partial_cmp(&other.0).unwrap_or(Ordering::Equal)
    }
}

use crate::store::GraphStore;

/// Degree distribution summary for the graph.
#[derive(Debug, Clone)]
pub struct DegreeDistribution {
    pub min_degree: usize,
    pub max_degree: usize,
    pub mean_degree: f64,
    pub median_degree: f64,
    /// degree -> count
    pub histogram: HashMap<usize, usize>,
}

/// Compute connected components on the undirected graph using BFS.
///
/// Uses integer-indexed adjacency lists for performance, resolves to
/// string vertex IDs at the end.
pub fn connected_components(store: &GraphStore) -> Vec<Vec<String>> {
    let n = store.vertex_count();
    if n == 0 {
        return Vec::new();
    }

    let mut visited = vec![false; n];
    let mut components: Vec<Vec<String>> = Vec::new();

    for idx in 0..n {
        let idx_u32 = idx as u32;
        if visited[idx] {
            continue;
        }

        // Skip deleted/empty slots (get_id returns None)
        if store.get_id(idx_u32).is_none() {
            visited[idx] = true;
            continue;
        }

        // BFS from this unvisited vertex
        let mut component_indices: Vec<u32> = Vec::new();
        let mut queue = VecDeque::new();
        visited[idx] = true;
        queue.push_back(idx_u32);

        while let Some(curr) = queue.pop_front() {
            component_indices.push(curr);

            // Outgoing neighbors
            for (nbr, _) in store.get_out_neighbors_idx(curr) {
                let nbr_usize = nbr as usize;
                if nbr_usize < n && !visited[nbr_usize] {
                    visited[nbr_usize] = true;
                    queue.push_back(nbr);
                }
            }

            // Incoming neighbors (undirected traversal)
            for (nbr, _) in store.get_in_neighbors_idx(curr) {
                let nbr_usize = nbr as usize;
                if nbr_usize < n && !visited[nbr_usize] {
                    visited[nbr_usize] = true;
                    queue.push_back(nbr);
                }
            }
        }

        let component: Vec<String> = component_indices
            .into_iter()
            .filter_map(|i| store.get_id(i))
            .collect();

        if !component.is_empty() {
            components.push(component);
        }
    }

    components
}

/// Compute degree distribution of the graph.
///
/// Degree = out_degree + in_degree for each vertex.
pub fn degree_distribution(store: &GraphStore) -> DegreeDistribution {
    let n = store.vertex_count();
    if n == 0 {
        return DegreeDistribution {
            min_degree: 0,
            max_degree: 0,
            mean_degree: 0.0,
            median_degree: 0.0,
            histogram: HashMap::new(),
        };
    }

    let mut degrees: Vec<usize> = Vec::with_capacity(n);
    let mut histogram: HashMap<usize, usize> = HashMap::new();

    for idx in 0..n {
        let idx_u32 = idx as u32;
        // Skip deleted/empty slots
        if store.get_id(idx_u32).is_none() {
            continue;
        }
        let out_deg = store.get_out_neighbors_idx(idx_u32).len();
        let in_deg = store.get_in_neighbors_idx(idx_u32).len();
        let degree = out_deg + in_deg;
        degrees.push(degree);
        *histogram.entry(degree).or_insert(0) += 1;
    }

    if degrees.is_empty() {
        return DegreeDistribution {
            min_degree: 0,
            max_degree: 0,
            mean_degree: 0.0,
            median_degree: 0.0,
            histogram: HashMap::new(),
        };
    }

    degrees.sort_unstable();

    let min_degree = degrees[0];
    let max_degree = degrees[degrees.len() - 1];
    let sum: usize = degrees.iter().sum();
    let mean_degree = sum as f64 / degrees.len() as f64;
    let median_degree = if degrees.len() % 2 == 0 {
        let mid = degrees.len() / 2;
        (degrees[mid - 1] + degrees[mid]) as f64 / 2.0
    } else {
        degrees[degrees.len() / 2] as f64
    };

    DegreeDistribution {
        min_degree,
        max_degree,
        mean_degree,
        median_degree,
        histogram,
    }
}

/// Standard PageRank algorithm.
///
/// Uses `adj_out` for outgoing links. Iterates until convergence or
/// `max_iterations`. Returns vertex_id -> rank mapping.
pub fn pagerank(
    store: &GraphStore,
    damping_factor: f64,
    max_iterations: usize,
    tolerance: f64,
) -> HashMap<String, f64> {
    let n = store.vertex_count();
    if n == 0 {
        return HashMap::new();
    }

    // Collect valid indices and build out-degree list
    let mut valid_indices: Vec<u32> = Vec::new();
    let mut out_degree: Vec<usize> = vec![0; n];
    for idx in 0..n {
        let idx_u32 = idx as u32;
        if store.get_id(idx_u32).is_some() {
            valid_indices.push(idx_u32);
            out_degree[idx] = store.get_out_neighbors_idx(idx_u32).len();
        }
    }

    let num_valid = valid_indices.len();
    if num_valid == 0 {
        return HashMap::new();
    }

    let initial_rank = 1.0 / num_valid as f64;
    let mut rank: Vec<f64> = vec![0.0; n];
    for &idx in &valid_indices {
        rank[idx as usize] = initial_rank;
    }

    for _iter in 0..max_iterations {
        let mut new_rank: Vec<f64> = vec![0.0; n];

        // Sum of rank for dangling nodes (nodes with no outgoing edges)
        let dangling_sum: f64 = valid_indices
            .iter()
            .filter(|&&idx| out_degree[idx as usize] == 0)
            .map(|&idx| rank[idx as usize])
            .sum();

        // Distribute dangling rank + random jump equally
        let base = (dangling_sum * damping_factor + (1.0 - damping_factor)) / num_valid as f64;

        for &idx in &valid_indices {
            new_rank[idx as usize] = base;
        }

        // Add contributions from outgoing links
        for &idx in &valid_indices {
            let od = out_degree[idx as usize];
            if od == 0 {
                continue;
            }
            let contribution = damping_factor * rank[idx as usize] / od as f64;
            for (nbr, _) in store.get_out_neighbors_idx(idx) {
                let nbr_usize = nbr as usize;
                if nbr_usize < n {
                    new_rank[nbr_usize] += contribution;
                }
            }
        }

        // Check convergence
        let diff: f64 = valid_indices
            .iter()
            .map(|&idx| (new_rank[idx as usize] - rank[idx as usize]).abs())
            .sum();

        rank = new_rank;

        if diff < tolerance {
            break;
        }
    }

    // Build result map
    let mut result = HashMap::with_capacity(num_valid);
    for &idx in &valid_indices {
        if let Some(id) = store.get_id(idx) {
            result.insert(id, rank[idx as usize]);
        }
    }
    result
}

/// Compute the average clustering coefficient of the graph.
///
/// For each vertex, the local clustering coefficient is:
///   triangles / possible_triangles
/// where triangles = number of edges among neighbors,
/// and possible_triangles = degree * (degree - 1) / 2.
///
/// Only vertices with degree >= 2 are considered.
/// Uses integer-indexed adjacency for performance.
pub fn average_clustering_coefficient(store: &GraphStore) -> f64 {
    let n = store.vertex_count();
    if n == 0 {
        return 0.0;
    }

    let mut total_cc = 0.0;
    let mut count = 0usize;

    for idx in 0..n {
        let idx_u32 = idx as u32;
        if store.get_id(idx_u32).is_none() {
            continue;
        }

        // Collect all neighbors (undirected) as a set of indices
        let mut neighbor_set: HashSet<u32> = HashSet::new();
        for (nbr, _) in store.get_out_neighbors_idx(idx_u32) {
            if nbr != idx_u32 {
                neighbor_set.insert(nbr);
            }
        }
        for (nbr, _) in store.get_in_neighbors_idx(idx_u32) {
            if nbr != idx_u32 {
                neighbor_set.insert(nbr);
            }
        }

        let degree = neighbor_set.len();
        if degree < 2 {
            continue;
        }

        let possible = (degree * (degree - 1)) / 2;

        // Count triangles: edges between pairs of neighbors
        let mut triangles = 0usize;
        let neighbors_vec: Vec<u32> = neighbor_set.iter().copied().collect();
        for i in 0..neighbors_vec.len() {
            let ni = neighbors_vec[i];
            // Get ni's outgoing neighbors and check if they're in the neighbor set
            for (nbr_of_ni, _) in store.get_out_neighbors_idx(ni) {
                if nbr_of_ni != idx_u32 && neighbor_set.contains(&nbr_of_ni) && nbr_of_ni > ni {
                    triangles += 1;
                }
            }
            // Also check incoming neighbors for undirected triangles
            for (nbr_of_ni, _) in store.get_in_neighbors_idx(ni) {
                if nbr_of_ni != idx_u32 && neighbor_set.contains(&nbr_of_ni) && nbr_of_ni > ni {
                    triangles += 1;
                }
            }
        }

        let cc = triangles as f64 / possible as f64;
        total_cc += cc;
        count += 1;
    }

    if count == 0 {
        0.0
    } else {
        total_cc / count as f64
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// Enterprise Edition: Advanced Algorithms
// ══════════════════════════════════════════════════════════════════════════════

/// Compute betweenness centrality for all vertices.
///
/// Betweenness centrality measures how often a vertex lies on shortest paths
/// between other vertices. Uses Brandes' algorithm for O(VE) time complexity.
///
/// Returns vertex_id -> centrality mapping.
#[cfg(feature = "enterprise")]
pub fn betweenness_centrality(store: &GraphStore) -> HashMap<String, f64> {
    let n = store.vertex_count();
    if n == 0 {
        return HashMap::new();
    }

    let mut centrality: Vec<f64> = vec![0.0; n];
    let valid_indices: Vec<u32> = (0..n as u32)
        .filter(|&idx| store.get_id(idx).is_some())
        .collect();

    for &s in &valid_indices {
        // BFS from s
        let mut stack: Vec<u32> = Vec::new();
        let mut predecessors: Vec<Vec<u32>> = vec![Vec::new(); n];
        let mut sigma: Vec<f64> = vec![0.0; n];
        let mut dist: Vec<i64> = vec![-1; n];

        sigma[s as usize] = 1.0;
        dist[s as usize] = 0;

        let mut queue = VecDeque::new();
        queue.push_back(s);

        while let Some(v) = queue.pop_front() {
            stack.push(v);
            for (w, _) in store.get_out_neighbors_idx(v) {
                let w_usize = w as usize;
                if w_usize >= n {
                    continue;
                }
                // First time visiting w
                if dist[w_usize] < 0 {
                    dist[w_usize] = dist[v as usize] + 1;
                    queue.push_back(w);
                }
                // Shortest path to w via v
                if dist[w_usize] == dist[v as usize] + 1 {
                    sigma[w_usize] += sigma[v as usize];
                    predecessors[w_usize].push(v);
                }
            }
            // Also check incoming edges for undirected betweenness
            for (w, _) in store.get_in_neighbors_idx(v) {
                let w_usize = w as usize;
                if w_usize >= n {
                    continue;
                }
                if dist[w_usize] < 0 {
                    dist[w_usize] = dist[v as usize] + 1;
                    queue.push_back(w);
                }
                if dist[w_usize] == dist[v as usize] + 1 {
                    sigma[w_usize] += sigma[v as usize];
                    predecessors[w_usize].push(v);
                }
            }
        }

        // Back-propagation
        let mut delta: Vec<f64> = vec![0.0; n];
        while let Some(w) = stack.pop() {
            for &v in &predecessors[w as usize] {
                let v_usize = v as usize;
                if sigma[w as usize] > 0.0 {
                    delta[v_usize] += (sigma[v_usize] / sigma[w as usize]) * (1.0 + delta[w as usize]);
                }
            }
            if w != s {
                centrality[w as usize] += delta[w as usize];
            }
        }
    }

    // Normalize for undirected graph
    let norm = if n > 2 {
        1.0 / ((n - 1) * (n - 2)) as f64
    } else {
        1.0
    };

    let mut result = HashMap::with_capacity(valid_indices.len());
    for &idx in &valid_indices {
        if let Some(id) = store.get_id(idx) {
            result.insert(id, centrality[idx as usize] * norm);
        }
    }
    result
}

/// Extract a subgraph containing only vertices and edges matching a filter.
///
/// The filter selects vertices by label. Returns a new GraphStore with the
/// filtered subgraph.
#[cfg(feature = "enterprise")]
pub fn subgraph_extraction(
    store: &GraphStore,
    vertex_labels: &[String],
) -> GraphStore {
    let subgraph = GraphStore::new();

    // Collect matching vertex IDs
    let mut matching_ids: HashSet<String> = HashSet::new();
    for label in vertex_labels {
        for vertex in store.get_vertices_by_label(label) {
            matching_ids.insert(vertex.id.clone());
        }
    }

    // Add matching vertices
    for id in &matching_ids {
        if let Some(vertex) = store.get_vertex(id) {
            let _ = subgraph.add_vertex(vertex);
        }
    }

    // Add edges where both endpoints are in the subgraph
    for vertex_id in &matching_ids {
        for edge in store.get_out_edges(vertex_id) {
            if matching_ids.contains(&edge.to) {
                let _ = subgraph.add_edge(edge);
            }
        }
    }

    subgraph
}

/// Extract a subgraph by edge label.
///
/// Returns a new GraphStore containing only edges with the specified label,
/// and the vertices they connect.
#[cfg(feature = "enterprise")]
pub fn subgraph_by_edge_label(
    store: &GraphStore,
    edge_label: &str,
) -> GraphStore {
    let subgraph = GraphStore::new();
    let mut added_vertices: HashSet<String> = HashSet::new();

    let edges = store.get_edges_by_label(edge_label);
    for edge in edges {
        // Add source vertex if not already added
        if !added_vertices.contains(&edge.from) {
            if let Some(v) = store.get_vertex(&edge.from) {
                let _ = subgraph.add_vertex(v);
            }
            added_vertices.insert(edge.from.clone());
        }
        // Add target vertex if not already added
        if !added_vertices.contains(&edge.to) {
            if let Some(v) = store.get_vertex(&edge.to) {
                let _ = subgraph.add_vertex(v);
            }
            added_vertices.insert(edge.to.clone());
        }
        // Add edge
        let _ = subgraph.add_edge(edge);
    }

    subgraph
}

/// Result of Dijkstra's shortest path algorithm.
#[cfg(feature = "enterprise")]
#[derive(Debug, Clone)]
pub struct DijkstraResult {
    /// Distance from source to each vertex (f64::INFINITY if unreachable).
    pub distances: HashMap<String, f64>,
    /// Previous vertex on the shortest path (for path reconstruction).
    pub previous: HashMap<String, Option<String>>,
    /// The target vertex (if specified).
    pub target: Option<String>,
}

/// Dijkstra's algorithm for weighted shortest paths.
///
/// Uses edge properties as weights. If an edge has no "weight" property,
/// defaults to 1.0. Returns shortest distances from source to all reachable vertices.
///
/// Optimized with BinaryHeap (O(log n) per operation) instead of Vec+sort (O(n log n)).
///
/// # Arguments
/// * `store` - The graph store
/// * `source` - Source vertex ID
/// * `target` - Optional target vertex ID (stops early if found)
/// * `weight_key` - Edge property key to use as weight (default: "weight")
#[cfg(feature = "enterprise")]
pub fn dijkstra(
    store: &GraphStore,
    source: &str,
    target: Option<&str>,
    weight_key: Option<&str>,
) -> DijkstraResult {
    let key = weight_key.unwrap_or("weight");
    let n = store.vertex_count();

    // Use integer indices for O(1) lookup instead of String HashMap
    let source_idx = match store.get_idx(source) {
        Some(idx) => idx,
        None => {
            return DijkstraResult {
                distances: HashMap::new(),
                previous: HashMap::new(),
                target: target.map(|t| t.to_string()),
            };
        }
    };

    let target_idx = target.and_then(|t| store.get_idx(t));

    // Pre-allocate with known size
    let mut dist: Vec<f64> = vec![f64::INFINITY; n];
    let mut prev: Vec<Option<u32>> = vec![None; n];
    let mut visited: Vec<bool> = vec![false; n];

    dist[source_idx as usize] = 0.0;

    // BinaryHeap with Reverse for min-heap behavior
    let mut heap: BinaryHeap<Reverse<(OrdF64, u32)>> = BinaryHeap::new();
    heap.push(Reverse((OrdF64(0.0), source_idx)));

    while let Some(Reverse((OrdF64(d), u))) = heap.pop() {
        let u_usize = u as usize;

        // Skip if already visited (lazy deletion)
        if visited[u_usize] {
            continue;
        }
        visited[u_usize] = true;

        // Early termination if target reached
        if Some(u) == target_idx {
            break;
        }

        // Skip if current distance is worse than known distance
        if d > dist[u_usize] {
            continue;
        }

        // Relax outgoing edges
        for (v, edge_id) in store.get_out_neighbors_idx(u) {
            let v_usize = v as usize;
            if v_usize >= n || visited[v_usize] {
                continue;
            }

            // Get edge weight
            let weight = store.get_edge(&edge_id)
                .and_then(|e| e.properties.get(key).and_then(|w| match w {
                    crate::model::PropValue::Float(f) => Some(*f),
                    crate::model::PropValue::Int(i) => Some(*i as f64),
                    _ => None,
                }))
                .unwrap_or(1.0);

            let new_dist = d + weight;
            if new_dist < dist[v_usize] {
                dist[v_usize] = new_dist;
                prev[v_usize] = Some(u);
                heap.push(Reverse((OrdF64(new_dist), v)));
            }
        }
    }

    // Convert from integer indices to String-based result
    let mut distances = HashMap::with_capacity(n);
    let mut previous = HashMap::with_capacity(n);

    for idx in 0..n {
        if let Some(id) = store.get_id(idx as u32) {
            if dist[idx] < f64::INFINITY {
                distances.insert(id.clone(), dist[idx]);
                previous.insert(id, prev[idx].and_then(|p| store.get_id(p)));
            }
        }
    }

    DijkstraResult {
        distances,
        previous,
        target: target.map(|t| t.to_string()),
    }
}

/// Reconstruct the shortest path from Dijkstra result.
#[cfg(feature = "enterprise")]
pub fn dijkstra_path(result: &DijkstraResult, source: &str, target: &str) -> Option<Vec<String>> {
    let mut path = Vec::new();
    let mut current = target.to_string();

    loop {
        path.push(current.clone());
        if current == source {
            path.reverse();
            return Some(path);
        }
        match result.previous.get(&current) {
            Some(Some(prev)) => current = prev.clone(),
            _ => return None, // No path found
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Edge, PropValue, Vertex};

    fn build_simple_graph() -> GraphStore {
        let store = GraphStore::new();
        // Component 1: triangle a-b-c
        store
            .add_vertex(Vertex::new("a", vec!["Node".to_string()]))
            .unwrap();
        store
            .add_vertex(Vertex::new("b", vec!["Node".to_string()]))
            .unwrap();
        store
            .add_vertex(Vertex::new("c", vec!["Node".to_string()]))
            .unwrap();
        store
            .add_edge(Edge::new("e1", "a", "b", "LINK"))
            .unwrap();
        store
            .add_edge(Edge::new("e2", "b", "c", "LINK"))
            .unwrap();
        store
            .add_edge(Edge::new("e3", "c", "a", "LINK"))
            .unwrap();

        // Component 2: isolated edge d-e
        store
            .add_vertex(Vertex::new("d", vec!["Node".to_string()]))
            .unwrap();
        store
            .add_vertex(Vertex::new("e", vec!["Node".to_string()]))
            .unwrap();
        store
            .add_edge(Edge::new("e4", "d", "e", "LINK"))
            .unwrap();

        store
    }

    fn build_linear_graph() -> GraphStore {
        let store = GraphStore::new();
        // Chain: 1 -> 2 -> 3 -> 4
        store.add_vertex(Vertex::new("1", vec![])).unwrap();
        store.add_vertex(Vertex::new("2", vec![])).unwrap();
        store.add_vertex(Vertex::new("3", vec![])).unwrap();
        store.add_vertex(Vertex::new("4", vec![])).unwrap();
        store
            .add_edge(Edge::new("e1", "1", "2", "NEXT"))
            .unwrap();
        store
            .add_edge(Edge::new("e2", "2", "3", "NEXT"))
            .unwrap();
        store
            .add_edge(Edge::new("e3", "3", "4", "NEXT"))
            .unwrap();
        store
    }

    // ── Connected Components Tests ──

    #[test]
    fn test_connected_components_two_components() {
        let store = build_simple_graph();
        let components = connected_components(&store);
        assert_eq!(components.len(), 2);

        // One component has 3 vertices (a, b, c), the other has 2 (d, e)
        let sizes: Vec<usize> = components.iter().map(|c| c.len()).collect();
        let mut sorted_sizes = sizes.clone();
        sorted_sizes.sort();
        assert_eq!(sorted_sizes, vec![2, 3]);
    }

    #[test]
    fn test_connected_components_single_vertex() {
        let store = GraphStore::new();
        store
            .add_vertex(Vertex::new("lonely", vec![]))
            .unwrap();
        let components = connected_components(&store);
        assert_eq!(components.len(), 1);
        assert_eq!(components[0].len(), 1);
        assert_eq!(components[0][0], "lonely");
    }

    #[test]
    fn test_connected_components_empty_graph() {
        let store = GraphStore::new();
        let components = connected_components(&store);
        assert!(components.is_empty());
    }

    #[test]
    fn test_connected_components_fully_connected() {
        let store = GraphStore::new();
        for i in 0..5 {
            store
                .add_vertex(Vertex::new(format!("v{}", i), vec![]))
                .unwrap();
        }
        // Connect all into one chain
        for i in 0..4 {
            store
                .add_edge(Edge::new(
                    format!("e{}", i),
                    format!("v{}", i),
                    format!("v{}", i + 1),
                    "LINK",
                ))
                .unwrap();
        }
        let components = connected_components(&store);
        assert_eq!(components.len(), 1);
        assert_eq!(components[0].len(), 5);
    }

    #[test]
    fn test_connected_components_all_isolated() {
        let store = GraphStore::new();
        for i in 0..4 {
            store
                .add_vertex(Vertex::new(format!("iso{}", i), vec![]))
                .unwrap();
        }
        // No edges
        let components = connected_components(&store);
        assert_eq!(components.len(), 4);
        for c in &components {
            assert_eq!(c.len(), 1);
        }
    }

    // ── Degree Distribution Tests ──

    #[test]
    fn test_degree_distribution_triangle() {
        let store = build_simple_graph();
        let dd = degree_distribution(&store);

        // Triangle a-b-c: each has out=1, in=1 → degree=2 (undirected total)
        // Edge d-e: d has out=1 → degree=1, e has in=1 → degree=1
        assert!(dd.min_degree <= 2);
        assert!(dd.max_degree >= 1);
        assert!(dd.mean_degree > 0.0);
        assert!(dd.histogram.values().sum::<usize>() == 5);
    }

    #[test]
    fn test_degree_distribution_empty() {
        let store = GraphStore::new();
        let dd = degree_distribution(&store);
        assert_eq!(dd.min_degree, 0);
        assert_eq!(dd.max_degree, 0);
        assert_eq!(dd.mean_degree, 0.0);
        assert!(dd.histogram.is_empty());
    }

    #[test]
    fn test_degree_distribution_linear() {
        let store = build_linear_graph();
        let dd = degree_distribution(&store);

        // 1: out=1, in=0 → deg=1
        // 2: out=1, in=1 → deg=2
        // 3: out=1, in=1 → deg=2
        // 4: out=0, in=1 → deg=1
        assert_eq!(dd.min_degree, 1);
        assert_eq!(dd.max_degree, 2);
        assert_eq!(dd.mean_degree, 1.5);
        assert_eq!(dd.median_degree, 1.5);
    }

    // ── PageRank Tests ──

    #[test]
    fn test_pagerank_convergence() {
        let store = build_simple_graph();
        let ranks = pagerank(&store, 0.85, 100, 1e-6);

        // All 5 vertices should have a rank
        assert_eq!(ranks.len(), 5);

        // Ranks should sum to approximately 1.0
        let total: f64 = ranks.values().sum();
        assert!(
            (total - 1.0).abs() < 0.01,
            "PageRank should sum to ~1.0, got {}",
            total
        );

        // All ranks should be positive
        for (_, rank) in &ranks {
            assert!(*rank > 0.0, "All ranks should be positive");
        }
    }

    #[test]
    fn test_pagerank_damping_factor() {
        let store = build_simple_graph();
        let ranks_low = pagerank(&store, 0.5, 100, 1e-6);
        let ranks_high = pagerank(&store, 0.95, 100, 1e-6);

        // Both should converge and sum to ~1
        let total_low: f64 = ranks_low.values().sum();
        let total_high: f64 = ranks_high.values().sum();
        assert!((total_low - 1.0).abs() < 0.01);
        assert!((total_high - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_pagerank_empty_graph() {
        let store = GraphStore::new();
        let ranks = pagerank(&store, 0.85, 100, 1e-6);
        assert!(ranks.is_empty());
    }

    #[test]
    fn test_pagerank_single_vertex() {
        let store = GraphStore::new();
        store
            .add_vertex(Vertex::new("alone", vec![]))
            .unwrap();
        let ranks = pagerank(&store, 0.85, 100, 1e-6);
        assert_eq!(ranks.len(), 1);
        assert!((ranks["alone"] - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_pagerank_star_topology() {
        let store = GraphStore::new();
        // Hub -> spoke1, hub -> spoke2, hub -> spoke3
        store
            .add_vertex(Vertex::new("hub", vec![]))
            .unwrap();
        for i in 1..=3 {
            store
                .add_vertex(Vertex::new(format!("spoke{}", i), vec![]))
                .unwrap();
            store
                .add_edge(Edge::new(
                    format!("e{}", i),
                    "hub",
                    format!("spoke{}", i),
                    "LINK",
                ))
                .unwrap();
        }

        let ranks = pagerank(&store, 0.85, 100, 1e-6);
        assert_eq!(ranks.len(), 4);

        // Hub sends rank out, so spokes should each receive some
        // Hub's rank should be lower than in a cycle
        let total: f64 = ranks.values().sum();
        assert!((total - 1.0).abs() < 0.01);
    }

    // ── Clustering Coefficient Tests ──

    #[test]
    fn test_clustering_coefficient_triangle() {
        let store = GraphStore::new();
        // Perfect triangle: a-b, b-c, c-a
        store
            .add_vertex(Vertex::new("a", vec![]))
            .unwrap();
        store
            .add_vertex(Vertex::new("b", vec![]))
            .unwrap();
        store
            .add_vertex(Vertex::new("c", vec![]))
            .unwrap();
        store
            .add_edge(Edge::new("e1", "a", "b", "LINK"))
            .unwrap();
        store
            .add_edge(Edge::new("e2", "b", "c", "LINK"))
            .unwrap();
        store
            .add_edge(Edge::new("e3", "c", "a", "LINK"))
            .unwrap();

        let cc = average_clustering_coefficient(&store);
        // All 3 vertices have degree 2, and each has 1 triangle out of 1 possible
        assert!(
            (cc - 1.0).abs() < 0.01,
            "Triangle should have CC ≈ 1.0, got {}",
            cc
        );
    }

    #[test]
    fn test_clustering_coefficient_no_triangles() {
        let store = build_linear_graph();
        let cc = average_clustering_coefficient(&store);
        // Linear chain has no triangles
        assert!(
            cc.abs() < 0.01,
            "Linear chain should have CC ≈ 0.0, got {}",
            cc
        );
    }

    #[test]
    fn test_clustering_coefficient_empty_graph() {
        let store = GraphStore::new();
        let cc = average_clustering_coefficient(&store);
        assert_eq!(cc, 0.0);
    }

    #[test]
    fn test_clustering_coefficient_range() {
        let store = build_simple_graph();
        let cc = average_clustering_coefficient(&store);
        // CC must be in [0, 1]
        assert!(
            cc >= 0.0 && cc <= 1.0,
            "Clustering coefficient should be in [0, 1], got {}",
            cc
        );
    }

    #[test]
    fn test_clustering_coefficient_two_triangles_sharing_edge() {
        let store = GraphStore::new();
        // Two triangles sharing edge b-c:
        //   a-b, b-c, c-a (triangle 1)
        //   d-b, b-c, c-d (triangle 2)
        store.add_vertex(Vertex::new("a", vec![])).unwrap();
        store.add_vertex(Vertex::new("b", vec![])).unwrap();
        store.add_vertex(Vertex::new("c", vec![])).unwrap();
        store.add_vertex(Vertex::new("d", vec![])).unwrap();

        store.add_edge(Edge::new("e1", "a", "b", "L")).unwrap();
        store.add_edge(Edge::new("e2", "b", "c", "L")).unwrap();
        store.add_edge(Edge::new("e3", "c", "a", "L")).unwrap();
        store.add_edge(Edge::new("e4", "d", "b", "L")).unwrap();
        store.add_edge(Edge::new("e5", "c", "d", "L")).unwrap();

        let cc = average_clustering_coefficient(&store);
        assert!(cc > 0.0, "Should have some clustering");
        assert!(cc <= 1.0, "CC must be <= 1.0");
    }

    // ── Stress / property tests ──

    #[test]
    fn test_analytics_on_larger_graph() {
        let store = GraphStore::new();
        // Create 20 vertices in a ring
        for i in 0..20 {
            store
                .add_vertex(Vertex::new(format!("v{}", i), vec![]))
                .unwrap();
        }
        for i in 0..20 {
            store
                .add_edge(Edge::new(
                    format!("e{}", i),
                    format!("v{}", i),
                    format!("v{}", (i + 1) % 20),
                    "RING",
                ))
                .unwrap();
        }

        // All in one component
        let cc = connected_components(&store);
        assert_eq!(cc.len(), 1);
        assert_eq!(cc[0].len(), 20);

        // Degree distribution: each vertex has out=1, in=1 → degree=2
        let dd = degree_distribution(&store);
        assert_eq!(dd.min_degree, 2);
        assert_eq!(dd.max_degree, 2);
        assert_eq!(dd.mean_degree, 2.0);

        // PageRank: in a symmetric ring, all ranks should be equal
        let ranks = pagerank(&store, 0.85, 100, 1e-6);
        let expected_rank = 1.0 / 20.0;
        for (_, rank) in &ranks {
            assert!(
                (rank - expected_rank).abs() < 0.01,
                "Symmetric ring should have equal ranks, got {} vs {}",
                rank,
                expected_rank
            );
        }

        // Clustering coefficient: ring has no triangles
        let ccoef = average_clustering_coefficient(&store);
        assert!(
            ccoef.abs() < 0.01,
            "Ring should have CC ≈ 0, got {}",
            ccoef
        );
    }

    #[test]
    fn test_pagerank_two_node_cycle() {
        let store = GraphStore::new();
        store.add_vertex(Vertex::new("x", vec![])).unwrap();
        store.add_vertex(Vertex::new("y", vec![])).unwrap();
        store.add_edge(Edge::new("e1", "x", "y", "L")).unwrap();
        store.add_edge(Edge::new("e2", "y", "x", "L")).unwrap();

        let ranks = pagerank(&store, 0.85, 100, 1e-6);
        assert_eq!(ranks.len(), 2);
        // Symmetric cycle: equal ranks
        let rx = ranks["x"];
        let ry = ranks["y"];
        assert!(
            (rx - ry).abs() < 0.01,
            "Symmetric 2-node cycle: equal ranks expected, got {} vs {}",
            rx,
            ry
        );
    }

    // ── Betweenness Centrality Tests (Enterprise) ──

    #[cfg(feature = "enterprise")]
    #[test]
    fn test_betweenness_centrality_linear() {
        let store = build_linear_graph();
        let bc = betweenness_centrality(&store);

        // In a linear chain 1->2->3->4, vertices 2 and 3 are on more shortest paths
        // than 1 and 4. So bc[2] and bc[3] should be higher.
        assert_eq!(bc.len(), 4);
        assert!(bc["2"] > 0.0, "Middle vertex should have positive betweenness");
        assert!(bc["3"] > 0.0, "Middle vertex should have positive betweenness");
    }

    #[cfg(feature = "enterprise")]
    #[test]
    fn test_betweenness_centrality_empty() {
        let store = GraphStore::new();
        let bc = betweenness_centrality(&store);
        assert!(bc.is_empty());
    }

    #[cfg(feature = "enterprise")]
    #[test]
    fn test_betweenness_centrality_triangle() {
        let store = GraphStore::new();
        store.add_vertex(Vertex::new("a", vec![])).unwrap();
        store.add_vertex(Vertex::new("b", vec![])).unwrap();
        store.add_vertex(Vertex::new("c", vec![])).unwrap();
        store.add_edge(Edge::new("e1", "a", "b", "L")).unwrap();
        store.add_edge(Edge::new("e2", "b", "c", "L")).unwrap();
        store.add_edge(Edge::new("e3", "c", "a", "L")).unwrap();

        let bc = betweenness_centrality(&store);
        assert_eq!(bc.len(), 3);
        // In a triangle, all vertices have equal betweenness
        let vals: Vec<f64> = bc.values().copied().collect();
        let first = vals[0];
        for v in &vals {
            assert!((v - first).abs() < 0.01, "Triangle should have equal betweenness");
        }
    }

    #[cfg(feature = "enterprise")]
    #[test]
    fn test_betweenness_centrality_star() {
        let store = GraphStore::new();
        store.add_vertex(Vertex::new("hub", vec![])).unwrap();
        for i in 1..=4 {
            store.add_vertex(Vertex::new(format!("spoke{}", i), vec![])).unwrap();
            store.add_edge(Edge::new(format!("e{}", i), "hub", format!("spoke{}", i), "L")).unwrap();
        }

        let bc = betweenness_centrality(&store);
        // Hub should have highest betweenness (it's on all paths between spokes)
        assert!(bc["hub"] > bc["spoke1"], "Hub should have higher betweenness than spokes");
    }

    // ── Subgraph Extraction Tests (Enterprise) ──

    #[cfg(feature = "enterprise")]
    #[test]
    fn test_subgraph_extraction_by_label() {
        let store = GraphStore::new();
        store.add_vertex(Vertex::new("d1", vec!["Drug".to_string()])).unwrap();
        store.add_vertex(Vertex::new("d2", vec!["Drug".to_string()])).unwrap();
        store.add_vertex(Vertex::new("p1", vec!["Protein".to_string()])).unwrap();
        store.add_edge(Edge::new("e1", "d1", "d2", "interacts")).unwrap();
        store.add_edge(Edge::new("e2", "d1", "p1", "targets")).unwrap();

        let sub = subgraph_extraction(&store, &["Drug".to_string()]);
        assert_eq!(sub.vertex_count(), 2);
        assert_eq!(sub.edge_count(), 1); // Only d1->d2 edge (both are Drugs)
    }

    #[cfg(feature = "enterprise")]
    #[test]
    fn test_subgraph_extraction_empty() {
        let store = GraphStore::new();
        store.add_vertex(Vertex::new("v1", vec!["A".to_string()])).unwrap();

        let sub = subgraph_extraction(&store, &["B".to_string()]);
        assert_eq!(sub.vertex_count(), 0);
        assert_eq!(sub.edge_count(), 0);
    }

    #[cfg(feature = "enterprise")]
    #[test]
    fn test_subgraph_by_edge_label() {
        let store = GraphStore::new();
        store.add_vertex(Vertex::new("d1", vec!["Drug".to_string()])).unwrap();
        store.add_vertex(Vertex::new("dis1", vec!["Disease".to_string()])).unwrap();
        store.add_vertex(Vertex::new("p1", vec!["Protein".to_string()])).unwrap();
        store.add_edge(Edge::new("e1", "d1", "dis1", "treats")).unwrap();
        store.add_edge(Edge::new("e2", "d1", "p1", "targets")).unwrap();

        let sub = subgraph_by_edge_label(&store, "treats");
        assert_eq!(sub.vertex_count(), 2); // d1 and dis1
        assert_eq!(sub.edge_count(), 1); // Only treats edge
    }

    #[cfg(feature = "enterprise")]
    #[test]
    fn test_subgraph_by_edge_label_no_matches() {
        let store = GraphStore::new();
        store.add_vertex(Vertex::new("v1", vec![])).unwrap();

        let sub = subgraph_by_edge_label(&store, "nonexistent");
        assert_eq!(sub.vertex_count(), 0);
        assert_eq!(sub.edge_count(), 0);
    }

    // ── Dijkstra Tests (Enterprise) ──

    #[cfg(feature = "enterprise")]
    #[test]
    fn test_dijkstra_simple_path() {
        let store = GraphStore::new();
        store.add_vertex(Vertex::new("a", vec![])).unwrap();
        store.add_vertex(Vertex::new("b", vec![])).unwrap();
        store.add_vertex(Vertex::new("c", vec![])).unwrap();

        let mut e1 = Edge::new("e1", "a", "b", "LINK");
        e1.properties.insert("weight".to_string(), PropValue::Float(1.0));
        store.add_edge(e1).unwrap();

        let mut e2 = Edge::new("e2", "b", "c", "LINK");
        e2.properties.insert("weight".to_string(), PropValue::Float(2.0));
        store.add_edge(e2).unwrap();

        let result = dijkstra(&store, "a", Some("c"), None);
        assert_eq!(result.distances.get("c"), Some(&3.0));

        let path = dijkstra_path(&result, "a", "c");
        assert_eq!(path, Some(vec!["a".to_string(), "b".to_string(), "c".to_string()]));
    }

    #[cfg(feature = "enterprise")]
    #[test]
    fn test_dijkstra_no_path() {
        let store = GraphStore::new();
        store.add_vertex(Vertex::new("a", vec![])).unwrap();
        store.add_vertex(Vertex::new("b", vec![])).unwrap();
        // No edge between a and b

        let result = dijkstra(&store, "a", Some("b"), None);
        // b is unreachable, so it won't be in distances map
        // or it will be INFINITY
        let dist_b = result.distances.get("b").copied().unwrap_or(f64::INFINITY);
        assert_eq!(dist_b, f64::INFINITY);
    }

    #[cfg(feature = "enterprise")]
    #[test]
    fn test_dijkstra_unweighted() {
        let store = GraphStore::new();
        store.add_vertex(Vertex::new("a", vec![])).unwrap();
        store.add_vertex(Vertex::new("b", vec![])).unwrap();
        store.add_edge(Edge::new("e1", "a", "b", "LINK")).unwrap(); // No weight property

        let result = dijkstra(&store, "a", Some("b"), None);
        assert_eq!(result.distances.get("b"), Some(&1.0)); // Default weight = 1.0
    }

    #[cfg(feature = "enterprise")]
    #[test]
    fn test_dijkstra_custom_weight_key() {
        let store = GraphStore::new();
        store.add_vertex(Vertex::new("a", vec![])).unwrap();
        store.add_vertex(Vertex::new("b", vec![])).unwrap();

        let mut e1 = Edge::new("e1", "a", "b", "LINK");
        e1.properties.insert("cost".to_string(), PropValue::Float(5.0));
        store.add_edge(e1).unwrap();

        let result = dijkstra(&store, "a", Some("b"), Some("cost"));
        assert_eq!(result.distances.get("b"), Some(&5.0));
    }

    #[cfg(feature = "enterprise")]
    #[test]
    fn test_dijkstra_parallel_bfs() {
        let store = GraphStore::new();
        // Build a wide graph
        store.add_vertex(Vertex::new("root", vec![])).unwrap();
        for i in 0..10 {
            store.add_vertex(Vertex::new(format!("child{}", i), vec![])).unwrap();
            store.add_edge(Edge::new(format!("e{}", i), "root", format!("child{}", i), "LINK")).unwrap();
        }

        let root_idx = store.get_idx("root").unwrap();
        let result = store.bfs_parallel(root_idx, 2, crate::traversal::Direction::Out);
        assert_eq!(result.len(), 10); // All 10 children
    }
}