// Copyright (c) 2024-2026 OntoDB Team
// Licensed under the Business Source License 1.1 (BUSL-1.1).
// See LICENSE for details. Change Date: 2031-09-15.
// On the Change Date, this file will be licensed under Apache License 2.0.
//! Graph storage - CRUD operations for vertices and edges.
//!
//! Supports both in-memory and persistent storage via LSM engine.
//!
//! Persistence key format:
//! - `__graph_v__{vertex_id}` → serialized Vertex
//! - `__graph_e__{edge_id}` → serialized Edge

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use onto_core::EntityId;

use crate::error::GraphError;
use crate::model::{Edge, PropValue, PropertyMap, Vertex};
use crate::traversal::Direction;

/// Cache status for a relationship type
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CacheStatus {
    /// Never loaded
    NotLoaded,
    /// Currently being loaded by another thread
    Loading,
    /// Successfully loaded and ready to use
    Loaded,
    /// Invalidated by a write operation, needs reload
    Invalidated,
    /// Loading failed (timeout, OOM, etc.)
    Failed,
}

/// Cache entry tracking the state of a loaded relationship type
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheEntry {
    /// Current status
    pub status: CacheStatus,
    /// Number of edges loaded
    pub edge_count: usize,
    /// Approximate memory usage in bytes
    pub memory_bytes: usize,
    /// When this entry was last loaded (seconds since epoch, 0 if never)
    pub loaded_at_secs: u64,
}

impl CacheEntry {
    fn not_loaded() -> Self {
        Self { status: CacheStatus::NotLoaded, edge_count: 0, memory_bytes: 0, loaded_at_secs: 0 }
    }
}

/// Key prefixes for graph persistence in LSM engine.
const GRAPH_VERTEX_PREFIX: &str = "__graph_v__";
const GRAPH_EDGE_PREFIX: &str = "__graph_e__";

/// Maximum number of vertices allowed in the in-memory graph store (DoS protection).
const MAX_VERTICES: usize = 1_000_000;

/// Maximum memory usage for graph cache in bytes (2GB).
const MAX_CACHE_MEMORY_BYTES: usize = 2 * 1024 * 1024 * 1024;

/// Batch size for paging when loading relations from LSM.
const LOAD_BATCH_SIZE: usize = 10_000;

/// Timeout for loading a single relation type (60 seconds).
const LOAD_TIMEOUT_SECS: u64 = 60;

/// Debounce cooldown period after invalidation (10 seconds).
/// During this period, queries won't trigger a rebuild.
const INVALIDATION_COOLDOWN_SECS: u64 = 10;

/// In-memory graph store with optional LSM persistence.
///
/// # Lock Ordering (deadlock prevention)
///
/// When acquiring multiple locks, always follow this sequence:
///
/// 1. `vertices`        — vertex data
/// 2. `edges`           — edge data by ID
/// 3. `label_index`     — label → vertex set
/// 4. `edge_label_index` — edge label → edge ID set
/// 5. `edge_prop_index` — edge property index
/// 6. `id_to_idx`       — string → integer mapping
/// 7. `idx_to_id` / `adj_out` / `adj_in` — integer-indexed structures (same level)
///
/// Never acquire a higher-numbered lock while holding a lower-numbered one.
/// Same-level locks can be acquired in any order relative to each other.
pub struct GraphStore {
    /// [LOCK 1] Vertices indexed by ID.
    vertices: RwLock<HashMap<String, Vertex>>,
    /// [LOCK 2] All edges indexed by ID. Single source of truth.
    edges: RwLock<HashMap<String, Edge>>,
    /// [LOCK 3] Labels index: label -> set of vertex IDs.
    label_index: RwLock<HashMap<String, HashSet<String>>>,
    /// [LOCK 4] Edge label index: label -> set of edge IDs.
    edge_label_index: RwLock<HashMap<String, HashSet<String>>>,
    /// [LOCK 5] Edge property index: prop_key -> prop_value -> set of edge IDs.
    /// Enables O(1) lookup for Eq queries on edge properties.
    edge_prop_index: RwLock<HashMap<String, HashMap<String, HashSet<String>>>>,
    /// Optional LSM engine for persistence.
    engine: Option<Arc<onto_storage::LsmEngine>>,
    /// [LOCK 6] Internal integer ID mapping for fast traversal.
    id_to_idx: RwLock<HashMap<String, u32>>,
    /// [LOCK 7a] Maps integer index -> string ID.
    idx_to_id: RwLock<Vec<String>>,
    /// [LOCK 7b] Outgoing adjacency: idx -> (neighbor_idx, edge_id).
    adj_out: RwLock<Vec<Vec<(u32, String)>>>,
    /// [LOCK 7c] Incoming adjacency: idx -> (neighbor_idx, edge_id).
    adj_in: RwLock<Vec<Vec<(u32, String)>>>,
    /// [LOCK 8] Cache state per relationship type (e.g., "treats", "causes").
    /// Tracks which relationships have been loaded into the graph.
    /// Uses parking_lot::Mutex for the loading locks (not RwLock).
    cache_state: RwLock<HashMap<String, CacheEntry>>,
    /// Loading locks: one Mutex per relation type to prevent concurrent loading.
    loading_locks: parking_lot::Mutex<HashMap<String, Arc<parking_lot::Mutex<()>>>>,
    /// Last invalidation timestamp per relation type (seconds since epoch).
    /// Used for debounce: queries within cooldown period won't trigger rebuild.
    last_invalidation: RwLock<HashMap<String, u64>>,
    /// Cache statistics for observability.
    cache_stats: RwLock<CacheStats>,
    /// Pending edge changes per relation type (for incremental updates).
    /// Each entry tracks edges that were added or removed since last full load.
    pending_changes: RwLock<HashMap<String, Vec<EdgeChange>>>,
}

/// A pending edge change for incremental cache updates.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EdgeChange {
    /// An edge was added.
    Added { source: String, target: String, edge_id: String },
    /// An edge was removed.
    Removed { edge_id: String },
}

/// Aggregate cache statistics for observability.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CacheStats {
    /// Total number of cache hits (relation was already loaded).
    pub hits: u64,
    /// Total number of cache misses (relation needed loading).
    pub misses: u64,
    /// Total number of cache invalidations.
    pub invalidations: u64,
    /// Total number of load operations.
    pub loads: u64,
    /// Total number of load failures.
    pub load_failures: u64,
    /// Total time spent loading (microseconds).
    pub load_time_us: u64,
    /// Total number of degraded queries (fell back to JOIN).
    pub degraded_queries: u64,
}

impl GraphStore {
    pub fn new() -> Self {
        Self {
            vertices: RwLock::new(HashMap::new()),
            edges: RwLock::new(HashMap::new()),
            label_index: RwLock::new(HashMap::new()),
            edge_label_index: RwLock::new(HashMap::new()),
            edge_prop_index: RwLock::new(HashMap::new()),
            engine: None,
            id_to_idx: RwLock::new(HashMap::new()),
            idx_to_id: RwLock::new(Vec::new()),
            adj_out: RwLock::new(Vec::new()),
            adj_in: RwLock::new(Vec::new()),
            cache_state: RwLock::new(HashMap::new()),
            loading_locks: parking_lot::Mutex::new(HashMap::new()),
            last_invalidation: RwLock::new(HashMap::new()),
            cache_stats: RwLock::new(CacheStats::default()),
            pending_changes: RwLock::new(HashMap::new()),
        }
    }

    /// Create a new persistent graph store backed by an LSM engine.
    ///
    /// Data is persisted to the engine with `__graph_v__` and `__graph_e__` prefixes.
    /// Call `load_from_engine()` after creation to restore data from disk.
    pub fn with_engine(engine: Arc<onto_storage::LsmEngine>) -> Self {
        Self {
            vertices: RwLock::new(HashMap::new()),
            edges: RwLock::new(HashMap::new()),
            label_index: RwLock::new(HashMap::new()),
            edge_label_index: RwLock::new(HashMap::new()),
            edge_prop_index: RwLock::new(HashMap::new()),
            engine: Some(engine),
            id_to_idx: RwLock::new(HashMap::new()),
            idx_to_id: RwLock::new(Vec::new()),
            adj_out: RwLock::new(Vec::new()),
            adj_in: RwLock::new(Vec::new()),
            cache_state: RwLock::new(HashMap::new()),
            loading_locks: parking_lot::Mutex::new(HashMap::new()),
            last_invalidation: RwLock::new(HashMap::new()),
            cache_stats: RwLock::new(CacheStats::default()),
            pending_changes: RwLock::new(HashMap::new()),
        }
    }

    /// Whether this store has persistence enabled.
    pub fn is_persistent(&self) -> bool {
        self.engine.is_some()
    }

