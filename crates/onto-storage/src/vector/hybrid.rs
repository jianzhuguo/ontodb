// Copyright (c) 2024-2026 OntoDB Team
// Licensed under the Business Source License 1.1 (BUSL-1.1).
// See LICENSE for details. Change Date: 2031-09-15.
// On the Change Date, this file will be licensed under Apache License 2.0.
//! Multi-vector hybrid query support.
//!
//! Supports searching multiple vector columns simultaneously and
//! combining results with configurable weights using Reciprocal Rank Fusion (RRF).
//!
//! Example:
//!   SELECT * FROM Product
//!   VECTOR SEARCH (title_embedding, image_embedding)
//!   QUERY ([0.1, 0.2, ...], [0.3, 0.4, ...])
//!   WEIGHTS (0.7, 0.3)
//!   TOP 10;

use std::collections::HashMap;

/// A single vector search request for one column.
#[derive(Debug, Clone)]
pub struct VectorSearchSpec {
    /// The column containing vectors.
    pub column: String,
    /// The query vector.
    pub query_vector: Vec<f32>,
    /// Number of results to return from this column.
    pub top_k: usize,
    /// Weight for combining scores (0.0-1.0).
    pub weight: f32,
}

/// Result from a multi-vector search.
#[derive(Debug, Clone)]
pub struct HybridSearchResult {
    /// Document key.
    pub doc_key: Vec<u8>,
    /// Combined weighted score.
    pub combined_score: f32,
    /// Per-column individual scores.
    pub per_column_scores: HashMap<String, f32>,
}

/// Multi-vector search combiner using Reciprocal Rank Fusion (RRF).
///
/// RRF formula: score = sum( weight / (k + rank) )
/// where k=60 (standard constant), rank is 0-based position in each result list.
pub struct HybridSearcher {
    specs: Vec<VectorSearchSpec>,
    rrf_k: f32,
}

impl HybridSearcher {
    pub fn new(specs: Vec<VectorSearchSpec>) -> Self {
        Self { specs, rrf_k: 60.0 }
    }

    /// Set custom RRF k constant (default: 60).
    pub fn with_rrf_k(mut self, k: f32) -> Self {
        self.rrf_k = k;
        self
    }

    /// Combine results from multiple vector searches using weighted RRF.
    ///
    /// Input: map of column_name -> list of (doc_key, distance) sorted by distance ascending
    /// Output: list of HybridSearchResult sorted by combined_score descending
    pub fn combine_results(
        &self,
        per_column_results: &HashMap<String, Vec<(Vec<u8>, f32)>>,
    ) -> Vec<HybridSearchResult> {
        let mut doc_scores: HashMap<Vec<u8>, HashMap<String, f32>> = HashMap::new();

        // Collect per-column RRF scores
        for spec in &self.specs {
            if let Some(results) = per_column_results.get(&spec.column) {
                for (rank, (doc_key, _distance)) in results.iter().enumerate() {
                    let rrf_score = spec.weight / (self.rrf_k + rank as f32);
                    doc_scores
                        .entry(doc_key.clone())
                        .or_default()
                        .insert(spec.column.clone(), rrf_score);
                }
            }
        }

        // Compute combined scores
        let mut results: Vec<HybridSearchResult> = doc_scores
            .into_iter()
            .map(|(doc_key, scores)| {
                let combined_score = scores.values().sum::<f32>();
                HybridSearchResult {
                    doc_key,
                    combined_score,
                    per_column_scores: scores,
                }
            })
            .collect();

        // Sort by combined score descending
        results.sort_by(|a, b| b.combined_score.partial_cmp(&a.combined_score).unwrap());
        results
    }

    /// Get the specs for this searcher.
    pub fn specs(&self) -> &[VectorSearchSpec] {
        &self.specs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hybrid_search_single_column() {
        let specs = vec![VectorSearchSpec {
            column: "embedding".to_string(),
            query_vector: vec![1.0, 0.0, 0.0],
            top_k: 10,
            weight: 1.0,
        }];

        let searcher = HybridSearcher::new(specs);

        let mut results = HashMap::new();
        results.insert(
            "embedding".to_string(),
            vec![
                (vec![1, 2, 3], 0.1),
                (vec![4, 5, 6], 0.2),
                (vec![7, 8, 9], 0.3),
            ],
        );

        let combined = searcher.combine_results(&results);
        assert_eq!(combined.len(), 3);
        // First result should have highest score (lowest rank)
        assert!(combined[0].combined_score > combined[1].combined_score);
    }

    #[test]
    fn test_hybrid_search_multi_column() {
        let specs = vec![
            VectorSearchSpec {
                column: "title".to_string(),
                query_vector: vec![1.0, 0.0],
                top_k: 10,
                weight: 0.7,
            },
            VectorSearchSpec {
                column: "image".to_string(),
                query_vector: vec![0.0, 1.0],
                top_k: 10,
                weight: 0.3,
            },
        ];

        let searcher = HybridSearcher::new(specs);

        let mut results = HashMap::new();
        results.insert(
            "title".to_string(),
            vec![
                (vec![1], 0.1), // doc1 rank 0
                (vec![2], 0.2), // doc2 rank 1
            ],
        );
        results.insert(
            "image".to_string(),
            vec![
                (vec![2], 0.1), // doc2 rank 0
                (vec![3], 0.2), // doc3 rank 1
            ],
        );

        let combined = searcher.combine_results(&results);

        // doc2 appears in both columns, should have highest combined score
        assert_eq!(combined[0].doc_key, vec![2]);
        assert!(combined[0].per_column_scores.contains_key("title"));
        assert!(combined[0].per_column_scores.contains_key("image"));
    }

    #[test]
    fn test_hybrid_search_empty_results() {
        let specs = vec![VectorSearchSpec {
            column: "embedding".to_string(),
            query_vector: vec![1.0],
            top_k: 10,
            weight: 1.0,
        }];

        let searcher = HybridSearcher::new(specs);
        let results = HashMap::new();
        let combined = searcher.combine_results(&results);
        assert!(combined.is_empty());
    }

    #[test]
    fn test_hybrid_search_weight_impact() {
        let specs = vec![
            VectorSearchSpec {
                column: "a".to_string(),
                query_vector: vec![],
                top_k: 10,
                weight: 0.9,
            },
            VectorSearchSpec {
                column: "b".to_string(),
                query_vector: vec![],
                top_k: 10,
                weight: 0.1,
            },
        ];

        let searcher = HybridSearcher::new(specs);

        let mut results = HashMap::new();
        // doc1 is rank 0 in column a (weight 0.9)
        results.insert("a".to_string(), vec![(vec![1], 0.1)]);
        // doc2 is rank 0 in column b (weight 0.1)
        results.insert("b".to_string(), vec![(vec![2], 0.1)]);

        let combined = searcher.combine_results(&results);
        // doc1 should score higher due to higher weight
        assert_eq!(combined[0].doc_key, vec![1]);
    }
}
