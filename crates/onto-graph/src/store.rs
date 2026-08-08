//! Graph storage - CRUD operations for vertices and edges.
//!
//! Supports both in-memory and persistent storage via LSM engine.

use std::collections::{HashMap, HashSet, VecDeque};
use parking_lot::RwLock;

use onto_core::EntityId;

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
    vertices: RwLock<HashMap<String, Vertex>>,
    /// Outgoing edges indexed by source vertex ID.
    out_edges: RwLock<HashMap<String, Vec<Edge>>>,
    /// Incoming edges indexed by target vertex ID.
    in_edges: RwLock<HashMap<String, Vec<Edge>>>,
    /// All edges indexed by ID.
    edges: RwLock<HashMap<String, Edge>>,
    /// Labels index: label -> set of vertex IDs.
    label_index: RwLock<HashMap<String, HashSet<String>>>,
    /// Storage mode.
    #[allow(dead_code)]
    storage_mode: StorageMode,
    /// Internal integer ID mapping for fast traversal.
    /// Maps string ID -> integer index.
    id_to_idx: RwLock<HashMap<String, u32>>,
    /// Maps integer index -> string ID.
    idx_to_id: RwLock<Vec<String>>,
    /// Adjacency list using integer indices: idx -> list of (neighbor_idx, edge_idx).
    adj_out: RwLock<Vec<Vec<(u32, u32)>>>,
    adj_in: RwLock<Vec<Vec<(u32, u32)>>>,
    /// Edge index -> (from_idx, to_idx, edge_id).
    edge_index: RwLock<Vec<(u32, u32, String)>>,
}

impl GraphStore {
    pub fn new() -> Self {
        Self {
            vertices: RwLock::new(HashMap::new()),
            out_edges: RwLock::new(HashMap::new()),
            in_edges: RwLock::new(HashMap::new()),
            edges: RwLock::new(HashMap::new()),
            label_index: RwLock::new(HashMap::new()),
            storage_mode: StorageMode::Memory,
            id_to_idx: RwLock::new(HashMap::new()),
            idx_to_id: RwLock::new(Vec::new()),
            adj_out: RwLock::new(Vec::new()),
            adj_in: RwLock::new(Vec::new()),
            edge_index: RwLock::new(Vec::new()),
        }
    }

