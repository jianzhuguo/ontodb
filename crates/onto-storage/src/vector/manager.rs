// Copyright (c) 2024-2026 OntoDB Team
// Licensed under the Business Source License 1.1 (BUSL-1.1).
// See LICENSE for details. Change Date: 2031-09-15.
// On the Change Date, this file will be licensed under Apache License 2.0.
//! Vector index manager for the storage engine.
//!
//! Manages HNSW vector indexes per class.column, handling:
//! - Index creation with configurable parameters
//! - Vector insertion (auto-indexing on document write)
//! - Vector similarity search (with optional ontology filtering)
//! - Persistence of vector index metadata

use crate::vector::normalize::{l2_normalize, should_normalize};
use crate::vector::{DistanceMetric, HnswConfig, HnswIndex, SearchResult, VecData, VectorEntry};
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
    /// Mapping from document key -> (class, column, vector data) for
    /// re-indexing and stale detection.  Bit-packed form keeps 1024-bit
    /// fingerprints at 128B instead of 4KB f32.
    doc_vectors: HashMap<Vec<u8>, Vec<(String, String, VecData)>>,
    /// Document keys that have been deleted (filtered out during search).
    /// HNSW doesn't support native deletion, so we track tombstones here.
    deleted_keys: HashSet<Vec<u8>>,
    /// False while a restore/reconcile is still running. Searches must not
    /// be served before this flips true: `deleted_keys` is in-memory only and
    /// is rebuilt by the post-restore reconcile, so serving earlier leaks
    /// ghost vectors whose documents no longer carry the column.
    ready: std::sync::atomic::AtomicBool,
    /// Vectors discovered during reconcile that are not yet inserted into
    /// their HNSW graph. Backfill runs after `mark_ready`, so searches are
    /// already safe (no ghosts) but may temporarily miss these rows.
    /// Exposed via schema so callers can see the recall gap.
    pending_backfill: std::sync::atomic::AtomicUsize,
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
            ready: std::sync::atomic::AtomicBool::new(false),
            pending_backfill: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Marks the manager as ready to serve searches (post-reconcile).
    pub fn mark_ready(&self) {
        self.ready
            .store(true, std::sync::atomic::Ordering::Release);
    }

    /// True once restore/reconcile has finished and searches are safe.
    pub fn is_ready(&self) -> bool {
        self.ready.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Sets how many vectors are still queued for background backfill.
    pub fn set_pending_backfill(&self, n: usize) {
        self.pending_backfill
            .store(n, std::sync::atomic::Ordering::Release);
    }

    /// Subtracts `n` from the pending-backfill counter (after insert/skip).
    pub fn sub_pending_backfill(&self, n: usize) {
        self.pending_backfill
            .fetch_sub(n.min(self.pending_backfill.load(std::sync::atomic::Ordering::Relaxed)), std::sync::atomic::Ordering::AcqRel);
    }

    /// Vectors still waiting for background backfill (0 when idle).
    pub fn pending_backfill_count(&self) -> usize {
        self.pending_backfill.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Live node count of one index (for schema introspection / probes).
    pub fn index_len(&self, class: &str, column: &str) -> Option<usize> {
        self.indexes
            .get(&IndexKey {
                class: class.to_string(),
                column: column.to_string(),
            })
            .map(|idx| idx.len())
    }

    /// Compact the deleted_keys set by removing entries that are no longer
    /// in doc_vectors (i.e., the document has been fully removed).
    pub fn compact_deleted_keys(&mut self) {
        self.deleted_keys
            .retain(|k| self.doc_vectors.contains_key(k));
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

        // Freshly created index is empty — nothing to leak. Mark ready so
        // CREATE VECTOR INDEX + immediate search works without a rebuild.
        // (The restore path uses install_graph, which deliberately does NOT
        // mark ready; rebuild_vector_indexes does so after reconcile.)
        self.mark_ready();

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

    /// True when the index has nodes modified since the last incremental persist.
    pub fn has_dirty_nodes(&self, class: &str, column: &str) -> bool {
        let key = IndexKey {
            class: class.to_string(),
            column: column.to_string(),
        };
        self.indexes.get(&key).is_some_and(|i| i.has_dirty_nodes())
    }

    /// Number of dirty (unpersisted) nodes for an index.
    pub fn dirty_count(&self, class: &str, column: &str) -> usize {
        let key = IndexKey {
            class: class.to_string(),
            column: column.to_string(),
        };
        self.indexes.get(&key).map(|i| i.dirty_count()).unwrap_or(0)
    }

    /// Drains dirty nodes into an incremental snapshot blob (self-contained
    /// node records).  Returns the serialized snapshot and the version.
    pub fn save_incremental(&mut self, class: &str, column: &str) -> Option<Vec<u8>> {
        let key = IndexKey {
            class: class.to_string(),
            column: column.to_string(),
        };
        let index = self.indexes.get_mut(&key)?;
        if !index.has_dirty_nodes() {
            return None;
        }
        let nodes = index.export_dirty_nodes();
        if nodes.is_empty() {
            return None;
        }
        let version = nodes.iter().map(|(id, _)| *id as u64).max().unwrap_or(0);
        let snap = crate::vector::incremental_persist::IncrementalSnapshot::new(version, nodes);
        Some(snap.to_bytes())
    }

    /// Applies an incremental snapshot on top of the in-memory index.
    /// No-op if the index does not exist yet (caller should create it first).
    pub fn apply_incremental(&mut self, class: &str, column: &str, data: &[u8]) -> bool {
        let key = IndexKey {
            class: class.to_string(),
            column: column.to_string(),
        };
        let Some(snap) = crate::vector::incremental_persist::IncrementalSnapshot::from_bytes(data)
        else {
            return false;
        };
        if let Some(index) = self.indexes.get_mut(&key) {
            index.import_nodes(&snap.nodes);
            true
        } else {
            false
        }
    }

    /// Load an HNSW graph structure from bytes.
    ///
    /// Rejects empty graphs (0 nodes): those are snapshots taken at CREATE
    /// INDEX before any vectors exist, and treating them as a successful
    /// restore would skip the vector rebuild entirely.
    /// Deserializes an HNSW snapshot and installs it under `class.column`.
    ///
    /// Prefer [`Self::install_graph`] with a pre-deserialized index when the
    /// caller holds other locks: `load_from_bytes` on a multi-hundred-MB
    /// snapshot can take minutes under memory pressure and must not run
    /// while this manager is write-locked (it blocks every search).
    pub fn load_graph(&mut self, class: &str, column: &str, data: &[u8]) -> Result<()> {
        let index = HnswIndex::load_from_bytes(data)
            .map_err(|e| CoreError::InvalidArgument(format!("Failed to load HNSW graph: {}", e)))?;
        self.install_graph(class, column, index).map(|_| ())
    }

    /// Installs an already-deserialized HNSW index. Returns node count.
    pub fn install_graph(
        &mut self,
        class: &str,
        column: &str,
        index: HnswIndex,
    ) -> Result<usize> {
        if index.is_empty() {
            return Err(CoreError::InvalidArgument(
                "HNSW graph snapshot has 0 nodes; rebuild required".to_string(),
            ));
        }

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

        let n = index.len();
        self.indexes.insert(key.clone(), index);
        self.metadata.insert(key, meta);

        Ok(n)
    }

    /// Returns the set of pks already present in the HNSW graph for
    /// `class.column`, or an empty set if the index does not exist.
    pub fn id_set(&self, class: &str, column: &str) -> std::collections::HashSet<Vec<u8>> {
        let key = IndexKey {
            class: class.to_string(),
            column: column.to_string(),
        };
        self.indexes
            .get(&key)
            .map(|idx| idx.id_set())
            .unwrap_or_default()
    }

    /// Indexes a batch of pre-converted vector data (base64→Packed fast path).
    /// Consumes the batch — no f32 conversion, no clones on the bulk path.
    pub fn index_vector_data_owned(&mut self, vectors: Vec<(Vec<u8>, String, String, VecData)>) {
        let mut by_index: HashMap<IndexKey, Vec<(Vec<u8>, VecData)>> = HashMap::new();
        for (doc_key, class, column, data) in vectors {
            if !self.doc_vectors.contains_key(&doc_key) {
                self.deleted_keys.remove(&doc_key);
            }

            let key = IndexKey {
                class: class.clone(),
                column: column.clone(),
            };

            // Normalize only for Cosine on Dense vectors; binary Packed
            // fingerprints must keep their 0/1 form or the fast path dies.
            let data = match data {
                VecData::Dense(mut v) => {
                    if let Some(meta) = self.metadata.get(&key) {
                        if should_normalize(&meta.metric) {
                            l2_normalize(&mut v);
                        }
                    }
                    VecData::Dense(v)
                }
                packed => packed,
            };

            by_index
                .entry(key)
                .or_default()
                .push((doc_key.clone(), data.clone()));
            self.doc_vectors.entry(doc_key).or_default().push((
                class,
                column,
                data,
            ));
        }

        for (idx_key, vecs) in by_index {
            if let Some(index) = self.indexes.get_mut(&idx_key) {
                for (id, data) in vecs {
                    index.insert_data(id, data);
                }
            }
        }
    }

    /// Guarded variant of [`Self::index_vector_data_owned`] for background
    /// backfill. Skips a document when a concurrent write already handled it:
    /// - `doc_vectors` already carries this (doc, class, column) — the write
    ///   path indexed it (possibly a newer value) after our reconcile scan.
    /// - `deleted_keys` contains the doc — it was deleted/cleared since.
    /// Inserting anyway would create a stale/duplicate HNSW node
    /// (`insert_data` does not dedup by id).
    /// Returns `(inserted, skipped)` counts.
    pub fn index_vector_data_owned_guarded(
        &mut self,
        vectors: Vec<(Vec<u8>, String, String, VecData)>,
    ) -> (usize, usize) {
        let mut inserted = 0usize;
        let mut skipped = 0usize;
        let mut by_index: HashMap<IndexKey, Vec<(Vec<u8>, VecData)>> = HashMap::new();
        for (doc_key, class, column, data) in vectors {
            if self.deleted_keys.contains(&doc_key) {
                skipped += 1;
                continue;
            }
            let already = self
                .doc_vectors
                .get(&doc_key)
                .map(|entries| {
                    entries
                        .iter()
                        .any(|(c, co, _)| c == &class && co == &column)
                })
                .unwrap_or(false);
            if already {
                skipped += 1;
                continue;
            }

            let key = IndexKey {
                class: class.clone(),
                column: column.clone(),
            };

            // Normalize only for Cosine on Dense vectors; binary Packed
            // fingerprints must keep their 0/1 form or the fast path dies.
            let data = match data {
                VecData::Dense(mut v) => {
                    if let Some(meta) = self.metadata.get(&key) {
                        if should_normalize(&meta.metric) {
                            l2_normalize(&mut v);
                        }
                    }
                    VecData::Dense(v)
                }
                packed => packed,
            };

            self.deleted_keys.remove(&doc_key);
            by_index
                .entry(key)
                .or_default()
                .push((doc_key.clone(), data.clone()));
            self.doc_vectors.entry(doc_key).or_default().push((
                class,
                column,
                data,
            ));
            inserted += 1;
        }

        for (idx_key, vecs) in by_index {
            if let Some(index) = self.indexes.get_mut(&idx_key) {
                for (id, data) in vecs {
                    index.insert_data(id, data);
                }
            }
        }
        (inserted, skipped)
    }

    /// Indexes a batch of vectors under a single lock acquisition.
    /// Consumes the batch (no extra clones on the bulk-load path).
    pub fn index_vector_batch_owned(&mut self, vectors: Vec<(Vec<u8>, String, String, Vec<f32>)>) {
        let mut by_index: HashMap<IndexKey, Vec<(Vec<u8>, VecData)>> = HashMap::new();
        for (doc_key, class, column, mut vector) in vectors {
            if !self.doc_vectors.contains_key(&doc_key) {
                self.deleted_keys.remove(&doc_key);
            }

            let key = IndexKey {
                class: class.clone(),
                column: column.clone(),
            };

            if let Some(meta) = self.metadata.get(&key) {
                if should_normalize(&meta.metric) {
                    l2_normalize(&mut vector);
                }
            }

            let data = VecData::from_f32(&vector);
            by_index
                .entry(key)
                .or_default()
                .push((doc_key.clone(), data.clone()));
            self.doc_vectors.entry(doc_key).or_default().push((
                class,
                column,
                data,
            ));
        }

        for (idx_key, vecs) in by_index {
            if let Some(index) = self.indexes.get_mut(&idx_key) {
                for (id, data) in vecs {
                    index.insert_data(id, data);
                }
            }
        }
    }

    /// Indexes a batch of vectors under a single lock acquisition.
    /// More efficient than calling `index_vector()` in a loop for bulk operations.
    pub fn index_vector_batch(&mut self, vectors: &[(Vec<u8>, &str, &str, Vec<f32>)]) {
        let owned: Vec<(Vec<u8>, String, String, Vec<f32>)> = vectors
            .iter()
            .map(|(k, c, o, v)| (k.clone(), c.to_string(), o.to_string(), v.clone()))
            .collect();
        self.index_vector_batch_owned(owned)
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

        // Track the vector for this document (compact form)
        self.doc_vectors.entry(doc_key.to_vec()).or_default().push((
            class.to_string(),
            column.to_string(),
            VecData::from_f32(&vector),
        ));

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
            // Accept both legacy float arrays and Bytes columns stored as
            // base64 bit-packed fingerprints (1024-bit Morgan → 128 bytes).
            if let Some(vec) = doc
                .get(&column)
                .and_then(|v| onto_core::binary_fp::json_to_f32_vec(v, dimension))
            {
                self.index_vector(doc_key, class, &column, vec);
            }
        }
    }

    /// Removes a document's vectors from all indexes. Called during DELETE.
    pub fn deindex_vectors(&mut self, doc_key: &[u8]) {
        self.deleted_keys.insert(doc_key.to_vec());
        self.doc_vectors.remove(doc_key);
    }

    /// Records that `doc_key` currently owns this vector WITHOUT inserting a
    /// new HNSW node. Used by snapshot-restore reconciliation: the node is
    /// already in the restored graph, but `doc_vectors` (in-memory only) was
    /// lost across restart and must be rebuilt so stale-entry filtering works.
    pub fn note_doc_vector(&mut self, doc_key: Vec<u8>, class: String, column: String, data: VecData) {
        self.deleted_keys.remove(&doc_key);
        self.doc_vectors
            .entry(doc_key)
            .or_default()
            .push((class, column, data));
    }

    /// Marks `doc_key` as deleted without touching the HNSW graph.
    /// Used when a restored graph node has no live document vector (the
    /// document's fp was cleared, or the row was deleted while `deleted_keys`
    /// was still in-memory-only and therefore lost on restart).
    pub fn mark_deleted(&mut self, doc_key: Vec<u8>) {
        self.doc_vectors.remove(&doc_key);
        self.deleted_keys.insert(doc_key);
    }

    /// In-place prune: drop every HNSW node that is not a live document
    /// vector (and collapse per-id duplicates left behind by `insert_data`),
    /// rewrite neighbour lists (all layers, bidirectional), and return the
    /// number of nodes removed.
    ///
    /// `keep` is `doc_vectors` keys for this index minus `deleted_keys` —
    /// i.e. exactly what a search is allowed to return. The live `VecData`
    /// is passed along so a duplicated id keeps the node whose vector
    /// matches the current document, not a stale leftover.
    pub fn prune_index_to_live(&mut self, class: &str, column: &str) -> Result<usize> {
        let key = IndexKey {
            class: class.to_string(),
            column: column.to_string(),
        };
        let mut keep: HashSet<Vec<u8>> = HashSet::new();
        let mut prefer: HashMap<Vec<u8>, VecData> = HashMap::new();
        for (dk, entries) in self.doc_vectors.iter() {
            if self.deleted_keys.contains(dk) {
                continue;
            }
            if let Some((_, _, data)) = entries
                .iter()
                .find(|(c, col, _)| c == class && col == column)
            {
                keep.insert(dk.clone());
                prefer.insert(dk.clone(), data.clone());
            }
        }

        let Some(index) = self.indexes.get_mut(&key) else {
            return Err(CoreError::InvalidArgument(format!(
                "no vector index on {}.{}",
                class, column
            )));
        };
        let before = index.len();
        let removed = index.prune_ids(&keep, &prefer);
        tracing::info!(
            table = %class,
            column = %column,
            before,
            after = index.len(),
            removed,
            keep = keep.len(),
            "vector index pruned to live entries"
        );
        // Tombstones are gone from the graph; clear them so over_fetch
        // returns to its low-boost path.
        self.deleted_keys.clear();
        self.compact_deleted_keys();
        Ok(removed)
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
        self.search_with_ef(class, column, query, k, None)
    }

    /// Search with optional ef_search override (recall/latency knob).
    pub fn search_with_ef(
        &self,
        class: &str,
        column: &str,
        query: &[f32],
        k: usize,
        ef_override: Option<usize>,
    ) -> Result<Vec<SearchResult>> {
        if !self.is_ready() {
            return Err(CoreError::InvalidArgument(
                "vector index not ready (restore/reconcile still running)".to_string(),
            ));
        }
        let key = IndexKey {
            class: class.to_string(),
            column: column.to_string(),
        };

        let index = self.indexes.get(&key).ok_or_else(|| {
            CoreError::InvalidArgument(format!("no vector index on {}.{}", class, column))
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

        // Over-fetch to compensate for deleted/stale entries. Scale by the
        // estimated live ratio, but cap the boost so a large tombstone set
        // (e.g. 400k ghosts after restore reconciliation) cannot push every
        // search into a full-graph scan. Compaction is the real fix for a
        // high deleted ratio; over-fetch only softens the edge.
        let deleted = self.deleted_keys.len().min(index.len());
        let live_estimate = index.len().saturating_sub(deleted).max(1);
        let boost = index.len().div_ceil(live_estimate).clamp(2, 8);
        let over_fetch = k.saturating_mul(boost).min(index.len()).max(k.min(index.len()));
        let results = match ef_override {
            Some(ef) => index.search_with_ef(&actual_query, over_fetch, ef),
            None => index.search(&actual_query, over_fetch),
        };

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
                entries
                    .iter()
                    .find(|(c, col, _)| c == class && col == column)
                    .map(|(_, _, v)| v)
            }) {
                let is_stale = !stored_vec.matches_f32(&r.entry.vector);
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
        if !self.is_ready() {
            return Err(CoreError::InvalidArgument(
                "vector index not ready (restore/reconcile still running)".to_string(),
            ));
        }
        let key = IndexKey {
            class: class.to_string(),
            column: column.to_string(),
        };

        let index = self.indexes.get(&key).ok_or_else(|| {
            CoreError::InvalidArgument(format!("no vector index on {}.{}", class, column))
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
                entries
                    .iter()
                    .find(|(c, col, _)| c == class && col == column)
                    .map(|(_, _, v)| v)
            }) {
                if !stored_vec.matches_f32(&r.entry.vector) {
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
            CoreError::InvalidArgument(format!("no vector index on {}.{}", class, column))
        })?;

        // Collect all vectors from doc_vectors (not from HNSW, which may have
        // stale entries).  Expand packed form for the clustering math.
        let dim = self
            .metadata
            .get(&IndexKey {
                class: class.to_string(),
                column: column.to_string(),
            })
            .map(|m| m.dimension)
            .unwrap_or(0);
        let vectors: Vec<Vec<f32>> = self
            .doc_vectors
            .values()
            .flat_map(|entries| {
                entries
                    .iter()
                    .filter(|(c, col, _)| c == class && col == column)
                    .map(|(_, _, v)| v.to_f32(dim))
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
        let results = mgr
            .search("Product", "embedding", &[1.0, 0.0, 0.0], 2)
            .unwrap();
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

    #[test]
    fn test_ready_gate_blocks_search_before_reconcile() {
        let mut mgr = VectorIndexManager::new();
        assert!(!mgr.is_ready(), "starts unready");

        // create_index flips ready (empty graph is safe to search).
        mgr.create_index("P", "e", 3, DistanceMetric::L2, 8, 50, 30)
            .unwrap();
        assert!(mgr.is_ready());
        mgr.index_vector(b"a", "P", "e", vec![1.0, 0.0, 0.0]);

        // Simulate the restore window: graph loaded, reconcile not done yet.
        let mut mgr2 = VectorIndexManager::new();
        mgr2
            .create_index("P", "e", 3, DistanceMetric::L2, 8, 50, 30)
            .unwrap();
        mgr2
            .ready
            .store(false, std::sync::atomic::Ordering::Release);
        assert!(mgr2.search("P", "e", &[1.0, 0.0, 0.0], 3).is_err());
        assert!(mgr2
            .search_filtered("P", "e", &[1.0, 0.0, 0.0], 3, &HashSet::new())
            .is_err());
        mgr2.mark_ready();
        assert!(mgr2.search("P", "e", &[1.0, 0.0, 0.0], 3).is_ok());
    }

    #[test]
    fn test_mark_deleted_hides_ghost_from_search() {
        let mut mgr = VectorIndexManager::new();
        mgr.create_index("Product", "embedding", 3, DistanceMetric::L2, 8, 50, 30)
            .unwrap();

        mgr.index_vector(b"alive", "Product", "embedding", vec![1.0, 0.0, 0.0]);
        mgr.index_vector(b"ghost", "Product", "embedding", vec![0.95, 0.05, 0.0]);

        let before = mgr
            .search("Product", "embedding", &[1.0, 0.0, 0.0], 5)
            .unwrap();
        assert_eq!(before.len(), 2);

        // Simulate restore reconciliation: ghost doc lost its fp.
        mgr.mark_deleted(b"ghost".to_vec());

        let after = mgr
            .search("Product", "embedding", &[1.0, 0.0, 0.0], 5)
            .unwrap();
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].entry.id, b"alive");
        assert!(mgr.deleted_keys_count() >= 1);
    }

    #[test]
    fn test_note_doc_vector_rebuilds_stale_filter() {
        let mut mgr = VectorIndexManager::new();
        mgr.create_index("Product", "embedding", 3, DistanceMetric::L2, 8, 50, 30)
            .unwrap();

        // Node lives in the graph, but doc_vectors was lost across restart.
        mgr.index_vector_data_owned(vec![(
            b"doc".to_vec(),
            "Product".to_string(),
            "embedding".to_string(),
            VecData::from_f32(&[1.0, 0.0, 0.0]),
        )]);
        // Simulate restart: doc_vectors empty, graph still has the node.
        // note_doc_vector restores the mapping without inserting a second node.
        let n_before = mgr.id_set("Product", "embedding").len();
        mgr.note_doc_vector(
            b"doc".to_vec(),
            "Product".to_string(),
            "embedding".to_string(),
            VecData::from_f32(&[1.0, 0.0, 0.0]),
        );
        assert_eq!(mgr.id_set("Product", "embedding").len(), n_before);

        // A matching live vector keeps the hit; a mismatched one is stale.
        let hits = mgr
            .search("Product", "embedding", &[1.0, 0.0, 0.0], 5)
            .unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn test_guarded_insert_skips_concurrent_write() {
        let mut mgr = VectorIndexManager::new();
        mgr.create_index("Product", "embedding", 3, DistanceMetric::L2, 8, 50, 30)
            .unwrap();

        // Concurrent write already indexed doc "a" after our reconcile scan.
        mgr.index_vector(b"a", "Product", "embedding", vec![0.9, 0.1, 0.0]);
        let n_before = mgr.id_set("Product", "embedding").len();

        // Background backfill still holds the (now stale) queued copy.
        let (inserted, skipped) = mgr.index_vector_data_owned_guarded(vec![
            (b"a".to_vec(), "Product".to_string(), "embedding".to_string(), VecData::from_f32(&[1.0, 0.0, 0.0])),
            (b"b".to_vec(), "Product".to_string(), "embedding".to_string(), VecData::from_f32(&[0.0, 1.0, 0.0])),
        ]);
        assert_eq!(inserted, 1);
        assert_eq!(skipped, 1);
        // No duplicate node for "a".
        assert_eq!(mgr.id_set("Product", "embedding").len(), n_before + 1);
    }

    #[test]
    fn test_guarded_insert_skips_deleted_doc() {
        let mut mgr = VectorIndexManager::new();
        mgr.create_index("Product", "embedding", 3, DistanceMetric::L2, 8, 50, 30)
            .unwrap();

        // Doc was deleted/cleared between reconcile and backfill.
        mgr.mark_deleted(b"gone".to_vec());
        let (inserted, skipped) = mgr.index_vector_data_owned_guarded(vec![(
            b"gone".to_vec(),
            "Product".to_string(),
            "embedding".to_string(),
            VecData::from_f32(&[1.0, 0.0, 0.0]),
        )]);
        assert_eq!(inserted, 0);
        assert_eq!(skipped, 1);
        assert!(mgr.id_set("Product", "embedding").is_empty());
    }

    #[test]
    fn test_pending_backfill_counter() {
        let mgr = VectorIndexManager::new();
        assert_eq!(mgr.pending_backfill_count(), 0);
        mgr.set_pending_backfill(100);
        assert_eq!(mgr.pending_backfill_count(), 100);
        mgr.sub_pending_backfill(30);
        assert_eq!(mgr.pending_backfill_count(), 70);
        // Over-subtract must not wrap.
        mgr.sub_pending_backfill(70);
        assert_eq!(mgr.pending_backfill_count(), 0);
    }
}
