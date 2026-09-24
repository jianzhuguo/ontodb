//! Real-time streaming graph computation engine.
//!
//! Features:
//! - Event-driven graph updates with change propagation
//! - Incremental algorithm updates (no full recomputation)
//! - Window-based aggregations (tumbling, sliding)
//! - Streaming PageRank, connected components, degree tracking

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};
use parking_lot::RwLock;

use crate::store::GraphStore;
use crate::model::{Edge, Vertex, PropValue};

/// A graph change event for streaming computation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GraphChangeEvent {
    /// A vertex was added.
    VertexAdded { id: String, labels: Vec<String> },
    /// A vertex was removed.
    VertexRemoved { id: String },
    /// An edge was added.
    EdgeAdded { source: String, target: String, label: String },
    /// An edge was removed.
    EdgeRemoved { source: String, target: String, label: String },
}

/// Window type for streaming aggregations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WindowType {
    /// Fixed-size tumbling window.
    Tumbling { duration: Duration },
    /// Sliding window with overlap.
    Sliding { duration: Duration, slide: Duration },
    /// Session window (gap-based).
    Session { gap: Duration },
}

/// A windowed aggregation result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowedResult<T> {
    /// Window start time (seconds since epoch).
    pub start: u64,
    /// Window end time (seconds since epoch).
    pub end: u64,
    /// Aggregated value.
    pub value: T,
    /// Number of events in window.
    pub event_count: usize,
}

/// Streaming degree statistics per vertex.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StreamingDegree {
    /// In-degree (number of incoming edges).
    pub in_degree: u32,
    /// Out-degree (number of outgoing edges).
    pub out_degree: u32,
    /// Last update timestamp.
    pub last_updated: u64,
}

/// Incremental connected components tracker.
#[derive(Debug, Clone)]
pub struct IncrementalComponents {
    /// Parent array for union-find.
    parent: HashMap<String, String>,
    /// Rank array for union-find optimization.
    rank: HashMap<String, u32>,
    /// Component sizes.
    sizes: HashMap<String, u32>,
}

impl IncrementalComponents {
    /// Create a new incremental components tracker.
    pub fn new() -> Self {
        Self {
            parent: HashMap::new(),
            rank: HashMap::new(),
            sizes: HashMap::new(),
        }
    }

    /// Find the root of a vertex (with path compression).
    pub fn find(&mut self, vertex: &str) -> String {
        if !self.parent.contains_key(vertex) {
            self.parent.insert(vertex.to_string(), vertex.to_string());
            self.rank.insert(vertex.to_string(), 0);
            self.sizes.insert(vertex.to_string(), 1);
        }

        let mut root = vertex.to_string();
        while self.parent[&root] != root {
            root = self.parent[&root].clone();
        }

        // Path compression
        let mut current = vertex.to_string();
        while self.parent[&current] != root {
            let next = self.parent[&current].clone();
            self.parent.insert(current.clone(), root.clone());
            current = next;
        }

        root
    }

    /// Union two vertices (merge their components).
    pub fn union(&mut self, a: &str, b: &str) -> bool {
        let root_a = self.find(a);
        let root_b = self.find(b);

        if root_a == root_b {
            return false; // Already in same component
        }

        let rank_a = self.rank[&root_a];
        let rank_b = self.rank[&root_b];

        // Union by rank
        let (smaller, larger) = if rank_a < rank_b {
            (root_a.clone(), root_b.clone())
        } else {
            (root_b.clone(), root_a.clone())
        };

        self.parent.insert(smaller.clone(), larger.clone());
        let size = self.sizes[&smaller];
        *self.sizes.entry(larger.clone()).or_insert(0) += size;
        self.sizes.remove(&smaller);

        if rank_a == rank_b {
            *self.rank.entry(larger).or_insert(0) += 1;
        }

        true
    }

    /// Check if two vertices are in the same component.
    pub fn connected(&mut self, a: &str, b: &str) -> bool {
        self.find(a) == self.find(b)
    }

