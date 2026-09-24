//! Distributed graph storage with consistent hashing and cross-partition traversal.
//!
//! Architecture:
//! - Vertices are assigned to partitions via consistent hashing on vertex ID
//! - Edges follow their endpoints (stored in the partition of the source vertex)
//! - Cross-partition edges store remote references (ghost vertices)
//! - Queries are routed to the appropriate partition(s)

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use parking_lot::RwLock;

use crate::error::GraphError;
use crate::model::{Edge, PropValue, PropertyMap, Vertex};
use crate::store::GraphStore;

/// Partition identifier.
pub type PartitionId = u32;

/// Remote vertex reference (ghost vertex).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteVertex {
    /// The vertex ID.
    pub id: String,
    /// The partition that owns this vertex.
    pub partition: PartitionId,
    /// Labels of the remote vertex.
    pub labels: Vec<String>,
}

/// A graph partition containing a local GraphStore.
pub struct GraphPartition {
    /// Partition ID.
    pub id: PartitionId,
    /// Local graph store.
    pub store: Arc<GraphStore>,
    /// Ghost vertices: remote vertex ID -> partition ID.
    /// These are vertices referenced by edges but owned by other partitions.
    ghost_vertices: RwLock<HashMap<String, RemoteVertex>>,
    /// Statistics.
    stats: RwLock<PartitionStats>,
}

/// Partition statistics.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PartitionStats {
    /// Number of local vertices.
    pub local_vertices: usize,
    /// Number of ghost vertices.
    pub ghost_vertices: usize,
    /// Number of local edges.
    pub local_edges: usize,
    /// Number of cross-partition edges.
    pub cross_partition_edges: usize,
}

impl GraphPartition {
    /// Create a new graph partition.
    pub fn new(id: PartitionId, store: Arc<GraphStore>) -> Self {
        Self {
            id,
            store,
            ghost_vertices: RwLock::new(HashMap::new()),
            stats: RwLock::new(PartitionStats::default()),
        }
    }

    /// Add a vertex to this partition.
    pub fn add_vertex(&self, vertex: Vertex) -> Result<(), GraphError> {
        self.store.add_vertex(vertex)?;
        self.stats.write().local_vertices = self.store.vertex_count();
        Ok(())
    }

    /// Add an edge. If the target is in another partition, creates a ghost vertex.
    pub fn add_edge(&self, edge: Edge, target_partition: PartitionId) -> Result<(), GraphError> {
        // Ensure source vertex exists locally
        if self.store.get_vertex(&edge.from).is_none() {
            return Err(GraphError::VertexNotFound(edge.from.clone()));
        }

        // If target is in another partition, create ghost vertex
        if target_partition != self.id {
            if self.store.get_vertex(&edge.to).is_none() {
                let ghost = RemoteVertex {
                    id: edge.to.clone(),
                    partition: target_partition,
                    labels: Vec::new(), // Will be filled on demand
                };
                self.ghost_vertices.write().insert(edge.to.clone(), ghost);
                self.stats.write().ghost_vertices = self.ghost_vertices.read().len();

                // Create a placeholder vertex for the ghost
                self.store.add_vertex(Vertex::new(&edge.to, vec!["__ghost__".to_string()]))?;
            }
            self.stats.write().cross_partition_edges += 1;
        }

        self.store.add_edge(edge)?;
        self.stats.write().local_edges = self.store.edge_count();
        Ok(())
    }

    /// Check if a vertex is a ghost (remote reference).
    pub fn is_ghost(&self, vertex_id: &str) -> bool {
        self.ghost_vertices.read().contains_key(vertex_id)
    }

    /// Get the owning partition of a ghost vertex.
    pub fn ghost_partition(&self, vertex_id: &str) -> Option<PartitionId> {
        self.ghost_vertices.read().get(vertex_id).map(|g| g.partition)
    }

    /// Get partition statistics.
    pub fn stats(&self) -> PartitionStats {
        self.stats.read().clone()
    }
}

