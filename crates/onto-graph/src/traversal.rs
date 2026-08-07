//! Graph traversal engine - BFS/DFS with property filtering.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::error::GraphError;
use crate::model::{Edge, PropValue, Vertex};
use crate::store::GraphStore;

/// Traversal direction.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Direction {
    Out,
    In,
    Both,
}

/// Traversal result containing visited vertices and edges.
#[derive(Debug, Clone)]
pub struct TraversalResult {
    /// Vertices visited during traversal.
    pub vertices: Vec<Vertex>,
    /// Edges traversed.
    pub edges: Vec<Edge>,
    /// Paths from source to each vertex.
    pub paths: Vec<TraversalPath>,
    /// Total vertices visited.
    pub visited_count: usize,
}

/// A path from source to a vertex.
#[derive(Debug, Clone)]
pub struct TraversalPath {
    /// Vertex IDs along the path.
    pub vertex_ids: Vec<String>,
    /// Edge IDs along the path.
    pub edge_ids: Vec<String>,
    /// The target vertex.
    pub target: Vertex,
    /// Path length (number of edges).
    pub length: usize,
}

/// Filter for vertex/edge properties.
#[derive(Debug, Clone)]
pub struct PropertyFilter {
    pub key: String,
    pub op: FilterOp,
    pub value: PropValue,
}

#[derive(Debug, Clone)]
pub enum FilterOp {
    Eq,
    Neq,
    Gt,
    Lt,
    Gte,
    Lte,
    Contains,
}

impl PropertyFilter {
    pub fn matches(&self, props: &HashMap<String, PropValue>) -> bool {
        let Some(val) = props.get(&self.key) else {
            return false;
        };
        match &self.op {
            FilterOp::Eq => val == &self.value,
            FilterOp::Neq => val != &self.value,
            FilterOp::Gt => val > &self.value,
            FilterOp::Lt => val < &self.value,
            FilterOp::Gte => val >= &self.value,
            FilterOp::Lte => val <= &self.value,
            FilterOp::Contains => {
                if let (PropValue::String(haystack), PropValue::String(needle)) = (val, &self.value) {
                    haystack.contains(needle.as_str())
                } else {
                    false
                }
            }
        }
    }
}

/// Traversal engine for graph queries.
pub struct TraversalEngine<'a> {
    store: &'a GraphStore,
}

impl<'a> TraversalEngine<'a> {
    pub fn new(store: &'a GraphStore) -> Self {
        Self { store }
    }

    /// Fast multi-hop BFS using integer indices (no string allocations).
    /// Returns vertex IDs of visited nodes.
    pub fn traverse_bfs_fast(
        &self,
        start_id: &str,
        max_depth: usize,
        direction: Direction,
    ) -> Result<Vec<String>, GraphError> {
        let start_idx = self.store.get_idx(start_id)
            .ok_or_else(|| GraphError::VertexNotFound(start_id.to_string()))?;

        let result_indices = self.store.bfs_fast(start_idx, max_depth, direction);

        let result: Vec<String> = result_indices
            .into_iter()
            .filter_map(|idx| self.store.get_id(idx))
            .collect();

        Ok(result)
    }

    /// Single-hop traversal: get neighbors of a vertex.
    pub fn hop(
        &self,
        vertex_id: &str,
        direction: Direction,
        edge_label: Option<&str>,
        vertex_filter: Option<&PropertyFilter>,
        edge_filter: Option<&PropertyFilter>,
    ) -> Result<Vec<Vertex>, GraphError> {
        let edges = match direction {
            Direction::Out => self.store.get_out_edges(vertex_id),
            Direction::In => self.store.get_in_edges(vertex_id),
            Direction::Both => {
                let mut e = self.store.get_out_edges(vertex_id);
                e.extend(self.store.get_in_edges(vertex_id));
                e
            }
        };

        let filtered_edges: Vec<&Edge> = edges
            .iter()
            .filter(|e| {
                if let Some(label) = edge_label {
                    if e.label != label {
                        return false;
                    }
                }
                if let Some(f) = edge_filter {
                    if !f.matches(&e.properties) {
                        return false;
                    }
                }
                true
            })
            .collect();

        let mut result = Vec::new();
        let mut seen = HashSet::new();

        for edge in filtered_edges {
            let target_id = match direction {
                Direction::In => &edge.from,
                _ => &edge.to,
            };

            if seen.contains(target_id) {
                continue;
            }
            seen.insert(target_id.clone());

            if let Some(vertex) = self.store.get_vertex(target_id) {
                if let Some(f) = vertex_filter {
                    if !f.matches(&vertex.properties) {
                        continue;
                    }
                }
                result.push(vertex);
            }
        }

        Ok(result)
    }

