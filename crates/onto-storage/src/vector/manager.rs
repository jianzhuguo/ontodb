//! Vector index manager for the storage engine.
//!
//! Manages HNSW vector indexes per class.column, handling:
//! - Index creation with configurable parameters
//! - Vector insertion (auto-indexing on document write)
//! - Vector similarity search (with optional ontology filtering)
//! - Persistence of vector index metadata

use crate::vector::{DistanceMetric, HnswConfig, HnswIndex, SearchResult, VectorEntry};
use crate::vector::normalize::{l2_normalize, should_normalize};
use onto_core::{CoreError, Result};
use std::collections::{HashMap, HashSet};

/// Metadata about a vector index.
#[derive(Debug, Clone)]
pub struct VectorIndexMeta {
    /// The class this index belongs to.
    pub class: String,
    /// The column containing vector data.
    pub column: String,
    /// Vector dimension.
    pub dimension: usize,
    /// Distance metric.
    pub metric: DistanceMetric,
    /// HNSW M parameter.
    pub m: usize,
    /// ef_construction parameter.
    pub ef_construction: usize,
    /// ef_search parameter.
    pub ef_search: usize,
}

/// Key for identifying a vector index (class + column).
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct IndexKey {
    class: String,
    column: String,
}

/// Manages vector indexes for the storage engine.
pub struct VectorIndexManager {
    /// Vector indexes keyed by (class, column).
    indexes: HashMap<IndexKey, HnswIndex>,
    /// Metadata for each index.
    metadata: HashMap<IndexKey, VectorIndexMeta>,
    /// Mapping from document key -> (class, column, vector) for re-indexing.
    doc_vectors: HashMap<Vec<u8>, Vec<(String, String, Vec<f32>)>>,
    /// Document keys that have been deleted (filtered out during search).
    /// HNSW doesn't support native deletion, so we track tombstones here.
    deleted_keys: HashSet<Vec<u8>>,
}

impl Default for VectorIndexManager {
    fn default() -> Self {
        Self::new()
    }
}

impl VectorIndexManager {
    pub fn new() -> Self {
        Self {
            indexes: HashMap::new(),
            metadata: HashMap::new(),
            doc_vectors: HashMap::new(),
            deleted_keys: HashSet::new(),
        }
    }

    /// Compact the deleted_keys set by removing entries that are no longer
    /// in doc_vectors (i.e., the document has been fully removed).
    pub fn compact_deleted_keys(&mut self) {
        self.deleted_keys.retain(|k| self.doc_vectors.contains_key(k));
    }

    /// Returns the number of tombstoned keys.
    pub fn deleted_keys_count(&self) -> usize {
        self.deleted_keys.len()
    }

    /// Creates a new vector index for a class.column.
    pub fn create_index(
        &mut self,
        class: &str,
        column: &str,
        dimension: usize,
        metric: DistanceMetric,
        m: usize,
        ef_construction: usize,
        ef_search: usize,
    ) -> Result<()> {
        let key = IndexKey {
            class: class.to_string(),
            column: column.to_string(),
        };

        if self.indexes.contains_key(&key) {
            return Err(CoreError::InvalidArgument(format!(
                "vector index already exists on {}.{}",
                class, column
            )));
        }

        let config = HnswConfig::new(dimension, metric)
            .with_m(m)
            .with_ef_construction(ef_construction)
            .with_ef_search(ef_search);

        let meta = VectorIndexMeta {
            class: class.to_string(),
            column: column.to_string(),
            dimension,
            metric,
            m,
            ef_construction,
            ef_search,
        };

        self.indexes.insert(key.clone(), HnswIndex::new(config));
        self.metadata.insert(key, meta);

        Ok(())
    }

    /// Drops a vector index.
    pub fn drop_index(&mut self, class: &str, column: &str) -> bool {
        let key = IndexKey {
            class: class.to_string(),
            column: column.to_string(),
        };
        let removed_index = self.indexes.remove(&key).is_some();
        let removed_meta = self.metadata.remove(&key).is_some();
        self.doc_vectors.retain(|_, entries| {
            entries.retain(|(c, col, _)| !(c == class && col == column));
            !entries.is_empty()
        });
        removed_index || removed_meta
    }