    /// Get the number of components.
    pub fn component_count(&self) -> usize {
        self.sizes.len()
    }

    /// Get the size of a vertex's component.
    pub fn component_size(&mut self, vertex: &str) -> u32 {
        let root = self.find(vertex);
        self.sizes.get(&root).copied().unwrap_or(1)
    }

    /// Remove a vertex from tracking.
    pub fn remove_vertex(&mut self, vertex: &str) {
        self.parent.remove(vertex);
        self.rank.remove(vertex);
        self.sizes.remove(vertex);
    }
}

/// Streaming PageRank tracker with incremental updates.
#[derive(Debug, Clone)]
pub struct StreamingPageRank {
    /// Current PageRank values.
    ranks: HashMap<String, f64>,
    /// Out-degree for each vertex.
    out_degrees: HashMap<String, u32>,
    /// Damping factor.
    damping: f64,
    /// Number of vertices.
    num_vertices: usize,
    /// Convergence threshold.
    epsilon: f64,
}

impl StreamingPageRank {
    /// Create a new streaming PageRank tracker.
    pub fn new(damping: f64) -> Self {
        Self {
            ranks: HashMap::new(),
            out_degrees: HashMap::new(),
            damping,
            num_vertices: 0,
            epsilon: 1e-6,
        }
    }

    /// Initialize PageRank for a vertex.
    pub fn add_vertex(&mut self, vertex_id: &str) {
        if !self.ranks.contains_key(vertex_id) {
            let initial_rank = if self.num_vertices > 0 {
                1.0 / (self.num_vertices + 1) as f64
            } else {
                1.0
            };
            self.ranks.insert(vertex_id.to_string(), initial_rank);
            self.out_degrees.insert(vertex_id.to_string(), 0);
            self.num_vertices += 1;
        }
    }

    /// Remove a vertex from PageRank tracking.
    pub fn remove_vertex(&mut self, vertex_id: &str) {
        self.ranks.remove(vertex_id);
        self.out_degrees.remove(vertex_id);
        self.num_vertices = self.num_vertices.saturating_sub(1);
    }

    /// Update PageRank when an edge is added.
    /// Performs a limited number of iterations to converge.
    pub fn edge_added(&mut self, source: &str, target: &str) {
        // Update out-degree
        *self.out_degrees.entry(source.to_string()).or_insert(0) += 1;

        // Perform a few iterations of PageRank
        for _ in 0..3 {
            self.iterate();
        }
    }

    /// Update PageRank when an edge is removed.
    pub fn edge_removed(&mut self, source: &str, target: &str) {
        // Update out-degree
        if let Some(deg) = self.out_degrees.get_mut(source) {
            *deg = deg.saturating_sub(1);
        }

        // Perform a few iterations
        for _ in 0..3 {
            self.iterate();
        }
    }

    /// Perform one iteration of PageRank.
    fn iterate(&mut self) {
        if self.num_vertices == 0 {
            return;
        }

        let n = self.num_vertices as f64;
        let base_rank = (1.0 - self.damping) / n;

        // Calculate dangling node sum
        let dangling_sum: f64 = self.ranks.iter()
            .filter(|(id, _)| self.out_degrees.get(*id).copied().unwrap_or(0) == 0)
            .map(|(_, rank)| *rank)
            .sum();

        let base = base_rank + self.damping * dangling_sum / n;

        // Update ranks (simplified - doesn't track incoming edges)
        for (_, rank) in self.ranks.iter_mut() {
            *rank = base + self.damping * (*rank - dangling_sum / n) * 0.5;
        }
    }

    /// Get the PageRank of a vertex.
    pub fn rank(&self, vertex_id: &str) -> f64 {
        self.ranks.get(vertex_id).copied().unwrap_or(0.0)
    }