/// Consistent hash ring for partition assignment.
pub struct ConsistentHash {
    /// Number of virtual nodes per physical partition.
    replicas: usize,
    /// Hash ring: hash -> partition ID.
    ring: Vec<(u32, PartitionId)>,
    /// Number of partitions.
    num_partitions: u32,
}

impl ConsistentHash {
    /// Create a new consistent hash ring.
    pub fn new(num_partitions: u32, replicas: usize) -> Self {
        let mut ring = Vec::with_capacity(num_partitions as usize * replicas);

        for partition in 0..num_partitions {
            for rep in 0..replicas {
                let hash = Self::hash(&format!("{}:{}", partition, rep));
                ring.push((hash, partition));
            }
        }

        ring.sort_by_key(|&(hash, _)| hash);

        Self {
            replicas,
            ring,
            num_partitions,
        }
    }

    /// Get the partition for a given key.
    pub fn get_partition(&self, key: &str) -> PartitionId {
        if self.ring.is_empty() {
            return 0;
        }

        let hash = Self::hash(key);

        // Binary search for the first ring entry >= hash
        match self.ring.binary_search_by_key(&hash, |&(h, _)| h) {
            Ok(idx) => self.ring[idx].1,
            Err(idx) => {
                if idx >= self.ring.len() {
                    self.ring[0].1 // Wrap around
                } else {
                    self.ring[idx].1
                }
            }
        }
    }

    /// Get all partitions (for broadcast queries).
    pub fn all_partitions(&self) -> Vec<PartitionId> {
        (0..self.num_partitions).collect()
    }

    /// Hash function (FNV-1a variant).
    fn hash(key: &str) -> u32 {
        let mut hash: u32 = 0x811c9dc5;
        for byte in key.bytes() {
            hash ^= byte as u32;
            hash = hash.wrapping_mul(0x01000193);
        }
        hash
    }
}

/// Distributed graph manager.
///
/// Manages multiple partitions and routes queries accordingly.
pub struct DistributedGraph {
    /// Partitions: partition ID -> GraphPartition.
    partitions: HashMap<PartitionId, Arc<GraphPartition>>,
    /// Consistent hash ring for partition assignment.
    hash_ring: ConsistentHash,
    /// Number of partitions.
    num_partitions: u32,
}

impl DistributedGraph {
    /// Create a new distributed graph with the given number of partitions.
    pub fn new(num_partitions: u32) -> Self {
        let mut partitions = HashMap::new();

        for id in 0..num_partitions {
            let store = Arc::new(GraphStore::new());
            let partition = Arc::new(GraphPartition::new(id, store));
            partitions.insert(id, partition);
        }

        let hash_ring = ConsistentHash::new(num_partitions, 150); // 150 virtual nodes per partition

        Self {
            partitions,
            hash_ring,
            num_partitions,
        }
    }

    /// Get the partition for a vertex ID.
    pub fn partition_for(&self, vertex_id: &str) -> PartitionId {
        self.hash_ring.get_partition(vertex_id)
    }

    /// Get a partition by ID.
    pub fn get_partition(&self, id: PartitionId) -> Option<&Arc<GraphPartition>> {
        self.partitions.get(&id)
    }

    /// Get all partition IDs.
    pub fn partition_ids(&self) -> Vec<PartitionId> {
        self.partitions.keys().copied().collect()
    }

    /// Add a vertex to the appropriate partition.
    pub fn add_vertex(&self, vertex: Vertex) -> Result<PartitionId, GraphError> {
        let partition_id = self.partition_for(&vertex.id);
        let partition = self.partitions.get(&partition_id)
            .ok_or_else(|| GraphError::StorageError("Partition not found".to_string()))?;
        partition.add_vertex(vertex)?;
        Ok(partition_id)
    }

    /// Add an edge, routing to the appropriate partition.
    pub fn add_edge(&self, edge: Edge) -> Result<PartitionId, GraphError> {
        let source_partition = self.partition_for(&edge.from);
        let target_partition = self.partition_for(&edge.to);

        let partition = self.partitions.get(&source_partition)
            .ok_or_else(|| GraphError::StorageError("Partition not found".to_string()))?;

        partition.add_edge(edge, target_partition)?;
        Ok(source_partition)
    }

