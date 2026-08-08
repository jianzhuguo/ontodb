//! Graph storage - CRUD operations for vertices and edges.
//!
//! Supports both in-memory and persistent storage via LSM engine.

use std::collections::{HashMap, HashSet, VecDeque};
use parking_lot::RwLock as PLRwLock;

use crate::error::GraphError;
use crate::model::{Edge, PropertyMap, Vertex};
use crate::traversal::Direction;

/// Storage mode for the graph store.
#[derive(Debug, Clone)]
pub enum StorageMode {
    /// In-memory only (no persistence).
    Memory,
    /// Persistent storage using LSM engine.
    Persistent { data_dir: String },
}

/// In-memory graph store with adjacency list representation.
pub struct GraphStore {
    /// Vertices indexed by ID.
    vertices: PLRwLock<HashMap<String, Vertex>>,
    /// Outgoing edges indexed by source vertex ID.
    out_edges: PLRwLock<HashMap<String, Vec<Edge>>>,
    /// Incoming edges indexed by target vertex ID.
    in_edges: PLRwLock<HashMap<String, Vec<Edge>>>,
    /// All edges indexed by ID.
    edges: PLRwLock<HashMap<String, Edge>>,
    /// Labels index: label -> set of vertex IDs.
    label_index: PLRwLock<HashMap<String, HashSet<String>>>,
    /// Storage mode.
    #[allow(dead_code)]
    storage_mode: StorageMode,
    /// Internal integer ID mapping for fast traversal.
    /// Maps string ID -> integer index.
    id_to_idx: PLRwLock<HashMap<String, u32>>,
    /// Maps integer index -> string ID.
    idx_to_id: PLRwLock<Vec<String>>,
    /// Adjacency list using integer indices: idx -> list of (neighbor_idx, edge_idx).
    adj_out: PLRwLock<Vec<Vec<(u32, u32)>>>,
    adj_in: PLRwLock<Vec<Vec<(u32, u32)>>>,
    /// Edge index -> (from_idx, to_idx, edge_id).
    edge_index: PLRwLock<Vec<(u32, u32, String)>>,
}

impl GraphStore {
    pub fn new() -> Self {
        Self {
            vertices: PLRwLock::new(HashMap::new()),
            out_edges: PLRwLock::new(HashMap::new()),
            in_edges: PLRwLock::new(HashMap::new()),
            edges: PLRwLock::new(HashMap::new()),
            label_index: PLRwLock::new(HashMap::new()),
            storage_mode: StorageMode::Memory,
            id_to_idx: PLRwLock::new(HashMap::new()),
            idx_to_id: PLRwLock::new(Vec::new()),
            adj_out: PLRwLock::new(Vec::new()),
            adj_in: PLRwLock::new(Vec::new()),
            edge_index: PLRwLock::new(Vec::new()),
        }
    }

    /// Create a new persistent graph store.
    pub fn new_persistent(data_dir: &str) -> Self {
        Self {
            vertices: PLRwLock::new(HashMap::new()),
            out_edges: PLRwLock::new(HashMap::new()),
            in_edges: PLRwLock::new(HashMap::new()),
            edges: PLRwLock::new(HashMap::new()),
            label_index: PLRwLock::new(HashMap::new()),
            storage_mode: StorageMode::Persistent { data_dir: data_dir.to_string() },
            id_to_idx: PLRwLock::new(HashMap::new()),
            idx_to_id: PLRwLock::new(Vec::new()),
            adj_out: PLRwLock::new(Vec::new()),
            adj_in: PLRwLock::new(Vec::new()),
            edge_index: PLRwLock::new(Vec::new()),
        }
    }

