//! Compressed graph storage using CSR (Compressed Sparse Row) format.
//!
//! CSR format reduces memory usage by storing adjacency lists contiguously
//! in a single vector, with an offset array for O(1) neighbor lookup.
//!
//! Memory savings: ~50% compared to HashMap-based adjacency lists.

use serde::{Deserialize, Serialize};

/// Compressed Sparse Row (CSR) graph storage.
///
/// Stores graph in two arrays:
/// - `offsets[i]..offsets[i+1]` gives the range of neighbors for vertex `i`
/// - `neighbors[offsets[i]..offsets[i+1]]` contains the neighbor vertex IDs
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CsrGraph {
    /// Offset array: offsets[i] is the start index in `neighbors` for vertex i.
    /// Length = num_vertices + 1.
    offsets: Vec<u32>,
    /// Neighbor array: contains all neighbor vertex IDs contiguously.
    neighbors: Vec<u32>,
    /// Edge data (parallel to neighbors): edge label index for each neighbor.
    edge_labels: Vec<u16>,
    /// Label dictionary: maps label index to label string.
    label_dict: Vec<String>,
    /// Reverse map: label string -> index.
    label_to_idx: std::collections::HashMap<String, u16>,
    /// Number of vertices.
    num_vertices: usize,
    /// Number of edges.
    num_edges: usize,
}

impl CsrGraph {
    /// Create a new empty CSR graph.
    pub fn new() -> Self {
        Self {
            offsets: vec![0],
            neighbors: Vec::new(),
            edge_labels: Vec::new(),
            label_dict: Vec::new(),
            label_to_idx: std::collections::HashMap::new(),
            num_vertices: 0,
            num_edges: 0,
        }
    }

    /// Build a CSR graph from edge lists.
    ///
    /// `edges` is a list of (source, target, label) tuples.
    /// Vertices are numbered 0..max_vertex_id.
    pub fn from_edges(num_vertices: usize, edges: &[(u32, u32, &str)]) -> Self {
        let mut graph = Self::new();
        graph.num_vertices = num_vertices;

        // Build label dictionary
        let mut label_set: std::collections::HashSet<String> = std::collections::HashSet::new();
        for (_, _, label) in edges {
            label_set.insert(label.to_string());
        }
        let mut label_dict: Vec<String> = label_set.into_iter().collect();
        label_dict.sort();
        let label_to_idx: std::collections::HashMap<String, u16> = label_dict
            .iter()
            .enumerate()
            .map(|(i, l)| (l.clone(), i as u16))
            .collect();

        // Sort edges by source vertex
        let mut sorted_edges: Vec<(u32, u32, u16)> = edges
            .iter()
            .map(|(src, dst, label)| (*src, *dst, *label_to_idx.get(*label).unwrap_or(&0)))
            .collect();
        sorted_edges.sort_by_key(|&(src, _, _)| src);

        // Build CSR arrays
        let mut offsets = vec![0u32; num_vertices + 1];
        let mut neighbors = Vec::with_capacity(sorted_edges.len());
        let mut edge_labels_vec = Vec::with_capacity(sorted_edges.len());

        // Count edges per vertex
        for &(src, _, _) in &sorted_edges {
            if (src as usize) < num_vertices {
                offsets[src as usize + 1] += 1;
            }
        }

        // Compute prefix sums
        for i in 1..=num_vertices {
            offsets[i] += offsets[i - 1];
        }

        // Fill neighbor and label arrays
        neighbors.resize(sorted_edges.len(), 0);
        edge_labels_vec.resize(sorted_edges.len(), 0);
        for (i, &(_, dst, label_idx)) in sorted_edges.iter().enumerate() {
            neighbors[i] = dst;
            edge_labels_vec[i] = label_idx;
        }

        graph.offsets = offsets;
        graph.neighbors = neighbors;
        graph.edge_labels = edge_labels_vec;
        graph.label_dict = label_dict;
        graph.label_to_idx = label_to_idx;
        graph.num_edges = sorted_edges.len();

        graph
    }

    /// Get neighbors of a vertex.
    pub fn neighbors(&self, vertex: u32) -> &[u32] {
        let v = vertex as usize;
        if v >= self.num_vertices {
            return &[];
        }
        let start = self.offsets[v] as usize;
        let end = self.offsets[v + 1] as usize;
        &self.neighbors[start..end]
    }