    /// Get a vertex from the appropriate partition.
    pub fn get_vertex(&self, vertex_id: &str) -> Option<Vertex> {
        let partition_id = self.partition_for(vertex_id);
        self.partitions.get(&partition_id)
            .and_then(|p| p.store.get_vertex(vertex_id))
    }

    /// Get outgoing edges for a vertex (handles cross-partition edges).
    pub fn get_out_edges(&self, vertex_id: &str) -> Vec<Edge> {
        let partition_id = self.partition_for(vertex_id);
        self.partitions.get(&partition_id)
            .map(|p| p.store.get_out_edges(vertex_id))
            .unwrap_or_default()
    }

    /// Get neighbors of a vertex (handles cross-partition traversal).
    pub fn get_neighbors(&self, vertex_id: &str) -> Vec<Vertex> {
        let edges = self.get_out_edges(vertex_id);
        let mut neighbors = Vec::new();

        for edge in edges {
            let target_partition = self.partition_for(&edge.to);
            if let Some(partition) = self.partitions.get(&target_partition) {
                if let Some(vertex) = partition.store.get_vertex(&edge.to) {
                    neighbors.push(vertex);
                }
            }
        }

        neighbors
    }

    /// BFS across partitions.
    /// Returns all vertices reachable within max_depth hops.
    pub fn distributed_bfs(&self, start_id: &str, max_depth: usize) -> Vec<String> {
        let mut visited = HashSet::new();
        visited.insert(start_id.to_string());

        let mut current_level = vec![start_id.to_string()];
        let mut result = Vec::new();

        for _ in 0..max_depth {
            let mut next_level = Vec::new();

            for vertex_id in &current_level {
                let neighbors = self.get_neighbors(vertex_id);
                for neighbor in neighbors {
                    if !visited.contains(&neighbor.id) {
                        visited.insert(neighbor.id.clone());
                        next_level.push(neighbor.id.clone());
                        result.push(neighbor.id.clone());
                    }
                }
            }

            if next_level.is_empty() {
                break;
            }
            current_level = next_level;
        }

        result
    }

    /// Get global statistics across all partitions.
    pub fn global_stats(&self) -> GlobalStats {
        let mut stats = GlobalStats::default();

        for partition in self.partitions.values() {
            let ps = partition.stats();
            stats.total_vertices += ps.local_vertices;
            stats.total_ghost_vertices += ps.ghost_vertices;
            stats.total_edges += ps.local_edges;
            stats.total_cross_partition_edges += ps.cross_partition_edges;
        }

        stats.num_partitions = self.num_partitions;
        stats
    }

    /// Rebalance partitions (move vertices to balance load).
    /// Returns the number of vertices moved.
    pub fn rebalance(&self) -> usize {
        // Simple rebalancing: move vertices from overloaded to underloaded partitions
        let stats: Vec<(PartitionId, usize)> = self.partitions.iter()
            .map(|(&id, p)| (id, p.store.vertex_count()))
            .collect();

        let total: usize = stats.iter().map(|(_, c)| c).sum();
        let target = total / self.num_partitions as usize;

        let mut moved = 0;

        // Find overloaded and underloaded partitions
        let overloaded: Vec<PartitionId> = stats.iter()
            .filter(|(_, c)| *c > target * 2)
            .map(|(id, _)| *id)
            .collect();

        let underloaded: Vec<PartitionId> = stats.iter()
            .filter(|(_, c)| *c < target / 2)
            .map(|(id, _)| *id)
            .collect();

        // Move vertices from overloaded to underloaded
        for &from_id in &overloaded {
            if underloaded.is_empty() {
                break;
            }

            if let Some(from_partition) = self.partitions.get(&from_id) {
                let vertices: Vec<Vertex> = from_partition.store.get_all_vertices()
                    .into_iter()
                    .take(from_partition.store.vertex_count() - target)
                    .collect();

                for vertex in vertices {
                    let to_id = underloaded[moved % underloaded.len()];
                    if let Some(to_partition) = self.partitions.get(&to_id) {
                        let _ = to_partition.add_vertex(vertex);
                        moved += 1;
                    }
                }
            }
        }

        moved
    }
}