    /// Load graph data from the LSM engine.
    ///
    /// Call this once after `with_engine()` to restore persisted state.
    /// Rebuilds all in-memory indexes (adjacency lists, label index, etc.).
    pub fn load_from_engine(&self) -> Result<(), GraphError> {
        let engine = match &self.engine {
            Some(e) => e,
            None => return Ok(()), // No persistence, nothing to load
        };

        // ── Pass 1: scan vertices into local Vec (no locks) ──
        let vertex_entries = engine
            .scan_prefix(GRAPH_VERTEX_PREFIX.as_bytes())
            .map_err(|e| GraphError::StorageError(e.to_string()))?;

        let mut vertices_vec: Vec<Vertex> = Vec::new();
        for (_key, value) in &vertex_entries {
            if let Ok(vertex) = serde_json::from_slice::<Vertex>(value) {
                vertices_vec.push(vertex);
            }
        }

        // ── Pass 2: scan edges into local Vec (no locks) ──
        let edge_entries = engine
            .scan_prefix(GRAPH_EDGE_PREFIX.as_bytes())
            .map_err(|e| GraphError::StorageError(e.to_string()))?;

        let mut edges_vec: Vec<Edge> = Vec::new();
        for (_key, value) in &edge_entries {
            if let Ok(edge) = serde_json::from_slice::<Edge>(value) {
                edges_vec.push(edge);
            }
        }

        // ── Build all indexes locally (no locks) ──
        let mut local_vertices: HashMap<String, Vertex> = HashMap::with_capacity(vertices_vec.len());
        let mut local_label_index: HashMap<String, HashSet<String>> = HashMap::new();
        let mut local_id_to_idx: HashMap<String, u32> = HashMap::new();
        let mut local_idx_to_id: Vec<String> = Vec::new();
        let mut local_adj_out: Vec<Vec<(u32, String)>> = Vec::new();
        let mut local_adj_in: Vec<Vec<(u32, String)>> = Vec::new();

        // Build vertex data and integer index
        for vertex in vertices_vec {
            let id = vertex.id.clone();
            let labels = vertex.labels.clone();

            // Integer index
            let idx: u32 = local_idx_to_id.len().try_into().unwrap_or_else(|_| {
                tracing::error!("Vertex index overflow: {} vertices exceed u32::MAX", local_idx_to_id.len());
                u32::MAX
            });
            local_id_to_idx.insert(id.clone(), idx);
            local_idx_to_id.push(id.clone());
            local_adj_out.push(Vec::new());
            local_adj_in.push(Vec::new());

            // Label index
            for label in &labels {
                local_label_index.entry(label.clone()).or_default().insert(id.clone());
            }

            local_vertices.insert(id, vertex);
        }

        // Build edge data and adjacency lists
        let mut local_edges: HashMap<String, Edge> = HashMap::with_capacity(edges_vec.len());
        let mut local_edge_label_index: HashMap<String, HashSet<String>> = HashMap::new();
        let mut local_edge_prop_index: HashMap<String, HashMap<String, HashSet<String>>> = HashMap::new();

        for edge in edges_vec {
            let edge_id = edge.id.clone();
            let from_id = edge.from.clone();
            let to_id = edge.to.clone();
            let edge_label = edge.label.clone();

            // Edge label index
            local_edge_label_index
                .entry(edge_label)
                .or_default()
                .insert(edge_id.clone());

            // Edge property index
            for (key, value) in &edge.properties {
                let val_str = match value {
                    PropValue::String(s) => s.clone(),
                    other => other.to_string(),
                };
                local_edge_prop_index
                    .entry(key.clone())
                    .or_default()
                    .entry(val_str)
                    .or_default()
                    .insert(edge_id.clone());
            }

            // Adjacency lists
            if let (Some(&from_idx), Some(&to_idx)) = (
                local_id_to_idx.get(&from_id),
                local_id_to_idx.get(&to_id),
            ) {
                let fi = from_idx as usize;
                let ti = to_idx as usize;
                if fi < local_adj_out.len() {
                    local_adj_out[fi].push((to_idx, edge_id.clone()));
                }
                if ti < local_adj_in.len() {
                    local_adj_in[ti].push((from_idx, edge_id.clone()));
                }
            }

            local_edges.insert(edge_id, edge);
        }

        // ── Bulk insert with single lock acquisitions ──
        // LOCK 1: vertices
        *self.vertices.write() = local_vertices;
        // LOCK 2: edges
        *self.edges.write() = local_edges;
        // LOCK 3: label_index
        *self.label_index.write() = local_label_index;
        // LOCK 4: edge_label_index
        *self.edge_label_index.write() = local_edge_label_index;
        // LOCK 5: edge_prop_index
        *self.edge_prop_index.write() = local_edge_prop_index;
        // LOCK 6: id_to_idx
        *self.id_to_idx.write() = local_id_to_idx;
        // LOCK 7: idx_to_id, adj_out, adj_in
        *self.idx_to_id.write() = local_idx_to_id;
        *self.adj_out.write() = local_adj_out;
        *self.adj_in.write() = local_adj_in;

        Ok(())
    }

    /// Get or create integer index for a vertex ID.
    ///
    /// Acquires all four index locks (id_to_idx → idx_to_id → adj_out → adj_in)
    /// in lock order, then releases them together.  Another thread reading any of
    /// these structures will either see none of the update or all of it — never
    /// a partial state.
    fn get_or_create_idx(&self, id: &str) -> u32 {
        // LOCK 5: id_to_idx (write)
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
        // LOCK 6a: idx_to_id (write) — hold all four together for atomicity
        let mut ids = self.idx_to_id.write();
        // LOCK 6b: adj_out (write)
        let mut ao = self.adj_out.write();
        // LOCK 6c: adj_in (write)
        let mut ai = self.adj_in.write();
        ids.push(id.to_string());
        ao.push(Vec::new());
        ai.push(Vec::new());
        // All four locks released together here — atomic update
        idx
    }

    // ── Persistence Helpers ──────────────────────────────────────

    /// Persist a vertex to the LSM engine.
    fn persist_vertex(&self, vertex: &Vertex) -> Result<(), GraphError> {
        if let Some(ref engine) = self.engine {
            let key = format!("{}{}", GRAPH_VERTEX_PREFIX, vertex.id);
            let value =
                serde_json::to_vec(vertex).map_err(|e| GraphError::StorageError(e.to_string()))?;
            engine
                .put(key.into_bytes(), value)
                .map_err(|e| GraphError::StorageError(e.to_string()))?;
        }
        Ok(())
    }

    /// Remove a vertex from the LSM engine.
    fn unpersist_vertex(&self, id: &str) -> Result<(), GraphError> {
        if let Some(ref engine) = self.engine {
            let key = format!("{}{}", GRAPH_VERTEX_PREFIX, id);
            engine
                .delete(key.into_bytes())
                .map_err(|e| GraphError::StorageError(e.to_string()))?;
        }
        Ok(())
    }

    /// Persist an edge to the LSM engine.
    #[allow(dead_code)]
    fn persist_edge(&self, edge: &Edge) -> Result<(), GraphError> {
        if let Some(ref engine) = self.engine {
            let key = format!("{}{}", GRAPH_EDGE_PREFIX, edge.id);
            let value =
                serde_json::to_vec(edge).map_err(|e| GraphError::StorageError(e.to_string()))?;
            engine
                .put(key.into_bytes(), value)
                .map_err(|e| GraphError::StorageError(e.to_string()))?;
        }
        Ok(())
    }

    /// Persist an edge to LSM by its ID (reads from the in-memory edges map).
    fn persist_edge_by_id(&self, id: &str) -> Result<(), GraphError> {
        if let Some(ref engine) = self.engine {
            let edges = self.edges.read();
            if let Some(edge) = edges.get(id) {
                let key = format!("{}{}", GRAPH_EDGE_PREFIX, id);
                let value = serde_json::to_vec(edge)
                    .map_err(|e| GraphError::StorageError(e.to_string()))?;
                engine
                    .put(key.into_bytes(), value)
                    .map_err(|e| GraphError::StorageError(e.to_string()))?;
            }
        }
        Ok(())
    }

    /// Remove an edge from the LSM engine.
    fn unpersist_edge(&self, id: &str) -> Result<(), GraphError> {
        if let Some(ref engine) = self.engine {
            let key = format!("{}{}", GRAPH_EDGE_PREFIX, id);
            engine
                .delete(key.into_bytes())
                .map_err(|e| GraphError::StorageError(e.to_string()))?;
        }
        Ok(())
    }

    // ── Vertex CRUD ──────────────────────────────────────────────