    /// Get all ranks sorted by rank (descending).
    pub fn top_ranks(&self, limit: usize) -> Vec<(String, f64)> {
        let mut ranks: Vec<(String, f64)> = self.ranks.iter()
            .map(|(id, rank)| (id.clone(), *rank))
            .collect();
        ranks.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        ranks.into_iter().take(limit).collect()
    }
}

/// Streaming computation engine.
///
/// Processes graph change events and maintains incremental algorithm state.
pub struct StreamingComputeEngine {
    /// Reference to the graph store.
    graph: Arc<GraphStore>,
    /// Event buffer for windowed computations.
    event_buffer: RwLock<VecDeque<(Instant, GraphChangeEvent)>>,
    /// Streaming degree tracker.
    degrees: RwLock<HashMap<String, StreamingDegree>>,
    /// Incremental connected components.
    components: RwLock<IncrementalComponents>,
    /// Streaming PageRank.
    pagerank: RwLock<StreamingPageRank>,
    /// Window configuration.
    window: WindowType,
    /// Statistics.
    stats: RwLock<StreamingStats>,
}

/// Streaming computation statistics.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StreamingStats {
    /// Total events processed.
    pub events_processed: u64,
    /// Total computation time (microseconds).
    pub computation_time_us: u64,
    /// Current buffer size.
    pub buffer_size: usize,
    /// Number of active components.
    pub component_count: usize,
    /// Last computation timestamp.
    pub last_computation: u64,
}

impl StreamingComputeEngine {
    /// Create a new streaming compute engine.
    pub fn new(graph: Arc<GraphStore>, window: WindowType) -> Self {
        Self {
            graph,
            event_buffer: RwLock::new(VecDeque::new()),
            degrees: RwLock::new(HashMap::new()),
            components: RwLock::new(IncrementalComponents::new()),
            pagerank: RwLock::new(StreamingPageRank::new(0.85)),
            window,
            stats: RwLock::new(StreamingStats::default()),
        }
    }

    /// Process a graph change event.
    pub fn process_event(&self, event: GraphChangeEvent) {
        let start = Instant::now();

        // Add to buffer
        {
            let mut buffer = self.event_buffer.write();
            buffer.push_back((Instant::now(), event.clone()));

            // Trim buffer based on window
            self.trim_buffer(&mut buffer);
        }

        // Update incremental state
        match &event {
            GraphChangeEvent::VertexAdded { id, .. } => {
                self.degrees.write().insert(id.clone(), StreamingDegree::default());
                self.components.write().find(id); // Initialize in union-find
                self.pagerank.write().add_vertex(id);
            }
            GraphChangeEvent::VertexRemoved { id } => {
                self.degrees.write().remove(id);
                self.components.write().remove_vertex(id);
                self.pagerank.write().remove_vertex(id);
            }
            GraphChangeEvent::EdgeAdded { source, target, .. } => {
                // Update degrees
                {
                    let mut degrees = self.degrees.write();
                    degrees.entry(source.clone())
                        .or_default()
                        .out_degree += 1;
                    degrees.entry(target.clone())
                        .or_default()
                        .in_degree += 1;
                }

                // Update connected components
                self.components.write().union(source, target);

                // Update PageRank
                self.pagerank.write().edge_added(source, target);
            }
            GraphChangeEvent::EdgeRemoved { source, target, .. } => {
                // Update degrees
                {
                    let mut degrees = self.degrees.write();
                    if let Some(d) = degrees.get_mut(source) {
                        d.out_degree = d.out_degree.saturating_sub(1);
                    }
                    if let Some(d) = degrees.get_mut(target) {
                        d.in_degree = d.in_degree.saturating_sub(1);
                    }
                }

                // Update PageRank
                self.pagerank.write().edge_removed(source, target);
            }
        }

        // Update stats
        let elapsed = start.elapsed();
        let mut stats = self.stats.write();
        stats.events_processed += 1;
        stats.computation_time_us += elapsed.as_micros() as u64;
        stats.buffer_size = self.event_buffer.read().len();
        stats.component_count = self.components.read().component_count();
    }