    /// Multi-hop BFS traversal (optimized).
    pub fn traverse_bfs(
        &self,
        start_id: &str,
        max_depth: usize,
        direction: Direction,
        edge_label: Option<&str>,
        vertex_filter: Option<&PropertyFilter>,
    ) -> Result<TraversalResult, GraphError> {
        let mut visited: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<(String, usize)> = VecDeque::new(); // (id, depth)
        let mut result_vertices = Vec::new();
        let mut result_edges = Vec::new();
        let mut result_paths = Vec::new();
        // Parent tracking for path reconstruction (child -> (parent, edge_id))
        let mut parent_map: HashMap<String, Option<(String, String)>> = HashMap::new();

        queue.push_back((start_id.to_string(), 0));
        visited.insert(start_id.to_string());
        parent_map.insert(start_id.to_string(), None);

        while let Some((current_id, depth)) = queue.pop_front() {
            if depth > 0 {
                if let Some(vertex) = self.store.get_vertex(&current_id) {
                    if let Some(f) = vertex_filter {
                        if !f.matches(&vertex.properties) {
                            continue;
                        }
                    }
                    // Reconstruct path from parent_map
                    let (path_verts, path_edges) = self.reconstruct_path(&parent_map, &current_id);
                    result_vertices.push(vertex.clone());
                    result_paths.push(TraversalPath {
                        vertex_ids: path_verts,
                        edge_ids: path_edges,
                        target: vertex,
                        length: depth,
                    });
                }
            }

            if depth >= max_depth {
                continue;
            }

            // Get neighbors
            let edges = match direction {
                Direction::Out => self.store.get_out_edges(&current_id),
                Direction::In => self.store.get_in_edges(&current_id),
                Direction::Both => {
                    let mut e = self.store.get_out_edges(&current_id);
                    e.extend(self.store.get_in_edges(&current_id));
                    e
                }
            };

            for edge in edges {
                if let Some(label) = edge_label {
                    if edge.label != label {
                        continue;
                    }
                }

                let next_id = match direction {
                    Direction::In => edge.from.clone(),
                    _ => edge.to.clone(),
                };

                if visited.contains(&next_id) {
                    continue;
                }
                visited.insert(next_id.clone());

                result_edges.push(edge.clone());
                parent_map.insert(next_id.clone(), Some((current_id.clone(), edge.id.clone())));
                queue.push_back((next_id, depth + 1));
            }
        }

        Ok(TraversalResult {
            vertices: result_vertices,
            edges: result_edges,
            paths: result_paths,
            visited_count: visited.len(),
        })
    }

    /// Reconstruct path from parent_map (lazy path construction).
    fn reconstruct_path(
        &self,
        parent_map: &HashMap<String, Option<(String, String)>>,
        target: &str,
    ) -> (Vec<String>, Vec<String>) {
        let mut verts = Vec::new();
        let mut edges = Vec::new();
        let mut current = target.to_string();

        loop {
            verts.push(current.clone());
            match parent_map.get(&current) {
                Some(Some((parent, edge_id))) => {
                    edges.push(edge_id.clone());
                    current = parent.clone();
                }
                _ => break,
            }
        }

        verts.reverse();
        edges.reverse();
        (verts, edges)
    }

