//! Graph streaming updates for real-time graph processing.
//!
//! Provides a streaming interface for applying graph changes incrementally
//! without full graph rebuilds.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::Arc;
use parking_lot::Mutex;

/// A graph change event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GraphEvent {
    /// A vertex was added.
    VertexAdded {
        id: String,
        labels: Vec<String>,
        properties: std::collections::HashMap<String, String>,
    },
    /// A vertex was removed.
    VertexRemoved { id: String },
    /// An edge was added.
    EdgeAdded {
        id: String,
        source: String,
        target: String,
        label: String,
        properties: std::collections::HashMap<String, String>,
    },
    /// An edge was removed.
    EdgeRemoved { id: String },
    /// A batch of events (for efficiency).
    Batch(Vec<GraphEvent>),
}

/// Statistics for the graph stream processor.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StreamStats {
    /// Total events processed.
    pub events_processed: u64,
    /// Total vertices added.
    pub vertices_added: u64,
    /// Total vertices removed.
    pub vertices_removed: u64,
    /// Total edges added.
    pub edges_added: u64,
    /// Total edges removed.
    pub edges_removed: u64,
    /// Total processing time in microseconds.
    pub processing_time_us: u64,
    /// Current queue size.
    pub queue_size: usize,
}

/// Graph stream processor for handling real-time graph updates.
pub struct GraphStream {
    /// Event queue for buffering incoming events.
    queue: Arc<Mutex<VecDeque<GraphEvent>>>,
    /// Processing statistics.
    stats: Arc<Mutex<StreamStats>>,
    /// Maximum queue size before backpressure.
    max_queue_size: usize,
    /// Batch size for processing.
    batch_size: usize,
}

impl GraphStream {
    /// Create a new graph stream processor.
    pub fn new(max_queue_size: usize, batch_size: usize) -> Self {
        Self {
            queue: Arc::new(Mutex::new(VecDeque::with_capacity(max_queue_size))),
            stats: Arc::new(Mutex::new(StreamStats::default())),
            max_queue_size,
            batch_size,
        }
    }

    /// Push an event to the queue.
    /// Returns false if queue is full (backpressure).
    pub fn push(&self, event: GraphEvent) -> bool {
        let mut queue = self.queue.lock();
        if queue.len() >= self.max_queue_size {
            return false; // Backpressure
        }
        queue.push_back(event);
        self.stats.lock().queue_size = queue.len();
        true
    }

    /// Push a batch of events.
    /// Returns the number of events successfully pushed.
    pub fn push_batch(&self, events: Vec<GraphEvent>) -> usize {
        let mut queue = self.queue.lock();
        let available = self.max_queue_size.saturating_sub(queue.len());
        let to_push = available.min(events.len());
        for event in events.into_iter().take(to_push) {
            queue.push_back(event);
        }
        self.stats.lock().queue_size = queue.len();
        to_push
    }

    /// Process events from the queue.
    /// Returns the events processed (up to batch_size).
    pub fn drain(&self) -> Vec<GraphEvent> {
        let mut queue = self.queue.lock();
        let batch_size = self.batch_size.min(queue.len());
        let events: Vec<GraphEvent> = queue.drain(..batch_size).collect();
        self.stats.lock().queue_size = queue.len();
        events
    }

    /// Process all pending events.
    pub fn drain_all(&self) -> Vec<GraphEvent> {
        let mut queue = self.queue.lock();
        let events: Vec<GraphEvent> = queue.drain(..).collect();
        self.stats.lock().queue_size = 0;
        events
    }

    /// Get current queue size.
    pub fn queue_size(&self) -> usize {
        self.queue.lock().len()
    }

    /// Check if queue is empty.
    pub fn is_empty(&self) -> bool {
        self.queue.lock().is_empty()
    }

    /// Get processing statistics.
    pub fn stats(&self) -> StreamStats {
        self.stats.lock().clone()
    }