    /// Returns true if a vector index exists for the given class.column.
    pub fn has_index(&self, class: &str, column: &str) -> bool {
        let key = IndexKey {
            class: class.to_string(),
            column: column.to_string(),
        };
        self.indexes.contains_key(&key)
    }

    /// Returns true if any vector index exists for the given class.
    pub fn has_any_index(&self, class: &str) -> bool {
        self.metadata.keys().any(|k| k.class == class)
    }

    /// Returns the metadata for a vector index.
    pub fn index_meta(&self, class: &str, column: &str) -> Option<&VectorIndexMeta> {
        let key = IndexKey {
            class: class.to_string(),
            column: column.to_string(),
        };
        self.metadata.get(&key)
    }

    /// Returns the number of vector indexes.
    pub fn index_count(&self) -> usize {
        self.indexes.len()
    }

    /// Save all HNSW graph structures to bytes for persistence.
    ///
    /// Returns a map of (class, column) -> serialized HnswIndex bytes.
    pub fn save_all_graphs(&self) -> HashMap<(String, String), Vec<u8>> {
        let mut result = HashMap::new();
        for (key, index) in &self.indexes {
            if let Ok(bytes) = index.save_to_bytes() {
                result.insert((key.class.clone(), key.column.clone()), bytes);
            }
        }
        result
    }

    /// Save a single HNSW graph structure to bytes.
    pub fn save_graph(&self, class: &str, column: &str) -> Option<Vec<u8>> {
        let key = IndexKey {
            class: class.to_string(),
            column: column.to_string(),
        };
        self.indexes.get(&key)?.save_to_bytes().ok()
    }

    /// Load an HNSW graph structure from bytes.
    pub fn load_graph(&mut self, class: &str, column: &str, data: &[u8]) -> Result<()> {
        let index = HnswIndex::load_from_bytes(data)
            .map_err(|e| CoreError::InvalidArgument(format!("Failed to load HNSW graph: {}", e)))?;

        let key = IndexKey {
            class: class.to_string(),
            column: column.to_string(),
        };

        // Update config metadata from loaded index
        let config = index.config();
        let meta = VectorIndexMeta {
            class: class.to_string(),
            column: column.to_string(),
            dimension: config.dimension,
            metric: config.metric,
            m: config.m,
            ef_construction: config.ef_construction,
            ef_search: config.ef_search,
        };

        self.indexes.insert(key.clone(), index);
        self.metadata.insert(key, meta);

        Ok(())
    }

    /// Indexes a batch of vectors under a single lock acquisition.
    /// More efficient than calling `index_vector()` in a loop for bulk operations.
    pub fn index_vector_batch(
        &mut self,
        vectors: &[(Vec<u8>, &str, &str, Vec<f32>)],
    ) {
        // Group by (class, column) for batch HNSW insert
        let mut by_index: HashMap<IndexKey, Vec<(Vec<u8>, Vec<f32>)>> = HashMap::new();
        for (doc_key, class, column, mut vector) in vectors.iter().cloned() {
            // Un-delete if this is a new document (not an update)
            if !self.doc_vectors.contains_key(&doc_key) {
                self.deleted_keys.remove(&doc_key);
            }

            let key = IndexKey {
                class: class.to_string(),
                column: column.to_string(),
            };

            // Pre-normalize for cosine metric
            if let Some(meta) = self.metadata.get(&key) {
                if should_normalize(&meta.metric) {
                    l2_normalize(&mut vector);
                }
            }

            by_index.entry(key).or_default().push((doc_key.clone(), vector.clone()));
            // Track doc_vectors
            self.doc_vectors
                .entry(doc_key)
                .or_default()
                .push((class.to_string(), column.to_string(), vector));
        }

        for (idx_key, vecs) in by_index {
            if let Some(index) = self.indexes.get_mut(&idx_key) {
                let batch: Vec<VectorEntry> = vecs
                    .into_iter()
                    .map(|(id, vector)| VectorEntry { id, vector })
                    .collect();
                index.insert_batch(batch);
            }
        }
    }