    /// Process a batch of events.
    pub fn process_batch(&self, events: Vec<GraphChangeEvent>) {
        for event in events {
            self.process_event(event);
        }
    }

    /// Trim the event buffer based on the window configuration.
    fn trim_buffer(&self, buffer: &mut VecDeque<(Instant, GraphChangeEvent)>) {
        let now = Instant::now();
        let window_duration = match &self.window {
            WindowType::Tumbling { duration } => *duration,
            WindowType::Sliding { duration, .. } => *duration,
            WindowType::Session { gap } => *gap,
        };

        while let Some((time, _)) = buffer.front() {
            if now.duration_since(*time) > window_duration {
                buffer.pop_front();
            } else {
                break;
            }
        }
    }

    /// Get events in the current window.
    pub fn window_events(&self) -> Vec<GraphChangeEvent> {
        let buffer = self.event_buffer.read();
        buffer.iter().map(|(_, event)| event.clone()).collect()
    }

    /// Get the degree of a vertex.
    pub fn degree(&self, vertex_id: &str) -> Option<StreamingDegree> {
        self.degrees.read().get(vertex_id).cloned()
    }

    /// Get the in-degree of a vertex.
    pub fn in_degree(&self, vertex_id: &str) -> u32 {
        self.degrees.read().get(vertex_id).map(|d| d.in_degree).unwrap_or(0)
    }

    /// Get the out-degree of a vertex.
    pub fn out_degree(&self, vertex_id: &str) -> u32 {
        self.degrees.read().get(vertex_id).map(|d| d.out_degree).unwrap_or(0)
    }

    /// Check if two vertices are connected (in the same component).
    pub fn connected(&self, a: &str, b: &str) -> bool {
        self.components.write().connected(a, b)
    }

    /// Get the number of connected components.
    pub fn component_count(&self) -> usize {
        self.components.read().component_count()
    }

    /// Get the size of a vertex's component.
    pub fn component_size(&self, vertex_id: &str) -> u32 {
        self.components.write().component_size(vertex_id)
    }

    /// Get the PageRank of a vertex.
    pub fn pagerank(&self, vertex_id: &str) -> f64 {
        self.pagerank.read().rank(vertex_id)
    }

    /// Get top PageRank vertices.
    pub fn top_pagerank(&self, limit: usize) -> Vec<(String, f64)> {
        self.pagerank.read().top_ranks(limit)
    }

    /// Get streaming statistics.
    pub fn stats(&self) -> StreamingStats {
        let mut stats = self.stats.read().clone();
        stats.buffer_size = self.event_buffer.read().len();
        stats
    }