    /// Update stats after processing events.
    pub fn update_stats(&self, events: &[GraphEvent], elapsed_us: u64) {
        let mut stats = self.stats.lock();
        stats.events_processed += events.len() as u64;
        stats.processing_time_us += elapsed_us;

        for event in events {
            match event {
                GraphEvent::VertexAdded { .. } => stats.vertices_added += 1,
                GraphEvent::VertexRemoved { .. } => stats.vertices_removed += 1,
                GraphEvent::EdgeAdded { .. } => stats.edges_added += 1,
                GraphEvent::EdgeRemoved { .. } => stats.edges_removed += 1,
                GraphEvent::Batch(events) => {
                    stats.events_processed += events.len() as u64;
                }
            }
        }
    }

    /// Apply events to a graph store.
    pub fn apply_to_graph(
        &self,
        store: &crate::store::GraphStore,
        events: &[GraphEvent],
    ) -> Result<usize, crate::error::GraphError> {
        let mut applied = 0;

        for event in events {
            match event {
                GraphEvent::VertexAdded { id, labels, properties } => {
                    let mut vertex = crate::model::Vertex::new(id, labels.clone());
                    for (k, v) in properties {
                        vertex.properties.insert(
                            k.clone(),
                            crate::model::PropValue::String(v.clone()),
                        );
                    }
                    store.add_vertex(vertex)?;
                    applied += 1;
                }
                GraphEvent::VertexRemoved { id } => {
                    store.delete_vertex(id)?;
                    applied += 1;
                }
                GraphEvent::EdgeAdded { id, source, target, label, properties } => {
                    let mut edge = crate::model::Edge::new(id, source, target, label);
                    for (k, v) in properties {
                        edge.properties.insert(
                            k.clone(),
                            crate::model::PropValue::String(v.clone()),
                        );
                    }
                    store.add_edge(edge)?;
                    applied += 1;
                }
                GraphEvent::EdgeRemoved { id } => {
                    store.delete_edge(id)?;
                    applied += 1;
                }
                GraphEvent::Batch(events) => {
                    applied += self.apply_to_graph(store, events)?;
                }
            }
        }

        Ok(applied)
    }
}

impl Default for GraphStream {
    fn default() -> Self {
        Self::new(10000, 100)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stream_basic() {
        let stream = GraphStream::new(100, 10);

        assert!(stream.push(GraphEvent::VertexAdded {
            id: "v1".to_string(),
            labels: vec!["Person".to_string()],
            properties: Default::default(),
        }));

        assert_eq!(stream.queue_size(), 1);
        assert!(!stream.is_empty());

        let events = stream.drain();
        assert_eq!(events.len(), 1);
        assert!(stream.is_empty());
    }

    #[test]
    fn test_stream_backpressure() {
        let stream = GraphStream::new(2, 10);

        assert!(stream.push(GraphEvent::VertexAdded {
            id: "v1".to_string(),
            labels: vec![],
            properties: Default::default(),
        }));
        assert!(stream.push(GraphEvent::VertexAdded {
            id: "v2".to_string(),
            labels: vec![],
            properties: Default::default(),
        }));
        // Queue full
        assert!(!stream.push(GraphEvent::VertexAdded {
            id: "v3".to_string(),
            labels: vec![],
            properties: Default::default(),
        }));
    }

    #[test]
    fn test_stream_batch() {
        let stream = GraphStream::new(100, 10);

        let events = vec![
            GraphEvent::VertexAdded {
                id: "v1".to_string(),
                labels: vec![],
                properties: Default::default(),
            },
            GraphEvent::VertexAdded {
                id: "v2".to_string(),
                labels: vec![],
                properties: Default::default(),
            },
        ];

        let pushed = stream.push_batch(events);
        assert_eq!(pushed, 2);
        assert_eq!(stream.queue_size(), 2);
    }

    #[test]
    fn test_stream_stats() {
        let stream = GraphStream::new(100, 10);

        stream.push(GraphEvent::VertexAdded {
            id: "v1".to_string(),
            labels: vec![],
            properties: Default::default(),
        });

        let events = stream.drain();
        stream.update_stats(&events, 1000);

        let stats = stream.stats();
        assert_eq!(stats.events_processed, 1);
        assert_eq!(stats.vertices_added, 1);
        assert_eq!(stats.processing_time_us, 1000);
    }
}