    /// Indexes a vector for a document. Called during INSERT/UPDATE.
    pub fn index_vector(
        &mut self,
        doc_key: &[u8],
        class: &str,
        column: &str,
        mut vector: Vec<f32>,
    ) {
        let key = IndexKey {
            class: class.to_string(),
            column: column.to_string(),
        };

        // Pre-normalize for cosine metric (2x search speedup)
        if let Some(meta) = self.metadata.get(&key) {
            if should_normalize(&meta.metric) {
                l2_normalize(&mut vector);
            }
        }

        // If this key was previously deleted AND is being re-inserted (not updated),
        // un-delete it. For UPDATE, deindex_vectors already removed doc_vectors,
        // so we keep deleted_keys to filter stale HNSW entries until the next compaction.
        if !self.doc_vectors.contains_key(doc_key) {
            self.deleted_keys.remove(doc_key);
        }

        // Track the vector for this document
        self.doc_vectors
            .entry(doc_key.to_vec())
            .or_default()
            .push((class.to_string(), column.to_string(), vector.clone()));

        if let Some(index) = self.indexes.get_mut(&key) {
            let entry = VectorEntry {
                id: doc_key.to_vec(),
                vector,
            };
            index.insert(entry);
        }
    }

    /// Indexes all vector columns from a JSON document for the given class.
    /// Called during commit_txn for INSERT/UPDATE operations.
    pub fn index_document_vectors(
        &mut self,
        doc_key: &[u8],
        class: &str,
        doc: &serde_json::Map<String, serde_json::Value>,
    ) {
        // Collect metadata keys first to avoid borrow checker conflict
        let relevant: Vec<(String, usize)> = self
            .metadata
            .iter()
            .filter(|(k, _)| k.class == class)
            .map(|(k, m)| (k.column.clone(), m.dimension))
            .collect();

        for (column, dimension) in relevant {
            if let Some(serde_json::Value::Array(arr)) = doc.get(&column) {
                let vec: Vec<f32> = arr
                    .iter()
                    .filter_map(|v| v.as_f64().map(|f| f as f32))
                    .collect();
                if vec.len() == dimension {
                    self.index_vector(doc_key, class, &column, vec);
                }
            }
        }
    }

    /// Removes a document's vectors from all indexes. Called during DELETE.
    pub fn deindex_vectors(&mut self, doc_key: &[u8]) {
        self.deleted_keys.insert(doc_key.to_vec());
        self.doc_vectors.remove(doc_key);
    }

    /// Searches for nearest neighbors to a query vector.
    /// Automatically filters out deleted document keys, stale entries
    /// (from vector updates), and deduplicates per document key.
    pub fn search(
        &self,
        class: &str,
        column: &str,
        query: &[f32],
        k: usize,
    ) -> Result<Vec<SearchResult>> {
        let key = IndexKey {
            class: class.to_string(),
            column: column.to_string(),
        };

        let index = self.indexes.get(&key).ok_or_else(|| {
            CoreError::InvalidArgument(format!(
                "no vector index on {}.{}",
                class, column
            ))
        })?;

        // Pre-normalize query for cosine metric
        let normalized_query: Vec<f32>;
        let actual_query = if let Some(meta) = self.metadata.get(&key) {
            if should_normalize(&meta.metric) {
                normalized_query = query.to_vec();
                let mut q = normalized_query.clone();
                l2_normalize(&mut q);
                q
            } else {
                query.to_vec()
            }
        } else {
            query.to_vec()
        };

        // Over-fetch to compensate for deleted/stale entries
        let over_fetch = (k * 5).max(k + self.deleted_keys.len()).min(index.len());
        let results = index.search(&actual_query, over_fetch);

        // Filter deleted keys, stale entries, and deduplicate
        let mut seen: HashMap<Vec<u8>, usize> = HashMap::new(); // id -> index in filtered
        let mut filtered = Vec::new();
        for r in results {
            if self.deleted_keys.contains(&r.entry.id) {
                continue;
            }
            // Skip stale entries: if the stored vector differs from the result,
            // the entry is from a previous version (HNSW can't update in place)
            if let Some(stored_vec) = self.doc_vectors.get(&r.entry.id).and_then(|entries| {
                entries.iter().find(|(c, col, _)| c == class && col == column).map(|(_, _, v)| v)
            }) {
                let is_stale = stored_vec.len() == r.entry.vector.len()
                    && stored_vec.iter().zip(r.entry.vector.iter()).any(|(a, b)| (a - b).abs() > 1e-6);
                if is_stale {
                    continue; // Stale entry from old vector
                }
            }
            // Deduplicate: if we already have this id, skip (keep first non-stale occurrence)
            if seen.contains_key(&r.entry.id) {
                continue;
            }
            seen.insert(r.entry.id.clone(), filtered.len());
            filtered.push(r);
            if filtered.len() >= k {
                break;
            }
        }
        Ok(filtered)
    }