    /// Get or create integer index for a vertex ID.
    fn get_or_create_idx(&self, id: &str) -> u32 {
        let mut map = self.id_to_idx.write();
        if let Some(&idx) = map.get(id) {
            return idx;
        }
        let len = map.len();
        let idx: u32 = len.try_into().unwrap_or_else(|_| {
            tracing::error!("Vertex index overflow: {} vertices exceed u32::MAX", len);
            u32::MAX
        });
        map.insert(id.to_string(), idx);
        drop(map);

        let mut ids = self.idx_to_id.write();
        ids.push(id.to_string());
        drop(ids);

        let mut adj_out = self.adj_out.write();
        adj_out.push(Vec::new());
        drop(adj_out);

        let mut adj_in = self.adj_in.write();
        adj_in.push(Vec::new());

        idx
    }

    // ── Vertex CRUD ──────────────────────────────────────────────

    /// Add a vertex to the graph.
    pub fn add_vertex(&self, vertex: Vertex) -> Result<(), GraphError> {
        let id = vertex.id.clone();
        let labels = vertex.labels.clone();

        {
            let mut verts = self.vertices.write();
            if verts.contains_key(&id) {
                return Err(GraphError::DuplicateVertex(id));
            }
            verts.insert(id.clone(), vertex);
        }

        // Update label index
        {
            let mut idx = self.label_index.write();
            for label in labels {
                idx.entry(label).or_insert_with(HashSet::new).insert(id.clone());
            }
        }

        // Initialize adjacency lists
        {
            let mut out = self.out_edges.write();
            out.entry(id.clone()).or_insert_with(Vec::new);
        }
        {
            let mut in_e = self.in_edges.write();
            in_e.entry(id.clone()).or_insert_with(Vec::new);
        }

        // Create integer index for fast traversal
        self.get_or_create_idx(&id);

        Ok(())
    }

    /// Get a vertex by ID.
    pub fn get_vertex(&self, id: &str) -> Option<Vertex> {
        self.vertices.read().get(id).cloned()
    }

    /// Update vertex properties.
    pub fn update_vertex(&self, id: &str, properties: PropertyMap) -> Result<(), GraphError> {
        let mut verts = self.vertices.write();
        let vertex = verts.get_mut(id).ok_or_else(|| GraphError::VertexNotFound(id.to_string()))?;
        for (k, v) in properties {
            vertex.properties.insert(k, v);
        }
        Ok(())
    }

    /// Delete a vertex and all its connected edges.
    pub fn delete_vertex(&self, id: &str) -> Result<(), GraphError> {
        let vertex = {
            let mut verts = self.vertices.write();
            verts.remove(id).ok_or_else(|| GraphError::VertexNotFound(id.to_string()))?
        };

        // Remove from label index
        {
            let mut idx = self.label_index.write();
            for label in &vertex.labels {
                if let Some(set) = idx.get_mut(label) {
                    set.remove(id);
                }
            }
        }

        // Remove all connected edges (string-keyed)
        // Collect edge info first, then clean up neighbor lists separately to avoid deadlock
        let connected_edges: Vec<(String, String, bool)> = {
            let out = self.out_edges.write().remove(id).unwrap_or_default();
            let inp = self.in_edges.write().remove(id).unwrap_or_default();

            let mut all_edges = Vec::new();
            let mut edges = self.edges.write();
            for edge in out {
                edges.remove(&edge.id);
                all_edges.push((edge.id.clone(), edge.to.clone(), true));
            }
            for edge in inp {
                edges.remove(&edge.id);
                all_edges.push((edge.id.clone(), edge.from.clone(), false));
            }
            all_edges
        };

        // Clean up neighbor adjacency lists (separate lock scope to avoid deadlock)
        for (edge_id, neighbor_id, is_outgoing) in &connected_edges {
            if *is_outgoing {
                if let Some(in_list) = self.in_edges.write().get_mut(neighbor_id.as_str()) {
                    in_list.retain(|e| &e.id != edge_id);
                }
            } else {
                if let Some(out_list) = self.out_edges.write().get_mut(neighbor_id.as_str()) {
                    out_list.retain(|e| &e.id != edge_id);
                }
            }
        }

        // Clean up integer adjacency lists
        if let Some(&idx) = self.id_to_idx.read().get(id) {
            let idx = idx as usize;
            // Clear adjacency lists for this vertex
            {
                let mut adj_out = self.adj_out.write();
                if idx < adj_out.len() {
                    adj_out[idx].clear();
                }
            }
            {
                let mut adj_in = self.adj_in.write();
                if idx < adj_in.len() {
                    adj_in[idx].clear();
                }
            }
            // Remove edges from edge_index that reference this vertex
            {
                let mut ei = self.edge_index.write();
                ei.retain(|(from, to, _)| *from as usize != idx && *to as usize != idx);
            }
            // Remove from ID mapping
            self.id_to_idx.write().remove(id);
            let mut ids = self.idx_to_id.write();
            if idx < ids.len() {
                ids[idx] = String::new(); // tombstone
            }
        }

        Ok(())
    }

