// Copyright (c) 2024-2026 OntoDB Team
// Licensed under the Business Source License 1.1 (BUSL-1.1).
// See LICENSE for details. Change Date: 2031-09-15.
// On the Change Date, this file will be licensed under Apache License 2.0.
//! HNSW graph compaction for removing deleted/stale entries.
//!
//! HNSW doesn't support native deletion. Over time, deleted and stale
//! entries accumulate as tombstones, degrading search quality and
//! wasting memory.
//!
//! This module provides online compaction that rebuilds the graph
//! from the current live entries, removing all tombstoned nodes.
//!
//! Strategy:
//! 1. Collect all live (non-deleted, non-stale) entries from doc_vectors
//! 2. Build a fresh HNSW graph from scratch
//! 3. Swap the old graph with the new one atomically
//!
//! This is the same approach used by the "slow path" in rebuild_vector_indexes().

use crate::vector::hnsw::{HnswConfig, HnswIndex, VectorEntry};
use std::collections::HashMap;

/// Result of a compaction operation.
#[derive(Debug, Clone)]
pub struct CompactionResult {
    /// Number of entries before compaction.
    pub entries_before: usize,
    /// Number of entries after compaction.
    pub entries_after: usize,
    /// Number of stale/deleted entries removed.
    pub entries_removed: usize,
    /// Time taken in milliseconds.
    pub duration_ms: u64,
}

/// Compact an HNSW index by rebuilding from live entries.
///
/// This removes all tombstoned and stale entries, improving search quality.
/// The new index is built in memory and returned; the caller is responsible
/// for swapping it in.
pub fn compact_index(
    config: &HnswConfig,
    live_entries: Vec<VectorEntry>,
    old_index_size: usize,
) -> (HnswIndex, CompactionResult) {
    let start = std::time::Instant::now();
    let entries_before = old_index_size;
    let entries_after = live_entries.len();

    let mut new_index = HnswIndex::new(config.clone());
    if !live_entries.is_empty() {
        new_index.insert_batch(live_entries);
    }

    let duration_ms = start.elapsed().as_millis() as u64;

    let result = CompactionResult {
        entries_before,
        entries_after,
        entries_removed: entries_before.saturating_sub(entries_after),
        duration_ms,
    };

    (new_index, result)
}

/// Collect live entries from doc_vectors, filtering out deleted keys.
pub fn collect_live_entries(
    doc_vectors: &HashMap<Vec<u8>, Vec<(String, String, Vec<f32>)>>,
    deleted_keys: &std::collections::HashSet<Vec<u8>>,
    class: &str,
    column: &str,
) -> Vec<VectorEntry> {
    doc_vectors
        .iter()
        .filter(|(key, _)| !deleted_keys.contains(*key))
        .filter_map(|(key, entries)| {
            entries
                .iter()
                .find(|(c, col, _)| c == class && col == column)
                .map(|(_, _, vector)| VectorEntry {
                    id: key.clone(),
                    vector: vector.clone(),
                })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vector::DistanceMetric;

    fn test_config() -> HnswConfig {
        HnswConfig {
            dimension: 3,
            metric: DistanceMetric::L2,
            m: 8,
            m_max0: 16,
            ef_construction: 50,
            ef_search: 30,
            ml: 1.0 / (8.0_f64).ln(),
        }
    }

    #[test]
    fn test_compact_empty() {
        let config = test_config();
        let (new_index, result) = compact_index(&config, vec![], 0);
        assert_eq!(result.entries_before, 0);
        assert_eq!(result.entries_after, 0);
        assert_eq!(result.entries_removed, 0);
        assert!(new_index.is_empty());
    }

    #[test]
    fn test_compact_with_entries() {
        let config = test_config();
        let entries = vec![
            VectorEntry {
                id: vec![1],
                vector: vec![1.0, 0.0, 0.0],
            },
            VectorEntry {
                id: vec![2],
                vector: vec![0.0, 1.0, 0.0],
            },
            VectorEntry {
                id: vec![3],
                vector: vec![0.0, 0.0, 1.0],
            },
        ];
        let (new_index, result) = compact_index(&config, entries, 5);
        assert_eq!(result.entries_before, 5);
        assert_eq!(result.entries_after, 3);
        assert_eq!(result.entries_removed, 2);
        assert_eq!(new_index.len(), 3);
    }

    #[test]
    fn test_collect_live_entries() {
        let mut doc_vectors = HashMap::new();
        doc_vectors.insert(
            vec![1],
            vec![("Product".into(), "emb".into(), vec![1.0, 0.0])],
        );
        doc_vectors.insert(
            vec![2],
            vec![("Product".into(), "emb".into(), vec![0.0, 1.0])],
        );
        doc_vectors.insert(
            vec![3],
            vec![("Product".into(), "emb".into(), vec![0.5, 0.5])],
        );

        let mut deleted = std::collections::HashSet::new();
        deleted.insert(vec![2]); // Delete doc2

        let entries = collect_live_entries(&doc_vectors, &deleted, "Product", "emb");
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().any(|e| e.id == vec![1]));
        assert!(entries.iter().any(|e| e.id == vec![3]));
    }

    #[test]
    fn test_collect_live_entries_wrong_class() {
        let mut doc_vectors = HashMap::new();
        doc_vectors.insert(vec![1], vec![("Product".into(), "emb".into(), vec![1.0])]);

        let deleted = std::collections::HashSet::new();
        let entries = collect_live_entries(&doc_vectors, &deleted, "Order", "emb");
        assert!(entries.is_empty());
    }
}