    /// Add a vertex to the graph.
    pub fn add_vertex(&self, vertex: Vertex) -> Result<(), GraphError> {
        let id = vertex.id.clone();
        let labels = vertex.labels.clone();

        // DoS protection: enforce max vertex count
        if self.vertices.read().len() >= MAX_VERTICES {
            return Err(GraphError::StorageError(format!(
                "vertex limit reached ({})",
                MAX_VERTICES
            )));
        }

        // Persist to LSM engine before updating in-memory state
        self.persist_vertex(&vertex)?;

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
                idx.entry(label).or_default().insert(id.clone());
            }
        }

        // Create integer index for fast traversal
        self.get_or_create_idx(&id);

        Ok(())
    }

    /// Get a vertex by ID.
    pub fn get_vertex(&self, id: &str) -> Option<Vertex> {
        self.vertices.read().get(id).cloned()
    }

    /// Update vertex properties.
    pub fn update_vertex(&self, id: &str, properties: PropertyMap) -> Result<(), GraphError> {
        let mut verts = self.vertices.write();
        let vertex = verts
            .get_mut(id)
            .ok_or_else(|| GraphError::VertexNotFound(id.to_string()))?;
        for (k, v) in properties {
            vertex.properties.insert(k, v);
        }
        let updated = vertex.clone();
        drop(verts);

        // Persist updated vertex
        self.persist_vertex(&updated)?;

        Ok(())
    }

    /// Delete a vertex and all its connected edges.
    ///
    /// Lock order: vertices → edges → label_index →
    ///             id_to_idx → idx_to_id / adj_out / adj_in
    pub fn delete_vertex(&self, id: &str) -> Result<(), GraphError> {
        // LOCK 1: vertices (write)
        let vertex = {
            let mut verts = self.vertices.write();
            verts
                .remove(id)
                .ok_or_else(|| GraphError::VertexNotFound(id.to_string()))?
        };

        // Unpersist vertex from LSM engine (no lock held)
        self.unpersist_vertex(id)?;

        // Get the integer index for this vertex
        let maybe_idx = self.id_to_idx.read().get(id).copied();

        // Collect edges to delete using adjacency lists, then remove from edges/edge_label_index.
        // Lock order: edges(2)
        let mut deleted_edge_ids: Vec<String> = Vec::new();

        if let Some(idx) = maybe_idx {
            // Collect edge IDs from adj_out and adj_in
            let out_entries: Vec<(u32, String)> = {
                let adj_out = self.adj_out.read();
                adj_out.get(idx as usize).cloned().unwrap_or_default()
            };
            let in_entries: Vec<(u32, String)> = {
                let adj_in = self.adj_in.read();
                adj_in.get(idx as usize).cloned().unwrap_or_default()
            };

            let mut outgoing_neighbor_ids: Vec<(String, String)> = Vec::new();
            let mut incoming_neighbor_ids: Vec<(String, String)> = Vec::new();

            // LOCK 2: edges (write) — collect and remove edges
            {
                let mut edges = self.edges.write();
                let idx_to_id = self.idx_to_id.read();

                for (nbr_idx, edge_id) in &out_entries {
                    if let Some(neighbor_id) = idx_to_id.get(*nbr_idx as usize) {
                        outgoing_neighbor_ids.push((neighbor_id.clone(), edge_id.clone()));
                    }
                    edges.remove(edge_id);
                    deleted_edge_ids.push(edge_id.clone());
                    let _ = self.unpersist_edge(edge_id);
                }
                for (nbr_idx, edge_id) in &in_entries {
                    if let Some(neighbor_id) = idx_to_id.get(*nbr_idx as usize) {
                        incoming_neighbor_ids.push((neighbor_id.clone(), edge_id.clone()));
                    }
                    if !deleted_edge_ids.contains(edge_id) {
                        edges.remove(edge_id);
                        deleted_edge_ids.push(edge_id.clone());
                        let _ = self.unpersist_edge(edge_id);
                    }
                }
            }

            // Clean up edge label index for deleted edges
            {
                let mut eli = self.edge_label_index.write();
                for eid in &deleted_edge_ids {
                    for set in eli.values_mut() {
                        set.remove(eid);
                    }
                }
                eli.retain(|_, set| !set.is_empty());
            }

            // Clean up edge property index for deleted edges
            {
                let mut epi = self.edge_prop_index.write();
                for eid in &deleted_edge_ids {
                    for val_map in epi.values_mut() {
                        for set in val_map.values_mut() {
                            set.remove(eid);
                        }
                    }
                }
                for val_map in epi.values_mut() {
                    val_map.retain(|_, set| !set.is_empty());
                }
                epi.retain(|_, val_map| !val_map.is_empty());
            }

            // Remove deleted edges from neighbors' adjacency lists
            {
                let mut adj_in = self.adj_in.write();
                for (neighbor_id, edge_id) in &outgoing_neighbor_ids {
                    if let Some(nbr_idx) = self.id_to_idx.read().get(neighbor_id.as_str()).copied() {
                        if let Some(list) = adj_in.get_mut(nbr_idx as usize) {
                            list.retain(|(_, eid)| eid != edge_id);
                        }
                    }
                }
            }
            {
                let mut adj_out = self.adj_out.write();
                for (neighbor_id, edge_id) in &incoming_neighbor_ids {
                    if let Some(nbr_idx) = self.id_to_idx.read().get(neighbor_id.as_str()).copied() {
                        if let Some(list) = adj_out.get_mut(nbr_idx as usize) {
                            list.retain(|(_, eid)| eid != edge_id);
                        }
                    }
                }
            }

            // Clear adjacency lists for the deleted vertex
            {
                let mut adj_out = self.adj_out.write();
                if (idx as usize) < adj_out.len() {
                    adj_out[idx as usize].clear();
                }
            }
            {
                let mut adj_in = self.adj_in.write();
                if (idx as usize) < adj_in.len() {
                    adj_in[idx as usize].clear();
                }
            }

            // Remove from id_to_idx and idx_to_id
            self.id_to_idx.write().remove(id);
            let mut ids = self.idx_to_id.write();
            if (idx as usize) < ids.len() {
                ids[idx as usize] = String::new();
            }
        }

        // LOCK 3: label_index (write)
        {
            let mut idx = self.label_index.write();
            for label in &vertex.labels {
                if let Some(set) = idx.get_mut(label) {
                    set.remove(id);
                }
            }
        }

        Ok(())
    }

    /// Get vertices by label.
    pub fn get_vertices_by_label(&self, label: &str) -> Vec<Vertex> {
        // Collect IDs under label_index lock, then release before acquiring vertices lock
        let ids: Vec<String> = {
            let idx = self.label_index.read();
            idx.get(label)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .collect()
        };

        let verts = self.vertices.read();
        ids.iter().filter_map(|id| verts.get(id).cloned()).collect()
    }

    /// Get all vertices.
    pub fn get_all_vertices(&self) -> Vec<Vertex> {
        self.vertices.read().values().cloned().collect()
    }

    // ── Edge CRUD ────────────────────────────────────────────────

    /// Add an edge to the graph.
    ///
    /// Holds the vertices read lock throughout the in-memory update to prevent
    /// TOCTOU: another thread deleting a vertex between the existence check and
    /// the edge insertion.  Lock ordering: vertices → edges (consistent with
    /// delete_vertex).
    pub fn add_edge(&self, edge: Edge) -> Result<(), GraphError> {
        let id = edge.id.clone();
        let from = edge.from.clone();
        let to = edge.to.clone();
        let label = edge.label.clone();

        // Hold vertices read lock for the entire in-memory update phase.
        // This prevents a concurrent delete_vertex from removing the vertex
        // between our existence check and edge insertion (TOCTOU fix).
        // Lock order: vertices(read) → edges(write)
        // — same order as delete_vertex uses, so no deadlock.
        {
            let verts = self.vertices.read();
            if !verts.contains_key(&edge.from) {
                return Err(GraphError::VertexNotFound(edge.from.clone()));
            }
            if !verts.contains_key(&edge.to) {
                return Err(GraphError::VertexNotFound(edge.to.clone()));
            }

            {
                let mut edges = self.edges.write();
                if edges.contains_key(&id) {
                    return Err(GraphError::DuplicateVertex(format!("Edge {}", id)));
                }
                edges.insert(id.clone(), edge.clone());
            }

            // Update edge label index
            self.edge_label_index
                .write()
                .entry(label)
                .or_default()
                .insert(id.clone());

            // Update edge property index
            {
                let mut epi = self.edge_prop_index.write();
                for (key, value) in &edge.properties {
                    let val_str = match value {
                        PropValue::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    epi.entry(key.clone())
                        .or_default()
                        .entry(val_str)
                        .or_default()
                        .insert(id.clone());
                }
            }

            // verts dropped here — vertex can no longer be concurrently deleted
            // while we were inserting the edge.
        }

        // Persist to LSM engine (I/O outside the vertices lock scope)
        self.persist_edge_by_id(&id)?;

        // Update integer adjacency lists
        let from_idx = self.get_or_create_idx(&from);
        let to_idx = self.get_or_create_idx(&to);
        {
            let mut adj_out = self.adj_out.write();
            if (from_idx as usize) < adj_out.len() {
                adj_out[from_idx as usize].push((to_idx, id.clone()));
            }
        }
        {
            let mut adj_in = self.adj_in.write();
            if (to_idx as usize) < adj_in.len() {
                adj_in[to_idx as usize].push((from_idx, id));
            }
        }

        Ok(())
    }

    /// Get an edge by ID.
    pub fn get_edge(&self, id: &str) -> Option<Edge> {
        self.edges.read().get(id).cloned()
    }

    /// Update edge properties.
    ///
    /// Only updates the canonical `edges` map — adjacency lists store edge IDs
    /// so they automatically reflect the change.
    pub fn update_edge(&self, id: &str, properties: PropertyMap) -> Result<(), GraphError> {
        let mut edges = self.edges.write();
        let edge = edges
            .get_mut(id)
            .ok_or_else(|| GraphError::EdgeNotFound(id.to_string()))?;
        for (k, v) in properties {
            edge.properties.insert(k, v);
        }
        // Persist updated edge
        let updated = edge.clone();
        drop(edges);
        self.persist_edge_by_id(&updated.id)?;

        Ok(())
    }

    /// Delete an edge.
    pub fn delete_edge(&self, id: &str) -> Result<(), GraphError> {
        let edge = {
            let mut edges = self.edges.write();
            edges
                .remove(id)
                .ok_or_else(|| GraphError::EdgeNotFound(id.to_string()))?
        };

        // Unpersist from LSM engine
        self.unpersist_edge(id)?;

        // Remove from edge label index
        {
            let mut eli = self.edge_label_index.write();
            if let Some(set) = eli.get_mut(&edge.label) {
                set.remove(id);
                if set.is_empty() {
                    eli.remove(&edge.label);
                }
            }
        }

        // Remove from edge property index
        {
            let mut epi = self.edge_prop_index.write();
            for (key, value) in &edge.properties {
                let val_str = match value {
                    PropValue::String(s) => s.clone(),
                    other => other.to_string(),
                };
                if let Some(val_map) = epi.get_mut(key) {
                    if let Some(set) = val_map.get_mut(&val_str) {
                        set.remove(id);
                        if set.is_empty() {
                            val_map.remove(&val_str);
                        }
                    }
                    if val_map.is_empty() {
                        epi.remove(key);
                    }
                }
            }
        }

        // Remove from adjacency lists by filtering edge_id
        if let Some(from_idx) = self.id_to_idx.read().get(&edge.from).copied() {
            let mut adj_out = self.adj_out.write();
            if (from_idx as usize) < adj_out.len() {
                adj_out[from_idx as usize].retain(|(_, eid)| eid != id);
            }
        }
        if let Some(to_idx) = self.id_to_idx.read().get(&edge.to).copied() {
            let mut adj_in = self.adj_in.write();
            if (to_idx as usize) < adj_in.len() {
                adj_in[to_idx as usize].retain(|(_, eid)| eid != id);
            }
        }

        Ok(())
    }

    /// Get outgoing edges from a vertex.
    pub fn get_out_edges(&self, vertex_id: &str) -> Vec<Edge> {
        let idx = match self.id_to_idx.read().get(vertex_id).copied() {
            Some(idx) => idx,
            None => return Vec::new(),
        };
        let edge_ids: Vec<String> = {
            let adj_out = self.adj_out.read();
            adj_out
                .get(idx as usize)
                .map(|list| list.iter().map(|(_, eid)| eid.clone()).collect())
                .unwrap_or_default()
        };
        let edges = self.edges.read();
        edge_ids
            .iter()
            .filter_map(|eid| edges.get(eid).cloned())
            .collect()
    }

    /// Get incoming edges to a vertex.
    pub fn get_in_edges(&self, vertex_id: &str) -> Vec<Edge> {
        let idx = match self.id_to_idx.read().get(vertex_id).copied() {
            Some(idx) => idx,
            None => return Vec::new(),
        };
        let edge_ids: Vec<String> = {
            let adj_in = self.adj_in.read();
            adj_in
                .get(idx as usize)
                .map(|list| list.iter().map(|(_, eid)| eid.clone()).collect())
                .unwrap_or_default()
        };
        let edges = self.edges.read();
        edge_ids
            .iter()
            .filter_map(|eid| edges.get(eid).cloned())
            .collect()
    }

    // ── P0: Edge Label Index Query ─────────────────────────────

    /// Get all edge IDs with a given label.
    /// Uses the edge label index for O(1) lookup (P0 optimization).
    pub fn get_edge_ids_by_label(&self, label: &str) -> Vec<String> {
        self.edge_label_index
            .read()
            .get(label)
            .map(|set| set.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Get all edges with a given label.
    /// Uses the edge label index for O(1) label lookup, then resolves edge data.
    pub fn get_edges_by_label(&self, label: &str) -> Vec<Edge> {
        let edge_ids: Vec<String> = self.get_edge_ids_by_label(label);
        let edges = self.edges.read();
        edge_ids
            .iter()
            .filter_map(|id| edges.get(id).cloned())
            .collect()
    }

    /// Get the number of distinct edge labels in the graph.
    pub fn edge_label_count(&self) -> usize {
        self.edge_label_index.read().len()
    }

    // ── P0: Edge Property Index Query ─────────────────────────

    /// Get all edge IDs where a property has a specific value.
    /// Uses the edge property index for O(1) lookup.
    pub fn get_edge_ids_by_property(&self, key: &str, value: &str) -> Vec<String> {
        self.edge_prop_index
            .read()
            .get(key)
            .and_then(|val_map| val_map.get(value))
            .map(|set| set.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Get all edges where a property has a specific value.
    pub fn get_edges_by_property(&self, key: &str, value: &str) -> Vec<Edge> {
        let edge_ids = self.get_edge_ids_by_property(key, value);
        let edges = self.edges.read();
        edge_ids
            .iter()
            .filter_map(|id| edges.get(id).cloned())
            .collect()
    }

    // ── P0: Ref-based Edge Queries (avoid cloning) ────────────

    /// Get outgoing edge IDs for a vertex without cloning edge data.
    /// Returns only the edge IDs — caller can use `get_edge()` to resolve on demand.
    pub fn get_out_edge_ids(&self, vertex_id: &str) -> Vec<String> {
        let idx = match self.id_to_idx.read().get(vertex_id).copied() {
            Some(idx) => idx,
            None => return Vec::new(),
        };
        self.adj_out
            .read()
            .get(idx as usize)
            .map(|list| list.iter().map(|(_, eid)| eid.clone()).collect())
            .unwrap_or_default()
    }

    /// Get incoming edge IDs for a vertex without cloning edge data.
    pub fn get_in_edge_ids(&self, vertex_id: &str) -> Vec<String> {
        let idx = match self.id_to_idx.read().get(vertex_id).copied() {
            Some(idx) => idx,
            None => return Vec::new(),
        };
        self.adj_in
            .read()
            .get(idx as usize)
            .map(|list| list.iter().map(|(_, eid)| eid.clone()).collect())
            .unwrap_or_default()
    }

    // ── P1: Batch Vertex Lookup ───────────────────────────────

    /// Get multiple vertices by IDs in a single lock acquisition.
    /// Returns a Vec of (id, Option<Vertex>) preserving input order.
    pub fn get_vertices_batch(&self, ids: &[String]) -> Vec<(String, Option<Vertex>)> {
        let verts = self.vertices.read();
        ids.iter()
            .map(|id| (id.clone(), verts.get(id).cloned()))
            .collect()
    }

    /// Get multiple vertices by string slice IDs in a single lock acquisition.
    pub fn get_vertices_batch_ref(&self, ids: &[&str]) -> Vec<Option<Vertex>> {
        let verts = self.vertices.read();
        ids.iter().map(|id| verts.get(*id).cloned()).collect()
    }

    /// Get neighbors of a vertex (outgoing direction).
    pub fn get_neighbors(&self, vertex_id: &str) -> Vec<Vertex> {
        let idx = match self.id_to_idx.read().get(vertex_id).copied() {
            Some(idx) => idx,
            None => return Vec::new(),
        };
        let neighbor_indices: Vec<u32> = {
            let adj_out = self.adj_out.read();
            adj_out
                .get(idx as usize)
                .map(|list| list.iter().map(|(nbr, _)| *nbr).collect())
                .unwrap_or_default()
        };
        let ids = self.idx_to_id.read();
        let verts = self.vertices.read();
        neighbor_indices
            .iter()
            .filter_map(|nbr_idx| {
                ids.get(*nbr_idx as usize)
                    .and_then(|nid| verts.get(nid).cloned())
            })
            .collect()
    }

    // ── Stats ────────────────────────────────────────────────────

    /// Get vertex count.
    pub fn vertex_count(&self) -> usize {
        self.vertices.read().len()
    }

    /// Get edge count.
    pub fn edge_count(&self) -> usize {
        self.edges.read().len()
    }

    /// Get average degree.
    pub fn avg_degree(&self) -> f64 {
        let adj = self.adj_out.read();
        if adj.is_empty() {
            return 0.0;
        }
        let total: usize = adj.iter().map(|v| v.len()).sum();
        total as f64 / adj.len() as f64
    }

    /// Get integer index for a vertex ID.
    pub fn get_idx(&self, id: &str) -> Option<u32> {
        self.id_to_idx.read().get(id).copied()
    }

    /// Get vertex ID from integer index.
    pub fn get_id(&self, idx: u32) -> Option<String> {
        self.idx_to_id.read().get(idx as usize).cloned()
    }

    /// Get outgoing neighbors using integer indices (fast path).
    pub fn get_out_neighbors_idx(&self, idx: u32) -> Vec<(u32, String)> {
        self.adj_out
            .read()
            .get(idx as usize)
            .cloned()
            .unwrap_or_default()
    }

    /// Get incoming neighbors using integer indices (fast path).
    pub fn get_in_neighbors_idx(&self, idx: u32) -> Vec<(u32, String)> {
        self.adj_in
            .read()
            .get(idx as usize)
            .cloned()
            .unwrap_or_default()
    }

    /// Fast BFS using integer indices (no string allocations during traversal).
    pub fn bfs_fast(&self, start_idx: u32, max_depth: usize, direction: Direction) -> Vec<u32> {
        let num_nodes = self.idx_to_id.read().len();
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
                if (nbr as usize) < visited.len() && !visited[nbr as usize] {
                    visited[nbr as usize] = true;
                    queue.push_back((nbr, depth + 1));
                }
            }
        }

        result
    }

    /// Parallel BFS using Rayon for multi-threaded neighbor expansion.
    ///
    /// At each level, neighbors are collected and deduplicated in parallel.
    /// Best for wide graphs (many neighbors per node).
    #[cfg(feature = "enterprise")]
    pub fn bfs_parallel(&self, start_idx: u32, max_depth: usize, direction: Direction) -> Vec<u32> {
        use rayon::prelude::*;

        let num_nodes = self.idx_to_id.read().len();
        if start_idx as usize >= num_nodes {
            return Vec::new();
        }

        // Use a bitset for visited tracking (thread-safe with atomic operations)
        let num_words = (num_nodes + 63) / 64;
        let visited_bits: Vec<std::sync::atomic::AtomicU64> = (0..num_words)
            .map(|_| std::sync::atomic::AtomicU64::new(0))
            .collect();

        // Mark start as visited
        let word_idx = start_idx as usize / 64;
        let bit_idx = start_idx as usize % 64;
        visited_bits[word_idx].fetch_or(1u64 << bit_idx, std::sync::atomic::Ordering::Relaxed);

        let mut current_level = vec![start_idx];
        let mut result = Vec::new();

        for depth in 0..max_depth {
            if current_level.is_empty() {
                break;
            }

            // Collect all neighbors in parallel
            let next_level: Vec<u32> = current_level
                .par_iter()
                .flat_map(|&node| {
                    let neighbors = match direction {
                        Direction::Out => self.get_out_neighbors_idx(node),
                        Direction::In => self.get_in_neighbors_idx(node),
                        Direction::Both => {
                            let mut n = self.get_out_neighbors_idx(node);
                            n.extend(self.get_in_neighbors_idx(node));
                            n
                        }
                    };
                    neighbors.into_iter().map(|(nbr, _)| nbr).collect::<Vec<_>>()
                })
                .collect();

            // Deduplicate and mark visited (sequential for correctness)
            let mut next_unique = Vec::new();
            for nbr in next_level {
                let nbr_usize = nbr as usize;
                if nbr_usize >= num_nodes {
                    continue;
                }
                let w = nbr_usize / 64;
                let b = nbr_usize % 64;
                let mask = 1u64 << b;
                let prev = visited_bits[w].fetch_or(mask, std::sync::atomic::Ordering::Relaxed);
                if prev & mask == 0 {
                    // Was not visited before
                    next_unique.push(nbr);
                    result.push(nbr); // Always add to result (depth 0 = first neighbors)
                }
            }

            current_level = next_unique;
        }

        result
    }

    /// Parallel BFS that returns all visited nodes (including start).
    #[cfg(feature = "enterprise")]
    pub fn bfs_parallel_all(&self, start_idx: u32, max_depth: usize, direction: Direction) -> Vec<u32> {
        let mut result = vec![start_idx];
        result.extend(self.bfs_parallel(start_idx, max_depth, direction));
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
        let num_nodes = self.idx_to_id.read().len();
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
                if (nbr as usize) < visited.len() && !visited[nbr as usize] {
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
        let vertices: Vec<Vertex> = self.vertices.read().values().cloned().collect();
        let edges: Vec<Edge> = self.edges.read().values().cloned().collect();

        serde_json::to_string(&serde_json::json!({
            "vertices": vertices,
            "edges": edges,
        }))
        .unwrap_or_default()
    }

    /// Import graph data from JSON.
    pub fn import_json(&self, json: &str) -> Result<(), GraphError> {
        let data: serde_json::Value = serde_json::from_str(json)?;

        // Clear existing data
        self.vertices.write().clear();
        self.edges.write().clear();
        self.label_index.write().clear();
        self.edge_label_index.write().clear();
        self.edge_prop_index.write().clear();
        self.id_to_idx.write().clear();
        self.idx_to_id.write().clear();
        self.adj_out.write().clear();
        self.adj_in.write().clear();

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
        let json =
            std::fs::read_to_string(path).map_err(|e| GraphError::StorageError(e.to_string()))?;
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

        edges
            .iter()
            .filter(|e| edge_label.is_none_or(|l| e.label == l))
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

    // ── Graph Cache Infrastructure ─────────────────────────────

    /// Check if a relationship type is loaded and ready to use.
    pub fn is_relation_loaded(&self, relation_type: &str) -> bool {
        self.cache_state.read()
            .get(relation_type)
            .map(|e| e.status == CacheStatus::Loaded)
            .unwrap_or(false)
    }

    /// Mark a relationship type as invalidated (called on write operations).
    /// Records the invalidation timestamp for debounce.
    pub fn invalidate_relation(&self, relation_type: &str) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        {
            let mut state = self.cache_state.write();
            if let Some(entry) = state.get_mut(relation_type) {
                entry.status = CacheStatus::Invalidated;
            }
        }

        // Record invalidation timestamp for debounce
        self.last_invalidation.write().insert(relation_type.to_string(), now);

        // Track statistics
        self.cache_stats.write().invalidations += 1;
    }

    /// Check if a relation type is in the cooldown period after invalidation.
    /// Returns true if the relation was recently invalidated and shouldn't be rebuilt yet.
    pub fn is_in_cooldown(&self, relation_type: &str) -> bool {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        self.last_invalidation.read()
            .get(relation_type)
            .map(|&last| now.saturating_sub(last) < INVALIDATION_COOLDOWN_SECS)
            .unwrap_or(false)
    }

    /// Get cache status for a relationship type.
    pub fn cache_status(&self, relation_type: &str) -> CacheEntry {
        self.cache_state.read()
            .get(relation_type)
            .cloned()
            .unwrap_or_else(CacheEntry::not_loaded)
    }

    /// Get all cache entries.
    pub fn all_cache_entries(&self) -> HashMap<String, CacheEntry> {
        self.cache_state.read().clone()
    }

    /// Get cache statistics.
    pub fn cache_stats(&self) -> CacheStats {
        self.cache_stats.read().clone()
    }

    /// Record a cache hit (relation was already loaded).
    pub fn record_cache_hit(&self) {
        self.cache_stats.write().hits += 1;
    }

    /// Record a cache miss (relation needed loading).
    pub fn record_cache_miss(&self) {
        self.cache_stats.write().misses += 1;
    }

    /// Record a degraded query (fell back to JOIN).
    pub fn record_degraded_query(&self) {
        self.cache_stats.write().degraded_queries += 1;
    }

    /// Record load time in microseconds.
    pub fn record_load_time(&self, us: u64) {
        self.cache_stats.write().load_time_us += us;
    }

    /// Get a summary of all cache states for observability.
    pub fn cache_summary(&self) -> serde_json::Value {
        let entries = self.all_cache_entries();
        let stats = self.cache_stats();

        let mut relations = serde_json::Map::new();
        for (name, entry) in &entries {
            let mut rel_info = serde_json::Map::new();
            rel_info.insert("status".to_string(), serde_json::json!(format!("{:?}", entry.status)));
            rel_info.insert("edge_count".to_string(), serde_json::json!(entry.edge_count));
            rel_info.insert("memory_bytes".to_string(), serde_json::json!(entry.memory_bytes));
            rel_info.insert("loaded_at_secs".to_string(), serde_json::json!(entry.loaded_at_secs));
            relations.insert(name.clone(), serde_json::Value::Object(rel_info));
        }

        serde_json::json!({
            "relations": relations,
            "stats": {
                "hits": stats.hits,
                "misses": stats.misses,
                "invalidations": stats.invalidations,
                "loads": stats.loads,
                "load_failures": stats.load_failures,
                "load_time_us": stats.load_time_us,
                "degraded_queries": stats.degraded_queries,
                "hit_rate": if stats.hits + stats.misses > 0 {
                    stats.hits as f64 / (stats.hits + stats.misses) as f64
                } else {
                    0.0
                }
            }
        })
    }

    /// Get or create a loading lock for a relation type.
    fn get_loading_lock(&self, relation_type: &str) -> Arc<parking_lot::Mutex<()>> {
        let mut locks = self.loading_locks.lock();
        locks.entry(relation_type.to_string())
            .or_insert_with(|| Arc::new(parking_lot::Mutex::new(())))
            .clone()
    }

    /// Load a relationship type from the LSM engine into the graph store.
    /// Returns the number of edges loaded, or an error.
    ///
    /// This method:
    /// 1. Acquires a loading lock (prevents concurrent loads of same type)
    /// 2. Marks status as Loading
    /// 3. Scans the LSM for the relationship table (paged, 10K batch)
    /// 4. Creates edges in the graph
    /// 5. Marks status as Loaded (or Failed on error/timeout/OOM)
    pub fn load_relation_from_lsm(&self, relation_type: &str) -> Result<usize, GraphError> {
        let engine = self.engine.as_ref()
            .ok_or_else(|| GraphError::StorageError("No LSM engine configured".to_string()))?;

        let lock = self.get_loading_lock(relation_type);
        let _guard = lock.lock();

        // Check if already loaded (cache hit)
        if self.is_relation_loaded(relation_type) {
            self.record_cache_hit();
            return Ok(0); // Already loaded
        }

        // Cache miss - will need to load
        self.record_cache_miss();

        // Check cooldown: don't rebuild if recently invalidated
        if self.is_in_cooldown(relation_type) {
            tracing::debug!(
                relation = relation_type,
                "Skipping rebuild: in cooldown period after invalidation"
            );
            return Ok(0);
        }

        // Enforce memory limit before loading (may evict LRU)
        self.enforce_memory_limit();

        // Mark as loading
        {
            let mut state = self.cache_state.write();
            state.insert(relation_type.to_string(), CacheEntry {
                status: CacheStatus::Loading,
                ..CacheEntry::not_loaded()
            });
        }

        // Execute load with timeout protection
        let load_start = std::time::Instant::now();
        let result = self.do_load_relation(engine, relation_type, &load_start);

        match result {
            Ok((edge_count, skipped)) => {
                let elapsed = load_start.elapsed();
                self.record_load_time(elapsed.as_micros() as u64);
                self.mark_loaded(relation_type, edge_count);
                tracing::info!(
                    relation = relation_type,
                    edges = edge_count,
                    skipped = skipped,
                    elapsed_ms = elapsed.as_millis(),
                    "Loaded relationship into graph store"
                );
                Ok(edge_count)
            }
            Err(e) => {
                self.mark_failed(relation_type);
                tracing::error!(
                    relation = relation_type,
                    error = %e,
                    "Failed to load relationship into graph store"
                );
                Err(e)
            }
        }
    }

    /// Internal load implementation with paging and memory checks.
    fn do_load_relation(
        &self,
        engine: &onto_storage::LsmEngine,
        relation_type: &str,
        load_start: &std::time::Instant,
    ) -> Result<(usize, usize), GraphError> {
        // Scan LSM for the relationship table
        let prefix = format!("sembio.{}::", relation_type);
        let entries = engine.scan_prefix(prefix.as_bytes())
            .map_err(|e| GraphError::StorageError(e.to_string()))?;

        let mut edge_count = 0usize;
        let mut skipped = 0usize;
        let mut batch_count = 0usize;

        for (_key, value) in &entries {
            // Timeout check
            if load_start.elapsed().as_secs() >= LOAD_TIMEOUT_SECS {
                return Err(GraphError::StorageError(format!(
                    "Loading relation '{}' timed out after {}s (loaded {} edges so far)",
                    relation_type, LOAD_TIMEOUT_SECS, edge_count
                )));
            }

            // Memory check (rough estimate: ~200 bytes per edge)
            let estimated_memory = edge_count * 200;
            if estimated_memory >= MAX_CACHE_MEMORY_BYTES {
                return Err(GraphError::StorageError(format!(
                    "Loading relation '{}' would exceed memory limit ({} edges, ~{}MB)",
                    relation_type, edge_count, estimated_memory / (1024 * 1024)
                )));
            }

            // Parse the relationship document
            if let Some(doc) = parse_relation_doc(value) {
                let source = doc.get("source").and_then(|v| v.as_str()).unwrap_or("");
                let target = doc.get("target").and_then(|v| v.as_str()).unwrap_or("");

                if source.is_empty() || target.is_empty() {
                    skipped += 1;
                    continue;
                }

                // Ensure source and target vertices exist
                if self.get_vertex(source).is_none() {
                    let labels = extract_labels_from_id(source);
                    let _ = self.add_vertex(Vertex::new(source, labels));
                }
                if self.get_vertex(target).is_none() {
                    let labels = extract_labels_from_id(target);
                    let _ = self.add_vertex(Vertex::new(target, labels));
                }

                // Create edge
                let edge_id = format!("{}->{}::{}", source, target, relation_type);
                let edge = Edge::new(&edge_id, source, target, relation_type);
                if self.add_edge(edge).is_ok() {
                    edge_count += 1;
                    batch_count += 1;

                    // Yield every LOAD_BATCH_SIZE edges to avoid memory spikes
                    if batch_count >= LOAD_BATCH_SIZE {
                        batch_count = 0;
                        // Brief yield point - in production this would be async yield
                        tracing::debug!(
                            relation = relation_type,
                            loaded_so_far = edge_count,
                            "Loaded batch"
                        );
                    }
                }
            }
        }

        Ok((edge_count, skipped))
    }

    /// Mark a relation type as successfully loaded.
    pub fn mark_loaded(&self, relation_type: &str, edge_count: usize) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let mut state = self.cache_state.write();
        state.insert(relation_type.to_string(), CacheEntry {
            status: CacheStatus::Loaded,
            edge_count,
            memory_bytes: edge_count * 200,
            loaded_at_secs: now,
        });

        // Track statistics
        self.cache_stats.write().loads += 1;
    }

    /// Mark a relation type as failed (timeout, OOM, etc.).
    pub fn mark_failed(&self, relation_type: &str) {
        let mut state = self.cache_state.write();
        state.insert(relation_type.to_string(), CacheEntry {
            status: CacheStatus::Failed,
            ..CacheEntry::not_loaded()
        });

        // Track statistics
        self.cache_stats.write().load_failures += 1;
    }

    /// Clear all loaded edges for a relationship type.
    pub fn unload_relation(&self, relation_type: &str) {
        let edge_ids: Vec<String> = self.edges.read()
            .iter()
            .filter(|(_, e)| e.label == relation_type)
            .map(|(id, _)| id.clone())
            .collect();

        for id in edge_ids {
            let _ = self.delete_edge(&id);
        }

        let mut state = self.cache_state.write();
        state.insert(relation_type.to_string(), CacheEntry::not_loaded());
    }

    /// Evict the least recently used relation type to free memory.
    /// Returns the name of the evicted relation, or None if nothing to evict.
    pub fn evict_lru(&self) -> Option<String> {
        let state = self.cache_state.read();
        let mut oldest: Option<(&String, &CacheEntry)> = None;

        for (name, entry) in state.iter() {
            if entry.status != CacheStatus::Loaded {
                continue;
            }
            match oldest {
                None => oldest = Some((name, entry)),
                Some((_, oldest_entry)) => {
                    if entry.loaded_at_secs < oldest_entry.loaded_at_secs {
                        oldest = Some((name, entry));
                    }
                }
            }
        }

        let evict_name = oldest.map(|(name, _)| name.clone())?;
        drop(state);

        tracing::info!(relation = evict_name.as_str(), "LRU evicting relation from graph cache");
        self.unload_relation(&evict_name);
        Some(evict_name)
    }

    /// Check if total cache memory exceeds the limit and evict if needed.
    pub fn enforce_memory_limit(&self) {
        let state = self.cache_state.read();
        let total_memory: usize = state.values()
            .filter(|e| e.status == CacheStatus::Loaded)
            .map(|e| e.memory_bytes)
            .sum();
        drop(state);

        if total_memory >= MAX_CACHE_MEMORY_BYTES {
            tracing::warn!(
                total_mb = total_memory / (1024 * 1024),
                limit_mb = MAX_CACHE_MEMORY_BYTES / (1024 * 1024),
                "Graph cache memory limit exceeded, evicting LRU"
            );
            self.evict_lru();
        }
    }

    /// Ensure cache is ready for graph algorithms.
    /// If a relation type is not loaded, attempts to load it.
    /// Returns true if all specified relation types are loaded.
    pub fn ensure_cache_for_algorithm(&self, relation_types: &[&str]) -> bool {
        for &rel_type in relation_types {
            if self.is_relation_loaded(rel_type) {
                continue;
            }
            if self.is_in_cooldown(rel_type) {
                continue;
            }
            if let Err(e) = self.load_relation_from_lsm(rel_type) {
                tracing::warn!(
                    relation = rel_type,
                    error = %e,
                    "Failed to load relation for algorithm"
                );
                return false;
            }
        }
        true
    }

    /// Record an edge change for incremental cache update.
    /// Called when a relation edge is added or removed.
    pub fn record_edge_change(&self, relation_type: &str, change: EdgeChange) {
        let mut changes = self.pending_changes.write();
        changes.entry(relation_type.to_string())
            .or_default()
            .push(change);
    }

    /// Apply pending changes incrementally instead of full reload.
    /// Returns the number of changes applied.
    pub fn apply_pending_changes(&self, relation_type: &str) -> usize {
        let changes: Vec<EdgeChange> = {
            let mut pending = self.pending_changes.write();
            pending.remove(relation_type).unwrap_or_default()
        };

        if changes.is_empty() {
            return 0;
        }

        let mut applied = 0;
        for change in &changes {
            match change {
                EdgeChange::Added { source, target, edge_id } => {
                    // Ensure vertices exist
                    if self.get_vertex(source).is_none() {
                        let labels = extract_labels_from_id(source);
                        let _ = self.add_vertex(Vertex::new(source, labels));
                    }
                    if self.get_vertex(target).is_none() {
                        let labels = extract_labels_from_id(target);
                        let _ = self.add_vertex(Vertex::new(target, labels));
                    }
                    // Add edge
                    let edge = Edge::new(edge_id, source, target, relation_type);
                    if self.add_edge(edge).is_ok() {
                        applied += 1;
                    }
                }
                EdgeChange::Removed { edge_id } => {
                    if self.delete_edge(edge_id).is_ok() {
                        applied += 1;
                    }
                }
            }
        }

        // Update cache entry stats
        if applied > 0 {
            let mut state = self.cache_state.write();
            if let Some(entry) = state.get_mut(relation_type) {
                entry.edge_count = self.edges.read().values()
                    .filter(|e| e.label == relation_type)
                    .count();
                entry.memory_bytes = entry.edge_count * 200;
            }
        }

        tracing::info!(
            relation = relation_type,
            applied,
            "Applied incremental cache changes"
        );

        applied
    }

    /// Check if there are pending changes for a relation type.
    pub fn has_pending_changes(&self, relation_type: &str) -> bool {
        self.pending_changes.read()
            .get(relation_type)
            .map(|changes| !changes.is_empty())
            .unwrap_or(false)
    }

    /// Get the number of pending changes for a relation type.
    pub fn pending_changes_count(&self, relation_type: &str) -> usize {
        self.pending_changes.read()
            .get(relation_type)
            .map(|changes| changes.len())
            .unwrap_or(0)
    }
}

/// Extract labels from an EntityId string like "sembio::Drug::CHEMBL1"
fn extract_labels_from_id(id: &str) -> Vec<String> {
    let parts: Vec<&str> = id.split("::").collect();
    if parts.len() >= 2 {
        vec![parts[1].to_string()]
    } else {
        vec![]
    }
}

/// Parse a relationship document from LSM bytes.
/// Tries binary BinaryRow format first, falls back to JSON.
fn parse_relation_doc(bytes: &[u8]) -> Option<std::collections::HashMap<String, serde_json::Value>> {
    // Try binary format first
    if let Some(row) = onto_core::binary_row::BinaryRow::parse(bytes) {
        return row.to_map().map(|m| {
            m.into_iter().collect()
        });
    }
    // Fall back to JSON
    serde_json::from_slice(bytes).ok()
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
        assert_eq!(
            retrieved.properties.get("name").unwrap(),
            &PropValue::String("Alice".to_string())
        );

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

        store
            .add_vertex(Vertex::new("v1", vec!["Person".to_string()]))
            .unwrap();
        store
            .add_vertex(Vertex::new("v2", vec!["Person".to_string()]))
            .unwrap();

        let e = Edge::new("e1", "v1", "v2", "KNOWS").with_property("since", PropValue::Int(2020));

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

        store
            .add_vertex(Vertex::new("v1", vec!["Person".to_string()]))
            .unwrap();
        store
            .add_vertex(Vertex::new("v2", vec!["Person".to_string()]))
            .unwrap();
        store
            .add_vertex(Vertex::new("v3", vec!["Company".to_string()]))
            .unwrap();

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

        store
            .add_edge(Edge::new("e1", "v1", "v2", "KNOWS"))
            .unwrap();
        store
            .add_edge(Edge::new("e2", "v1", "v3", "KNOWS"))
            .unwrap();
        store
            .add_edge(Edge::new("e3", "v2", "v3", "KNOWS"))
            .unwrap();

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
        let id = EntityId::new("default", "Product", "001");

        store
            .upsert_vertex_from_entity(&id, &["Product".to_string()])
            .unwrap();

        assert!(store.has_entity(&id));
        let vertex = store.get_entity_vertex(&id).unwrap();
        assert_eq!(vertex.id, "default::Product::001");
        assert_eq!(vertex.labels, vec!["Product"]);
    }

    #[test]
    fn test_entity_upsert_idempotent() {
        let store = GraphStore::new();
        let id = EntityId::new("default", "Product", "001");

        // First insert
        store
            .upsert_vertex_from_entity(&id, &["Product".to_string()])
            .unwrap();
        // Second insert should not fail
        store
            .upsert_vertex_from_entity(&id, &["Product".to_string()])
            .unwrap();

        assert!(store.has_entity(&id));
    }

    #[test]
    fn test_entity_delete_vertex() {
        let store = GraphStore::new();
        let id = EntityId::new("default", "Product", "001");

        store
            .upsert_vertex_from_entity(&id, &["Product".to_string()])
            .unwrap();
        assert!(store.has_entity(&id));

        store.delete_vertex_by_entity(&id).unwrap();
        assert!(!store.has_entity(&id));
    }

    #[test]
    fn test_entity_delete_nonexistent() {
        let store = GraphStore::new();
        let id = EntityId::new("default", "Product", "999");

        // Should not fail
        store.delete_vertex_by_entity(&id).unwrap();
    }

    #[test]
    fn test_entity_add_relationship() {
        let store = GraphStore::new();
        let product = EntityId::new("default", "Product", "001");
        let category = EntityId::new("default", "Category", "electronics");

        store
            .add_relationship(&product, &category, "belongs_to")
            .unwrap();

        let neighbors = store.get_entity_neighbors(&product, Direction::Out, Some("belongs_to"));
        assert_eq!(neighbors.len(), 1);
        assert_eq!(neighbors[0], category);
    }

    #[test]
    fn test_entity_add_relationship_creates_vertices() {
        let store = GraphStore::new();
        let from = EntityId::new("default", "Employee", "alice");
        let to = EntityId::new("default", "Employee", "bob");

        store.add_relationship(&from, &to, "reports_to").unwrap();

        // Both vertices should be auto-created
        assert!(store.has_entity(&from));
        assert!(store.has_entity(&to));
    }

    #[test]
    fn test_entity_add_relationship_with_props() {
        let store = GraphStore::new();
        let from = EntityId::new("default", "Person", "alice");
        let to = EntityId::new("default", "Person", "bob");

        let mut props = PropertyMap::new();
        props.insert("since".to_string(), PropValue::Int(2020));

        store
            .add_relationship_with_props(&from, &to, "knows", props)
            .unwrap();

        let neighbors = store.get_entity_neighbors(&from, Direction::Out, Some("knows"));
        assert_eq!(neighbors.len(), 1);
        assert_eq!(neighbors[0], to);
    }

    #[test]
    fn test_entity_delete_relationship() {
        let store = GraphStore::new();
        let from = EntityId::new("default", "Product", "001");
        let to = EntityId::new("default", "Category", "electronics");

        store.add_relationship(&from, &to, "belongs_to").unwrap();
        assert_eq!(store.edge_count(), 1);

        store.delete_relationship(&from, &to, "belongs_to").unwrap();
        assert_eq!(store.edge_count(), 0);
    }

    #[test]
    fn test_entity_get_neighbors_both_directions() {
        let store = GraphStore::new();
        let alice = EntityId::new("default", "Person", "alice");
        let bob = EntityId::new("default", "Person", "bob");

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
        let alice = EntityId::new("default", "Person", "alice");
        let bob = EntityId::new("default", "Person", "bob");
        let company = EntityId::new("default", "Company", "acme");

        store.add_relationship(&alice, &bob, "knows").unwrap();
        store
            .add_relationship(&alice, &company, "works_at")
            .unwrap();

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

        store
            .upsert_vertex_from_entity(
                &EntityId::new("default", "Product", "001"),
                &["Product".to_string()],
            )
            .unwrap();
        store
            .upsert_vertex_from_entity(
                &EntityId::new("default", "Product", "002"),
                &["Product".to_string()],
            )
            .unwrap();
        store
            .upsert_vertex_from_entity(
                &EntityId::new("default", "Category", "electronics"),
                &["Category".to_string()],
            )
            .unwrap();

        let products = store.get_entities_by_class("Product");
        assert_eq!(products.len(), 2);

        let categories = store.get_entities_by_class("Category");
        assert_eq!(categories.len(), 1);
    }

    // ── Persistence Tests ──

    #[test]
    fn test_graph_persistence() {
        use onto_storage::{LsmEngine, StorageOptions};
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            memtable_size_limit: 4 * 1024 * 1024,
            ..Default::default()
        };
        let engine = Arc::new(LsmEngine::open(options).unwrap());

        // Create graph and add data
        {
            let store = GraphStore::with_engine(engine.clone());
            store
                .add_vertex(Vertex::new("v1", vec!["Person".to_string()]))
                .unwrap();
            store
                .add_vertex(Vertex::new("v2", vec!["Person".to_string()]))
                .unwrap();
            store
                .add_edge(Edge::new("e1", "v1", "v2", "knows"))
                .unwrap();
        }

        // Reload from engine
        {
            let store = GraphStore::with_engine(engine.clone());
            store.load_from_engine().unwrap();

            // Verify vertices
            assert!(store.get_vertex("v1").is_some());
            assert!(store.get_vertex("v2").is_some());
            assert_eq!(store.vertex_count(), 2);

            // Verify edges
            assert!(store.get_edge("e1").is_some());
            assert_eq!(store.edge_count(), 1);

            // Verify adjacency
            let out = store.get_out_edges("v1");
            assert_eq!(out.len(), 1);
            assert_eq!(out[0].id, "e1");
        }
    }

    #[test]
    fn test_graph_persistence_with_entity_id() {
        use onto_storage::{LsmEngine, StorageOptions};
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            memtable_size_limit: 4 * 1024 * 1024,
            ..Default::default()
        };
        let engine = Arc::new(LsmEngine::open(options).unwrap());

        let product = EntityId::new("default", "Product", "001");
        let category = EntityId::new("default", "Category", "electronics");

        // Create graph and add relationships
        {
            let store = GraphStore::with_engine(engine.clone());
            store
                .add_relationship(&product, &category, "belongs_to")
                .unwrap();
        }

        // Reload from engine
        {
            let store = GraphStore::with_engine(engine.clone());
            store.load_from_engine().unwrap();

            // Verify entity exists
            assert!(store.has_entity(&product));
            assert!(store.has_entity(&category));

            // Verify relationship
            let neighbors =
                store.get_entity_neighbors(&product, Direction::Out, Some("belongs_to"));
            assert_eq!(neighbors.len(), 1);
            assert_eq!(neighbors[0], category);
        }
    }

    #[test]
    fn test_graph_persistence_delete() {
        use onto_storage::{LsmEngine, StorageOptions};
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            memtable_size_limit: 4 * 1024 * 1024,
            ..Default::default()
        };
        let engine = Arc::new(LsmEngine::open(options).unwrap());

        // Create, then delete
        {
            let store = GraphStore::with_engine(engine.clone());
            store
                .add_vertex(Vertex::new("v1", vec!["Person".to_string()]))
                .unwrap();
            store
                .add_vertex(Vertex::new("v2", vec!["Person".to_string()]))
                .unwrap();
            store
                .add_edge(Edge::new("e1", "v1", "v2", "knows"))
                .unwrap();

            // Delete v1 (should cascade e1)
            store.delete_vertex("v1").unwrap();
        }

        // Reload - v1 and e1 should be gone, v2 should remain
        {
            let store = GraphStore::with_engine(engine.clone());
            store.load_from_engine().unwrap();

            assert!(store.get_vertex("v1").is_none());
            assert!(store.get_vertex("v2").is_some());
            assert_eq!(store.vertex_count(), 1);
            assert_eq!(store.edge_count(), 0);
        }
    }

    // ── Cache Infrastructure Tests ──

    #[test]
    fn test_cache_state_initial() {
        let store = GraphStore::new();
        assert!(!store.is_relation_loaded("treats"));
        assert_eq!(store.cache_status("treats").status, CacheStatus::NotLoaded);
    }

    #[test]
    fn test_invalidate_relation() {
        let store = GraphStore::new();
        // Manually set to loaded via cache_state
        store.cache_state.write().insert("treats".to_string(), CacheEntry {
            status: CacheStatus::Loaded,
            edge_count: 100,
            ..CacheEntry::not_loaded()
        });
        assert!(store.is_relation_loaded("treats"));
        store.invalidate_relation("treats");
        assert!(!store.is_relation_loaded("treats"));
        assert_eq!(store.cache_status("treats").status, CacheStatus::Invalidated);
    }

    #[test]
    fn test_unload_relation() {
        let store = GraphStore::new();
        store.add_vertex(Vertex::new("a", vec![])).unwrap();
        store.add_vertex(Vertex::new("b", vec![])).unwrap();
        store.add_edge(Edge::new("e1", "a", "b", "treats")).unwrap();
        assert_eq!(store.edge_count(), 1);
        store.unload_relation("treats");
        assert_eq!(store.edge_count(), 0);
    }

    #[test]
    fn test_all_cache_entries() {
        let store = GraphStore::new();
        let entries = store.all_cache_entries();
        assert!(entries.is_empty());
    }

    #[test]
    fn test_cache_status_unknown() {
        let store = GraphStore::new();
        let entry = store.cache_status("nonexistent");
        assert_eq!(entry.status, CacheStatus::NotLoaded);
    }

    #[test]
    fn test_mark_loaded() {
        let store = GraphStore::new();
        store.mark_loaded("treats", 500);
        assert!(store.is_relation_loaded("treats"));
        let entry = store.cache_status("treats");
        assert_eq!(entry.status, CacheStatus::Loaded);
        assert_eq!(entry.edge_count, 500);
        assert_eq!(entry.memory_bytes, 500 * 200);
        assert!(entry.loaded_at_secs > 0);
    }

    #[test]
    fn test_mark_failed() {
        let store = GraphStore::new();
        store.mark_failed("treats");
        assert!(!store.is_relation_loaded("treats"));
        let entry = store.cache_status("treats");
        assert_eq!(entry.status, CacheStatus::Failed);
    }

    #[test]
    fn test_cache_state_transitions() {
        let store = GraphStore::new();

        // NotLoaded → Loaded
        assert_eq!(store.cache_status("treats").status, CacheStatus::NotLoaded);
        store.mark_loaded("treats", 100);
        assert_eq!(store.cache_status("treats").status, CacheStatus::Loaded);

        // Loaded → Invalidated
        store.invalidate_relation("treats");
        assert_eq!(store.cache_status("treats").status, CacheStatus::Invalidated);

        // Invalidated → Loaded (re-load)
        store.mark_loaded("treats", 200);
        assert_eq!(store.cache_status("treats").status, CacheStatus::Loaded);
        assert_eq!(store.cache_status("treats").edge_count, 200);
    }

    #[test]
    fn test_loading_lock_prevents_concurrent_load() {
        let store = GraphStore::new();
        let lock1 = store.get_loading_lock("treats");
        let lock2 = store.get_loading_lock("treats");

        // Same Arc means same lock
        assert!(Arc::ptr_eq(&lock1, &lock2));

        // Different relation types get different locks
        let lock3 = store.get_loading_lock("causes");
        assert!(!Arc::ptr_eq(&lock1, &lock3));
    }

    #[test]
    fn test_debounce_cooldown() {
        let store = GraphStore::new();

        // Initially not in cooldown
        assert!(!store.is_in_cooldown("treats"));

        // After invalidation, should be in cooldown
        store.invalidate_relation("treats");
        assert!(store.is_in_cooldown("treats"));

        // Different relation type not affected
        assert!(!store.is_in_cooldown("causes"));
    }

    #[test]
    fn test_cache_stats_tracking() {
        let store = GraphStore::new();

        // Initial stats should be zero
        let stats = store.cache_stats();
        assert_eq!(stats.hits, 0);
        assert_eq!(stats.misses, 0);
        assert_eq!(stats.invalidations, 0);

        // Track some operations
        store.record_cache_hit();
        store.record_cache_hit();
        store.record_cache_miss();
        store.invalidate_relation("treats");

        let stats = store.cache_stats();
        assert_eq!(stats.hits, 2);
        assert_eq!(stats.misses, 1);
        assert_eq!(stats.invalidations, 1);
    }

    #[test]
    fn test_cache_summary() {
        let store = GraphStore::new();
        store.mark_loaded("treats", 100);
        store.record_cache_hit();
        store.record_cache_miss();

        let summary = store.cache_summary();
        assert!(summary.get("relations").is_some());
        assert!(summary.get("stats").is_some());

        let stats = summary.get("stats").unwrap();
        assert_eq!(stats.get("hits").unwrap().as_u64().unwrap(), 1);
        assert_eq!(stats.get("misses").unwrap().as_u64().unwrap(), 1);
    }

    #[test]
    fn test_lru_eviction() {
        let store = GraphStore::new();
        store.add_vertex(Vertex::new("a", vec![])).unwrap();
        store.add_vertex(Vertex::new("b", vec![])).unwrap();
        store.add_edge(Edge::new("e1", "a", "b", "treats")).unwrap();

        store.mark_loaded("treats", 1);
        assert!(store.is_relation_loaded("treats"));

        let evicted = store.evict_lru();
        assert_eq!(evicted.unwrap(), "treats");
        assert!(!store.is_relation_loaded("treats"));
        assert_eq!(store.edge_count(), 0);
    }

    #[test]
    fn test_lru_eviction_empty() {
        let store = GraphStore::new();
        let evicted = store.evict_lru();
        assert!(evicted.is_none());
    }

    #[test]
    fn test_ensure_cache_for_algorithm() {
        let store = GraphStore::new();
        // No engine, so load will fail - but ensure_cache_for_algorithm should handle it gracefully
        let ready = store.ensure_cache_for_algorithm(&["treats"]);
        assert!(!ready); // Should fail without engine
    }

    #[test]
    fn test_degraded_query_tracking() {
        let store = GraphStore::new();
        assert_eq!(store.cache_stats().degraded_queries, 0);

        store.record_degraded_query();
        store.record_degraded_query();
        assert_eq!(store.cache_stats().degraded_queries, 2);
    }
}