    /// Searches for nearest neighbors with ontology filtering.
    /// Filters deleted keys, stale entries, and deduplicates per document key.
    pub fn search_filtered(
        &self,
        class: &str,
        column: &str,
        query: &[f32],
        k: usize,
        allowed_ids: &HashSet<Vec<u8>>,
    ) -> Result<Vec<SearchResult>> {
        let key = IndexKey {
            class: class.to_string(),
            column: column.to_string(),
        };

        let index = self.indexes.get(&key).ok_or_else(|| {
            CoreError::InvalidArgument(format!(
                "no vector index on {}.{}",
                class, column
            ))
        })?;

        // Pre-normalize query for cosine metric
        let normalized_query: Vec<f32>;
        let actual_query = if let Some(meta) = self.metadata.get(&key) {
            if should_normalize(&meta.metric) {
                normalized_query = query.to_vec();
                let mut q = normalized_query.clone();
                l2_normalize(&mut q);
                q
            } else {
                query.to_vec()
            }
        } else {
            query.to_vec()
        };

        // Combine allowed_ids filter with deleted_keys exclusion
        let effective_ids: HashSet<Vec<u8>> = allowed_ids
            .difference(&self.deleted_keys)
            .cloned()
            .collect();

        let over_fetch = (k * 5).max(k).min(index.len());
        let results = index.search_filtered(&actual_query, over_fetch, &effective_ids);

        // Filter stale entries and deduplicate
        let mut seen: HashMap<Vec<u8>, usize> = HashMap::new();
        let mut filtered = Vec::new();
        for r in results {
            if let Some(stored_vec) = self.doc_vectors.get(&r.entry.id).and_then(|entries| {
                entries.iter().find(|(c, col, _)| c == class && col == column).map(|(_, _, v)| v)
            }) {
                if stored_vec.len() == r.entry.vector.len()
                    && stored_vec.iter().zip(r.entry.vector.iter()).any(|(a, b)| (a - b).abs() > 1e-6)
                {
                    continue;
                }
            }
            if seen.contains_key(&r.entry.id) {
                continue;
            }
            seen.insert(r.entry.id.clone(), filtered.len());
            filtered.push(r);
            if filtered.len() >= k {
                break;
            }
        }
        Ok(filtered)
    }

    /// Returns the list of all vector index metadata.
    pub fn list_indexes(&self) -> Vec<&VectorIndexMeta> {
        self.metadata.values().collect()
    }

    /// Returns the number of deleted document keys tracked.
    pub fn deleted_count(&self) -> usize {
        self.deleted_keys.len()
    }

    /// Returns true if the given document key has been deleted.
    pub fn is_deleted(&self, doc_key: &[u8]) -> bool {
        self.deleted_keys.contains(doc_key)
    }

    /// Multi-vector search: search multiple vector columns and combine results.
    ///
    /// Uses Reciprocal Rank Fusion (RRF) to combine results from multiple columns.
    pub fn search_multi(
        &self,
        class: &str,
        specs: &[crate::vector::hybrid::VectorSearchSpec],
    ) -> Result<Vec<crate::vector::hybrid::HybridSearchResult>> {
        let mut per_column_results = HashMap::new();

        for spec in specs {
            let results = self.search(class, &spec.column, &spec.query_vector, spec.top_k * 3)?;
            let scored: Vec<(Vec<u8>, f32)> = results
                .into_iter()
                .map(|r| (r.entry.id.clone(), r.distance))
                .collect();
            per_column_results.insert(spec.column.clone(), scored);
        }

        let searcher = crate::vector::hybrid::HybridSearcher::new(specs.to_vec());
        Ok(searcher.combine_results(&per_column_results))
    }