    /// Multi-hop DFS traversal.
    pub fn traverse_dfs(
        &self,
        start_id: &str,
        max_depth: usize,
        direction: Direction,
        edge_label: Option<&str>,
        vertex_filter: Option<&PropertyFilter>,
    ) -> Result<TraversalResult, GraphError> {
        let mut visited = HashSet::new();
        let mut result_vertices = Vec::new();
        let mut result_edges = Vec::new();
        let mut result_paths = Vec::new();

        self.dfs_recursive(
            start_id,
            0,
            max_depth,
            &direction,
            edge_label,
            vertex_filter,
            &mut visited,
            &mut result_vertices,
            &mut result_edges,
            &mut result_paths,
            &vec![start_id.to_string()],
            &vec![],
        )?;

        Ok(TraversalResult {
            vertices: result_vertices,
            edges: result_edges,
            paths: result_paths,
            visited_count: visited.len(),
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn dfs_recursive(
        &self,
        current_id: &str,
        depth: usize,
        max_depth: usize,
        direction: &Direction,
        edge_label: Option<&str>,
        vertex_filter: Option<&PropertyFilter>,
        visited: &mut HashSet<String>,
        result_vertices: &mut Vec<Vertex>,
        result_edges: &mut Vec<Edge>,
        result_paths: &mut Vec<TraversalPath>,
        path_verts: &[String],
        path_edges: &[String],
    ) -> Result<(), GraphError> {
        if depth > 0 {
            if visited.contains(current_id) {
                return Ok(());
            }
            visited.insert(current_id.to_string());

            if let Some(vertex) = self.store.get_vertex(current_id) {
                if let Some(f) = vertex_filter {
                    if !f.matches(&vertex.properties) {
                        return Ok(());
                    }
                }
                result_vertices.push(vertex.clone());
                result_paths.push(TraversalPath {
                    vertex_ids: path_verts.to_vec(),
                    edge_ids: path_edges.to_vec(),
                    target: vertex,
                    length: depth,
                });
            }
        } else {
            visited.insert(current_id.to_string());
        }

        if depth >= max_depth {
            return Ok(());
        }

        let edges = match direction {
            Direction::Out => self.store.get_out_edges(current_id),
            Direction::In => self.store.get_in_edges(current_id),
            Direction::Both => {
                let mut e = self.store.get_out_edges(current_id);
                e.extend(self.store.get_in_edges(current_id));
                e
            }
        };

        for edge in edges {
            if let Some(label) = edge_label {
                if edge.label != label {
                    continue;
                }
            }

            let next_id = match direction {
                Direction::In => edge.from.clone(),
                _ => edge.to.clone(),
            };

            result_edges.push(edge.clone());

            let mut new_path_verts = path_verts.to_vec();
            new_path_verts.push(next_id.clone());

            let mut new_path_edges = path_edges.to_vec();
            new_path_edges.push(edge.id.clone());

            self.dfs_recursive(
                &next_id,
                depth + 1,
                max_depth,
                direction,
                edge_label,
                vertex_filter,
                visited,
                result_vertices,
                result_edges,
                result_paths,
                &new_path_verts,
                &new_path_edges,
            )?;
        }

        Ok(())
    }

    /// Find shortest path between two vertices (BFS).
    pub fn shortest_path(
        &self,
        from_id: &str,
        to_id: &str,
        max_depth: usize,
    ) -> Result<Option<TraversalPath>, GraphError> {
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();

        queue.push_back((from_id.to_string(), 0, vec![from_id.to_string()], vec![]));
        visited.insert(from_id.to_string());

        while let Some((current_id, depth, path_verts, path_edges)) = queue.pop_front() {
            if current_id == to_id && depth > 0 {
                let target = self.store.get_vertex(to_id)
                    .ok_or_else(|| GraphError::VertexNotFound(to_id.to_string()))?;
                return Ok(Some(TraversalPath {
                    vertex_ids: path_verts,
                    edge_ids: path_edges,
                    target,
                    length: depth,
                }));
            }

            if depth >= max_depth {
                continue;
            }

            for edge in self.store.get_out_edges(&current_id) {
                let next_id = edge.to.clone();
                if visited.contains(&next_id) {
                    continue;
                }
                visited.insert(next_id.clone());

                let mut new_path_verts = path_verts.clone();
                new_path_verts.push(next_id.clone());

                let mut new_path_edges = path_edges.clone();
                new_path_edges.push(edge.id.clone());

                queue.push_back((next_id, depth + 1, new_path_verts, new_path_edges));
            }
        }

        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Vertex;

    fn build_social_graph() -> GraphStore {
        let store = GraphStore::new();

        // Add vertices
        store.add_vertex(Vertex::new("alice", vec!["Person".to_string()])
            .with_property("name", PropValue::String("Alice".to_string()))
            .with_property("age", PropValue::Int(30))).unwrap();
        store.add_vertex(Vertex::new("bob", vec!["Person".to_string()])
            .with_property("name", PropValue::String("Bob".to_string()))
            .with_property("age", PropValue::Int(25))).unwrap();
        store.add_vertex(Vertex::new("charlie", vec!["Person".to_string()])
            .with_property("name", PropValue::String("Charlie".to_string()))
            .with_property("age", PropValue::Int(35))).unwrap();
        store.add_vertex(Vertex::new("dave", vec!["Person".to_string()])
            .with_property("name", PropValue::String("Dave".to_string()))
            .with_property("age", PropValue::Int(28))).unwrap();
        store.add_vertex(Vertex::new("acme", vec!["Company".to_string()])
            .with_property("name", PropValue::String("Acme Corp".to_string()))).unwrap();

        // Add edges
        store.add_edge(Edge::new("e1", "alice", "bob", "KNOWS")).unwrap();
        store.add_edge(Edge::new("e2", "alice", "charlie", "KNOWS")).unwrap();
        store.add_edge(Edge::new("e3", "bob", "dave", "KNOWS")).unwrap();
        store.add_edge(Edge::new("e4", "charlie", "dave", "KNOWS")).unwrap();
        store.add_edge(Edge::new("e5", "alice", "acme", "WORKS_AT")).unwrap();
        store.add_edge(Edge::new("e6", "bob", "acme", "WORKS_AT")).unwrap();

        store
    }

    #[test]
    fn test_single_hop() {
        let store = build_social_graph();
        let engine = TraversalEngine::new(&store);

        // Alice's friends
        let friends = engine.hop("alice", Direction::Out, Some("KNOWS"), None, None).unwrap();
        assert_eq!(friends.len(), 2);
        let names: Vec<String> = friends.iter().map(|v| {
            v.properties.get("name").unwrap().to_string()
        }).collect();
        assert!(names.contains(&"Bob".to_string()));
        assert!(names.contains(&"Charlie".to_string()));
    }

    #[test]
    fn test_single_hop_with_filter() {
        let store = build_social_graph();
        let engine = TraversalEngine::new(&store);

        // Alice's friends older than 30
        let filter = PropertyFilter {
            key: "age".to_string(),
            op: FilterOp::Gt,
            value: PropValue::Int(30),
        };
        let friends = engine.hop("alice", Direction::Out, Some("KNOWS"), Some(&filter), None).unwrap();
        assert_eq!(friends.len(), 1);
        assert_eq!(friends[0].id, "charlie");
    }

    #[test]
    fn test_multi_hop_bfs() {
        let store = build_social_graph();
        let engine = TraversalEngine::new(&store);

        // 2-hop from Alice
        let result = engine.traverse_bfs("alice", 2, Direction::Out, Some("KNOWS"), None).unwrap();
        assert_eq!(result.vertices.len(), 3); // Bob, Charlie, Dave
        assert_eq!(result.paths.len(), 3);
    }

    #[test]
    fn test_shortest_path() {
        let store = build_social_graph();
        let engine = TraversalEngine::new(&store);

        // Shortest path from Alice to Dave
        let path = engine.shortest_path("alice", "dave", 5).unwrap().unwrap();
        assert_eq!(path.length, 2); // Alice -> Bob -> Dave or Alice -> Charlie -> Dave
        assert_eq!(path.vertex_ids[0], "alice");
        assert_eq!(path.vertex_ids[2], "dave");
    }

    #[test]
    fn test_direction_in() {
        let store = build_social_graph();
        let engine = TraversalEngine::new(&store);

        // Who works at Acme?
        let employees = engine.hop("acme", Direction::In, Some("WORKS_AT"), None, None).unwrap();
        assert_eq!(employees.len(), 2);
    }
}
