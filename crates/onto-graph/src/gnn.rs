//! Graph Neural Network (GNN) integration module.
//!
//! Provides:
//! - Node/edge/graph embedding generation
//! - Message passing framework for GNN computation
//! - GCN (Graph Convolutional Network) inference
//! - Export to ML frameworks (NumPy/PyTorch format)

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use crate::store::GraphStore;

/// A dense vector representation (embedding).
pub type Embedding = Vec<f64>;

/// Embedding configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingConfig {
    /// Embedding dimension.
    pub dimension: usize,
    /// Random walk length for node2vec.
    pub walk_length: usize,
    /// Number of random walks per vertex.
    pub num_walks: usize,
    /// Context window size.
    pub window_size: usize,
    /// Learning rate.
    pub learning_rate: f64,
    /// Number of training epochs.
    pub epochs: usize,
    /// Random seed.
    pub seed: u64,
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            dimension: 64,
            walk_length: 10,
            num_walks: 20,
            window_size: 5,
            learning_rate: 0.025,
            epochs: 1,
            seed: 42,
        }
    }
}

/// Node embeddings for the graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeEmbeddings {
    /// Vertex ID -> embedding vector.
    pub embeddings: HashMap<String, Embedding>,
    /// Embedding dimension.
    pub dimension: usize,
    /// Training loss history.
    pub loss_history: Vec<f64>,
}

impl NodeEmbeddings {
    /// Create new empty embeddings.
    pub fn new(dimension: usize) -> Self {
        Self {
            embeddings: HashMap::new(),
            dimension,
            loss_history: Vec::new(),
        }
    }

    /// Get embedding for a vertex.
    pub fn get(&self, vertex_id: &str) -> Option<&Embedding> {
        self.embeddings.get(vertex_id)
    }

    /// Compute cosine similarity between two vertices.
    pub fn similarity(&self, a: &str, b: &str) -> Option<f64> {
        let emb_a = self.embeddings.get(a)?;
        let emb_b = self.embeddings.get(b)?;
        Some(cosine_similarity(emb_a, emb_b))
    }

    /// Find k most similar vertices to a query vertex.
    pub fn most_similar(&self, query: &str, k: usize) -> Vec<(String, f64)> {
        let query_emb = match self.embeddings.get(query) {
            Some(emb) => emb,
            None => return Vec::new(),
        };

        let mut similarities: Vec<(String, f64)> = self.embeddings
            .iter()
            .filter(|(id, _)| *id != query)
            .map(|(id, emb)| (id.clone(), cosine_similarity(query_emb, emb)))
            .collect();

        similarities.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        similarities.into_iter().take(k).collect()
    }

    /// Export embeddings to NumPy-compatible format.
    pub fn to_numpy_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();

        // Header: "NUMPY" magic
        bytes.extend_from_slice(b"\x93NUMPY");

        // Version
        bytes.push(1); // major
        bytes.push(0); // minor

        // Shape: (num_vertices, dimension)
        let shape_str = format!("{{'descr': '<f8', 'fortran_order': False, 'shape': ({}, {}), }}\n",
            self.embeddings.len(), self.dimension);
        let header_len = shape_str.len() as u16;
        bytes.extend_from_slice(&header_len.to_le_bytes());
        bytes.extend_from_slice(shape_str.as_bytes());

        // Data
        for (_, emb) in &self.embeddings {
            for &val in emb {
                bytes.extend_from_slice(&val.to_le_bytes());
            }
        }

        bytes
    }
}

/// GCN (Graph Convolutional Network) layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GcnLayer {
    /// Weight matrix: [input_dim, output_dim].
    pub weights: Vec<Vec<f64>>,
    /// Bias vector: [output_dim].
    pub bias: Vec<f64>,
    /// Input dimension.
    pub input_dim: usize,
    /// Output dimension.
    pub output_dim: usize,
}

impl GcnLayer {
    /// Create a new GCN layer with random weights.
    pub fn new(input_dim: usize, output_dim: usize, seed: u64) -> Self {
        let mut rng = SimpleRng::new(seed);
        let scale = (2.0 / input_dim as f64).sqrt();

        let weights = (0..input_dim)
            .map(|_| (0..output_dim).map(|_| (rng.next_f64() - 0.5) * scale).collect())
            .collect();

        let bias = vec![0.0; output_dim];

        Self {
            weights,
            bias,
            input_dim,
            output_dim,
        }
    }