    /// Create a new persistent graph store.
    pub fn new_persistent(data_dir: &str) -> Self {
        Self {
            vertices: RwLock::new(HashMap::new()),
            out_edges: RwLock::new(HashMap::new()),
            in_edges: RwLock::new(HashMap::new()),
            edges: RwLock::new(HashMap::new()),
            label_index: RwLock::new(HashMap::new()),
            storage_mode: StorageMode::Persistent { data_dir: data_dir.to_string() },
            id_to_idx: RwLock::new(HashMap::new()),
            idx_to_id: RwLock::new(Vec::new()),
            adj_out: RwLock::new(Vec::new()),
            adj_in: RwLock::new(Vec::new()),
            edge_index: RwLock::new(Vec::new()),
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
        self.vertices.write().get(id).cloned()
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

        // Collect outgoing and incoming edges, remove them from the edges map,
        // and build neighbor cleanup lists — all under a single lock scope.
        let (outgoing_neighbors, incoming_neighbors, outgoing_edge_ids, incoming_edge_ids) = {
            let out = self.out_edges.write().remove(id).unwrap_or_default();
            let inp = self.in_edges.write().remove(id).unwrap_or_default();

            let mut out_neighbors: Vec<(String, String)> = Vec::new(); // (neighbor_id, edge_id)
            let mut in_neighbors: Vec<(String, String)> = Vec::new();
            let mut out_eids: Vec<String> = Vec::new();
            let mut in_eids: Vec<String> = Vec::new();

            let mut edges = self.edges.write();
            for edge in out {
                out_eids.push(edge.id.clone());
                out_neighbors.push((edge.to.clone(), edge.id.clone()));
                edges.remove(&edge.id);
            }
            for edge in inp {
                in_eids.push(edge.id.clone());
                in_neighbors.push((edge.from.clone(), edge.id.clone()));
                edges.remove(&edge.id);
            }
            (out_neighbors, in_neighbors, out_eids, in_eids)
        };

        // Clean up neighbor adjacency lists — single lock acquisition per neighbor
        // For outgoing edges: remove from neighbor's in_edges
        {
            let mut in_map = self.in_edges.write();
            for (neighbor_id, edge_id) in &outgoing_neighbors {
                if let Some(in_list) = in_map.get_mut(neighbor_id.as_str()) {
                    in_list.retain(|e| &e.id != edge_id);
                }
            }
        }
        // For incoming edges: remove from neighbor's out_edges
        {
            let mut out_map = self.out_edges.write();
            for (neighbor_id, edge_id) in &incoming_neighbors {
                if let Some(out_list) = out_map.get_mut(neighbor_id.as_str()) {
                    out_list.retain(|e| &e.id != edge_id);
                }
            }
        }

        // Clean up integer adjacency lists
        // First, get the idx under a short lock scope
        let maybe_idx = self.id_to_idx.write().get(id).copied();

        if let Some(idx) = maybe_idx {
            let idx = idx as usize;
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
            {
                let mut ei = self.edge_index.write();
                ei.retain(|(from, to, _)| *from as usize != idx && *to as usize != idx);
            }
            self.id_to_idx.write().remove(id);
            let mut ids = self.idx_to_id.write();
            if idx < ids.len() {
                ids[idx] = String::new();
            }
        }

        Ok(())
    }

    /// Get vertices by label.
    pub fn get_vertices_by_label(&self, label: &str) -> Vec<Vertex> {
        let idx = self.label_index.write();
        let verts = self.vertices.write();

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
        self.vertices.write().values().cloned().collect()
    }

    // ── Edge CRUD ────────────────────────────────────────────────

    /// Add an edge to the graph.
    pub fn add_edge(&self, edge: Edge) -> Result<(), GraphError> {
        // Verify source and target exist
        {
            let verts = self.vertices.write();
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
        self.edges.write().get(id).cloned()
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
            .write()
            .get(vertex_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Get incoming edges to a vertex.
    pub fn get_in_edges(&self, vertex_id: &str) -> Vec<Edge> {
        self.in_edges
            .write()
            .get(vertex_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Get neighbors of a vertex (outgoing direction).
    pub fn get_neighbors(&self, vertex_id: &str) -> Vec<Vertex> {
        let verts = self.vertices.write();
        self.out_edges
            .write()
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
        self.vertices.write().len()
    }

    /// Get edge count.
    pub fn edge_count(&self) -> usize {
        self.edges.write().len()
    }

    /// Get average degree.
    pub fn avg_degree(&self) -> f64 {
        let out = self.out_edges.write();
        if out.is_empty() {
            return 0.0;
        }
        let total: usize = out.values().map(|v| v.len()).sum();
        total as f64 / out.len() as f64
    }

    /// Get integer index for a vertex ID.
    pub fn get_idx(&self, id: &str) -> Option<u32> {
        self.id_to_idx.write().get(id).copied()
    }

    /// Get vertex ID from integer index.
    pub fn get_id(&self, idx: u32) -> Option<String> {
        self.idx_to_id.write().get(idx as usize).cloned()
    }

    /// Get outgoing neighbors using integer indices (fast path).
    pub fn get_out_neighbors_idx(&self, idx: u32) -> Vec<(u32, u32)> {
        self.adj_out.write().get(idx as usize).cloned().unwrap_or_default()
    }

    /// Get incoming neighbors using integer indices (fast path).
    pub fn get_in_neighbors_idx(&self, idx: u32) -> Vec<(u32, u32)> {
        self.adj_in.write().get(idx as usize).cloned().unwrap_or_default()
    }

    /// Fast BFS using integer indices (no string allocations during traversal).
    pub fn bfs_fast(
        &self,
        start_idx: u32,
        max_depth: usize,
        direction: Direction,
    ) -> Vec<u32> {
        let num_nodes = self.idx_to_id.write().len();
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
        let num_nodes = self.idx_to_id.write().len();
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
        let vertices: Vec<Vertex> = self.vertices.write().values().cloned().collect();
        let edges: Vec<Edge> = self.edges.write().values().cloned().collect();

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

    // ── EntityId Integration ──────────────────────────────────────

    /// Create or update a graph vertex from an EntityId.
    ///
    /// If the vertex already exists, does nothing (idempotent).
    /// The vertex ID will be `{class}::{pk}` — shared with relational storage.
    pub fn upsert_vertex_from_entity(
        &self,
        entity_id: &EntityId,
        labels: &[String],
    ) -> Result<(), GraphError> {
        let vertex_id = entity_id.to_vertex_id();
        if self.get_vertex(&vertex_id).is_some() {
            return Ok(()); // already exists
        }
        let vertex = Vertex::new(&vertex_id, labels.to_vec());
        self.add_vertex(vertex)
    }

    /// Create or update a graph vertex with properties from an EntityId.
    pub fn upsert_vertex_from_entity_with_props(
        &self,
        entity_id: &EntityId,
        labels: &[String],
        properties: PropertyMap,
    ) -> Result<(), GraphError> {
        let vertex_id = entity_id.to_vertex_id();
        if let Some(mut existing) = self.get_vertex(&vertex_id) {
            // Update properties
            for (k, v) in properties {
                existing.properties.insert(k, v);
            }
            self.update_vertex(&vertex_id, existing.properties)?;
            return Ok(());
        }
        let mut vertex = Vertex::new(&vertex_id, labels.to_vec());
        vertex.properties = properties;
        self.add_vertex(vertex)
    }

    /// Delete a graph vertex by EntityId (cascades to edges).
    pub fn delete_vertex_by_entity(&self, entity_id: &EntityId) -> Result<(), GraphError> {
        let vertex_id = entity_id.to_vertex_id();
        if self.get_vertex(&vertex_id).is_some() {
            self.delete_vertex(&vertex_id)?;
        }
        Ok(())
    }

    /// Add a relationship (directed edge) between two entities.
    ///
    /// Creates vertices if they don't exist.
    pub fn add_relationship(
        &self,
        from: &EntityId,
        to: &EntityId,
        label: &str,
    ) -> Result<(), GraphError> {
        self.add_relationship_with_props(from, to, label, PropertyMap::new())
    }

    /// Add a relationship with properties between two entities.
    pub fn add_relationship_with_props(
        &self,
        from: &EntityId,
        to: &EntityId,
        label: &str,
        properties: PropertyMap,
    ) -> Result<(), GraphError> {
        let from_id = from.to_vertex_id();
        let to_id = to.to_vertex_id();

        // Ensure both vertices exist
        if self.get_vertex(&from_id).is_none() {
            self.add_vertex(Vertex::new(&from_id, vec![from.class().to_string()]))?;
        }
        if self.get_vertex(&to_id).is_none() {
            self.add_vertex(Vertex::new(&to_id, vec![to.class().to_string()]))?;
        }

        let edge_id = format!("{}->{}::{}", from_id, to_id, label);
        let mut edge = Edge::new(&edge_id, &from_id, &to_id, label);
        edge.properties = properties;
        self.add_edge(edge)
    }

    /// Delete a relationship between two entities.
    pub fn delete_relationship(
        &self,
        from: &EntityId,
        to: &EntityId,
        label: &str,
    ) -> Result<(), GraphError> {
        let from_id = from.to_vertex_id();
        let to_id = to.to_vertex_id();
        let edge_id = format!("{}->{}::{}", from_id, to_id, label);
        if self.get_edge(&edge_id).is_some() {
            self.delete_edge(&edge_id)?;
        }
        Ok(())
    }

    /// Get neighboring entities (outgoing, incoming, or both).
    pub fn get_entity_neighbors(
        &self,
        entity_id: &EntityId,
        direction: Direction,
        edge_label: Option<&str>,
    ) -> Vec<EntityId> {
        let vertex_id = entity_id.to_vertex_id();
        let edges = match direction {
            Direction::Out => self.get_out_edges(&vertex_id),
            Direction::In => self.get_in_edges(&vertex_id),
            Direction::Both => {
                let mut e = self.get_out_edges(&vertex_id);
                e.extend(self.get_in_edges(&vertex_id));
                e
            }
        };

        edges.iter()
            .filter(|e| edge_label.map_or(true, |l| e.label == l))
            .filter_map(|e| {
                let target_id = match direction {
                    Direction::In => &e.from,
                    _ => &e.to,
                };
                EntityId::from_str(target_id)
            })
            .collect()
    }

    /// Get vertex by EntityId.
    pub fn get_entity_vertex(&self, entity_id: &EntityId) -> Option<Vertex> {
        self.get_vertex(&entity_id.to_vertex_id())
    }

    /// Check if an entity exists in the graph.
    pub fn has_entity(&self, entity_id: &EntityId) -> bool {
        self.get_vertex(&entity_id.to_vertex_id()).is_some()
    }

    /// Get all entities of a given class (by label).
    pub fn get_entities_by_class(&self, class: &str) -> Vec<EntityId> {
        self.get_vertices_by_label(class)
            .iter()
            .filter_map(|v| EntityId::from_str(&v.id))
            .collect()
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

    // ── EntityId Integration Tests ──

    #[test]
    fn test_entity_upsert_vertex() {
        let store = GraphStore::new();
        let id = EntityId::new("Product", "001");

        store.upsert_vertex_from_entity(&id, &["Product".to_string()]).unwrap();

        assert!(store.has_entity(&id));
        let vertex = store.get_entity_vertex(&id).unwrap();
        assert_eq!(vertex.id, "Product::001");
        assert_eq!(vertex.labels, vec!["Product"]);
    }

    #[test]
    fn test_entity_upsert_idempotent() {
        let store = GraphStore::new();
        let id = EntityId::new("Product", "001");

        // First insert
        store.upsert_vertex_from_entity(&id, &["Product".to_string()]).unwrap();
        // Second insert should not fail
        store.upsert_vertex_from_entity(&id, &["Product".to_string()]).unwrap();

        assert!(store.has_entity(&id));
    }

    #[test]
    fn test_entity_delete_vertex() {
        let store = GraphStore::new();
        let id = EntityId::new("Product", "001");

        store.upsert_vertex_from_entity(&id, &["Product".to_string()]).unwrap();
        assert!(store.has_entity(&id));

        store.delete_vertex_by_entity(&id).unwrap();
        assert!(!store.has_entity(&id));
    }

    #[test]
    fn test_entity_delete_nonexistent() {
        let store = GraphStore::new();
        let id = EntityId::new("Product", "999");

        // Should not fail
        store.delete_vertex_by_entity(&id).unwrap();
    }

    #[test]
    fn test_entity_add_relationship() {
        let store = GraphStore::new();
        let product = EntityId::new("Product", "001");
        let category = EntityId::new("Category", "electronics");

        store.add_relationship(&product, &category, "belongs_to").unwrap();

        let neighbors = store.get_entity_neighbors(&product, Direction::Out, Some("belongs_to"));
        assert_eq!(neighbors.len(), 1);
        assert_eq!(neighbors[0], category);
    }

    #[test]
    fn test_entity_add_relationship_creates_vertices() {
        let store = GraphStore::new();
        let from = EntityId::new("Employee", "alice");
        let to = EntityId::new("Employee", "bob");

        store.add_relationship(&from, &to, "reports_to").unwrap();

        // Both vertices should be auto-created
        assert!(store.has_entity(&from));
        assert!(store.has_entity(&to));
    }

    #[test]
    fn test_entity_add_relationship_with_props() {
        let store = GraphStore::new();
        let from = EntityId::new("Person", "alice");
        let to = EntityId::new("Person", "bob");

        let mut props = PropertyMap::new();
        props.insert("since".to_string(), PropValue::Int(2020));

        store.add_relationship_with_props(&from, &to, "knows", props).unwrap();

        let neighbors = store.get_entity_neighbors(&from, Direction::Out, Some("knows"));
        assert_eq!(neighbors.len(), 1);
        assert_eq!(neighbors[0], to);
    }

    #[test]
    fn test_entity_delete_relationship() {
        let store = GraphStore::new();
        let from = EntityId::new("Product", "001");
        let to = EntityId::new("Category", "electronics");

        store.add_relationship(&from, &to, "belongs_to").unwrap();
        assert_eq!(store.edge_count(), 1);

        store.delete_relationship(&from, &to, "belongs_to").unwrap();
        assert_eq!(store.edge_count(), 0);
    }

    #[test]
    fn test_entity_get_neighbors_both_directions() {
        let store = GraphStore::new();
        let alice = EntityId::new("Person", "alice");
        let bob = EntityId::new("Person", "bob");

        store.add_relationship(&alice, &bob, "knows").unwrap();

        // Outgoing
        let out = store.get_entity_neighbors(&alice, Direction::Out, None);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0], bob);

        // Incoming
        let inp = store.get_entity_neighbors(&bob, Direction::In, None);
        assert_eq!(inp.len(), 1);
        assert_eq!(inp[0], alice);

        // Both
        let both = store.get_entity_neighbors(&alice, Direction::Both, None);
        assert_eq!(both.len(), 1);
    }

    #[test]
    fn test_entity_get_neighbors_filter_label() {
        let store = GraphStore::new();
        let alice = EntityId::new("Person", "alice");
        let bob = EntityId::new("Person", "bob");
        let company = EntityId::new("Company", "acme");

        store.add_relationship(&alice, &bob, "knows").unwrap();
        store.add_relationship(&alice, &company, "works_at").unwrap();

        // Filter by label
        let friends = store.get_entity_neighbors(&alice, Direction::Out, Some("knows"));
        assert_eq!(friends.len(), 1);
        assert_eq!(friends[0], bob);

        let workplaces = store.get_entity_neighbors(&alice, Direction::Out, Some("works_at"));
        assert_eq!(workplaces.len(), 1);
        assert_eq!(workplaces[0], company);
    }

    #[test]
    fn test_entity_get_entities_by_class() {
        let store = GraphStore::new();

        store.upsert_vertex_from_entity(
            &EntityId::new("Product", "001"),
            &["Product".to_string()],
        ).unwrap();
        store.upsert_vertex_from_entity(
            &EntityId::new("Product", "002"),
            &["Product".to_string()],
        ).unwrap();
        store.upsert_vertex_from_entity(
            &EntityId::new("Category", "electronics"),
            &["Category".to_string()],
        ).unwrap();

        let products = store.get_entities_by_class("Product");
        assert_eq!(products.len(), 2);

        let categories = store.get_entities_by_class("Category");
        assert_eq!(categories.len(), 1);
    }
}