/// Global statistics across all partitions.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GlobalStats {
    /// Number of partitions.
    pub num_partitions: u32,
    /// Total local vertices across all partitions.
    pub total_vertices: usize,
    /// Total ghost vertices across all partitions.
    pub total_ghost_vertices: usize,
    /// Total edges across all partitions.
    pub total_edges: usize,
    /// Total cross-partition edges.
    pub total_cross_partition_edges: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Edge, Vertex};

    #[test]
    fn test_consistent_hash_basic() {
        let hash = ConsistentHash::new(4, 10);

        // Same key should always go to same partition
        let p1 = hash.get_partition("vertex_1");
        let p2 = hash.get_partition("vertex_1");
        assert_eq!(p1, p2);

        // Different keys may go to different partitions
        let p3 = hash.get_partition("vertex_2");
        // Not guaranteed to be different, but should be valid
        assert!(p3 < 4);
    }

    #[test]
    fn test_consistent_hash_distribution() {
        let hash = ConsistentHash::new(4, 150);
        let mut counts = vec![0; 4];

        for i in 0..1000 {
            let key = format!("key_{}", i);
            let partition = hash.get_partition(&key);
            counts[partition as usize] += 1;
        }

        // Each partition should get roughly 25% of keys (within 50% tolerance)
        for count in &counts {
            assert!(*count > 100, "Partition got too few keys: {}", count);
            assert!(*count < 400, "Partition got too many keys: {}", count);
        }
    }

    #[test]
    fn test_distributed_graph_basic() {
        let graph = DistributedGraph::new(4);

        // Add vertices
        let v1 = Vertex::new("user_1", vec!["Person".to_string()]);
        let v2 = Vertex::new("user_2", vec!["Person".to_string()]);

        let p1 = graph.add_vertex(v1).unwrap();
        let p2 = graph.add_vertex(v2).unwrap();

        // Add edge
        let edge = Edge::new("e1", "user_1", "user_2", "knows");
        let p_edge = graph.add_edge(edge).unwrap();

        // Edge should be in source's partition
        assert_eq!(p_edge, p1);

        // Get vertex
        let retrieved = graph.get_vertex("user_1");
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().id, "user_1");
    }

    #[test]
    fn test_distributed_bfs() {
        let graph = DistributedGraph::new(2);

        // Create a chain: v1 -> v2 -> v3
        graph.add_vertex(Vertex::new("v1", vec![])).unwrap();
        graph.add_vertex(Vertex::new("v2", vec![])).unwrap();
        graph.add_vertex(Vertex::new("v3", vec![])).unwrap();

        graph.add_edge(Edge::new("e1", "v1", "v2", "LINK")).unwrap();
        graph.add_edge(Edge::new("e2", "v2", "v3", "LINK")).unwrap();

        // BFS from v1 with depth 2
        let result = graph.distributed_bfs("v1", 2);
        assert!(result.contains(&"v2".to_string()));
        assert!(result.contains(&"v3".to_string()));
    }

    #[test]
    fn test_cross_partition_edge() {
        let graph = DistributedGraph::new(2);

        // Force vertices into different partitions
        let v1 = Vertex::new("aaa", vec![]); // hash will assign to some partition
        let v2 = Vertex::new("bbb", vec![]); // may be in different partition

        graph.add_vertex(v1).unwrap();
        graph.add_vertex(v2).unwrap();

        // Add cross-partition edge
        graph.add_edge(Edge::new("e1", "aaa", "bbb", "LINK")).unwrap();

        // Should be able to traverse
        let neighbors = graph.get_neighbors("aaa");
        assert_eq!(neighbors.len(), 1);
        assert_eq!(neighbors[0].id, "bbb");
    }

    #[test]
    fn test_global_stats() {
        let graph = DistributedGraph::new(4);

        for i in 0..100 {
            graph.add_vertex(Vertex::new(format!("v{}", i), vec![])).unwrap();
        }

        let stats = graph.global_stats();
        assert_eq!(stats.num_partitions, 4);
        assert_eq!(stats.total_vertices, 100);
    }
}