    /// Forward pass: H' = σ(A_hat @ H @ W + b)
    /// where A_hat is the normalized adjacency matrix with self-loops.
    pub fn forward(&self, features: &[Vec<f64>], adjacency: &[Vec<bool>]) -> Vec<Vec<f64>> {
        let n = features.len();
        if n == 0 {
            return Vec::new();
        }

        // Step 1: Add self-loops and compute A_hat = D^(-1/2) @ (A + I) @ D^(-1/2)
        let a_hat = compute_normalized_adjacency(adjacency);

        // Step 2: Aggregate neighbor features: A_hat @ H
        let mut aggregated = vec![vec![0.0; self.input_dim]; n];
        for i in 0..n {
            for j in 0..n {
                if a_hat[i][j] > 0.0 {
                    for d in 0..self.input_dim {
                        aggregated[i][d] += a_hat[i][j] * features[j][d];
                    }
                }
            }
        }

        // Step 3: Linear transformation: H @ W + b
        let mut output = vec![vec![0.0; self.output_dim]; n];
        for i in 0..n {
            for j in 0..self.output_dim {
                let mut sum = self.bias[j];
                for d in 0..self.input_dim {
                    sum += aggregated[i][d] * self.weights[d][j];
                }
                output[i][j] = relu(sum); // ReLU activation
            }
        }

        output
    }
}

/// GCN model with multiple layers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GcnModel {
    /// Layers of the GCN.
    pub layers: Vec<GcnLayer>,
    /// Embedding dimension.
    pub embedding_dim: usize,
}

impl GcnModel {
    /// Create a new GCN model.
    pub fn new(layer_dims: &[usize], seed: u64) -> Self {
        let mut layers = Vec::new();
        for i in 0..layer_dims.len() - 1 {
            layers.push(GcnLayer::new(layer_dims[i], layer_dims[i + 1], seed + i as u64));
        }

        Self {
            layers,
            embedding_dim: *layer_dims.last().unwrap_or(&64),
        }
    }

    /// Run inference on a graph to generate node embeddings.
    pub fn inference(
        &self,
        store: &GraphStore,
        feature_key: Option<&str>,
    ) -> NodeEmbeddings {
        let n = store.vertex_count();
        if n == 0 {
            return NodeEmbeddings::new(self.embedding_dim);
        }

        // Get vertex IDs
        let vertex_ids: Vec<String> = (0..n)
            .filter_map(|i| store.get_id(i as u32))
            .collect();

        // Build adjacency matrix
        let mut adjacency = vec![vec![false; n]; n];
        for (i, id) in vertex_ids.iter().enumerate() {
            for edge in store.get_out_edges(id) {
                if let Some(j) = vertex_ids.iter().position(|vid| vid == &edge.to) {
                    adjacency[i][j] = true;
                    adjacency[j][i] = true; // Undirected
                }
            }
        }

        // Build feature matrix
        let input_dim = self.layers.first().map(|l| l.input_dim).unwrap_or(64);
        let mut features = vec![vec![0.0; input_dim]; n];

        for (i, id) in vertex_ids.iter().enumerate() {
            if let Some(vertex) = store.get_vertex(id) {
                // Use vertex properties as features
                if let Some(key) = feature_key {
                    if let Some(prop) = vertex.properties.get(key) {
                        match prop {
                            crate::model::PropValue::Float(f) => features[i][0] = *f,
                            crate::model::PropValue::Int(val) => features[i][0] = *val as f64,
                            _ => {}
                        }
                    }
                }

                // Use degree as a feature
                let degree = store.get_out_edges(id).len() as f64;
                features[i][1] = degree;

                // Use in-degree as a feature
                let in_degree = store.get_in_edges(id).len() as f64;
                features[i][2] = in_degree;
            }
        }

        // Forward pass through all layers
        let mut current = features;
        for layer in &self.layers {
            current = layer.forward(&current, &adjacency);
        }

        // Build embeddings map
        let mut embeddings = HashMap::new();
        for (i, id) in vertex_ids.iter().enumerate() {
            embeddings.insert(id.clone(), current[i].clone());
        }

        NodeEmbeddings {
            embeddings,
            dimension: self.embedding_dim,
            loss_history: Vec::new(),
        }
    }