    /// Get vertices by label.
    pub fn get_vertices_by_label(&self, label: &str) -> Vec<Vertex> {
        let idx = self.label_index.read();
        let verts = self.vertices.read();

        idx.get(label)
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| verts.get(id).cloned())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get all vertices.
    pub fn get_all_vertices(&self) -> Vec<Vertex> {
        self.vertices.read().values().cloned().collect()
    }

    // ── Edge CRUD ────────────────────────────────────────────────

    /// Add an edge to the graph.
    pub fn add_edge(&self, edge: Edge) -> Result<(), GraphError> {
        // Verify source and target exist
        {
            let verts = self.vertices.read();
            if !verts.contains_key(&edge.from) {
                return Err(GraphError::VertexNotFound(edge.from.clone()));
            }
            if !verts.contains_key(&edge.to) {
                return Err(GraphError::VertexNotFound(edge.to.clone()));
            }
        }

        let id = edge.id.clone();
        let from = edge.from.clone();
        let to = edge.to.clone();

        {
            let mut edges = self.edges.write();
            if edges.contains_key(&id) {
                return Err(GraphError::DuplicateVertex(format!("Edge {}", id)));
            }
            edges.insert(id.clone(), edge.clone());
        }

        {
            let mut out = self.out_edges.write();
            out.entry(from.clone()).or_insert_with(Vec::new).push(edge.clone());
        }
        {
            let mut inp = self.in_edges.write();
            inp.entry(to.clone()).or_insert_with(Vec::new).push(edge);
        }

        // Update integer adjacency lists
        let from_idx = self.get_or_create_idx(&from);
        let to_idx = self.get_or_create_idx(&to);
        let edge_idx = {
            let mut ei = self.edge_index.write();
            let idx = ei.len() as u32;
            ei.push((from_idx, to_idx, id));
            idx
        };
        {
            let mut adj_out = self.adj_out.write();
            adj_out[from_idx as usize].push((to_idx, edge_idx));
        }
        {
            let mut adj_in = self.adj_in.write();
            adj_in[to_idx as usize].push((from_idx, edge_idx));
        }

        Ok(())
    }

    /// Get an edge by ID.
    pub fn get_edge(&self, id: &str) -> Option<Edge> {
        self.edges.read().get(id).cloned()
    }

    /// Update edge properties.
    pub fn update_edge(&self, id: &str, properties: PropertyMap) -> Result<(), GraphError> {
        let mut edges = self.edges.write();
        let edge = edges.get_mut(id).ok_or_else(|| GraphError::EdgeNotFound(id.to_string()))?;
        for (k, v) in properties {
            edge.properties.insert(k, v);
        }

        // Also update in adjacency lists
        let updated = edge.clone();
        drop(edges);

        let mut out = self.out_edges.write();
        if let Some(list) = out.get_mut(&updated.from) {
            if let Some(e) = list.iter_mut().find(|e| e.id == id) {
                e.properties = updated.properties.clone();
            }
        }

        let mut inp = self.in_edges.write();
        if let Some(list) = inp.get_mut(&updated.to) {
            if let Some(e) = list.iter_mut().find(|e| e.id == id) {
                e.properties = updated.properties;
            }
        }

        Ok(())
    }