    /// Cluster vectors in a specific index using K-Means.
    ///
    /// Returns cluster assignments for each document in the index.
    pub fn cluster_vectors(
        &self,
        class: &str,
        column: &str,
        k: usize,
    ) -> Result<crate::vector::cluster::ClusteringResult> {
        let key = IndexKey {
            class: class.to_string(),
            column: column.to_string(),
        };

        let _index = self.indexes.get(&key).ok_or_else(|| {
            CoreError::InvalidArgument(format!(
                "no vector index on {}.{}",
                class, column
            ))
        })?;

        // Collect all vectors from doc_vectors (not from HNSW, which may have stale entries)
        let vectors: Vec<Vec<f32>> = self.doc_vectors.values()
            .flat_map(|entries| {
                entries.iter()
                    .filter(|(c, col, _)| c == class && col == column)
                    .map(|(_, _, v)| v.clone())
            })
            .collect();

        if vectors.is_empty() {
            return Ok(crate::vector::cluster::ClusteringResult {
                clusters: Vec::new(),
                assignments: Vec::new(),
                iterations: 0,
                converged: true,
            });
        }

        let clusterer = crate::vector::cluster::KMeans::new(k);
        Ok(clusterer.fit(&vectors))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vector_index_manager_create_and_search() {
        let mut mgr = VectorIndexManager::new();

        mgr.create_index("Product", "embedding", 3, DistanceMetric::L2, 8, 50, 30)
            .unwrap();

        assert!(mgr.has_index("Product", "embedding"));
        assert!(!mgr.has_index("Product", "name"));
        assert_eq!(mgr.index_count(), 1);

        // Index some vectors
        mgr.index_vector(b"doc1", "Product", "embedding", vec![1.0, 0.0, 0.0]);
        mgr.index_vector(b"doc2", "Product", "embedding", vec![0.0, 1.0, 0.0]);
        mgr.index_vector(b"doc3", "Product", "embedding", vec![0.9, 0.1, 0.0]);

        // Search
        let results = mgr.search("Product", "embedding", &[1.0, 0.0, 0.0], 2).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].entry.id, b"doc1");
    }

    #[test]
    fn test_vector_index_manager_filtered_search() {
        let mut mgr = VectorIndexManager::new();

        mgr.create_index("Product", "embedding", 3, DistanceMetric::L2, 8, 50, 30)
            .unwrap();

        mgr.index_vector(b"doc1", "Product", "embedding", vec![1.0, 0.0, 0.0]);
        mgr.index_vector(b"doc2", "Product", "embedding", vec![0.0, 1.0, 0.0]);
        mgr.index_vector(b"doc3", "Product", "embedding", vec![0.9, 0.1, 0.0]);

        // Only allow doc1 and doc3
        let mut allowed = HashSet::new();
        allowed.insert(b"doc1".to_vec());
        allowed.insert(b"doc3".to_vec());

        let results = mgr
            .search_filtered("Product", "embedding", &[1.0, 0.0, 0.0], 2, &allowed)
            .unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].entry.id, b"doc1");
        assert_eq!(results[1].entry.id, b"doc3");
    }

    #[test]
    fn test_vector_index_manager_drop() {
        let mut mgr = VectorIndexManager::new();
        mgr.create_index("Product", "embedding", 3, DistanceMetric::L2, 8, 50, 30)
            .unwrap();
        assert_eq!(mgr.index_count(), 1);

        mgr.drop_index("Product", "embedding");
        assert_eq!(mgr.index_count(), 0);
        assert!(!mgr.has_index("Product", "embedding"));
    }

    #[test]
    fn test_vector_index_manager_no_duplicate() {
        let mut mgr = VectorIndexManager::new();
        mgr.create_index("Product", "embedding", 3, DistanceMetric::L2, 8, 50, 30)
            .unwrap();

        let result = mgr.create_index("Product", "embedding", 3, DistanceMetric::L2, 8, 50, 30);
        assert!(result.is_err());
    }
}