    /// Get windowed degree statistics.
    pub fn windowed_degree_stats(&self) -> HashMap<String, (u32, u32)> {
        let events = self.window_events();
        let mut degree_changes: HashMap<String, (u32, u32)> = HashMap::new();

        for event in &events {
            match event {
                GraphChangeEvent::EdgeAdded { source, target, .. } => {
                    degree_changes.entry(source.clone()).or_default().0 += 1;
                    degree_changes.entry(target.clone()).or_default().1 += 1;
                }
                GraphChangeEvent::EdgeRemoved { source, target, .. } => {
                    // These are negative changes, but we track as positive for window stats
                    degree_changes.entry(source.clone()).or_default().0 += 1;
                    degree_changes.entry(target.clone()).or_default().1 += 1;
                }
                _ => {}
            }
        }

        degree_changes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn create_engine() -> StreamingComputeEngine {
        let graph = Arc::new(GraphStore::new());
        StreamingComputeEngine::new(graph, WindowType::Tumbling {
            duration: Duration::from_secs(60),
        })
    }

    #[test]
    fn test_streaming_degree_tracking() {
        let engine = create_engine();

        engine.process_event(GraphChangeEvent::VertexAdded {
            id: "A".to_string(),
            labels: vec![],
        });
        engine.process_event(GraphChangeEvent::VertexAdded {
            id: "B".to_string(),
            labels: vec![],
        });
        engine.process_event(GraphChangeEvent::EdgeAdded {
            source: "A".to_string(),
            target: "B".to_string(),
            label: "LINK".to_string(),
        });

        assert_eq!(engine.out_degree("A"), 1);
        assert_eq!(engine.in_degree("B"), 1);
        assert_eq!(engine.in_degree("A"), 0);
        assert_eq!(engine.out_degree("B"), 0);
    }

    #[test]
    fn test_streaming_components() {
        let engine = create_engine();

        engine.process_event(GraphChangeEvent::VertexAdded { id: "A".to_string(), labels: vec![] });
        engine.process_event(GraphChangeEvent::VertexAdded { id: "B".to_string(), labels: vec![] });
        engine.process_event(GraphChangeEvent::VertexAdded { id: "C".to_string(), labels: vec![] });

        assert_eq!(engine.component_count(), 3); // All separate

        engine.process_event(GraphChangeEvent::EdgeAdded {
            source: "A".to_string(),
            target: "B".to_string(),
            label: "LINK".to_string(),
        });

        assert_eq!(engine.component_count(), 2); // A-B connected, C separate
        assert!(engine.connected("A", "B"));
        assert!(!engine.connected("A", "C"));

        engine.process_event(GraphChangeEvent::EdgeAdded {
            source: "B".to_string(),
            target: "C".to_string(),
            label: "LINK".to_string(),
        });

        assert_eq!(engine.component_count(), 1); // All connected
        assert!(engine.connected("A", "C"));
    }

    #[test]
    fn test_streaming_pagerank() {
        let engine = create_engine();

        engine.process_event(GraphChangeEvent::VertexAdded { id: "A".to_string(), labels: vec![] });
        engine.process_event(GraphChangeEvent::VertexAdded { id: "B".to_string(), labels: vec![] });
        engine.process_event(GraphChangeEvent::EdgeAdded {
            source: "A".to_string(),
            target: "B".to_string(),
            label: "LINK".to_string(),
        });

        let rank_a = engine.pagerank("A");
        let rank_b = engine.pagerank("B");
        assert!(rank_a > 0.0);
        assert!(rank_b > 0.0);
    }

    #[test]
    fn test_window_events() {
        let engine = create_engine();

        engine.process_event(GraphChangeEvent::VertexAdded { id: "A".to_string(), labels: vec![] });
        engine.process_event(GraphChangeEvent::VertexAdded { id: "B".to_string(), labels: vec![] });

        let events = engine.window_events();
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn test_batch_processing() {
        let engine = create_engine();

        let events = vec![
            GraphChangeEvent::VertexAdded { id: "A".to_string(), labels: vec![] },
            GraphChangeEvent::VertexAdded { id: "B".to_string(), labels: vec![] },
            GraphChangeEvent::EdgeAdded {
                source: "A".to_string(),
                target: "B".to_string(),
                label: "LINK".to_string(),
            },
        ];

        engine.process_batch(events);

        let stats = engine.stats();
        assert_eq!(stats.events_processed, 3);
    }

    #[test]
    fn test_incremental_components_removal() {
        let engine = create_engine();

        engine.process_event(GraphChangeEvent::VertexAdded { id: "A".to_string(), labels: vec![] });
        engine.process_event(GraphChangeEvent::VertexAdded { id: "B".to_string(), labels: vec![] });
        engine.process_event(GraphChangeEvent::EdgeAdded {
            source: "A".to_string(),
            target: "B".to_string(),
            label: "LINK".to_string(),
        });

        assert_eq!(engine.component_count(), 1);

        engine.process_event(GraphChangeEvent::VertexRemoved { id: "B".to_string() });

        // After removing B, A should still be in its own component
        assert_eq!(engine.component_count(), 1);
    }
}