    /// Delete an edge.
    pub fn delete_edge(&self, id: &str) -> Result<(), GraphError> {
        let edge = {
            let mut edges = self.edges.write();
            edges.remove(id).ok_or_else(|| GraphError::EdgeNotFound(id.to_string()))?
        };

        {
            let mut out = self.out_edges.write();
            if let Some(list) = out.get_mut(&edge.from) {
                list.retain(|e| e.id != id);
            }
        }
        {
            let mut inp = self.in_edges.write();
            if let Some(list) = inp.get_mut(&edge.to) {
                list.retain(|e| e.id != id);
            }
        }

        Ok(())
    }

    /// Get outgoing edges from a vertex.
    pub fn get_out_edges(&self, vertex_id: &str) -> Vec<Edge> {
        self.out_edges
            .read()
            .get(vertex_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Get incoming edges to a vertex.
    pub fn get_in_edges(&self, vertex_id: &str) -> Vec<Edge> {
        self.in_edges
            .read()
            .get(vertex_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Get neighbors of a vertex (outgoing direction).
    pub fn get_neighbors(&self, vertex_id: &str) -> Vec<Vertex> {
        let verts = self.vertices.read();
        self.out_edges
            .read()
            .get(vertex_id)
            .map(|edges| {
                edges
                    .iter()
                    .filter_map(|e| verts.get(&e.to).cloned())
                    .collect()
            })
            .unwrap_or_default()
    }

    // ── Stats ────────────────────────────────────────────────────

    /// Get vertex count.
    pub fn vertex_count(&self) -> usize {
        self.vertices.read().len()
    }

    /// Get edge count.
    pub fn edge_count(&self) -> usize {
        self.edges.read().len()
    }

    /// Get average degree.
    pub fn avg_degree(&self) -> f64 {
        let out = self.out_edges.read();
        if out.is_empty() {
            return 0.0;
        }
        let total: usize = out.values().map(|v| v.len()).sum();
        total as f64 / out.len() as f64
    }

    /// Get integer index for a vertex ID.
    pub fn get_idx(&self, id: &str) -> Option<u32> {
        self.id_to_idx.read().get(id).copied()
    }

    /// Get vertex ID from integer index.
    pub fn get_id(&self, idx: u32) -> Option<String> {
        self.idx_to_id.read().get(idx as usize).cloned()
    }

    /// Get outgoing neighbors using integer indices (fast path).
    pub fn get_out_neighbors_idx(&self, idx: u32) -> Vec<(u32, u32)> {
        self.adj_out.read().get(idx as usize).cloned().unwrap_or_default()
    }

    /// Get incoming neighbors using integer indices (fast path).
    pub fn get_in_neighbors_idx(&self, idx: u32) -> Vec<(u32, u32)> {
        self.adj_in.read().get(idx as usize).cloned().unwrap_or_default()
    }

    /// Fast BFS using integer indices (no string allocations during traversal).
    pub fn bfs_fast(
        &self,
        start_idx: u32,
        max_depth: usize,
        direction: Direction,
    ) -> Vec<u32> {
        let num_nodes = self.idx_to_id.read().len();
        if start_idx as usize >= num_nodes {
            return Vec::new();
        }
        let mut visited = vec![false; num_nodes];
        visited[start_idx as usize] = true;
        let mut queue = VecDeque::new();
        queue.push_back((start_idx, 0usize));
        let mut result = Vec::new();

        while let Some((curr, depth)) = queue.pop_front() {
            if depth > 0 {
                result.push(curr);
            }
            if depth >= max_depth {
                continue;
            }

            let neighbors = match direction {
                Direction::Out => self.get_out_neighbors_idx(curr),
                Direction::In => self.get_in_neighbors_idx(curr),
                Direction::Both => {
                    let mut n = self.get_out_neighbors_idx(curr);
                    n.extend(self.get_in_neighbors_idx(curr));
                    n
                }
            };

            for (nbr, _) in neighbors {
                if !visited[nbr as usize] {
                    visited[nbr as usize] = true;
                    queue.push_back((nbr, depth + 1));
                }
            }
        }

        result
    }

    /// Fast BFS with parent tracking for lazy path reconstruction.
    /// Returns (visited_nodes, parent_map) where parent_map[node] = parent_node.
    pub fn bfs_fast_with_parents(
        &self,
        start_idx: u32,
        max_depth: usize,
        direction: Direction,
    ) -> (Vec<u32>, Vec<Option<u32>>) {
        let num_nodes = self.idx_to_id.read().len();
        if start_idx as usize >= num_nodes {
            return (Vec::new(), vec![None; num_nodes]);
        }
        let mut visited = vec![false; num_nodes];
        let mut parent: Vec<Option<u32>> = vec![None; num_nodes];
        visited[start_idx as usize] = true;
        let mut queue = VecDeque::new();
        queue.push_back((start_idx, 0usize));
        let mut result = Vec::new();

        while let Some((curr, depth)) = queue.pop_front() {
            if depth > 0 {
                result.push(curr);
            }
            if depth >= max_depth {
                continue;
            }

            let neighbors = match direction {
                Direction::Out => self.get_out_neighbors_idx(curr),
                Direction::In => self.get_in_neighbors_idx(curr),
                Direction::Both => {
                    let mut n = self.get_out_neighbors_idx(curr);
                    n.extend(self.get_in_neighbors_idx(curr));
                    n
                }
            };

            for (nbr, _) in neighbors {
                if !visited[nbr as usize] {
                    visited[nbr as usize] = true;
                    parent[nbr as usize] = Some(curr);
                    queue.push_back((nbr, depth + 1));
                }
            }
        }

        (result, parent)
    }

    /// Reconstruct path from start to target using parent map.
    pub fn reconstruct_path(
        &self,
        parent: &[Option<u32>],
        start_idx: u32,
        target_idx: u32,
    ) -> Vec<String> {
        let mut path = Vec::new();
        let mut current = target_idx;

        loop {
            if let Some(id) = self.get_id(current) {
                path.push(id);
            }
            if current == start_idx {
                break;
            }
            match parent[current as usize] {
                Some(p) => current = p,
                None => break, // No path found
            }
        }

        path.reverse();
        path
    }

    // ── Persistence ──────────────────────────────────────────────

    /// Export graph data to JSON for persistence.
    pub fn export_json(&self) -> String {
        let vertices: Vec<Vertex> = self.vertices.read().values().cloned().collect();
        let edges: Vec<Edge> = self.edges.read().values().cloned().collect();

        serde_json::to_string(&serde_json::json!({
            "vertices": vertices,
            "edges": edges,
        })).unwrap_or_default()
    }

    /// Import graph data from JSON.
    pub fn import_json(&self, json: &str) -> Result<(), GraphError> {
        let data: serde_json::Value = serde_json::from_str(json)?;

        // Clear existing data
        self.vertices.write().clear();
        self.edges.write().clear();
        self.out_edges.write().clear();
        self.in_edges.write().clear();
        self.label_index.write().clear();

        // Import vertices
        if let Some(vertices) = data.get("vertices").and_then(|v| v.as_array()) {
            for v in vertices {
                let vertex: Vertex = serde_json::from_value(v.clone())?;
                self.add_vertex(vertex)?;
            }
        }

        // Import edges
        if let Some(edges) = data.get("edges").and_then(|v| v.as_array()) {
            for e in edges {
                let edge: Edge = serde_json::from_value(e.clone())?;
                self.add_edge(edge)?;
            }
        }

        Ok(())
    }

    /// Save graph to file.
    pub fn save_to_file(&self, path: &str) -> Result<(), GraphError> {
        let json = self.export_json();
        std::fs::write(path, json).map_err(|e| GraphError::StorageError(e.to_string()))?;
        Ok(())
    }

    /// Load graph from file.
    pub fn load_from_file(&self, path: &str) -> Result<(), GraphError> {
        let json = std::fs::read_to_string(path).map_err(|e| GraphError::StorageError(e.to_string()))?;
        self.import_json(&json)
    }
}

impl Default for GraphStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::PropValue;

    #[test]
    fn test_vertex_crud() {
        let store = GraphStore::new();

        let v = Vertex::new("v1", vec!["Person".to_string()])
            .with_property("name", PropValue::String("Alice".to_string()))
            .with_property("age", PropValue::Int(30));

        store.add_vertex(v).unwrap();

        let retrieved = store.get_vertex("v1").unwrap();
        assert_eq!(retrieved.id, "v1");
        assert_eq!(retrieved.labels, vec!["Person"]);
        assert_eq!(retrieved.properties.get("name").unwrap(), &PropValue::String("Alice".to_string()));

        // Update
        let mut props = PropertyMap::new();
        props.insert("age".to_string(), PropValue::Int(31));
        store.update_vertex("v1", props).unwrap();

        let updated = store.get_vertex("v1").unwrap();
        assert_eq!(updated.properties.get("age").unwrap(), &PropValue::Int(31));

        // Delete
        store.delete_vertex("v1").unwrap();
        assert!(store.get_vertex("v1").is_none());
    }

    #[test]
    fn test_edge_crud() {
        let store = GraphStore::new();

        store.add_vertex(Vertex::new("v1", vec!["Person".to_string()])).unwrap();
        store.add_vertex(Vertex::new("v2", vec!["Person".to_string()])).unwrap();

        let e = Edge::new("e1", "v1", "v2", "KNOWS")
            .with_property("since", PropValue::Int(2020));

        store.add_edge(e).unwrap();

        let out = store.get_out_edges("v1");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].label, "KNOWS");

        let inp = store.get_in_edges("v2");
        assert_eq!(inp.len(), 1);

        // Delete
        store.delete_edge("e1").unwrap();
        assert!(store.get_edge("e1").is_none());
        assert_eq!(store.get_out_edges("v1").len(), 0);
    }

    #[test]
    fn test_label_index() {
        let store = GraphStore::new();

        store.add_vertex(Vertex::new("v1", vec!["Person".to_string()])).unwrap();
        store.add_vertex(Vertex::new("v2", vec!["Person".to_string()])).unwrap();
        store.add_vertex(Vertex::new("v3", vec!["Company".to_string()])).unwrap();

        let persons = store.get_vertices_by_label("Person");
        assert_eq!(persons.len(), 2);

        let companies = store.get_vertices_by_label("Company");
        assert_eq!(companies.len(), 1);
    }

    #[test]
    fn test_delete_vertex_cascades_edges() {
        let store = GraphStore::new();

        store.add_vertex(Vertex::new("v1", vec![])).unwrap();
        store.add_vertex(Vertex::new("v2", vec![])).unwrap();
        store.add_vertex(Vertex::new("v3", vec![])).unwrap();

        store.add_edge(Edge::new("e1", "v1", "v2", "KNOWS")).unwrap();
        store.add_edge(Edge::new("e2", "v1", "v3", "KNOWS")).unwrap();
        store.add_edge(Edge::new("e3", "v2", "v3", "KNOWS")).unwrap();

        assert_eq!(store.edge_count(), 3);

        // Delete v1 should remove e1 and e2
        store.delete_vertex("v1").unwrap();
        assert_eq!(store.edge_count(), 1);
        assert!(store.get_edge("e1").is_none());
        assert!(store.get_edge("e2").is_none());
        assert!(store.get_edge("e3").is_some());
    }
}