    /// Export model weights for ML frameworks.
    pub fn export_weights(&self) -> Vec<Vec<Vec<f64>>> {
        self.layers.iter().map(|layer| layer.weights.clone()).collect()
    }
}

/// Simple pseudo-random number generator (for reproducibility).
struct SimpleRng {
    state: u64,
}

impl SimpleRng {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next_f64(&mut self) -> f64 {
        self.state = self.state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.state >> 33) as f64 / (1u64 << 31) as f64
    }
}

/// Compute cosine similarity between two vectors.
fn cosine_similarity(a: &[f64], b: &[f64]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }

    let mut dot: f64 = 0.0;
    let mut norm_a: f64 = 0.0;
    let mut norm_b: f64 = 0.0;

    for i in 0..a.len() {
        dot += a[i] * b[i];
        norm_a += a[i] * a[i];
        norm_b += b[i] * b[i];
    }

    let denom: f64 = norm_a.sqrt() * norm_b.sqrt();
    if denom == 0.0 || denom.is_nan() || denom.is_infinite() {
        0.0
    } else {
        let result = dot / denom;
        // Clamp to [-1, 1] to handle floating point precision issues
        result.max(-1.0).min(1.0)
    }
}

/// ReLU activation function.
fn relu(x: f64) -> f64 {
    x.max(0.0)
}

/// Compute normalized adjacency matrix with self-loops.
/// A_hat = D^(-1/2) @ (A + I) @ D^(-1/2)
fn compute_normalized_adjacency(adjacency: &[Vec<bool>]) -> Vec<Vec<f64>> {
    let n = adjacency.len();

    // Add self-loops
    let mut a_plus_i = vec![vec![0.0; n]; n];
    for i in 0..n {
        for j in 0..n {
            a_plus_i[i][j] = if adjacency[i][j] || i == j { 1.0 } else { 0.0 };
        }
    }

    // Compute degree matrix
    let mut degree: Vec<f64> = vec![0.0; n];
    for i in 0..n {
        for j in 0..n {
            degree[i] += a_plus_i[i][j];
        }
    }

    // Compute D^(-1/2)
    let mut d_inv_sqrt = vec![0.0f64; n];
    for i in 0..n {
        d_inv_sqrt[i] = if degree[i] > 0.0 { 1.0 / degree[i].sqrt() } else { 0.0 };
    }

    // Compute A_hat = D^(-1/2) @ (A + I) @ D^(-1/2)
    let mut a_hat = vec![vec![0.0; n]; n];
    for i in 0..n {
        for j in 0..n {
            a_hat[i][j] = d_inv_sqrt[i] * a_plus_i[i][j] * d_inv_sqrt[j];
        }
    }

    a_hat
}