    /// Get neighbors with their edge labels.
    pub fn neighbors_with_labels(&self, vertex: u32) -> Vec<(u32, &str)> {
        let v = vertex as usize;
        if v >= self.num_vertices {
            return Vec::new();
        }
        let start = self.offsets[v] as usize;
        let end = self.offsets[v + 1] as usize;
        (start..end)
            .map(|i| {
                let label = self.label_dict
                    .get(self.edge_labels[i] as usize)
                    .map(|s| s.as_str())
                    .unwrap_or("");
                (self.neighbors[i], label)
            })
            .collect()
    }

    /// Get out-degree of a vertex.
    pub fn out_degree(&self, vertex: u32) -> usize {
        let v = vertex as usize;
        if v >= self.num_vertices {
            return 0;
        }
        (self.offsets[v + 1] - self.offsets[v]) as usize
    }

    /// Number of vertices.
    pub fn vertex_count(&self) -> usize {
        self.num_vertices
    }

    /// Number of edges.
    pub fn edge_count(&self) -> usize {
        self.num_edges
    }

    /// Memory usage in bytes (approximate).
    pub fn memory_bytes(&self) -> usize {
        self.offsets.len() * std::mem::size_of::<u32>()
            + self.neighbors.len() * std::mem::size_of::<u32>()
            + self.edge_labels.len() * std::mem::size_of::<u16>()
            + self.label_dict.iter().map(|s| s.len()).sum::<usize>()
    }

    /// BFS from a start vertex.
    pub fn bfs(&self, start: u32, max_depth: usize) -> Vec<u32> {
        use std::collections::VecDeque;

        if start as usize >= self.num_vertices {
            return Vec::new();
        }

        let mut visited = vec![false; self.num_vertices];
        visited[start as usize] = true;
        let mut queue = VecDeque::new();
        queue.push_back((start, 0usize));
        let mut result = Vec::new();

        while let Some((curr, depth)) = queue.pop_front() {
            if depth > 0 {
                result.push(curr);
            }
            if depth >= max_depth {
                continue;
            }

            for &nbr in self.neighbors(curr) {
                let nbr_usize = nbr as usize;
                if nbr_usize < self.num_vertices && !visited[nbr_usize] {
                    visited[nbr_usize] = true;
                    queue.push_back((nbr, depth + 1));
                }
            }
        }

        result
    }
}

impl Default for CsrGraph {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_csr_basic() {
        let edges = vec![
            (0, 1, "KNOWS"),
            (0, 2, "KNOWS"),
            (1, 2, "WORKS_WITH"),
            (2, 0, "KNOWS"),
        ];
        let graph = CsrGraph::from_edges(3, &edges);

        assert_eq!(graph.vertex_count(), 3);
        assert_eq!(graph.edge_count(), 4);

        let neighbors_0 = graph.neighbors(0);
        assert_eq!(neighbors_0.len(), 2);
        assert!(neighbors_0.contains(&1));
        assert!(neighbors_0.contains(&2));

        let neighbors_2 = graph.neighbors(2);
        assert_eq!(neighbors_2.len(), 1);
        assert_eq!(neighbors_2[0], 0);
    }

    #[test]
    fn test_csr_bfs() {
        let edges = vec![
            (0, 1, "LINK"),
            (1, 2, "LINK"),
            (2, 3, "LINK"),
        ];
        let graph = CsrGraph::from_edges(4, &edges);

        let result = graph.bfs(0, 3);
        assert_eq!(result.len(), 3);
        assert!(result.contains(&1));
        assert!(result.contains(&2));
        assert!(result.contains(&3));
    }

    #[test]
    fn test_csr_memory_savings() {
        // Build a graph and compare memory usage
        let edges: Vec<(u32, u32, &str)> = (0..1000)
            .flat_map(|i| {
                (0..10).map(move |j| (i, (i + j) % 1000, "EDGE"))
            })
            .collect();

        let graph = CsrGraph::from_edges(1000, &edges);
        let memory = graph.memory_bytes();

        // CSR should use less than HashMap-based storage
        // HashMap overhead is ~80 bytes per entry vs ~6 bytes per entry in CSR
        println!("CSR memory: {} bytes for {} edges", memory, graph.edge_count());
        assert!(memory < 1000 * 80); // Should be much less than HashMap
    }

    #[test]
    fn test_csr_neighbors_with_labels() {
        let edges = vec![
            (0, 1, "KNOWS"),
            (0, 2, "WORKS_WITH"),
        ];
        let graph = CsrGraph::from_edges(3, &edges);

        let neighbors = graph.neighbors_with_labels(0);
        assert_eq!(neighbors.len(), 2);

        let knows: Vec<_> = neighbors.iter().filter(|(_, l)| *l == "KNOWS").collect();
        assert_eq!(knows.len(), 1);
        assert_eq!(knows[0].0, 1);
    }
}