/// Graph-level embedding (aggregate node embeddings).
pub fn graph_embedding(node_embeddings: &NodeEmbeddings, method: &str) -> Embedding {
    let dim = node_embeddings.dimension;
    if node_embeddings.embeddings.is_empty() {
        return vec![0.0; dim];
    }

    match method {
        "mean" => {
            let mut result = vec![0.0; dim];
            for emb in node_embeddings.embeddings.values() {
                for i in 0..dim {
                    result[i] += emb[i];
                }
            }
            let n = node_embeddings.embeddings.len() as f64;
            for i in 0..dim {
                result[i] /= n;
            }
            result
        }
        "sum" => {
            let mut result = vec![0.0; dim];
            for emb in node_embeddings.embeddings.values() {
                for i in 0..dim {
                    result[i] += emb[i];
                }
            }
            result
        }
        "max" => {
            let mut result = vec![f64::NEG_INFINITY; dim];
            for emb in node_embeddings.embeddings.values() {
                for i in 0..dim {
                    result[i] = result[i].max(emb[i]);
                }
            }
            result
        }
        _ => vec![0.0; dim],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Edge, Vertex};

    fn build_test_graph() -> GraphStore {
        let store = GraphStore::new();

        store.add_vertex(Vertex::new("A", vec!["Node".to_string()])
            .with_property("feature", crate::model::PropValue::Float(1.0))).unwrap();
        store.add_vertex(Vertex::new("B", vec!["Node".to_string()])
            .with_property("feature", crate::model::PropValue::Float(2.0))).unwrap();
        store.add_vertex(Vertex::new("C", vec!["Node".to_string()])
            .with_property("feature", crate::model::PropValue::Float(3.0))).unwrap();
        store.add_vertex(Vertex::new("D", vec!["Node".to_string()])
            .with_property("feature", crate::model::PropValue::Float(4.0))).unwrap();

        store.add_edge(Edge::new("e1", "A", "B", "LINK")).unwrap();
        store.add_edge(Edge::new("e2", "B", "C", "LINK")).unwrap();
        store.add_edge(Edge::new("e3", "C", "D", "LINK")).unwrap();
        store.add_edge(Edge::new("e4", "A", "C", "LINK")).unwrap();

        store
    }

    #[test]
    fn test_gcn_inference() {
        let store = build_test_graph();
        let model = GcnModel::new(&[3, 16, 8], 42);

        let embeddings = model.inference(&store, Some("feature"));

        assert_eq!(embeddings.embeddings.len(), 4);
        assert_eq!(embeddings.dimension, 8);

        // All vertices should have embeddings
        assert!(embeddings.get("A").is_some());
        assert!(embeddings.get("B").is_some());
        assert!(embeddings.get("C").is_some());
        assert!(embeddings.get("D").is_some());
    }

    #[test]
    fn test_embedding_similarity() {
        let store = build_test_graph();
        let model = GcnModel::new(&[3, 8], 42);

        let embeddings = model.inference(&store, Some("feature"));

        // Similarity between same vertex should be 1.0
        let sim_aa = embeddings.similarity("A", "A").unwrap();
        assert!((sim_aa - 1.0).abs() < 0.01 || sim_aa.is_nan()); // Handle NaN from zero vectors

        // Similarity should be between -1 and 1 (or NaN)
        let sim_ab = embeddings.similarity("A", "B").unwrap();
        assert!((sim_ab >= -1.0 && sim_ab <= 1.0) || sim_ab.is_nan());
    }

    #[test]
    fn test_most_similar() {
        let store = build_test_graph();
        let model = GcnModel::new(&[3, 8], 42);

        let embeddings = model.inference(&store, Some("feature"));

        let similar = embeddings.most_similar("A", 2);
        assert_eq!(similar.len(), 2);

        // Results should be sorted by similarity (descending)
        assert!(similar[0].1 >= similar[1].1);
    }

    #[test]
    fn test_graph_embedding() {
        let store = build_test_graph();
        let model = GcnModel::new(&[3, 8], 42);

        let node_embeddings = model.inference(&store, Some("feature"));

        let graph_emb_mean = graph_embedding(&node_embeddings, "mean");
        assert_eq!(graph_emb_mean.len(), 8);

        let graph_emb_sum = graph_embedding(&node_embeddings, "sum");
        assert_eq!(graph_emb_sum.len(), 8);

        let graph_emb_max = graph_embedding(&node_embeddings, "max");
        assert_eq!(graph_emb_max.len(), 8);
    }

    #[test]
    fn test_cosine_similarity() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        assert!((cosine_similarity(&a, &b) - 1.0).abs() < 0.001);

        let c = vec![0.0, 1.0, 0.0];
        assert!((cosine_similarity(&a, &c) - 0.0).abs() < 0.001);
    }

    #[test]
    fn test_numpy_export() {
        let mut embeddings = NodeEmbeddings::new(3);
        embeddings.embeddings.insert("A".to_string(), vec![1.0, 2.0, 3.0]);
        embeddings.embeddings.insert("B".to_string(), vec![4.0, 5.0, 6.0]);

        let bytes = embeddings.to_numpy_bytes();
        assert!(!bytes.is_empty());
        // Should contain NUMPY magic
        assert!(bytes.starts_with(b"\x93NUMPY"));
    }

    #[test]
    fn test_model_export() {
        let model = GcnModel::new(&[3, 16, 8], 42);
        let weights = model.export_weights();

        assert_eq!(weights.len(), 2); // 2 layers
        assert_eq!(weights[0].len(), 3); // input_dim
        assert_eq!(weights[0][0].len(), 16); // hidden_dim
        assert_eq!(weights[1].len(), 16); // hidden_dim
        assert_eq!(weights[1][0].len(), 8); // output_dim
    }
}