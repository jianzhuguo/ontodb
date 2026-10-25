// Copyright (c) 2024-2026 OntoDB Team
// Licensed under the Business Source License 1.1 (BUSL-1.1).
// See LICENSE for details. Change Date: 2031-09-15.
// On the Change Date, this file will be licensed under Apache License 2.0.
//! LSM Engine: The main storage engine that orchestrates WAL, MemTable, and SSTables.
//!
//! Write path:  WAL -> MemTable -> (when full) flush to SSTable
//! Read path:   MemTable -> SSTables (newest to oldest)
//! Delete:      Write tombstone entry
//!
//! Compaction runs in a background thread to avoid blocking the write path.
//! Level metadata is shared between the engine and the compaction worker via
//! `Arc<Mutex>`. The engine drains compaction notifications before reads to
//! invalidate stale SSTable cache entries.

use crate::index::IndexManager;
use crate::lsm::compaction_worker::{
    CompactionMsg, CompactionNotification, CompactionWorker, SsTableInfo,
};
use crate::lsm::memtable::MemTable;
use crate::lsm::sstable::{SsTable, SsTableBuilder};
use crate::lsm::wal::{self, Wal};
use crate::mvcc::{TxnManager, WriteOp};
use crate::options::StorageOptions;
use crate::vector::{VecData, VectorIndexManager};
use onto_core::binary_row::BinaryRow;
use onto_core::{Entry, EntryKind, Key, Result, SeqNo, Value};
use std::collections::HashMap;
use std::fs;

/// Parse storage bytes to a serde_json::Map. Tries binary format first, then JSON.
fn parse_doc_bytes(bytes: &[u8]) -> Option<serde_json::Map<String, serde_json::Value>> {
    if let Some(row) = BinaryRow::parse(bytes) {
        return row.to_map();
    }
    match serde_json::from_slice::<serde_json::Value>(bytes) {
        Ok(serde_json::Value::Object(doc)) => Some(doc),
        _ => None,
    }
}

/// Fast path for CREATE INDEX backfill: pull one column (+ class check) without
/// materializing the full document map. `to_map()` on 600k wide rows dominates
/// backfill time; `get_value` touches only the target field.
fn extract_index_field(
    bytes: &[u8],
    class: &str,
    column: &str,
) -> Option<serde_json::Value> {
    if let Some(row) = BinaryRow::parse(bytes) {
        if row.class_value() != Some(class) {
            return None;
        }
        return row.get_value(column);
    }
    match serde_json::from_slice::<serde_json::Value>(bytes) {
        Ok(serde_json::Value::Object(doc)) => {
            if doc.get("__class__").and_then(|v| v.as_str()) != Some(class) {
                return None;
            }
            doc.get(column).cloned()
        }
        _ => None,
    }
}
use parking_lot::RwLock as FairRwLock;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, RwLock};
use std::thread::JoinHandle;

/// Mutable write-path state, protected by RwLock for concurrent read access.
///
/// Read-path operations (get, scan_prefix) acquire a shared read lock.
/// Write-path operations (put, delete, flush) acquire an exclusive write lock.
/// SST cache is separated into its own Mutex because read-path code may
/// lazily open SSTable files (cache miss).
struct WriteState {
    /// Active MemTable for writes.
    memtable: MemTable,

    /// Read-only MemTable waiting to be flushed.
    immutable_memtable: Option<MemTable>,

    /// Write-Ahead Log for durability.
    wal: Wal,

    /// MVCC transaction manager.
    txn_manager: TxnManager,

    /// Number of appends since last WAL flush. Batched to reduce syscalls.
    wal_pending_count: u32,
}

/// Result of a single `get_once` attempt.
enum GetOutcome {
    /// Live Put found.
    Hit(Value),
    /// Tombstone or memtable delete — key is gone.
    Deleted,
    /// No version anywhere.
    Absent,
    /// Candidate SST was compacted away mid-lookup; retry against the
    /// post-compaction `levels` snapshot.
    StaleSst,
}

/// The main LSM-Tree storage engine with MVCC support.
///
/// All methods take `&self` — the engine is `Sync` and can be shared via
/// `Arc<LsmEngine>` without an outer `RwLock`.  Write-path state lives
/// behind `RwLock<WriteState>` for concurrent read access; SST cache
/// behind a separate `Mutex` for lazy loading; index managers behind `RwLock`.
pub struct LsmEngine {
    /// Write-path state (memtable, WAL, txn manager).
    /// Read lock for get/scan, write lock for put/delete/flush.
    /// Uses parking_lot::RwLock for FIFO fairness — prevents writer starvation
    /// when multiple readers hold concurrent read locks.
    /// Wrapped in Arc for sharing with background WAL sync thread.
    write_state: Arc<FairRwLock<WriteState>>,

    /// SSTable handle cache (separate Mutex — read path may lazily open files).
    sst_cache: Mutex<HashMap<PathBuf, Arc<SsTable>>>,

    /// SSTables organized by level. Level 0 is newest.
    /// Shared with the background compaction worker via Arc<Mutex>.
    levels: Arc<Mutex<Vec<Vec<SsTableInfo>>>>,

    /// Engine configuration.
    options: StorageOptions,

    /// Sequence number generator.
    seq_counter: AtomicU64,

    /// SSTable file ID counter (shared with compaction worker).
    sst_counter: Arc<AtomicU64>,

    /// Secondary index manager (RwLock for concurrent read-path access).
    index_manager: RwLock<IndexManager>,

    /// Index creation progress: (total_entries, processed_entries, class.column)
    /// When no index creation is in progress, total = 0.
    index_progress: Arc<std::sync::Mutex<Option<(String, usize, usize)>>>,

    /// Vector index manager — HNSW (RwLock for concurrent read-path access).
    vector_index_manager: RwLock<VectorIndexManager>,

    /// Channel to send flush notifications to the background compaction worker.
    compaction_sender: mpsc::Sender<CompactionMsg>,

    /// Channel to receive compaction notifications (for cache invalidation).
    /// Wrapped in Mutex because mpsc::Receiver is not Sync.
    compaction_notif_receiver: std::sync::Mutex<mpsc::Receiver<CompactionNotification>>,

    /// Fast flag: set by the notification drain when a compaction notification arrives.
    /// Cleared after the sst_cache is invalidated.
    /// Allows the common path (no compaction) to skip sst_cache lock acquisition.
    compaction_pending: AtomicBool,

    /// Handle to the background compaction worker thread for graceful shutdown.
    #[allow(dead_code)]
    worker_handle: Mutex<Option<JoinHandle<()>>>,

    /// Group commit coordinator for batching WAL syncs.
    group_commit: Arc<crate::lsm::group_commit::GroupCommitCoordinator>,

    /// Dynamic memory manager for adaptive MemTable and Block Cache sizing.
    memory_manager: Arc<crate::lsm::memory_manager::MemoryManager>,

    /// Shutdown flag for background threads.
    shutdown: Arc<AtomicBool>,

    /// Handle to the background WAL sync thread for graceful shutdown.
    wal_sync_handle: Mutex<Option<JoinHandle<()>>>,

    /// Serializes flush_memtable. Without it, two concurrent flushes both swap
    /// into the single `immutable_memtable` slot and the loser's entries are
    /// silently dropped (observed as lost UPDATEs/DELETEs under bulk load).
    flush_lock: Mutex<()>,
}

/// Temporary helper for loading WAL + SSTables before spawning the compaction worker.
struct PreLoadEngine {
    write_state: WriteState,
    sst_cache: HashMap<PathBuf, Arc<SsTable>>,
    levels: Vec<Vec<SsTableInfo>>,
    options: StorageOptions,
    seq_counter: AtomicU64,
    sst_counter: Arc<AtomicU64>,
}

impl PreLoadEngine {
    fn recover(&mut self) -> Result<()> {
        let wal_path = self.options.data_dir.join("wal.log");
        if !wal_path.exists() {
            return Ok(());
        }
        let entries = wal::replay_wal(&wal_path)?;
        let mut max_seq = 0u64;
        let mut skipped_index_entries = 0u64;
        for entry in entries {
            // Skip index Puts during WAL replay — indexes are rebuilt
            // separately by rebuild_indexes() after the engine is loaded.
            // Delete tombstones for index keys MUST be applied: DROP INDEX
            // relies on them to suppress rebuild from leftover index entries.
            if (entry.key.starts_with(b"__idx2__") || entry.key.starts_with(b"__idx__"))
                && entry.kind == EntryKind::Put
            {
                skipped_index_entries += 1;
                max_seq = max_seq.max(entry.seq_no);
                continue;
            }
            match entry.kind {
                EntryKind::Put => {
                    self.write_state
                        .memtable
                        .put_with_seq(entry.key, entry.value, entry.seq_no)
                }
                EntryKind::Delete => self
                    .write_state
                    .memtable
                    .delete_with_seq(entry.key, entry.seq_no),
            }
            max_seq = max_seq.max(entry.seq_no);
        }
        if skipped_index_entries > 0 {
            tracing::info!(
                skipped_index_entries,
                "Skipped index entries during WAL replay (will be rebuilt)"
            );
        }
        self.seq_counter = AtomicU64::new(max_seq + 1);
        Ok(())
    }

    fn load_sstables(&mut self) -> Result<Vec<PathBuf>> {
        let dir_entries = fs::read_dir(&self.options.data_dir)?;
        let mut sst_files: Vec<PathBuf> = Vec::new();
        for entry in dir_entries {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "sst") {
                sst_files.push(path);
            }
        }
        sst_files.sort();
        // Use max_seq from WAL replay AND scan SST entries for their max seq_no
        let mut max_seq = self.seq_counter.load(Ordering::Relaxed);
        let mut old_ssts: Vec<PathBuf> = Vec::new();
        for path in sst_files {
            let fname = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            let level = fname
                .strip_prefix('L')
                .and_then(|s| s.split('_').next())
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(0);
            if level >= self.levels.len() {
                continue;
            }
            let sst = Arc::new(SsTable::open(&path)?);
            // Use max_seq_no from SST footer (O(1)) for new files,
            // fall back to scanning for old files without the footer.
            let sst_max_seq = sst.max_seq_no();
            if sst_max_seq == 0 {
                // Old format: scan entries to find max seq_no
                old_ssts.push(path.clone());
                if let Ok(iter) = sst.iter() {
                    let mut sst_iter = iter;
                    while sst_iter.is_valid() {
                        max_seq = max_seq.max(sst_iter.seq_no());
                        sst_iter.next();
                    }
                }
            } else {
                max_seq = max_seq.max(sst_max_seq);
            }
            let min_key = sst.first_key().unwrap_or_default();
            let max_key = sst.max_key().to_vec();
            let metadata = fs::metadata(&path)?;
            if let Some(id_str) = fname.split('_').nth(1) {
                if let Ok(id) = id_str.parse::<u64>() {
                    let current = self.sst_counter.load(Ordering::Relaxed);
                    if id >= current {
                        self.sst_counter.store(id + 1, Ordering::Relaxed);
                    }
                }
            }
            self.sst_cache.insert(path.clone(), sst);
            self.levels[level].push(SsTableInfo {
                path,
                size: metadata.len(),
                min_key,
                max_key,
            });
        }
        self.seq_counter.store(max_seq, Ordering::Relaxed);
        Ok(old_ssts)
    }
}

impl LsmEngine {
    /// Removes orphaned `.sst.tmp` files left by interrupted compaction/flush.
    /// These are incomplete writes that were not atomically renamed to `.sst`.
    fn cleanup_tmp_files(data_dir: &std::path::Path) -> Result<()> {
        let mut cleaned = 0usize;
        for entry in fs::read_dir(data_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().map_or(false, |ext| ext == "tmp")
                && path.file_name().map_or(false, |name| {
                    name.to_string_lossy().ends_with(".sst.tmp")
                })
            {
                if let Err(e) = fs::remove_file(&path) {
                    tracing::warn!("Failed to remove orphaned tmp file {:?}: {}", path, e);
                } else {
                    cleaned += 1;
                }
            }
        }
        if cleaned > 0 {
            tracing::info!(count = cleaned, "Cleaned up orphaned .sst.tmp files");
        }
        Ok(())
    }

    /// Opens or creates an LSM engine at the given directory.
    pub fn open(options: StorageOptions) -> Result<Self> {
        fs::create_dir_all(&options.data_dir)?;

        // Clean up orphaned .sst.tmp files from interrupted compaction/flush.
        // These are incomplete writes that were not atomically renamed.
        Self::cleanup_tmp_files(&options.data_dir)?;

        let wal_path = options.data_dir.join("wal.log");
        let wal = Wal::open(&wal_path)?;

        let sst_counter = Arc::new(AtomicU64::new(0));

        // Pre-load existing SSTables into a temporary Vec before spawning the worker
        let mut pre_engine = PreLoadEngine {
            write_state: WriteState {
                memtable: MemTable::new(),
                immutable_memtable: None,
                wal,
                txn_manager: TxnManager::new(),
                wal_pending_count: 0,
            },
            sst_cache: HashMap::new(),
            levels: vec![Vec::new(); options.num_levels],
            options: options.clone(),
            seq_counter: AtomicU64::new(0),
            sst_counter: sst_counter.clone(),
        };
        pre_engine.recover()?;
        let old_ssts = pre_engine.load_sstables()?;

        // Spawn background footer upgrade for old SSTs (one-time migration)
        if !old_ssts.is_empty() {
            let count = old_ssts.len();
            tracing::info!("Starting background footer upgrade for {} old SSTs", count);
            std::thread::Builder::new()
                .name("sst-footer-upgrade".into())
                .spawn(move || {
                    let mut upgraded = 0usize;
                    for path in &old_ssts {
                        match SsTable::upgrade_footer(path) {
                            Ok(seq) if seq > 0 => {
                                upgraded += 1;
                                if upgraded % 50 == 0 {
                                    tracing::info!("Footer upgrade progress: {}/{}", upgraded, count);
                                }
                            }
                            Ok(_) => {}
                            Err(e) => {
                                tracing::warn!("Footer upgrade failed for {:?}: {}", path, e);
                            }
                        }
                    }
                    tracing::info!("Footer upgrade complete: {}/{} SSTs upgraded", upgraded, count);
                })
                .ok();
        }

        // Spawn the background compaction worker with the loaded levels
        let (levels, compaction_sender, compaction_notif_receiver, _worker_handle) =
            CompactionWorker::spawn(options.clone(), pre_engine.levels, sst_counter.clone());

        // Create index manager with disk storage if configured
        // NOTE: disk indexes are opened in the background to avoid blocking startup
        let index_manager = match &options.index_storage_mode {
            None | Some(crate::index::IndexStorageMode::InMemory) => IndexManager::new(),
            Some(mode) => {
                let idx_dir = options.data_dir.join("indexes");
                fs::create_dir_all(&idx_dir)?;
                IndexManager::with_disk_storage(&idx_dir, mode.clone())
                // open_disk_indexes() is deferred to background thread below
            }
        };

        // Initialize memory manager for adaptive sizing
        let mm_config = crate::lsm::memory_manager::MemoryManagerConfig::default();
        let memory_manager = Arc::new(
            crate::lsm::memory_manager::MemoryManager::with_initial_sizes(
                mm_config,
                options.memtable_size_limit,
                64 * 1024 * 1024, // 64 MB default block cache
            ),
        );

        let engine = LsmEngine {
            write_state: Arc::new(FairRwLock::new(pre_engine.write_state)),
            sst_cache: Mutex::new(pre_engine.sst_cache),
            levels,
            options,
            seq_counter: pre_engine.seq_counter,
            sst_counter,
            index_manager: RwLock::new(index_manager),
            index_progress: Arc::new(std::sync::Mutex::new(None)),
            vector_index_manager: RwLock::new(VectorIndexManager::new()),
            compaction_sender,
            compaction_notif_receiver: Mutex::new(compaction_notif_receiver),
            compaction_pending: AtomicBool::new(false),
            worker_handle: Mutex::new(Some(_worker_handle)),
            group_commit: Arc::new(crate::lsm::group_commit::GroupCommitCoordinator::new()),
            memory_manager,
            shutdown: Arc::new(AtomicBool::new(false)),
            wal_sync_handle: Mutex::new(None),
            flush_lock: Mutex::new(()),
        };

        // Spawn background WAL sync thread
        // Every 100ms, flush WAL buffer to OS cache and fsync if dirty
        // This ensures single put() calls are persisted within 100ms
        let sync_write_state = engine.write_state.clone();
        let sync_wal_on_commit = engine.options.sync_wal_on_commit;
        let sync_shutdown = engine.shutdown.clone();
        let sync_handle = std::thread::Builder::new()
            .name("ontodb-wal-sync".into())
            .spawn(move || {
                while !sync_shutdown.load(Ordering::Relaxed) {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    if let Some(mut ws) =
                        sync_write_state.try_write_for(std::time::Duration::from_millis(5))
                    {
                        if ws.wal_pending_count > 0 {
                            let _ = ws.wal.flush_buf();
                            ws.wal_pending_count = 0;
                            if sync_wal_on_commit {
                                let _ = ws.wal.sync();
                            }
                        } else if sync_wal_on_commit {
                            let _ = ws.wal.sync_if_dirty();
                        }
                    }
                }
            })
            .ok();
        // Save the handle for graceful shutdown
        if let Some(h) = sync_handle {
            *engine
                .wal_sync_handle
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(h);
        }

        // Index rebuild is deferred to on-demand or background thread by caller.
        // This keeps startup fast (~1s) even with millions of index entries.
        // Indexes will be rebuilt on first query that needs them.

        Ok(engine)
    }

    /// Returns the data directory path.
    pub fn data_dir(&self) -> PathBuf {
        self.options.data_dir.clone()
    }

    /// Returns a reference to the storage options.
    pub fn options(&self) -> &StorageOptions {
        &self.options
    }

    /// Gets the memory manager for monitoring and control.
    pub fn memory_manager(&self) -> &Arc<crate::lsm::memory_manager::MemoryManager> {
        &self.memory_manager
    }

    /// Gracefully shuts down the engine, stopping background threads.
    pub fn shutdown(&self) {
        // Signal background threads to stop
        self.shutdown.store(true, Ordering::Relaxed);

        // Wait for WAL sync thread to finish
        if let Some(handle) = self
            .wal_sync_handle
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            let _ = handle.join();
        }

        // Persist any dirty HNSW nodes before exiting so in-flight vector
        // inserts land in the file snapshot (documents remain the recovery
        // source of truth either way).
        if let Err(e) = self.persist_vector_deltas(true) {
            tracing::warn!("Failed to persist vector snapshot on shutdown: {}", e);
        }

        // Flush any remaining data
        if let Some(mut ws) = self
            .write_state
            .try_write_for(std::time::Duration::from_secs(1))
        {
            if ws.wal_pending_count > 0 {
                let _ = ws.wal.flush_buf();
                let _ = ws.wal.sync();
            }
        }
    }

    /// Persist dirty HNSW nodes to the file snapshot when needed.
    ///
    /// Incremental `__vec_delta__` writes are gone: restore never read them
    /// (file snapshot + document backfill is the only recovery path), and the
    /// single-key delta was overwritten every flush — a fake backup. New
    /// vectors stay in memory until the dirty set is large enough to justify
    /// a full snapshot, or the caller forces one (graceful shutdown).
    ///
    /// Whatever is not snapshotted is recovered on the next restart by
    /// `reconcile_vector_index` + `backfill_vector_missing` from the
    /// document's vector column (e.g. `fp`) — the source of truth.
    pub fn persist_vector_deltas(&self, force: bool) -> Result<()> {
        let dirty: Vec<(String, String)> = {
            let mgr = self
                .vector_index_manager
                .read()
                .unwrap_or_else(|e| e.into_inner());
            mgr.list_indexes()
                .iter()
                .filter(|m| mgr.has_dirty_nodes(&m.class, &m.column))
                .map(|m| (m.class.clone(), m.column.clone()))
                .collect()
        };
        if dirty.is_empty() {
            return Ok(());
        }
        for (class, column) in dirty {
            let dirty_count = {
                let mgr = self
                    .vector_index_manager
                    .read()
                    .unwrap_or_else(|e| e.into_inner());
                mgr.dirty_count(&class, &column)
            };
            // Full snapshot only when it earns its keep: a large dirty set
            // (bulk insert) or an explicit force (shutdown). Small incremental
            // bursts are left in memory and rebuilt from documents on restart.
            if dirty_count > 20_000 || force {
                if let Err(e) = self.persist_vector_graph(&class, &column) {
                    tracing::warn!(
                        table = %class,
                        column = %column,
                        dirty = dirty_count,
                        error = %e,
                        "vector snapshot failed; nodes recover from documents on restart"
                    );
                } else {
                    tracing::info!(
                        table = %class,
                        column = %column,
                        dirty = dirty_count,
                        "vector snapshot written"
                    );
                }
            }
        }
        Ok(())
    }

    /// Tombstones any legacy `__vec_delta__` key after a full snapshot so a
    /// stale delta left over from the old write-only path cannot confuse
    /// future tooling. New deltas are no longer written.
    fn clear_vector_delta(&self, class: &str, column: &str) {
        let key = format!("__vec_delta__{}_{}", class, column).into_bytes();
        let seq = self.next_seq();
        let entry = Entry::delete(key.clone(), seq);
        if let Some(mut ws) = self.write_state.try_write_for(std::time::Duration::from_secs(1)) {
            let _ = ws.wal.append(&entry);
            let _ = ws.wal.flush_buf();
            ws.memtable.delete_with_seq(key, seq);
        }
    }

    /// Persists the current in-memory HNSW graph as a snapshot file
    /// (`indexes/{class}_{column}.hnsw`) and clears any incremental delta.
    ///
    /// File-based on purpose: a 1024-dim graph for millions of vectors is
    /// hundreds of MB — far beyond a safe single LSM value (SST block limit),
    /// which corrupts the read path.  Must be called after bulk backfill so
    /// the graph survives restarts.
    pub fn persist_vector_graph(&self, class: &str, column: &str) -> Result<()> {
        let graph_bytes = self
            .vector_index_manager
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .save_graph(class, column);
        if let Some(graph_bytes) = graph_bytes {
            let idx_dir = self.options.data_dir.join("indexes");
            if let Err(e) = std::fs::create_dir_all(&idx_dir) {
                tracing::warn!("Failed to create indexes dir: {}", e);
                return Ok(());
            }
            let path = idx_dir.join(format!("{}_{}.hnsw", class, column));
            // Write to a temp file then rename so a crash cannot leave a torn file.
            let tmp = path.with_extension("hnsw.tmp");
            if let Err(e) = std::fs::write(&tmp, &graph_bytes) {
                tracing::warn!(
                    table = %class,
                    column = %column,
                    error = %e,
                    "Failed to write HNSW snapshot file"
                );
                return Ok(());
            }
            if let Err(e) = std::fs::rename(&tmp, &path) {
                tracing::warn!(
                    table = %class,
                    column = %column,
                    error = %e,
                    "Failed to finalize HNSW snapshot file"
                );
                return Ok(());
            }
            self.clear_vector_delta(class, column);
            // Snapshot file now holds every node — drop dirty marks so
            // persist_vector_deltas does not re-attempt (and warn) forever.
            self.vector_index_manager
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .clear_dirty(class, column);
            tracing::info!(
                table = %class,
                column = %column,
                bytes = graph_bytes.len(),
                path = %path.display(),
                "Persisted HNSW graph snapshot"
            );
        }
        Ok(())
    }

    /// Loads a persisted HNSW snapshot file if present.
    pub fn load_vector_graph_file(&self, class: &str, column: &str) -> Option<Vec<u8>> {
        let path = self
            .options
            .data_dir
            .join("indexes")
            .join(format!("{}_{}.hnsw", class, column));
        std::fs::read(&path).ok()
    }

    /// Adds document `column` vectors that are not already in the HNSW graph.
    /// Returns how many vectors were added. Used after a snapshot restore so a
    /// stale snapshot (written before more rows gained the fp field) does not
    /// require a full rebuild.
    /// Backfills missing vectors AND reconciles the restored graph against
    /// live documents.
    ///
    /// `deleted_keys` / `doc_vectors` are in-memory only and do not survive in
    /// the HNSW snapshot. After a restore the graph therefore still contains
    /// nodes for documents whose `fp` was later cleared (or that were deleted)
    /// — those are the "ghost" vectors. This pass rebuilds `doc_vectors` for
    /// live rows and marks every graph node without a live vector as deleted,
    /// so search filters them out again.
    ///
    /// Split into two phases so the ready-gate window is O(documents) and does
    /// not grow with the number of missing vectors (HNSW insert is the
    /// expensive part and is deferred to [`Self::backfill_vector_missing`]):
    /// - **reconcile** (this fn): note + ghost-mark only. Ghost safety is
    ///   complete when it returns, so the caller may `mark_ready()`.
    /// - **backfill** ([`Self::backfill_vector_missing`]): insert missing
    ///   vectors afterwards. Until it finishes, search can miss those rows
    ///   (false negatives only — never stale/ghost hits).
    fn reconcile_vector_index(
        &self,
        class: &str,
        column: &str,
        dimension: usize,
        existing: &std::collections::HashSet<Vec<u8>>,
    ) -> Vec<(Vec<u8>, String, String, crate::vector::hnsw::VecData)> {
        let prefix = format!("{}::", class);
        let mut missing: Vec<(Vec<u8>, String, String, crate::vector::hnsw::VecData)> =
            Vec::new();
        let mut notes: Vec<(Vec<u8>, String, String, crate::vector::hnsw::VecData)> =
            Vec::new();
        let mut ghosts: Vec<Vec<u8>> = Vec::new();
        let mut seen: std::collections::HashSet<Vec<u8>> =
            std::collections::HashSet::with_capacity(existing.len());
        let mut noted = 0usize;
        let mut marked = 0usize;
        const VEC_BATCH: usize = 10_000;
        let cls = class.to_string();
        let col = column.to_string();
        {
            let mut sink = |pk: &[u8], val_bytes: &[u8]| -> Result<bool> {
                let in_graph = existing.contains(pk);
                if in_graph {
                    seen.insert(pk.to_vec());
                }
                let has_vec = match parse_doc_bytes(val_bytes) {
                    Some(doc)
                        if doc.get("__class__").and_then(|v| v.as_str()) == Some(cls.as_str()) =>
                    {
                        match doc
                            .get(col.as_str())
                            .and_then(|v| crate::vector::hnsw::VecData::from_json(v, dimension))
                        {
                            Some(data) => Some(data),
                            None => None,
                        }
                    }
                    _ => None,
                };
                if in_graph {
                    match has_vec {
                        Some(data) => notes.push((pk.to_vec(), cls.clone(), col.clone(), data)),
                        // Graph node whose document no longer carries a vector.
                        None => ghosts.push(pk.to_vec()),
                    }
                } else if let Some(data) = has_vec {
                    // Not in graph: queue for post-gate backfill, do not insert
                    // here (HNSW insert is what makes the gate window grow).
                    missing.push((pk.to_vec(), cls.clone(), col.clone(), data));
                }
                if notes.len() >= VEC_BATCH {
                    noted += notes.len();
                    let mut mgr = self
                        .vector_index_manager
                        .write()
                        .unwrap_or_else(|e| e.into_inner());
                    for (pk, c, co, data) in std::mem::take(&mut notes) {
                        mgr.note_doc_vector(pk, c, co, data);
                    }
                }
                if ghosts.len() >= VEC_BATCH {
                    marked += ghosts.len();
                    let mut mgr = self
                        .vector_index_manager
                        .write()
                        .unwrap_or_else(|e| e.into_inner());
                    for pk in std::mem::take(&mut ghosts) {
                        mgr.mark_deleted(pk);
                    }
                }
                Ok(true)
            };
            if let Err(e) = self.for_each_live_prefix_until(prefix.as_bytes(), None, None, &mut sink)
            {
                tracing::warn!(
                    table = %class,
                    column = %column,
                    error = %e,
                    "Vector snapshot reconcile scan failed"
                );
            }
        }

        // Graph nodes whose pk never appeared in the live scan: deleted rows
        // whose tombstone (and thus `deleted_keys`) was lost on restart.
        for pk in existing.difference(&seen) {
            ghosts.push(pk.clone());
        }

        if !notes.is_empty() {
            noted += notes.len();
            let mut mgr = self
                .vector_index_manager
                .write()
                .unwrap_or_else(|e| e.into_inner());
            for (pk, c, co, data) in notes {
                mgr.note_doc_vector(pk, c, co, data);
            }
        }
        if !ghosts.is_empty() {
            marked += ghosts.len();
            let mut mgr = self
                .vector_index_manager
                .write()
                .unwrap_or_else(|e| e.into_inner());
            for pk in ghosts {
                mgr.mark_deleted(pk);
            }
        }
        tracing::info!(
            table = %class,
            column = %column,
            missing = missing.len(),
            noted,
            ghosts_marked = marked,
            "Vector snapshot reconcile complete"
        );
        // A large ghost count means historical vectors outlived their
        // documents (deleted_keys was lost across restart). Surface it as a
        // warning so operators know to run COMPACT VECTOR INDEX.
        if marked > 0 {
            let pct = if existing.is_empty() {
                0.0
            } else {
                (marked as f64) * 100.0 / (existing.len() as f64)
            };
            if marked > 100_000 {
                tracing::error!(
                    table = %class,
                    column = %column,
                    ghosts_marked = marked,
                    pct = format!("{:.1}%", pct),
                    "vector ghosts reconciled at scale — run COMPACT VECTOR INDEX ON {} ({})",
                    class, column
                );
            } else if pct > 1.0 {
                tracing::warn!(
                    table = %class,
                    column = %column,
                    ghosts_marked = marked,
                    pct = format!("{:.1}%", pct),
                    "vector ghosts reconciled (>{:.0}% of graph) — consider COMPACT VECTOR INDEX ON {} ({})",
                    pct, class, column
                );
            } else {
                tracing::warn!(
                    table = %class,
                    column = %column,
                    ghosts_marked = marked,
                    "vector ghosts reconciled"
                );
            }
        }
        missing
    }

    /// Inserts vectors queued by [`Self::reconcile_vector_index`].
    ///
    /// Runs **after** `mark_ready`: ghost safety is already established, and
    /// this only adds nodes (never stale/ghost hits). Inserts are guarded —
    /// a concurrent UPDATE/DELETE that already handled the document wins and
    /// the queued copy is skipped, so `insert_data`'s lack of id-dedup cannot
    /// produce a duplicate node.
    fn backfill_vector_missing(
        &self,
        class: &str,
        column: &str,
        missing: Vec<(Vec<u8>, String, String, crate::vector::hnsw::VecData)>,
    ) -> (usize, usize) {
        const VEC_BATCH: usize = 10_000;
        let total = missing.len();
        if total == 0 {
            return (0, 0);
        }
        let mut inserted = 0usize;
        let mut skipped = 0usize;
        let mut batch: Vec<(Vec<u8>, String, String, crate::vector::hnsw::VecData)> =
            Vec::with_capacity(VEC_BATCH.min(total));
        for item in missing {
            batch.push(item);
            if batch.len() >= VEC_BATCH {
                let (ins, skp) = self
                    .vector_index_manager
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .index_vector_data_owned_guarded(std::mem::take(&mut batch));
                inserted += ins;
                skipped += skp;
                self.vector_index_manager
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .sub_pending_backfill(ins + skp);
                tracing::info!(
                    table = %class,
                    column = %column,
                    inserted,
                    skipped,
                    pending = self
                        .vector_index_manager
                        .read()
                        .unwrap_or_else(|e| e.into_inner())
                        .pending_backfill_count(),
                    "Vector snapshot backfill progress"
                );
            }
        }
        if !batch.is_empty() {
            let (ins, skp) = self
                .vector_index_manager
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .index_vector_data_owned_guarded(std::mem::take(&mut batch));
            inserted += ins;
            skipped += skp;
            self.vector_index_manager
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .sub_pending_backfill(ins + skp);
        }
        tracing::info!(
            table = %class,
            column = %column,
            inserted,
            skipped,
            total,
            "Vector snapshot backfill complete"
        );
        (inserted, skipped)
    }

    /// Puts a key-value pair.
    /// Single writes rely on the background WAL sync thread (100ms window).
    ///
    /// Optimized: zero-clone path — WAL serializes directly from key/value
    /// references, then moves originals into MemTable.
    pub fn put(&self, key: Key, value: Value) -> Result<()> {
        let seq = self.next_seq();

        let needs_flush = {
            let mut ws = self.write_state.write();
            // WAL: serialize directly from references (no Entry clone)
            ws.wal.append_raw_put(&key, &value, seq)?;
            ws.wal_pending_count += 1;
            // Batch flush: only flush WAL buffer every 64 writes
            if ws.wal_pending_count >= 64 {
                ws.wal.flush_buf()?;
                ws.wal_pending_count = 0;
            }
            // MemTable: move key/value (no clone)
            ws.memtable.put_with_seq(key, value, seq);
            ws.memtable.size() >= self.options.memtable_size_limit
                || ws.wal.needs_rotation()
        };

        if needs_flush {
            self.flush_memtable()?;
        }

        Ok(())
    }

    /// Batch put: inserts multiple key-value pairs in a single lock acquisition.
    ///
    /// This is significantly faster than calling `put()` in a loop because:
    /// 1. Single write_state lock acquisition for all entries
    /// 2. WAL entries are written in bulk with a single flush
    /// 3. MemTable size check only once at the end
    /// 4. Group commit: single sync for the entire batch
    ///
    /// Returns the number of entries inserted.
    pub fn put_batch(&self, entries: Vec<(Key, Value)>) -> Result<usize> {
        if entries.is_empty() {
            return Ok(0);
        }

        let count = entries.len();

        let (needs_flush, should_sync) = {
            let mut ws = self.write_state.write();
            for (key, value) in entries {
                let seq = self.next_seq();
                // WAL: serialize directly (zero-clone)
                ws.wal.append_raw_put(&key, &value, seq)?;
                ws.wal_pending_count += 1;
                // MemTable: move (no clone)
                ws.memtable.put_with_seq(key, value, seq);
            }
            // Flush WAL buffer once for the entire batch
            ws.wal.flush_buf()?;
            ws.wal_pending_count = 0;
            (
                ws.memtable.size() >= self.options.memtable_size_limit
                    || ws.wal.needs_rotation(),
                self.options.sync_wal_on_commit,
            )
        };

        // Group commit: batch sync across concurrent writers
        if should_sync {
            let current_seq = self.seq_counter.load(Ordering::Relaxed);
            let is_leader = self.group_commit.register(current_seq);
            if is_leader {
                let _batch_size = self.group_commit.wait_for_batch();
                {
                    let mut ws = self.write_state.write();
                    ws.wal.sync()?;
                }
                self.group_commit.complete_sync(current_seq);
            } else {
                self.group_commit.wait_for_sync(current_seq);
            }
        }

        if needs_flush {
            self.flush_memtable()?;
        }

        Ok(count)
    }

    /// Gets a value by key.
    pub fn get(&self, key: &[u8]) -> Result<Option<Value>> {
        // Retry loop: candidate paths snapshotted from `levels` can be
        // compacted away (and deleted from disk) before we open them. The
        // data is not lost — it was merged into a newer SST — but that SST
        // is not in our stale snapshot. Re-collect and retry.
        for _ in 0..3 {
            match self.get_once(key)? {
                GetOutcome::Hit(v) => return Ok(Some(v)),
                GetOutcome::Deleted => return Ok(None),
                GetOutcome::Absent => return Ok(None),
                GetOutcome::StaleSst => continue,
            }
        }
        // Three consecutive stale snapshots: give up and report absent —
        // every retry re-read memtables first, so a live version would have
        // been found; a permanently vanishing SST set means nothing is there.
        Ok(None)
    }

    fn get_once(&self, key: &[u8]) -> Result<GetOutcome> {
        self.drain_compaction_notifications();

        // 1. Check MemTables FIRST (under read lock), before touching SSTs.
        //    A memtable tombstone is terminal: falling through to SSTs would
        //    resurrect a deleted row from a stale Put below.
        //
        //    Order matters vs step 2: `persist_immutable_to_sst` pushes the
        //    new L0 SST into `levels` BEFORE clearing the immutable memtable,
        //    so if we see nothing in the memtables here, any flushed version
        //    is already visible to the candidate collection below. The reverse
        //    order (collect candidates, then check memtables) leaves a window
        //    where a concurrent flush lands the tombstone in `levels` after the
        //    snapshot and clears the memtable before the check — both sides
        //    miss the delete and the stale Put wins. That was the flaky
        //    resurrection in `test_tombstones_survive_compaction_no_resurrect`.
        {
            let ws = self.write_state.read();
            use crate::lsm::memtable::VersionLookup;
            match ws.memtable.get_version(key) {
                Some(VersionLookup::Value(v, _)) => return Ok(GetOutcome::Hit(v)),
                Some(VersionLookup::Tombstone) => return Ok(GetOutcome::Deleted),
                None => {}
            }
            if let Some(ref imm) = ws.immutable_memtable {
                match imm.get_version(key) {
                    Some(VersionLookup::Value(v, _)) => return Ok(GetOutcome::Hit(v)),
                    Some(VersionLookup::Tombstone) => return Ok(GetOutcome::Deleted),
                    None => {}
                }
            }
        }

        // 2. Collect candidate SST paths from levels (brief lock).
        //    The min/max range check is an optimization only. An empty bound
        //    (unlabeled SST) must not exclude the file — that silently drops
        //    rows. Be conservative: skip the range check when either bound is
        //    empty, and treat missing bounds as unbounded.
        let candidates = {
            let levels = self.levels.lock().unwrap_or_else(|e| e.into_inner());
            let mut cands = Vec::new();
            for level in levels.iter() {
                for sst_info in level.iter().rev() {
                    let min_ok = sst_info.min_key.is_empty() || key >= sst_info.min_key.as_slice();
                    let max_ok = sst_info.max_key.is_empty() || key <= sst_info.max_key.as_slice();
                    if !(min_ok && max_ok) {
                        continue;
                    }
                    cands.push(sst_info.path.clone());
                }
            }
            cands
        };

        // 3. Snapshot SST handles from cache (opens happen outside the lock).
        //    A candidate may already be compacted away on disk; that is a
        //    stale snapshot, not an error — retry against the new layout.
        let sst_handles = match self.try_get_or_open_ssts(&candidates)? {
            Some(h) => h,
            None => return Ok(GetOutcome::StaleSst),
        };

        // 4. Iterate SST handles outside the lock (I/O-heavy).
        //    Collect the highest-seq version across all candidates — returning
        //    the first hit is wrong when an older SST is examined before the
        //    one holding the newer version (observed as stale UPDATE reads).
        let mut best: Option<(Vec<u8>, SeqNo, EntryKind)> = None;
        for sst in &sst_handles {
            if let Some((value, seq, kind)) = sst.get_full(key)? {
                let is_newer = best.as_ref().map(|(_, s, _)| seq > *s).unwrap_or(true);
                if is_newer {
                    best = Some((value, seq, kind));
                }
            }
        }
        match best {
            Some((value, _, EntryKind::Put)) => Ok(GetOutcome::Hit(value)),
            Some((_, _, EntryKind::Delete)) => Ok(GetOutcome::Deleted),
            None => Ok(GetOutcome::Absent),
        }
    }

    /// Batch point-lookup: fetches values for multiple keys efficiently.
    ///
    /// Keys are sorted internally so that MemTable range queries and SST
    /// block reads are sequential rather than random. Returns a Vec of
    /// `Option<Value>` in the same order as the input `keys`.
    ///
    /// Compared to calling [`Self::get`] in a loop, this avoids per-key:
    /// - lock acquisition (levels, write_state, sst_cache)
    /// - SST candidate filtering
    /// - repeated block-cache lock/unlock
    pub fn multi_get(&self, keys: &[Vec<u8>]) -> Result<Vec<Option<Value>>> {
        if keys.is_empty() {
            return Ok(Vec::new());
        }

        self.drain_compaction_notifications();

        // Sort indices by key for sequential access patterns.
        let mut sorted_idx: Vec<usize> = (0..keys.len()).collect();
        sorted_idx.sort_unstable_by(|&a, &b| keys[a].cmp(&keys[b]));

        let mut out: Vec<Option<Value>> = vec![None; keys.len()];

        // Phase 1: MemTable lookup under brief read lock.
        // Memtable tombstones are terminal — a deleted key must not fall
        // through to a stale Put in the SSTs below.
        let mut deleted = vec![false; keys.len()];
        {
            let ws = self.write_state.read();
            use crate::lsm::memtable::VersionLookup;
            for &orig_i in &sorted_idx {
                let key = keys[orig_i].as_slice();
                let mut resolved = false;
                match ws.memtable.get_version(key) {
                    Some(VersionLookup::Value(v, _)) => {
                        out[orig_i] = Some(v);
                        resolved = true;
                    }
                    Some(VersionLookup::Tombstone) => {
                        deleted[orig_i] = true;
                        resolved = true;
                    }
                    None => {}
                }
                if !resolved {
                    if let Some(ref imm) = ws.immutable_memtable {
                        match imm.get_version(key) {
                            Some(VersionLookup::Value(v, _)) => {
                                out[orig_i] = Some(v);
                            }
                            Some(VersionLookup::Tombstone) => {
                                deleted[orig_i] = true;
                            }
                            None => {}
                        }
                    }
                }
            }
        }

        // Phase 2: SST lookup outside write_state lock (I/O heavy).
        let sst_handles: Vec<(Arc<SsTable>, Vec<u8>, Vec<u8>)> = {
            let levels = self.levels.lock().unwrap_or_else(|e| e.into_inner());
            let mut cache = self.sst_cache.lock().unwrap_or_else(|e| e.into_inner());
            let mut handles = Vec::new();
            let mut seen = std::collections::HashSet::new();
            for level in levels.iter() {
                for sst_info in level.iter().rev() {
                    if !seen.insert(sst_info.path.clone()) {
                        continue;
                    }
                    if !cache.contains_key(&sst_info.path) {
                        if let Ok(sst) = SsTable::open(&sst_info.path) {
                            cache.insert(sst_info.path.clone(), Arc::new(sst));
                        }
                    }
                    if let Some(h) = cache.get(&sst_info.path) {
                        handles.push((
                            h.clone(),
                            sst_info.min_key.clone(),
                            sst_info.max_key.clone(),
                        ));
                    }
                }
            }
            handles
        };

        // For each unresolved key (sorted), walk SSTs in order.
        for &orig_i in &sorted_idx {
            if out[orig_i].is_some() || deleted[orig_i] {
                continue;
            }
            let key = keys[orig_i].as_slice();
            for (sst, min, max) in &sst_handles {
                if key < min.as_slice() || key > max.as_slice() {
                    continue;
                }
                match sst.get_full(key) {
                    Ok(Some((value, _, EntryKind::Put))) => {
                        out[orig_i] = Some(value);
                        break;
                    }
                    Ok(Some((_, _, EntryKind::Delete))) => break,
                    Ok(None) => continue,
                    Err(_) => continue,
                }
            }
        }

        Ok(out)
    }

    /// True when an SST open failed because the file was already compacted
    /// away (deleted from disk after we snapshotted its path from `levels`).
    fn is_not_found(e: &onto_core::CoreError) -> bool {
        match e {
            onto_core::CoreError::Io(io) => io.kind() == std::io::ErrorKind::NotFound,
            onto_core::CoreError::Custom(msg) => {
                msg.contains("NotFound") || msg.contains("os error 2")
            }
            _ => false,
        }
    }

    /// Fetch an SST handle from the cache, opening the file OUTSIDE the
    /// `sst_cache` lock on a miss. `SsTable::open` reads footer/index/bloom
    /// from disk — holding the global cache mutex across that serializes
    /// every concurrent reader behind one cold open.
    fn get_or_open_sst(&self, path: &std::path::Path) -> Result<Arc<SsTable>> {
        {
            let cache = self.sst_cache.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(sst) = cache.get(path) {
                return Ok(Arc::clone(sst));
            }
        }
        // Open outside the lock (I/O-heavy).
        let sst = Arc::new(SsTable::open(path)?);
        let mut cache = self.sst_cache.lock().unwrap_or_else(|e| e.into_inner());
        // Double-check: another thread may have inserted while we were opening.
        if let Some(existing) = cache.get(path) {
            return Ok(Arc::clone(existing));
        }
        cache.insert(path.to_path_buf(), Arc::clone(&sst));
        Ok(sst)
    }

    /// Bulk variant of [`Self::get_or_open_sst`]: one brief lock for cache
    /// hits, then opens only the misses outside the lock. Preserves input order.
    ///
    /// Returns `Ok(None)` when any path was already compacted away (NotFound)
    /// — the caller's `levels` snapshot is stale and should re-collect paths
    /// and retry. Skipping those files silently would drop their keys.
    fn try_get_or_open_ssts(
        &self,
        paths: &[std::path::PathBuf],
    ) -> Result<Option<Vec<Arc<SsTable>>>> {
        let mut handles: Vec<Option<Arc<SsTable>>> = vec![None; paths.len()];
        let mut missing: Vec<usize> = Vec::new();
        {
            let cache = self.sst_cache.lock().unwrap_or_else(|e| e.into_inner());
            for (i, path) in paths.iter().enumerate() {
                if let Some(sst) = cache.get(path) {
                    handles[i] = Some(Arc::clone(sst));
                } else {
                    missing.push(i);
                }
            }
        }
        for i in missing {
            match self.get_or_open_sst(&paths[i]) {
                Ok(sst) => handles[i] = Some(sst),
                Err(e) if Self::is_not_found(&e) => return Ok(None),
                Err(e) => return Err(e),
            }
        }
        Ok(Some(
            handles.into_iter().map(|h| h.expect("should be valid")).collect(),
        ))
    }

    fn get_or_open_ssts(&self, paths: &[std::path::PathBuf]) -> Result<Vec<Arc<SsTable>>> {
        self.try_get_or_open_ssts(paths)?
            .ok_or_else(|| onto_core::CoreError::Custom("stale sst snapshot".into()))
    }

    /// Scans all entries whose key starts with the given prefix.
    /// Returns a sorted Vec of (key, value) pairs.
    ///
    /// This leverages the LSM-Tree's sorted key structure:
    /// entries with `{class}::` prefix are contiguous in sorted order.
    pub fn scan_prefix(&self, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        tracing::debug!(prefix = %String::from_utf8_lossy(prefix), "scan_prefix start");
        let result = self.scan_prefix_internal(prefix, None, None)?;
        tracing::debug!(prefix = %String::from_utf8_lossy(prefix), count = result.len(), "scan_prefix done");
        Ok(result)
    }

    /// Recovery mode: returns ALL Put entries regardless of tombstones.
    /// Used to recover data that was deleted.
    pub fn scan_prefix_recovery(&self, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        eprintln!("[STORAGE recovery] prefix='{}', len={}", String::from_utf8_lossy(prefix), prefix.len());
        self.drain_compaction_notifications();

        let mut results: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();

        // Collect SST paths under lock
        let sst_paths: Vec<PathBuf> = {
            let levels = self.levels.lock().unwrap_or_else(|e| e.into_inner());
            let mut paths = Vec::new();
            for level in levels.iter().rev() {
                for sst_info in level.iter() {
                    if !prefix.is_empty() {
                        let sst_max = sst_info.max_key.as_slice();
                        if sst_max < prefix {
                            continue;
                        }
                        let sst_min = sst_info.min_key.as_slice();
                        if !Self::prefix_may_overlap(prefix, sst_min, sst_max) {
                            continue;
                        }
                    }
                    paths.push(sst_info.path.clone());
                }
            }
            paths
        };

        // Open SST handles from cache
        let sst_handles: Vec<Arc<SsTable>> = {
            let mut cache = self.sst_cache.lock().unwrap_or_else(|e| e.into_inner());
            let mut handles = Vec::with_capacity(sst_paths.len());
            for path in &sst_paths {
                if let Some(sst) = cache.get(path) {
                    handles.push(Arc::clone(sst));
                } else {
                    let sst = Arc::new(SsTable::open(path)?);
                    cache.insert(path.to_path_buf(), Arc::clone(&sst));
                    handles.push(sst);
                }
            }
            handles
        };

        // Scan ALL SSTs, collect ALL Put entries (no tombstone filtering)
        for sst in &sst_handles {
            let mut iter = sst.iter()?;
            iter.seek(prefix);
            while iter.is_valid() {
                if !iter.key().starts_with(prefix) {
                    break;
                }
                if iter.kind() == EntryKind::Put {
                    results.push((iter.key().to_vec(), iter.value().to_vec()));
                }
                iter.next();
            }
        }

        // Also scan MemTables
        {
            let ws = self.write_state.read();
            if let Some(ref imm) = ws.immutable_memtable {
                for entry in imm.scan_prefix(prefix) {
                    if entry.kind == EntryKind::Put {
                        results.push((entry.key.clone(), entry.value.clone()));
                    }
                }
            }
            for entry in ws.memtable.scan_prefix(prefix) {
                if entry.kind == EntryKind::Put {
                    results.push((entry.key.clone(), entry.value.clone()));
                }
            }
        }

        eprintln!("[STORAGE recovery] returned {} Put entries", results.len());
        Ok(results)
    }

    /// Scan with prefix, returning at most `limit` live entries.
    ///
    /// Optimized for LIMIT queries: scans **newest-first** (memtable → L0 → …)
    /// so the first occurrence of each key is the winning version, then stops
    /// once `limit` live Puts are collected. Does **not** walk the entire
    /// prefix — older SSTs are skipped entirely once the limit is met.
    pub fn scan_prefix_limit(&self, prefix: &[u8], limit: usize) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        tracing::debug!(prefix = %String::from_utf8_lossy(prefix), "scan_prefix_limit start");
        let result = self.scan_prefix_limit_newest(prefix, limit)?;
        tracing::debug!(prefix = %String::from_utf8_lossy(prefix), count = result.len(), "scan_prefix_limit done");
        Ok(result)
    }

    /// Newest-first limited scan with early exit.
    ///
    /// Visibility rule: first occurrence of a key wins (scan order is
    /// newest → oldest). Memtables override SSTs; within L0, later-flushed
    /// (higher path name / max_seq) SSTs are scanned first.
    fn scan_prefix_limit_newest(
        &self,
        prefix: &[u8],
        limit: usize,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        self.drain_compaction_notifications();

        if limit == 0 {
            return Ok(Vec::new());
        }

        // Newest-first SST list: L0 before L1 … ; within a level, reverse
        // insertion order (later flushes are newer). Collect paths under the
        // brief levels lock, then open outside it. Retry on stale paths —
        // compaction may delete a snapshotted SST before we open it.
        let sst_handles: Vec<Arc<SsTable>> = {
            let mut handles = None;
            for _ in 0..3 {
                let sst_paths: Vec<std::path::PathBuf> = {
                    let levels = self.levels.lock().unwrap_or_else(|e| e.into_inner());
                    let mut paths = Vec::new();
                    for level in levels.iter() {
                        // Reverse so later-pushed (newer) SSTs come first
                        for sst_info in level.iter().rev() {
                            if !prefix.is_empty() {
                                if sst_info.max_key.as_slice() < prefix {
                                    continue;
                                }
                                if !Self::prefix_may_overlap(
                                    prefix,
                                    sst_info.min_key.as_slice(),
                                    sst_info.max_key.as_slice(),
                                ) {
                                    continue;
                                }
                            }
                            paths.push(sst_info.path.clone());
                        }
                    }
                    paths
                };
                match self.try_get_or_open_ssts(&sst_paths)? {
                    Some(h) => {
                        handles = Some(h);
                        break;
                    }
                    None => continue,
                }
            }
            handles.unwrap_or_default()
        };

        let mut seen: HashMap<Vec<u8>, (Vec<u8>, SeqNo, EntryKind)> = HashMap::new();
        let mut live_puts = 0usize;

        let mut note = |key: Vec<u8>, value: Vec<u8>, seq: SeqNo, kind: EntryKind, seen: &mut HashMap<_, _>, live_puts: &mut usize| -> bool {
            // returns true if caller should stop (limit reached)
            if seen.contains_key(&key) {
                return false; // first-seen wins (newest already recorded)
            }
            let is_live = kind == EntryKind::Put;
            seen.insert(key, (value, seq, kind));
            if is_live {
                *live_puts += 1;
                return *live_puts >= limit;
            }
            false
        };

        // 1. Active memtable (newest)
        {
            let ws = self.write_state.read();
            for entry in ws.memtable.scan_prefix(prefix) {
                if note(
                    entry.key.clone(),
                    entry.value.clone(),
                    entry.seq_no,
                    entry.kind,
                    &mut seen,
                    &mut live_puts,
                ) {
                    return Ok(Self::collect_live(seen, limit));
                }
            }
            // 2. Immutable memtable
            if let Some(ref imm) = ws.immutable_memtable {
                for entry in imm.scan_prefix(prefix) {
                    if note(
                        entry.key.clone(),
                        entry.value.clone(),
                        entry.seq_no,
                        entry.kind,
                        &mut seen,
                        &mut live_puts,
                    ) {
                        return Ok(Self::collect_live(seen, limit));
                    }
                }
            }
        }

        // 3. SSTs newest → oldest, early-exit on limit.
        // Decode whole blocks at once (batch) + block prefetch (in load_block).
        for sst in &sst_handles {
            let mut iter = sst.iter()?;
            iter.seek(prefix);
            let mut batch: Vec<(Vec<u8>, Vec<u8>, SeqNo, EntryKind)> = Vec::with_capacity(64);
            loop {
                if !iter.is_valid() || !iter.key().starts_with(prefix) {
                    break;
                }
                batch.clear();
                iter.drain_current_block(&mut batch);
                let mut past_prefix = false;
                for (key, value, seq, kind) in batch.drain(..) {
                    if !key.starts_with(prefix) {
                        past_prefix = true;
                        break;
                    }
                    if note(key, value, seq, kind, &mut seen, &mut live_puts) {
                        return Ok(Self::collect_live(seen, limit));
                    }
                }
                if past_prefix {
                    break;
                }
            }
        }

        Ok(Self::collect_live(seen, limit))
    }

    fn collect_live(
        seen: HashMap<Vec<u8>, (Vec<u8>, SeqNo, EntryKind)>,
        limit: usize,
    ) -> Vec<(Vec<u8>, Vec<u8>)> {
        seen.into_iter()
            .filter(|(_, (_, _, kind))| *kind == EntryKind::Put)
            .take(limit)
            .map(|(key, (value, _, _))| (key, value))
            .collect()
    }

    /// Live rows in **user-key order**, strictly after `after`, at most `limit`.
    ///
    /// Version selection matches [`Self::for_each_live_prefix`]: sources are
    /// walked newest → oldest (active memtable, immutable memtable, L0…L6) and
    /// the first version of a key wins. A winning `Delete` means the key is gone.
    ///
    /// Memory is O(memtable snapshot + limit), not O(table) — this is the
    /// pagination primitive filtered DELETE needs so it can stream instead of
    /// materializing every row via `scan_prefix`.
    pub fn scan_prefix_after(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        self.drain_compaction_notifications();

        // Memtables are small (bounded by memtable_size_limit) and already
        // ordered by user key then inverted-seq (newest version first).
        let mut mem_active: Vec<(Vec<u8>, Vec<u8>, SeqNo, EntryKind)> = Vec::new();
        let mut mem_imm: Vec<(Vec<u8>, Vec<u8>, SeqNo, EntryKind)> = Vec::new();
        {
            let ws = self.write_state.read();
            for e in ws.memtable.scan_prefix(prefix) {
                if after.is_some_and(|a| e.key.as_slice() <= a) {
                    continue;
                }
                mem_active.push((e.key.clone(), e.value.clone(), e.seq_no, e.kind));
            }
            if let Some(ref imm) = ws.immutable_memtable {
                for e in imm.scan_prefix(prefix) {
                    if after.is_some_and(|a| e.key.as_slice() <= a) {
                        continue;
                    }
                    mem_imm.push((e.key.clone(), e.value.clone(), e.seq_no, e.kind));
                }
            }
        }

        // Newest-first SST list with retry on stale paths (compaction can
        // delete a snapshotted SST before we open it). Same pattern as
        // for_each_live_prefix_until / scan_prefix_limit_newest.
        let sst_handles: Vec<Arc<SsTable>> = {
            let mut handles = None;
            for _ in 0..3 {
                let sst_paths: Vec<std::path::PathBuf> = {
                    let levels = self.levels.lock().unwrap_or_else(|e| e.into_inner());
                    let mut paths = Vec::new();
                    for level in levels.iter() {
                        for sst_info in level.iter().rev() {
                            if !prefix.is_empty() {
                                if sst_info.max_key.as_slice() < prefix {
                                    continue;
                                }
                                if !Self::prefix_may_overlap(
                                    prefix,
                                    sst_info.min_key.as_slice(),
                                    sst_info.max_key.as_slice(),
                                ) {
                                    continue;
                                }
                            }
                            paths.push(sst_info.path.clone());
                        }
                    }
                    paths
                };
                match self.try_get_or_open_ssts(&sst_paths)? {
                    Some(h) => {
                        handles = Some(h);
                        break;
                    }
                    None => continue,
                }
            }
            match handles {
                Some(h) => h,
                None => Vec::new(), // nothing readable in the prefix
            }
        };
        let mut sst_iters = Vec::with_capacity(sst_handles.len());
        for sst in &sst_handles {
            let mut it = sst.iter()?;
            match after {
                Some(a) => it.seek(a),
                None => it.seek(prefix),
            }
            // Drop cursor entries the caller already paged past.
            while it.is_valid() {
                if !it.key().starts_with(prefix) {
                    break;
                }
                if after.is_some_and(|a| it.key() <= a) {
                    it.next();
                    continue;
                }
                break;
            }
            sst_iters.push(it);
        }

        let mut out: Vec<(Vec<u8>, Vec<u8>)> = Vec::with_capacity(limit.min(4096));
        let mut mem_a_i = 0usize;
        let mut mem_i_i = 0usize;

        while out.len() < limit {
            // Smallest live user key across sources.
            let mut min_key: Option<Vec<u8>> = None;
            {
                let mut consider = |k: &[u8]| {
                    if !k.starts_with(prefix) {
                        return;
                    }
                    if after.is_some_and(|a| k <= a) {
                        return;
                    }
                    if min_key.as_ref().is_none_or(|m| k < m.as_slice()) {
                        min_key = Some(k.to_vec());
                    }
                };
                if mem_a_i < mem_active.len() {
                    consider(mem_active[mem_a_i].0.as_slice());
                }
                if mem_i_i < mem_imm.len() {
                    consider(mem_imm[mem_i_i].0.as_slice());
                }
                for it in &sst_iters {
                    if it.is_valid() {
                        consider(it.key());
                    }
                }
            }

            let Some(group_key) = min_key else {
                break;
            };

            // First-seen wins across sources in priority order.
            let mut winner: Option<(Vec<u8>, EntryKind)> = None;
            while mem_a_i < mem_active.len() && mem_active[mem_a_i].0 == group_key {
                if winner.is_none() {
                    let e = &mem_active[mem_a_i];
                    winner = Some((e.1.clone(), e.3));
                }
                mem_a_i += 1;
            }
            while mem_i_i < mem_imm.len() && mem_imm[mem_i_i].0 == group_key {
                if winner.is_none() {
                    let e = &mem_imm[mem_i_i];
                    winner = Some((e.1.clone(), e.3));
                }
                mem_i_i += 1;
            }
            for it in sst_iters.iter_mut() {
                while it.is_valid() && it.key() == group_key.as_slice() {
                    if winner.is_none() {
                        winner = Some((it.value().to_vec(), it.kind()));
                    }
                    it.next();
                }
            }

            if let Some((value, kind)) = winner {
                if kind == EntryKind::Put {
                    out.push((group_key, value));
                }
            }
        }

        Ok(out)
    }

    /// Internal scan implementation shared by scan_prefix and scan_prefix_with_visibility.
    /// When `vis` is Some, only entries visible to the snapshot are included.
    fn scan_prefix_internal(
        &self,
        prefix: &[u8],
        vis: Option<&crate::mvcc::Visibility>,
        limit: Option<usize>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        self.drain_compaction_notifications();

        // Use HashMap instead of BTreeMap — we don't need sorted iteration here;
        // callers either iterate in insertion order or re-sort themselves.
        // HashMap avoids O(log n) byte-vector comparisons per insert.
        let mut seen: HashMap<Vec<u8>, (Vec<u8>, SeqNo, EntryKind)> = HashMap::new();

        let is_visible = |seq: SeqNo| -> bool {
            match vis {
                Some(v) => v.is_visible(seq),
                None => true,
            }
        };

        // Version selection: newest-first source order + first-seen-wins.
        //
        // Do NOT pick the winner by `seq` comparison. `seq` can be 0 (or
        // duplicated) when a file was rewritten by clean_index / migration,
        // and `seq > existing` then keeps the OLDER level's version. LSM
        // level order is always trustworthy: new writes enter at the memtable
        // and L0, so scanning memtable → L0 → … → L6 and keeping the first
        // occurrence matches `get()` and `scan_prefix_limit_newest`.
        let mut note = |key: Vec<u8>, value: Vec<u8>, seq: SeqNo, kind: EntryKind,
                        seen: &mut HashMap<Vec<u8>, (Vec<u8>, SeqNo, EntryKind)>| {
            if is_visible(seq) && !seen.contains_key(&key) {
                seen.insert(key, (value, seq, kind));
            }
        };

        // Step 1: Snapshot SST handles (cache hits under one brief lock;
        // cold opens happen outside it via get_or_open_sst). Retry on stale
        // paths — compaction may delete a snapshotted SST before we open it.
        let sst_handles: Vec<Arc<SsTable>> = {
            let mut handles = None;
            for _ in 0..3 {
                let sst_paths: Vec<PathBuf> = {
                    let levels = self.levels.lock().unwrap_or_else(|e| e.into_inner());
                    let mut paths = Vec::new();
                    for level in levels.iter() {
                        for sst_info in level.iter().rev() {
                            if !prefix.is_empty() {
                                let sst_max = sst_info.max_key.as_slice();
                                if sst_max < prefix {
                                    continue;
                                }
                                let sst_min = sst_info.min_key.as_slice();
                                if !Self::prefix_may_overlap(prefix, sst_min, sst_max) {
                                    continue;
                                }
                            }
                            paths.push(sst_info.path.clone());
                        }
                    }
                    paths
                };
                match self.try_get_or_open_ssts(&sst_paths)? {
                    Some(h) => {
                        handles = Some(h);
                        break;
                    }
                    None => continue,
                }
            }
            handles.unwrap_or_default()
        };

        // Step 2: MemTables FIRST (newest source).
        {
            let ws = self.write_state.read();
            if let Some(ref imm) = ws.immutable_memtable {
                for entry in imm.scan_prefix(prefix) {
                    note(
                        entry.key.clone(),
                        entry.value.clone(),
                        entry.seq_no,
                        entry.kind,
                        &mut seen,
                    );
                }
            }
            for entry in ws.memtable.scan_prefix(prefix) {
                note(
                    entry.key.clone(),
                    entry.value.clone(),
                    entry.seq_no,
                    entry.kind,
                    &mut seen,
                );
            }
        }

        // Step 3: Scan SSTables OUTSIDE the lock (I/O-heavy), newest-first.
        // First-seen-wins: a key already recorded by a newer source is skipped.
        for (sst_idx, sst) in sst_handles.iter().enumerate() {
            let mut iter = sst.iter()?;

            // Use seek to jump directly to the prefix start instead of linear scan
            iter.seek(prefix);

            let mut sst_count = 0usize;
            let mut sst_put_count = 0usize;
            let mut sst_del_count = 0usize;
            while iter.is_valid() {
                if !iter.key().starts_with(prefix) {
                    break;
                }
                let key = iter.key().to_vec();
                let value = iter.value().to_vec();
                let seq = iter.seq_no();
                let kind = iter.kind();

                if kind == EntryKind::Put {
                    sst_put_count += 1;
                } else {
                    sst_del_count += 1;
                }

                note(key, value, seq, kind, &mut seen);

                sst_count += 1;
                iter.next();
            }
            if sst_count > 0 {
                tracing::debug!(
                    sst_idx,
                    prefix = %String::from_utf8_lossy(prefix),
                    entries = sst_count,
                    puts = sst_put_count,
                    dels = sst_del_count,
                    "sst_scan"
                );
            }

            // NOTE: Do NOT early-exit based on put_count here.
            // A later SST in this newest-first list is older, but a key can
            // still appear only there (not in a newer SST) — and tombstones
            // already recorded win via first-seen-wins. The SST list is
            // pruned by prefix range, so scanning all of them is cheap.
        }

        // Filter out tombstones, with early termination when limit is set
        let total_seen = seen.len();
        let tombstone_count = seen.values().filter(|(_, _, kind)| *kind != EntryKind::Put).count();
        let put_count = total_seen - tombstone_count;
        // Show sample tombstone keys (first 5) for debugging
        if tombstone_count > 0 && tombstone_count == total_seen {
            let sample: Vec<String> = seen.keys().take(5)
                .map(|k| String::from_utf8_lossy(k).to_string())
                .collect();
            tracing::debug!(
                prefix = %String::from_utf8_lossy(prefix),
                seen = total_seen,
                tombstones = tombstone_count,
                sample_keys = ?sample,
                "all tombstones"
            );
        }

        // When limit is set, stop collecting as soon as we have enough live entries.
        // This avoids materializing the entire result set for large tables.
        let mut result: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
        if let Some(lim) = limit {
            result.reserve(lim.min(put_count));
            for (key, (value, _, kind)) in seen {
                if kind == EntryKind::Put {
                    result.push((key, value));
                    if result.len() >= lim {
                        break;
                    }
                }
            }
        } else {
            result = seen
                .into_iter()
                .filter(|(_, (_, _, kind))| *kind == EntryKind::Put)
                .map(|(key, (value, _, _))| (key, value))
                .collect();
        }

        tracing::debug!(
            prefix = %String::from_utf8_lossy(prefix),
            seen = total_seen,
            puts = put_count,
            tombstones = tombstone_count,
            after_filter = result.len(),
            "scan_prefix_internal done"
        );

        Ok(result)
    }

    /// Count live entries matching a prefix. More efficient than scan_prefix().len()
    /// because it never copies values. Returns (live_count, tombstone_count).
    pub fn count_prefix(&self, prefix: &[u8]) -> Result<(usize, usize)> {
        // Pass 1 only: every winning Put is live. No value predicate → no pass 2.
        let winners = self.collect_prefix_winners(prefix, None)?;
        let mut live = 0usize;
        let mut tombstones = 0usize;
        for kind in winners.values().map(|(_, k)| *k) {
            match kind {
                EntryKind::Put => live += 1,
                EntryKind::Delete => tombstones += 1,
            }
        }
        Ok((live, tombstones))
    }

    /// Collect latest-version-wins `key → (seq, kind)` under `prefix`.
    /// Never copies or retains values — used by COUNT paths.
    fn collect_prefix_winners(
        &self,
        prefix: &[u8],
        vis: Option<&crate::mvcc::Visibility>,
    ) -> Result<HashMap<Vec<u8>, (SeqNo, EntryKind)>> {
        self.drain_compaction_notifications();

        let is_visible = |seq: SeqNo| -> bool {
            match vis {
                Some(v) => v.is_visible(seq),
                None => true,
            }
        };

        let mut winners: HashMap<Vec<u8>, (SeqNo, EntryKind)> = HashMap::new();

        // Newest-first SST list: L0 before L1 … ; within a level, reverse
        // insertion order (later flushes are newer). First-seen-wins below —
        // do NOT select the winner by `seq` (it can be 0 after clean_index).
        let sst_handles: Vec<Arc<SsTable>> = {
            let levels = self.levels.lock().unwrap_or_else(|e| e.into_inner());
            let mut cache = self.sst_cache.lock().unwrap_or_else(|e| e.into_inner());
            let mut handles = Vec::new();
            for level in levels.iter() {
                for sst_info in level.iter().rev() {
                    if !prefix.is_empty() {
                        if sst_info.max_key.as_slice() < prefix {
                            continue;
                        }
                        if !Self::prefix_may_overlap(
                            prefix,
                            sst_info.min_key.as_slice(),
                            sst_info.max_key.as_slice(),
                        ) {
                            continue;
                        }
                    }
                    match cache.get(&sst_info.path) {
                        Some(sst) => handles.push(Arc::clone(sst)),
                        None => {
                            let sst = Arc::new(SsTable::open(&sst_info.path)?);
                            cache.insert(sst_info.path.clone(), Arc::clone(&sst));
                            handles.push(sst);
                        }
                    }
                }
            }
            handles
        };

        {
            let mut note = |key: Vec<u8>, seq: SeqNo, kind: EntryKind| {
                // First-seen wins (newest source presents the key first).
                if !winners.contains_key(&key) {
                    winners.insert(key, (seq, kind));
                }
            };

            // MemTables first (newest source) — active before immutable:
            // immutable is OLDER; scanning it first let a stale version win
            // first-seen-wins over a fresher tombstone/Put in the active
            // memtable (count-path resurrection window).
            {
                let ws = self.write_state.read();
                for entry in ws.memtable.scan_prefix(prefix) {
                    if is_visible(entry.seq_no) {
                        note(entry.key.clone(), entry.seq_no, entry.kind);
                    }
                }
                if let Some(ref imm) = ws.immutable_memtable {
                    for entry in imm.scan_prefix(prefix) {
                        if is_visible(entry.seq_no) {
                            note(entry.key.clone(), entry.seq_no, entry.kind);
                        }
                    }
                }
            }

            // Then SSTs, newest-first.
            for sst in &sst_handles {
                let mut iter = sst.iter()?;
                iter.seek(prefix);
                // Key-only batch: values are framed past but never copied.
                let mut batch: Vec<(Vec<u8>, SeqNo, EntryKind)> = Vec::with_capacity(64);
                loop {
                    if !iter.is_valid() || !iter.key().starts_with(prefix) {
                        break;
                    }
                    batch.clear();
                    iter.drain_current_block_keys(&mut batch);
                    let mut past = false;
                    for (key, seq, kind) in batch.drain(..) {
                        if !key.starts_with(prefix) {
                            past = true;
                            break;
                        }
                        if is_visible(seq) {
                            note(key, seq, kind);
                        }
                    }
                    if past {
                        break;
                    }
                }
            }
        }

        Ok(winners)
    }

    /// Count live entries matching a prefix whose value passes `passes`.
    ///
    /// Two-pass design (same as [`Self::for_each_live_prefix`]):
    /// 1. Collect `key → (seq, kind)` only — **no value copies, no filter eval**.
    /// 2. Re-stream values zero-copy and evaluate `passes` **only on winning Puts**.
    ///
    /// Pass 1 never allocates values. Pass 2 streams one entry at a time via
    /// slices into the block buffer; values are discarded after the predicate.
    /// Peak memory is O(keys) plus one block buffer.
    ///
    /// Returns (matched_live_count, tombstone_count).
    pub fn count_prefix_with<F>(
        &self,
        prefix: &[u8],
        vis: Option<&crate::mvcc::Visibility>,
        mut passes: F,
    ) -> Result<(usize, usize)>
    where
        F: FnMut(&[u8]) -> Result<bool>,
    {
        let mut winners = self.collect_prefix_winners(prefix, vis)?;

        let mut matched_live = 0usize;
        let mut pending_puts = winners
            .values()
            .filter(|(_, kind)| *kind == EntryKind::Put)
            .count();
        let tombstones = winners.len() - pending_puts;

        // ---- Pass 2: evaluate filter only on winning Puts (zero-copy values) ----
        {
            fn try_count<F>(
                winners: &mut HashMap<Vec<u8>, (SeqNo, EntryKind)>,
                pending_puts: &mut usize,
                matched_live: &mut usize,
                passes: &mut F,
                key: &[u8],
                _seq: SeqNo,
                kind: EntryKind,
                value: &[u8],
            ) -> Result<()>
            where
                F: FnMut(&[u8]) -> Result<bool>,
            {
                if kind != EntryKind::Put || *pending_puts == 0 {
                    return Ok(());
                }
                let Some((_, w_kind)) = winners.get(key).copied() else {
                    return Ok(());
                };
                // First Put in newest-first order is the pass-1 winner. Do NOT
                // match on `seq` — `clean_index` zeroes it and collisions then
                // pick the OLDER level's value (pass-2 used to walk L6→L0).
                // `EntryKind::Delete` doubles as the "already processed" mark.
                if w_kind == EntryKind::Put {
                    if let Some(slot) = winners.get_mut(key) {
                        slot.1 = EntryKind::Delete; // mark processed
                    }
                    *pending_puts -= 1;
                    if passes(value)? {
                        *matched_live += 1;
                    }
                }
                Ok(())
            }

            // Newest-first: memtable before SSTs. The first Put met for a key
            // must be the pass-1 winner (same order as collect_prefix_winners).
            {
                let ws = self.write_state.read();
                if let Some(ref imm) = ws.immutable_memtable {
                    for entry in imm.scan_prefix(prefix) {
                        if pending_puts == 0 {
                            break;
                        }
                        try_count(
                            &mut winners,
                            &mut pending_puts,
                            &mut matched_live,
                            &mut passes,
                            &entry.key,
                            entry.seq_no,
                            entry.kind,
                            &entry.value,
                        )?;
                    }
                }
                for entry in ws.memtable.scan_prefix(prefix) {
                    if pending_puts == 0 {
                        break;
                    }
                    try_count(
                        &mut winners,
                        &mut pending_puts,
                        &mut matched_live,
                        &mut passes,
                        &entry.key,
                        entry.seq_no,
                        entry.kind,
                        &entry.value,
                    )?;
                }
            }

            let sst_handles = self.snapshot_sst_handles(prefix)?;
            for sst in &sst_handles {
                if pending_puts == 0 {
                    break;
                }
                let mut iter = sst.iter()?;
                iter.seek(prefix);
                while iter.is_valid() && pending_puts > 0 {
                    if !iter.key().starts_with(prefix) {
                        break;
                    }
                    try_count(
                        &mut winners,
                        &mut pending_puts,
                        &mut matched_live,
                        &mut passes,
                        iter.key(),
                        iter.seq_no(),
                        iter.kind(),
                        iter.value(),
                    )?;
                    iter.next();
                }
            }
        }

        Ok((matched_live, tombstones))
    }

    /// Snapshot open SST handles overlapping `prefix` (opens missing files).
    ///
    /// Order is **newest-first** (L0 → L6, within a level later flushes first),
    /// matching pass-1's `collect_prefix_winners` walk. Pass-2 relies on this
    /// so the first Put it meets for a key is the same entry pass-1 chose as
    /// winner — `seq` must not be used for matching (`clean_index` zeroes it).
    fn snapshot_sst_handles(&self, prefix: &[u8]) -> Result<Vec<Arc<SsTable>>> {
        let levels = self.levels.lock().unwrap_or_else(|e| e.into_inner());
        let mut cache = self.sst_cache.lock().unwrap_or_else(|e| e.into_inner());
        let mut handles = Vec::new();
        for level in levels.iter() {
            for sst_info in level.iter().rev() {
                if !prefix.is_empty() {
                    if sst_info.max_key.as_slice() < prefix {
                        continue;
                    }
                    if !Self::prefix_may_overlap(
                        prefix,
                        sst_info.min_key.as_slice(),
                        sst_info.max_key.as_slice(),
                    ) {
                        continue;
                    }
                }
                match cache.get(&sst_info.path) {
                    Some(sst) => handles.push(Arc::clone(sst)),
                    None => {
                        let sst = Arc::new(SsTable::open(&sst_info.path)?);
                        cache.insert(sst_info.path.clone(), Arc::clone(&sst));
                        handles.push(sst);
                    }
                }
            }
        }
        Ok(handles)
    }

    /// Visit every live (Put) entry under `prefix` exactly once, latest-version-wins,
    /// without materializing the full value set.
    ///
    /// Two-pass design:
    /// 1. Collect `key → (seq, kind)` only (O(keys) memory, no values).
    /// 2. Re-stream values and invoke `f(key, value)` for each winning Put.
    ///
    /// `f` sees each key at most once. Values are discarded after the callback —
    /// peak row memory is one value plus the key map.
    ///
    /// `overlay` (when provided) represents uncommitted write-buffer entries that
    /// win over storage: `Some(value)` is a Put, `None` is a Delete. Overlay keys
    /// suppress any storage version of the same key.
    pub fn for_each_live_prefix<F>(
        &self,
        prefix: &[u8],
        vis: Option<&crate::mvcc::Visibility>,
        overlay: Option<&std::collections::HashMap<Vec<u8>, Option<Vec<u8>>>>,
        mut f: F,
    ) -> Result<()>
    where
        F: FnMut(&[u8], &[u8]) -> Result<()>,
    {
        self.for_each_live_prefix_until(prefix, vis, overlay, |k, v| {
            f(k, v)?;
            Ok(true)
        })
    }

    /// Like [`Self::for_each_live_prefix`], but the callback returns `Ok(false)`
    /// to stop the scan early (e.g. once a LIMIT has been satisfied).
    pub fn for_each_live_prefix_until<F>(
        &self,
        prefix: &[u8],
        vis: Option<&crate::mvcc::Visibility>,
        overlay: Option<&std::collections::HashMap<Vec<u8>, Option<Vec<u8>>>>,
        mut f: F,
    ) -> Result<()>
    where
        F: FnMut(&[u8], &[u8]) -> Result<bool>,
    {
        self.drain_compaction_notifications();

        let is_visible = |seq: SeqNo| -> bool {
            match vis {
                Some(v) => v.is_visible(seq),
                None => true,
            }
        };

        let shadowed = |key: &[u8]| -> bool {
            overlay.is_some_and(|ov| ov.contains_key(key))
        };

        // ---- Pass 1: storage winners = key → (seq, kind), no values ----
        //
        // Version selection: newest-first source order + first-seen-wins.
        // Do NOT pick the winner by `seq` comparison — `seq` can be 0 when a
        // file was rewritten by clean_index / migration, and `seq > existing`
        // then keeps the OLDER level's version. LSM level order is always
        // trustworthy: new writes enter at the memtable and L0.
        let mut winners: HashMap<Vec<u8>, (SeqNo, EntryKind)> = HashMap::new();

        // Newest-first SST list: L0 before L1 … ; within a level, reverse
        // insertion order (later flushes are newer).
        // Retry on stale paths: compaction can delete a snapshotted SST before
        // we open it. No callback has run yet, so restarting is side-effect-free.
        let sst_handles: Vec<Arc<SsTable>> = {
            let mut handles = None;
            for _ in 0..3 {
                let sst_paths: Vec<PathBuf> = {
                    let levels = self.levels.lock().unwrap_or_else(|e| e.into_inner());
                    let mut paths = Vec::new();
                    for level in levels.iter() {
                        for sst_info in level.iter().rev() {
                            if !prefix.is_empty() {
                                if sst_info.max_key.as_slice() < prefix {
                                    continue;
                                }
                                if !Self::prefix_may_overlap(
                                    prefix,
                                    sst_info.min_key.as_slice(),
                                    sst_info.max_key.as_slice(),
                                ) {
                                    continue;
                                }
                            }
                            paths.push(sst_info.path.clone());
                        }
                    }
                    paths
                };
                match self.try_get_or_open_ssts(&sst_paths)? {
                    Some(h) => {
                        handles = Some(h);
                        break;
                    }
                    None => continue,
                }
            }
            match handles {
                Some(h) => h,
                None => return Ok(()), // give up: nothing readable in the prefix
            }
        };

        {
            let mut note = |key: Vec<u8>, seq: SeqNo, kind: EntryKind| {
                if shadowed(&key) {
                    return;
                }
                // First-seen wins: the first source to present a key is the
                // newest (we walk memtable → L0 → … → L6).
                if !winners.contains_key(&key) {
                    winners.insert(key, (seq, kind));
                }
            };

            // MemTables first (newest source) — active before immutable:
            // immutable is OLDER; scanning it first let a stale version win
            // first-seen-wins over a fresher tombstone/Put in the active
            // memtable (count-path resurrection window).
            {
                let ws = self.write_state.read();
                for entry in ws.memtable.scan_prefix(prefix) {
                    if is_visible(entry.seq_no) {
                        note(entry.key.clone(), entry.seq_no, entry.kind);
                    }
                }
                if let Some(ref imm) = ws.immutable_memtable {
                    for entry in imm.scan_prefix(prefix) {
                        if is_visible(entry.seq_no) {
                            note(entry.key.clone(), entry.seq_no, entry.kind);
                        }
                    }
                }
            }

            // Then SSTs, newest-first.
            for sst in &sst_handles {
                let mut iter = sst.iter()?;
                iter.seek(prefix);
                while iter.is_valid() {
                    if !iter.key().starts_with(prefix) {
                        break;
                    }
                    if is_visible(iter.seq_no()) {
                        note(iter.key().to_vec(), iter.seq_no(), iter.kind());
                    }
                    iter.next();
                }
            }
        }

        // ---- Pass 2: re-stream values, emit winning Puts, drop value after f ----
        // consumed[kind=Delete] is used as a "already emitted" marker for Puts.
        let mut pending_puts = winners
            .values()
            .filter(|(_, kind)| *kind == EntryKind::Put)
            .count();

        {
            // fn (not closure) so we can still read `pending_puts` in the loops
            fn try_emit<F>(
                winners: &mut HashMap<Vec<u8>, (SeqNo, EntryKind)>,
                pending_puts: &mut usize,
                f: &mut F,
                key: &[u8],
                _seq: SeqNo,
                kind: EntryKind,
                value: &[u8],
            ) -> Result<bool>
            where
                F: FnMut(&[u8], &[u8]) -> Result<bool>,
            {
                if kind != EntryKind::Put || *pending_puts == 0 {
                    return Ok(true);
                }
                let Some((_, w_kind)) = winners.get(key).copied() else {
                    return Ok(true);
                };
                // First Put in newest-first order is the pass-1 winner. Do NOT
                // match on `seq` — `clean_index` zeroes it and collisions then
                // pick the OLDER level's value (pass-2 used to walk L6→L0).
                if w_kind == EntryKind::Put {
                    if let Some(slot) = winners.get_mut(key) {
                        slot.1 = EntryKind::Delete; // mark emitted
                    }
                    *pending_puts -= 1;
                    return f(key, value);
                }
                Ok(true)
            }

            // Newest-first: memtable before SSTs, so the first Put met for a
            // key is the same entry pass-1 chose as winner.
            {
                let ws = self.write_state.read();
                if let Some(ref imm) = ws.immutable_memtable {
                    for entry in imm.scan_prefix(prefix) {
                        if pending_puts == 0 {
                            break;
                        }
                        if is_visible(entry.seq_no) {
                            let keep_going = try_emit(
                                &mut winners,
                                &mut pending_puts,
                                &mut f,
                                &entry.key,
                                entry.seq_no,
                                entry.kind,
                                &entry.value,
                            )?;
                            if !keep_going {
                                return Ok(());
                            }
                        }
                    }
                }
                for entry in ws.memtable.scan_prefix(prefix) {
                    if pending_puts == 0 {
                        break;
                    }
                    if is_visible(entry.seq_no) {
                        let keep_going = try_emit(
                            &mut winners,
                            &mut pending_puts,
                            &mut f,
                            &entry.key,
                            entry.seq_no,
                            entry.kind,
                            &entry.value,
                        )?;
                        if !keep_going {
                            return Ok(());
                        }
                    }
                }
            }

            let sst_handles = self.snapshot_sst_handles(prefix)?;
            for sst in &sst_handles {
                if pending_puts == 0 {
                    break;
                }
                let mut iter = sst.iter()?;
                iter.seek(prefix);
                while iter.is_valid() {
                    if !iter.key().starts_with(prefix) {
                        break;
                    }
                    if is_visible(iter.seq_no()) {
                        let keep_going = try_emit(
                            &mut winners,
                            &mut pending_puts,
                            &mut f,
                            iter.key(),
                            iter.seq_no(),
                            iter.kind(),
                            iter.value(),
                        )?;
                        if !keep_going {
                            return Ok(());
                        }
                    }
                    iter.next();
                }
            }
        }

        // Overlay Puts win outright and may not exist in storage at all.
        if let Some(ov) = overlay {
            for (key, val) in ov {
                if key.starts_with(prefix) {
                    if let Some(v) = val {
                        if !f(key, v)? {
                            return Ok(());
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Transaction-aware streaming visit: same as [`Self::for_each_live_prefix`]
    /// but overlays the transaction's own write buffer (uncommitted puts/deletes
    /// win over committed storage).
    pub fn txn_for_each_live_prefix<F>(&self, txn_id: SeqNo, prefix: &[u8], mut f: F) -> Result<()>
    where
        F: FnMut(&[u8], &[u8]) -> Result<()>,
    {
        let (vis, buf) = {
            let ws = self.write_state.read();
            let vis = ws.txn_manager.visibility_for(txn_id);
            let buf: Vec<(Vec<u8>, WriteOp)> = ws
                .txn_manager
                .get(txn_id)
                .map(|t| {
                    t.write_buffer_iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect()
                })
                .unwrap_or_default();
            (vis, buf)
        };
        let overlay: std::collections::HashMap<Vec<u8>, Option<Vec<u8>>> = buf
            .into_iter()
            .map(|(k, op)| match op {
                WriteOp::Put(v) => (k, Some(v)),
                WriteOp::Delete => (k, None),
            })
            .collect();
        self.for_each_live_prefix(prefix, Some(&vis), Some(&overlay), &mut f)
    }

    /// Checks if a prefix could match any key in the range [min_key, max_key].
    ///
    /// A key starting with `prefix` exists in [min_key, max_key] iff:
    /// 1. prefix <= max_key (some prefixed key is not past the end), AND
    /// 2. min_key starts with prefix (min_key itself has this prefix), OR
    ///    min_key < prefix (the smallest prefixed key is after min_key).
    fn prefix_may_overlap(prefix: &[u8], min_key: &[u8], max_key: &[u8]) -> bool {
        // Condition 2: can some key starting with `prefix` reach min_key?
        // True when min_key already carries the prefix (it is itself a match
        // candidate), or min_key sorts before the prefix range entirely.
        if !(min_key.starts_with(prefix) || min_key < prefix) {
            return false;
        }

        // Condition 1: can some key starting with `prefix` reach max_key?
        // The last possible key with this prefix is prefix + 0xFF..., which is
        // effectively "infinity", so we only need: max_key >= prefix.
        //
        // Byte comparison with different lengths is tricky. Compare up to the
        // shorter length; if equal up to that point, the longer key is "greater".
        let cmp_len = prefix.len().min(max_key.len());
        let cmp = &max_key[..cmp_len].cmp(&prefix[..cmp_len]);
        match cmp {
            std::cmp::Ordering::Less => false,     // max_key < prefix → no overlap
            std::cmp::Ordering::Greater => true,   // max_key > prefix → overlap
            std::cmp::Ordering::Equal => {
                // Equal up to shorter length: overlap if max_key is at least as long as prefix
                max_key.len() >= prefix.len()
            }
        }
    }

    /// Deletes a key (writes a tombstone).
    pub fn delete(&self, key: Key) -> Result<()> {
        let seq = self.next_seq();
        let entry = Entry::delete(key.clone(), seq);

        let needs_flush = {
            let mut ws = self.write_state.write();
            ws.wal.append(&entry)?;
            ws.wal_pending_count += 1;
            // Batch flush: only flush WAL buffer every 64 writes
            if ws.wal_pending_count >= 64 {
                ws.wal.flush_buf()?;
                ws.wal_pending_count = 0;
            }
            ws.memtable.delete_with_seq(key, seq);
            ws.memtable.size() >= self.options.memtable_size_limit
                || ws.wal.needs_rotation()
        };

        if needs_flush {
            self.flush_memtable()?;
        }

        Ok(())
    }

    /// Deletes a row and removes its entries from secondary and vector indexes.
    ///
    /// [`Self::delete`] only writes an LSM tombstone — index entries survive and
    /// later COUNT/SELECT via index return stale pks. TRUNCATE and any bulk
    /// delete path must go through here.
    pub fn delete_with_deindex(&self, key: Key, value: &[u8]) -> Result<()> {
        if let Some(class) = Self::extract_class_from_key(&key) {
            if let Some(doc) = parse_doc_bytes(value) {
                self.index_manager
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .deindex_document(&class, &key, &doc);
                self.vector_index_manager
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .deindex_vectors(&key);
            }
        }
        self.delete(key)
    }

    /// Manually flushes the current MemTable to an SSTable.
    /// Call this after a batch of writes to ensure all data is persisted.
    pub fn flush(&self) -> Result<()> {
        let needs_flush = {
            let ws = self.write_state.read();
            !ws.memtable.is_empty()
        };
        if needs_flush {
            self.flush_memtable()?;
        }
        Ok(())
    }

    // ── Value Metadata (Live Data) ──────────────────────────────

    /// Gets value metadata for an entity, computing real-time decay.
    /// Returns None if no metadata exists (old data without meta → defaults to score 1.0).
    pub fn get_value_meta(
        &self,
        class: &str,
        pk: &str,
    ) -> Result<Option<crate::value_meta::ValueMetadata>> {
        let key = crate::value_meta::ValueMetadata::meta_key(class, pk);
        match self.get(&key)? {
            Some(bytes) => Ok(crate::value_meta::ValueMetadata::from_bytes(&bytes)),
            None => Ok(None),
        }
    }

    /// Gets the current decayed score for an entity.
    /// Returns 1.0 if no metadata exists (old data is assumed high-value).
    pub fn get_value_score(&self, class: &str, pk: &str) -> Result<f64> {
        match self.get_value_meta(class, pk)? {
            Some(meta) => Ok(meta.current_score()),
            None => Ok(1.0), // Old data without meta defaults to max score
        }
    }

    /// Puts value metadata for an entity.
    pub fn put_value_meta(
        &self,
        class: &str,
        pk: &str,
        meta: &crate::value_meta::ValueMetadata,
    ) -> Result<()> {
        let key = crate::value_meta::ValueMetadata::meta_key(class, pk);
        let value = meta.to_bytes();
        self.put(key, value)
    }

    /// Activates an entity: resets decay baseline and boosts value_score.
    /// Creates metadata if it doesn't exist.
    pub fn activate(&self, class: &str, pk: &str, delta: f64, _reason: &str) -> Result<()> {
        let mut meta = match self.get_value_meta(class, pk)? {
            Some(m) => m,
            None => crate::value_meta::ValueMetadata::new(
                self.options.default_lambda,
                self.options.default_lambda,
            ),
        };
        meta.activate(delta);
        self.put_value_meta(class, pk, &meta)
    }
}

impl Drop for LsmEngine {
    fn drop(&mut self) {
        // Flush any remaining WAL buffer to OS cache
        if let Some(mut ws) = self
            .write_state
            .try_write_for(std::time::Duration::from_millis(100))
        {
            if ws.wal_pending_count > 0 {
                let _ = ws.wal.flush_buf();
                ws.wal_pending_count = 0;
            }
            // fsync to ensure data survives crash
            let _ = ws.wal.sync();
        }
    }
}

impl LsmEngine {
    /// Flushes the current MemTable to an SSTable on disk.
    fn flush_memtable(&self) -> Result<()> {
        // Serialize flushes: without this guard two concurrent flushes swap
        // into the single `immutable_memtable` slot and the loser's entries
        // are silently dropped (lost Puts/Deletes under bulk load).
        let _flush_guard = self.flush_lock.lock().unwrap_or_else(|e| e.into_inner());

        // A previously failed flush may have left an immutable memtable behind.
        // Persist it instead of overwriting the slot — overwrite loses data.
        if self.write_state.read().immutable_memtable.is_some() {
            tracing::warn!("flush_memtable: persisting leftover immutable memtable");
            self.persist_immutable_to_sst()?;
        }

        // Step 1: Swap current MemTable to immutable (brief lock)
        {
            let mut ws = self.write_state.write();
            // Flush any pending WAL writes before swapping
            if ws.wal_pending_count > 0 {
                ws.wal.flush_buf()?;
                ws.wal_pending_count = 0;
            }
            let old_mem = std::mem::take(&mut ws.memtable);
            ws.immutable_memtable = Some(old_mem);
        }

        self.persist_immutable_to_sst()?;

        // Notify the background compaction worker
        let _ = self
            .compaction_sender
            .send(CompactionMsg::Flushed { level: 0 });

        // Persist dirty HNSW nodes to the file snapshot when the dirty set
        // is large. Small bursts stay in memory and are rebuilt from
        // documents on restart.
        if let Err(e) = self.persist_vector_deltas(false) {
            tracing::warn!("Failed to persist vector snapshot on flush: {}", e);
        }

        Ok(())
    }

    /// Builds an SST from the current immutable memtable, installs it, clears
    /// the slot, and rewrites the WAL to cover only unpersisted entries.
    /// Caller must hold `flush_lock`.
    fn persist_immutable_to_sst(&self) -> Result<()> {
        // Step 2: Snapshot immutable MemTable entries (brief lock), then build SSTable outside lock
        let sst_id = self.sst_counter.fetch_add(1, Ordering::Relaxed);
        let sst_path = self.options.data_dir.join(format!("L0_{}.sst", sst_id));
        let (mut entries_snapshot, _) = {
            let ws = self.write_state.read();
            if let Some(ref imm) = ws.immutable_memtable {
                // Use the flat log, not BTreeMap::values(): the inlined std
                // iterator can treat a leaf as an internal node and load a
                // neighbouring entry's `key.len` (e.g. 0x80) as a child
                // pointer, then AV at *(ptr+0x3D2). See ptr_guard / memtable.
                let entries = imm.snapshot_flat();
                (entries, ())
            } else {
                return Ok(());
            }
        };

        // Empty memtable → nothing to persist. Building a 0-entry SST here
        // creates a permanent "old SST" zombie (footer max_seq_no=0) and was
        // observed bursting 6-7 empty L0 files per reset_wal_for_index_cleanup.
        if entries_snapshot.is_empty() {
            let mut ws = self.write_state.write();
            ws.immutable_memtable = None;
            return Ok(());
        }

        // Build SSTable from snapshot OUTSIDE the lock (disk I/O).
        //
        // Sort by user key (then seq descending) before adding to the builder.
        // MemTable stores composite keys `user_key ++ !seq`, which mis-orders
        // entries when one user key is a prefix of another (e.g. `CHEMBL1`
        // vs `CHEMBL10`): the inverted-seq suffix of the shorter key sorts
        // after the longer key's next byte, so `entries()` is not user-key
        // sorted. The SST builder/lookup assume user-key order and silently
        // miss keys in the middle (observed as lost rows after flush).
        entries_snapshot.sort_by(|a, b| {
            a.0.cmp(&b.0).then(b.2.cmp(&a.2)) // user key asc, seq desc
        });
        let min_key = entries_snapshot
            .first()
            .map(|(k, _, _, _)| k.clone())
            .unwrap_or_default();
        let max_key = entries_snapshot
            .last()
            .map(|(k, _, _, _)| k.clone())
            .unwrap_or_default();
        // Use add_owned + into_iter to avoid cloning key/value a second time.
        let mut builder = SsTableBuilder::with_expected_entries(entries_snapshot.len());
        builder.set_compression_level(self.options.compression_level);
        for (key, value, seq_no, kind) in entries_snapshot {
            builder.add_owned(Entry {
                key,
                value,
                seq_no,
                kind,
            });
        }
        builder.build(&sst_path)?;

        // Step 3: Add to shared levels (brief lock)
        let metadata = fs::metadata(&sst_path)?;
        {
            let mut levels = self.levels.lock().unwrap_or_else(|e| e.into_inner());
            levels[0].push(SsTableInfo {
                path: sst_path.clone(),
                size: metadata.len(),
                min_key,
                max_key,
            });
        }

        // Step 4: Insert SST into cache + clear immutable MemTable + reset WAL (single lock).
        // The SST must be in sst_cache BEFORE clearing immutable_memtable to prevent
        // commit_txn's get_from_locked() from missing data in the window between
        // immutable MT cleared and the next lazy SST open.
        let new_sst = SsTable::open(&sst_path)?;
        {
            self.sst_cache
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(sst_path, Arc::new(new_sst));
        }
        {
            let mut ws = self.write_state.write();
            ws.immutable_memtable = None;
            self.reset_wal_internal(&mut ws)?;
        }

        Ok(())
    }

    /// Drains compaction notifications from the background worker.
    /// Evicts stale SSTable cache entries when compaction replaces files.
    ///
    /// Lock ordering: compaction_notif_receiver → sst_cache (never reversed).
    /// The fast path (no compaction) only checks an atomic flag — no lock acquisition.
    fn drain_compaction_notifications(&self) {
        // Fast path: skip entirely if no compaction has occurred since last drain.
        if !self.compaction_pending.load(Ordering::Acquire) {
            return;
        }

        // Drain notifications under the receiver lock only (NOT holding write_state).
        let mut evicted_paths: Vec<PathBuf> = Vec::new();
        {
            let receiver = self
                .compaction_notif_receiver
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            while let Ok(notif) = receiver.try_recv() {
                match notif {
                    CompactionNotification::Compacted {
                        evicted_paths: paths,
                    } => {
                        evicted_paths.extend(paths);
                    }
                    CompactionNotification::FlushDone => {}
                }
            }
        }

        // Selectively remove only the evicted SST paths from the cache.
        if !evicted_paths.is_empty() {
            let mut cache = self.sst_cache.lock().unwrap_or_else(|e| e.into_inner());
            for path in &evicted_paths {
                cache.remove(path);
            }
        }

        // Clear the flag AFTER cache invalidation to ensure correctness.
        self.compaction_pending.store(false, Ordering::Release);
    }

    /// Blocks until all pending background compaction work is complete.
    /// Useful for testing and graceful shutdown.
    pub fn flush_compaction(&self) -> Result<()> {
        // Drain any pending notifications first
        self.drain_compaction_notifications();

        let _ = self.compaction_sender.send(CompactionMsg::FlushAndNotify);
        let receiver = self
            .compaction_notif_receiver
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut evicted_paths: Vec<PathBuf> = Vec::new();
        loop {
            match receiver.recv_timeout(std::time::Duration::from_secs(10)) {
                Ok(CompactionNotification::FlushDone) => break,
                Ok(CompactionNotification::Compacted {
                    evicted_paths: paths,
                }) => {
                    evicted_paths.extend(paths);
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    tracing::warn!("flush_compaction timed out waiting for FlushDone");
                    break;
                }
                Err(_) => break,
            }
        }
        drop(receiver);
        if !evicted_paths.is_empty() {
            let mut cache = self.sst_cache.lock().unwrap_or_else(|e| e.into_inner());
            for path in &evicted_paths {
                cache.remove(path);
            }
        }
        Ok(())
    }

    /// Resets the WAL file after a successful flush.
    ///
    /// If WAL archiving is enabled, copies the current WAL to the archive directory
    /// before resetting. This enables incremental backup.
    ///
    /// Uses write-new-then-rename for atomicity:
    /// 1. Archive current WAL (if enabled)
    /// 2. Create a new WAL at a temp path
    /// 3. Atomically rename temp → wal.log
    fn reset_wal_internal(&self, ws: &mut WriteState) -> Result<()> {
        // Rebuild the WAL so it covers exactly the unpersisted entries (the
        // active memtable). A blind file replace drops records appended during
        // the SST build window — those writes live in the active memtable and
        // must stay recoverable (observed as post-restart data loss).
        let wal_path = self.options.data_dir.join("wal.log");
        let tmp_path = self.options.data_dir.join("wal.log.tmp");

        // Archive current WAL before resetting (if enabled)
        if let Some(ref archive_dir) = self.options.wal_archive_dir {
            if wal_path.exists() {
                self.archive_wal(&wal_path, archive_dir)?;
            }
        }

        // If a stale temp file exists from a previous crash, remove it
        if tmp_path.exists() {
            fs::remove_file(&tmp_path)?;
        }

        // Build the new WAL from every entry not yet persisted to an SST.
        let mut new_wal = Wal::open(&tmp_path)?;
        for e in ws.memtable.entries() {
            match e.kind {
                EntryKind::Put => {
                    let entry = Entry::put(e.key.clone(), e.value.clone(), e.seq_no);
                    new_wal.append(&entry)?;
                }
                EntryKind::Delete => {
                    let entry = Entry::delete(e.key.clone(), e.seq_no);
                    new_wal.append(&entry)?;
                }
            }
        }
        new_wal.flush_buf()?;
        new_wal.sync()?;

        // Atomically replace the old WAL
        fs::rename(&tmp_path, &wal_path)?;

        ws.wal = new_wal;
        ws.wal_pending_count = 0;
        Ok(())
    }

    /// Archives a WAL file by copying it to the archive directory with a timestamp.
    fn archive_wal(&self, wal_path: &Path, archive_dir: &Path) -> Result<()> {
        fs::create_dir_all(archive_dir)?;

        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let seq = self.seq_counter.load(Ordering::Relaxed);
        let archive_name = format!("wal_{}_{:08}.log", timestamp, seq);
        let archive_path = archive_dir.join(&archive_name);

        fs::copy(wal_path, &archive_path)?;
        tracing::debug!("WAL archived to {}", archive_path.display());

        if self.options.wal_archive_max_files > 0 {
            self.cleanup_old_archives(archive_dir)?;
        }
        Ok(())
    }

    /// Removes oldest archived WAL files when limit is exceeded.
    fn cleanup_old_archives(&self, archive_dir: &Path) -> Result<()> {
        let max_files = self.options.wal_archive_max_files;
        if max_files == 0 {
            return Ok(());
        }

        let mut entries: Vec<_> = fs::read_dir(archive_dir)?
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path()
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with("wal_") && n.ends_with(".log"))
                    .unwrap_or(false)
            })
            .collect();

        if entries.len() <= max_files {
            return Ok(());
        }

        entries.sort_by_key(|e| e.file_name());
        let to_delete = entries.len() - max_files;
        for entry in entries.iter().take(to_delete) {
            let _ = fs::remove_file(entry.path());
            tracing::debug!("Deleted old WAL archive: {:?}", entry.path());
        }
        Ok(())
    }

    fn next_seq(&self) -> SeqNo {
        // AcqRel is required for snapshot isolation correctness.
        // Batch allocation with thread_local breaks snapshot isolation
        // because sequence numbers within a batch are assigned locally
        // without memory ordering guarantees across threads.
        self.seq_counter.fetch_add(1, Ordering::AcqRel)
    }

    /// Opens persisted `.idx` files into the index manager.
    /// Called from the background index-rebuild thread at startup so that
    /// secondary indexes survive restarts without a full LSM re-scan.
    pub fn open_disk_indexes(&self) -> std::result::Result<(), String> {
        self.index_manager
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .open_disk_indexes()
    }

    /// Rebuilds secondary indexes by scanning persisted index entries from SSTables.
    /// Can be called from a background thread after engine startup.
    ///
    /// v1→v2 migration: if no `__idx2__` entries exist but legacy `__idx__`
    /// entries do, the old index entries and `.idx` files are dropped entirely
    /// and indexes are rebuilt from the source documents (JSON values are
    /// exact — no heuristic decoding needed).
    pub fn rebuild_indexes(&self) -> Result<()> {
        // Cheap liveness probe first — avoids materializing the whole keyspace
        // just to discover there is nothing to do.
        let (v2_live, _) = self.count_prefix(b"__idx2__")?;
        if v2_live == 0 {
            // v1 → v2 migration: drop old, rebuild from documents
            let (v1_live, _) = self.count_prefix(b"__idx__")?;
            if v1_live > 0 {
                let v1_entries = self.scan_prefix(b"__idx__")?;
                if !v1_entries.is_empty() {
                    tracing::info!(
                        "Found {} legacy __idx__ entries, dropping and rebuilding from documents",
                        v1_entries.len()
                    );
                    self.migrate_v1_drop_and_rebuild(v1_entries)?;
                    // fall through and rebuild the migrated __idx2__ entries
                }
            }
        }

        // Discover (class, column) pairs from key headers only — never holds
        // the full entry set. The previous path did scan_prefix(b"__idx2__")
        // over the entire keyspace and OOM'd at 26M+ entries.
        let discovered = self.collect_index_columns()?;
        if discovered.is_empty() {
            return Ok(());
        }

        let active_keys: std::collections::HashSet<(String, String)> = self
            .index_manager
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .all_index_keys()
            .into_iter()
            .collect();

        let mut total_rebuilt = 0usize;
        let mut orphaned = 0usize;

        for (class, column) in discovered {
            let prefix = crate::index::IndexManager::make_index_prefix(&class, &column);
            let is_active = active_keys.contains(&(class.clone(), column.clone()));

            // Count live entries without copying values. Peak RAM is one
            // index's key set, and it is released before the next index.
            let (live, _) = self.count_prefix(&prefix)?;
            if live == 0 {
                continue;
            }

            // Large disk-only index whose .idx already matches the LSM count:
            // skip both the entry scan and the disk rewrite.
            let keep_mem = self
                .index_manager
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .keep_in_memory(live);
            let disk_sync = !keep_mem
                && self
                    .index_manager
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .disk_in_sync(&class, &column, live);

            if disk_sync {
                tracing::info!(
                    table = %class,
                    column = %column,
                    entries = live,
                    "Disk index already in sync, skipping rebuild"
                );
                total_rebuilt += live;
                if !is_active {
                    orphaned += live;
                    let entries = self.scan_prefix(&prefix)?;
                    self.cleanup_orphaned_index_entries(&entries, &active_keys)?;
                }
                continue;
            }

            // Materialize only this index's entries, rebuild, then release.
            let entries = self.scan_prefix(&prefix)?;
            if entries.is_empty() {
                continue;
            }
            self.index_manager
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .rebuild_from_entries(&entries);
            total_rebuilt += entries.len();

            // Orphan cleanup AFTER rebuild so InMemory restart still sees the
            // index (LSM `__idx2__` is its only persistence).
            if !is_active {
                orphaned += entries.len();
                tracing::warn!(
                    table = %class,
                    column = %column,
                    orphaned_index_entries = entries.len(),
                    "Found orphaned index entries from interrupted CREATE INDEX. \
                     Writing tombstones and flushing to reclaim WAL space."
                );
                self.cleanup_orphaned_index_entries(&entries, &active_keys)?;
            }
        }

        if orphaned > 0 {
            tracing::warn!(
                orphaned_index_entries = orphaned,
                "Orphaned index entries tombstoned"
            );
        }

        tracing::info!(
            "Rebuilt {} index entries across {} indexes ({} orphaned)",
            total_rebuilt,
            self.index_manager
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .index_count(),
            orphaned
        );

        // If the WAL contains stale index entries (from interrupted CREATE INDEX),
        // flush memtable then reset it. This reclaims the hundreds of MB of WAL space.
        if total_rebuilt > 10000 {
            if let Err(e) = self.reset_wal_for_index_cleanup() {
                tracing::warn!("Failed to reset WAL after index rebuild: {}", e);
            }
        }
        Ok(())
    }

    /// Collect distinct (class, column) pairs appearing under `__idx2__`.
    /// Walks keys only — values and entry bodies are never retained.
    fn collect_index_columns(
        &self,
    ) -> Result<std::collections::HashSet<(String, String)>> {
        self.drain_compaction_notifications();
        let prefix: &[u8] = b"__idx2__";
        let mut cols: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();

        let mut note_key = |key: &[u8]| {
            if let Some((class, column, _, _)) =
                crate::index::IndexManager::parse_index_key(key)
            {
                cols.insert((class, column));
            }
        };

        // MemTables first (newest source).
        {
            let ws = self.write_state.read();
            for entry in ws.memtable.scan_prefix(prefix) {
                note_key(&entry.key);
            }
            if let Some(ref imm) = ws.immutable_memtable {
                for entry in imm.scan_prefix(prefix) {
                    note_key(&entry.key);
                }
            }
        }

        // SSTables, newest-first, with the same prefix pruning as scan_prefix.
        let sst_handles: Vec<Arc<SsTable>> = {
            let levels = self.levels.lock().unwrap_or_else(|e| e.into_inner());
            let mut cache = self.sst_cache.lock().unwrap_or_else(|e| e.into_inner());
            let mut handles = Vec::new();
            for level in levels.iter() {
                for sst_info in level.iter().rev() {
                    if sst_info.max_key.as_slice() < prefix {
                        continue;
                    }
                    if !Self::prefix_may_overlap(
                        prefix,
                        sst_info.min_key.as_slice(),
                        sst_info.max_key.as_slice(),
                    ) {
                        continue;
                    }
                    match cache.get(&sst_info.path) {
                        Some(sst) => handles.push(Arc::clone(sst)),
                        None => {
                            let sst = Arc::new(SsTable::open(&sst_info.path)?);
                            cache.insert(sst_info.path.clone(), Arc::clone(&sst));
                            handles.push(sst);
                        }
                    }
                }
            }
            handles
        };

        for sst in &sst_handles {
            let mut iter = sst.iter()?;
            iter.seek(prefix);
            while iter.is_valid() {
                if !iter.key().starts_with(prefix) {
                    break;
                }
                note_key(iter.key());
                iter.next();
            }
        }

        drop(note_key);
        Ok(cols)
    }

    /// Reset the WAL file after index rebuild cleanup.
    ///
    /// The memtable is flushed to SST *before* the WAL is truncated. Index
    /// entries written during rebuild live in memtable + WAL; truncating the
    /// WAL first would leave them unrecoverable if the process crashed before
    /// the next memtable flush.
    fn reset_wal_for_index_cleanup(&self) -> Result<()> {
        // Persist memtable to SST so every entry is durable on disk.
        self.flush_memtable()?;

        let wal_path = self.options.data_dir.join("wal.log");
        let old_size = std::fs::metadata(&wal_path).map(|m| m.len()).unwrap_or(0);

        // Truncate under the write lock so no concurrent put() can append to
        // the WAL between the emptiness check and the file reset.
        let mut ws = self.write_state.write();
        if !ws.memtable.is_empty() {
            // Writes arrived during flush_memtable; their WAL records must stay.
            tracing::warn!(
                entries = ws.memtable.len(),
                "Skip WAL reset: memtable not empty after flush"
            );
            return Ok(());
        }
        ws.wal.flush_buf()?;
        std::fs::write(&wal_path, [])?;
        ws.wal = Wal::open(&wal_path)?;
        drop(ws);

        tracing::info!(
            old_wal_bytes = old_size,
            "Reset WAL after index rebuild cleanup"
        );
        Ok(())
    }

    /// v1→v2 migration: drop all old index artifacts and rebuild from documents.
    ///
    /// 1. Collect (class, column) pairs from legacy `__idx__` entries
    /// 2. Delete all `.idx` disk index files (old encoding)
    /// 3. Tombstone all legacy `__idx__` LSM entries (batch, releases lock)
    /// 4. Rebuild each index from source documents via `create_index` backfill
    fn migrate_v1_drop_and_rebuild(&self, v1_entries: Vec<(Vec<u8>, Vec<u8>)>) -> Result<()> {
        use std::collections::BTreeSet;

        // Step 1: extract (class, column) pairs from legacy entries
        let mut index_specs: BTreeSet<(String, String)> = BTreeSet::new();
        let mut old_keys: Vec<Vec<u8>> = Vec::with_capacity(v1_entries.len());
        for (key, _) in &v1_entries {
            old_keys.push(key.clone());
            if let Some((class, column, _, _)) =
                crate::index::IndexManager::parse_index_key_v1(key)
            {
                index_specs.insert((class, column));
            }
        }
        tracing::info!(
            "Migration: {} legacy entries, {} index(es) to rebuild: {:?}",
            old_keys.len(),
            index_specs.len(),
            index_specs
        );

        // Step 2: delete .idx disk index files (old encoding is invalid)
        {
            let mgr = self
                .index_manager
                .read()
                .unwrap_or_else(|e| e.into_inner());
            for (class, column) in &index_specs {
                if let Some(path) = mgr.data_dir_ref().map(|d| {
                    d.join(format!("{}_{}.idx", class, column))
                }) {
                    if path.exists() {
                        let _ = std::fs::remove_file(&path);
                        tracing::info!("Deleted stale index file: {:?}", path);
                    }
                }
            }
        }

        // Step 3: tombstone all legacy `__idx__` entries in batches
        // (holds write lock only per batch, flushes WAL once per batch)
        for chunk in old_keys.chunks(10_000) {
            let mut ws = self.write_state.write();
            for key in chunk {
                let seq = self.next_seq();
                let entry = Entry::delete(key.clone(), seq);
                ws.wal.append(&entry)?;
                ws.memtable.delete_with_seq(key.clone(), seq);
            }
            ws.wal.flush_buf()?;
            // lock released at end of iteration
        }
        tracing::info!("Migration: tombstoned {} legacy index entries", old_keys.len());

        // Step 4: rebuild each index from documents
        // Drop stale in-memory state first so create_index starts clean.
        {
            let mut mgr = self
                .index_manager
                .write()
                .unwrap_or_else(|e| e.into_inner());
            for (class, column) in &index_specs {
                mgr.drop_index(class, column);
            }
        }
        for (class, column) in &index_specs {
            tracing::info!("Migration: rebuilding {}.{} from documents", class, column);
            self.create_index(class, column)?;
        }
        tracing::info!(
            "Migration complete: rebuilt {} index(es) from source documents",
            index_specs.len()
        );
        Ok(())
    }

    /// Write delete tombstones for orphaned index entries so compaction can reclaim them.
    fn cleanup_orphaned_index_entries(
        &self,
        entries: &[(Vec<u8>, Vec<u8>)],
        active_keys: &std::collections::HashSet<(String, String)>,
    ) -> Result<()> {
        let mut ws = self.write_state.write();
        let mut cleaned = 0usize;
        for (key, _value) in entries {
            if let Some((class, column, _, _)) =
                crate::index::IndexManager::parse_index_key(key)
            {
                if !active_keys.contains(&(class, column)) {
                    let seq = self.next_seq();
                    let entry = Entry::delete(key.clone(), seq);
                    ws.wal.append(&entry)?;
                    ws.memtable.delete_with_seq(key.clone(), seq);
                    cleaned += 1;
                }
            }
        }
        if cleaned > 0 {
            ws.wal.flush_buf()?;
            tracing::info!(cleaned = cleaned, "Wrote tombstones for orphaned index entries");
        }
        Ok(())
    }

    /// Validates all registered indexes by performing a probe (insert + lookup + delete).
    /// Corrupted indexes are logged and removed so they can be rebuilt on demand.
    fn validate_indexes(&self) {
        let index_keys: Vec<(String, String)> = self
            .index_manager
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .all_index_keys();

        let mut corrupted = Vec::new();
        for (class, column) in &index_keys {
            let probe_pk = b"__probe_pk__";
            let mut mgr = self
                .index_manager
                .write()
                .unwrap_or_else(|e| e.into_inner());
            // Insert probe entry
            mgr.index_document(
                class,
                probe_pk,
                &serde_json::Map::from_iter(vec![(
                    column.clone(),
                    serde_json::Value::String("__validate_probe__".to_string()),
                )]),
            );
            // Lookup to verify
            let results = mgr.lookup_eq(
                class,
                column,
                &serde_json::Value::String("__validate_probe__".to_string()),
            );
            // Remove probe entry
            mgr.deindex_document(
                class,
                probe_pk,
                &serde_json::Map::from_iter(vec![(
                    column.clone(),
                    serde_json::Value::String("__validate_probe__".to_string()),
                )]),
            );
            drop(mgr);

            match results {
                Some(ref pks) if pks.iter().any(|pk| pk == probe_pk) => {
                    // Index is functional
                }
                _ => {
                    tracing::warn!(
                        table = class.as_str(),
                        column = column.as_str(),
                        "Index validation failed — marking for removal"
                    );
                    corrupted.push((class.clone(), column.clone()));
                }
            }
        }

        // Remove corrupted indexes
        if !corrupted.is_empty() {
            let mut mgr = self
                .index_manager
                .write()
                .unwrap_or_else(|e| e.into_inner());
            for (class, column) in &corrupted {
                mgr.drop_index(class, column);
                tracing::warn!(
                    table = class.as_str(),
                    column = column.as_str(),
                    "Removed corrupted index — will be rebuilt on next CREATE INDEX"
                );
            }
            tracing::info!(
                count = corrupted.len(),
                "Index validation complete — removed corrupted indexes"
            );
        }
    }

    /// Rebuilds vector indexes by scanning persisted metadata and document data.
    ///
    /// First tries to load persisted HNSW graph structures (fast path).
    /// Falls back to rebuilding from document data if graph structures not found.
    pub fn rebuild_vector_indexes(&self) -> Result<()> {
        let meta_entries = self.scan_prefix(b"__vec_meta__")?;
        if meta_entries.is_empty() {
            return Ok(());
        }
        tracing::info!(
            count = meta_entries.len(),
            "Vector index rebuild starting (snapshot file / document backfill)"
        );

        // Parse and recreate vector indexes from metadata
        let mut index_configs: Vec<(
            String,
            String,
            usize,
            crate::vector::DistanceMetric,
            usize,
            usize,
            usize,
        )> = Vec::new();
        for (_key, val_bytes) in &meta_entries {
            if let Ok(meta) = serde_json::from_slice::<serde_json::Value>(val_bytes) {
                let class = meta["class"].as_str().unwrap_or("").to_string();
                let column = meta["column"].as_str().unwrap_or("").to_string();
                let dimension = meta["dimension"].as_u64().unwrap_or(0) as usize;
                let metric_str = meta["metric"].as_str().unwrap_or("Cosine");
                let metric = match metric_str {
                    "L2" => crate::vector::DistanceMetric::L2,
                    "InnerProduct" => crate::vector::DistanceMetric::InnerProduct,
                    "Tanimoto" | "Jaccard" => crate::vector::DistanceMetric::Tanimoto,
                    "Hamming" => crate::vector::DistanceMetric::Hamming,
                    _ => crate::vector::DistanceMetric::Cosine,
                };
                let m = meta["m"].as_u64().unwrap_or(8) as usize;
                let ef_construction = meta["ef_construction"].as_u64().unwrap_or(32) as usize;
                let ef_search = meta["ef_search"].as_u64().unwrap_or(64) as usize;

                if !class.is_empty() && !column.is_empty() && dimension > 0 {
                    index_configs.push((
                        class,
                        column,
                        dimension,
                        metric,
                        m,
                        ef_construction,
                        ef_search,
                    ));
                }
            }
        }

        // Fast path: load HNSW graph from snapshot files ONLY.
        //
        // Legacy `__vec_graph__` LSM entries are deliberately not read: they
        // can be hundreds of MB in a single value, which (a) makes
        // scan_prefix load them into memory and (b) trips the SST block-size
        // limit.  Files under `indexes/*.hnsw` are the one true snapshot
        // format now.  If no file exists we rebuild from documents.
        // Incremental deltas are NOT scanned from LSM: a delta entry can be
        // hundreds of MB and `scan_prefix` materializes values, which wedges
        // startup.  The file snapshot (`indexes/*.hnsw`) is the single
        // restore source; anything written after the snapshot is covered by
        // the document backfill on the next restart.
        tracing::info!("Vector index restore: using snapshot files only");

        // Phase 1 (gate-blocking): install graphs + reconcile (note/ghost only).
        // Phase 2 (after mark_ready): insert missing vectors with a guard.
        // Keeping HNSW inserts out of the gate window makes it O(documents)
        // and independent of how many vectors need backfilling.
        let mut pending_backfill: Vec<(
            String,
            String,
            Vec<(Vec<u8>, String, String, crate::vector::hnsw::VecData)>,
        )> = Vec::new();

        // Create the indexes and try to load graph structures
        for (class, column, dimension, metric, m, ef_construction, ef_search) in &index_configs {
            let graph_key = format!("{}_{}", class, column);

            let graph_data = self.load_vector_graph_file(class, column);
            if graph_data.is_none() {
                tracing::info!(
                    table = %class,
                    column = %column,
                    "No vector snapshot file found, will rebuild from documents"
                );
            }

            if let Some(graph_data) = graph_data {
                // Fast path: load HNSW graph structure directly.
                // load_graph rejects empty snapshots (written at CREATE INDEX
                // before any vectors) so we fall through to rebuild.
                //
                // Deserialize BEFORE taking the manager write lock: bincode of
                // a multi-hundred-MB snapshot can take minutes under memory
                // pressure, and holding the lock that long blocks every search.
                tracing::info!(
                    table = %class,
                    column = %column,
                    bytes = graph_data.len(),
                    "Deserializing vector index snapshot (outside lock)"
                );
                let deserialize_started = std::time::Instant::now();
                let loaded = crate::vector::hnsw::HnswIndex::load_from_bytes(&graph_data);
                tracing::info!(
                    table = %class,
                    column = %column,
                    elapsed_ms = deserialize_started.elapsed().as_millis() as u64,
                    "Vector snapshot deserialize done"
                );
                match loaded {
                    Ok(index) => {
                        // Extract the result first: the write guard from
                        // `.write()` would otherwise live for the whole
                        // `match` (Rust temporary lifetime) and a later
                        // `.read()` in the arm deadlocks on the same thread.
                        let installed = self
                            .vector_index_manager
                            .write()
                            .unwrap_or_else(|e| e.into_inner())
                            .install_graph(class, column, index);
                        match installed {
                            Ok(n) => {
                                tracing::info!(
                                    "Loaded vector index graph for {}.{} ({} vectors, fast restore)",
                                    class,
                                    column,
                                    n
                                );
                                // Snapshot may be stale (written before a
                                // scan-visibility fix revealed more fp rows).
                                // Backfill only the missing pks so we do not
                                // rebuild 2M+ vectors from scratch.
                                let existing = self
                                    .vector_index_manager
                                    .read()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .id_set(class, column);
                                tracing::info!(
                                    table = %class,
                                    column = %column,
                                    existing = existing.len(),
                                    "Vector snapshot reconcile scan starting"
                                );
                                let missing = self.reconcile_vector_index(
                                    class,
                                    column,
                                    *dimension,
                                    &existing,
                                );
                                if !missing.is_empty() {
                                    tracing::info!(
                                        table = %class,
                                        column = %column,
                                        pending = missing.len(),
                                        total = existing.len() + missing.len(),
                                        "Vector snapshot reconcile queued missing vectors for backfill"
                                    );
                                    pending_backfill
                                        .push((class.clone(), column.clone(), missing));
                                }
                                continue;
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "Failed to install HNSW graph for {}.{}, dropping stale snapshot and rebuilding: {}",
                                    class,
                                    column,
                                    e
                                );
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Failed to deserialize HNSW graph for {}.{}, dropping stale snapshot and rebuilding: {}",
                            class,
                            column,
                            e
                        );
                    }
                }
                // Tombstone the unusable snapshot so the next restart
                // does not retry it and log the same failure again.
                {
                    let graph_key = format!("__vec_graph__{}_{}", class, column).into_bytes();
                    let seq = self.next_seq();
                    let del = Entry::delete(graph_key.clone(), seq);
                    if let Some(mut ws) = self.write_state.try_write_for(std::time::Duration::from_secs(1)) {
                        let _ = ws.wal.append(&del);
                        let _ = ws.wal.flush_buf();
                        ws.memtable.delete_with_seq(graph_key, seq);
                    }
                    let idx_dir = self.options.data_dir.join("indexes");
                    let _ = std::fs::remove_file(
                        idx_dir.join(format!("{}_{}.hnsw", class, column)),
                    );
                }
            }

            // Slow path: create empty index and backfill from documents
            let _ = self
                .vector_index_manager
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .create_index(
                    class,
                    column,
                    *dimension,
                    *metric,
                    *m,
                    *ef_construction,
                    *ef_search,
                );

            let prefix = format!("{}::", class);
            let mut total = 0usize;
            let mut batch: Vec<(Vec<u8>, String, String, VecData)> = Vec::new();
            const VEC_BATCH: usize = 10_000;
            {
                let dim = *dimension;
                let cls = class.clone();
                let col = column.clone();
                let mut sink = |pk: &[u8], val_bytes: &[u8]| -> Result<bool> {
                    if let Some(doc) = parse_doc_bytes(val_bytes) {
                        if doc.get("__class__").and_then(|v| v.as_str()) == Some(cls.as_str()) {
                            // base64 Bytes → Packed directly (no f32 round-trip)
                            if let Some(data) = doc
                                .get(col.as_str())
                                .and_then(|v| VecData::from_json(v, dim))
                            {
                                batch.push((pk.to_vec(), cls.clone(), col.clone(), data));
                            }
                        }
                    }
                    if batch.len() >= VEC_BATCH {
                        self.vector_index_manager
                            .write()
                            .unwrap_or_else(|e| e.into_inner())
                            .index_vector_data_owned(std::mem::take(&mut batch));
                        total += VEC_BATCH;
                        tracing::info!(
                            table = %cls,
                            column = %col,
                            indexed = total,
                            "Vector index rebuild progress"
                        );
                    }
                    Ok(true)
                };
                self.for_each_live_prefix_until(prefix.as_bytes(), None, None, &mut sink)?;
            }
            if !batch.is_empty() {
                total += batch.len();
                self.vector_index_manager
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .index_vector_data_owned(std::mem::take(&mut batch));
            }
            let count = total;

            // Persist the rebuilt HNSW graph for next restart (file snapshot).
            if let Err(e) = self.persist_vector_graph(class, column) {
                tracing::warn!(
                    table = %class,
                    column = %column,
                    error = %e,
                    "Failed to persist HNSW graph snapshot after rebuild"
                );
            }

            tracing::info!(
                "Rebuilt vector index on {}.{} ({} vectors)",
                class,
                column,
                count
            );
        }

        // Phase 1 done: every restored graph is reconciled (deleted_keys
        // rebuilt). Ghost safety is complete — safe to serve searches now.
        // Backfill is deferred so the gate window does not grow with it.
        {
            let pending_total: usize = pending_backfill.iter().map(|(_, _, m)| m.len()).sum();
            self.vector_index_manager
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .set_pending_backfill(pending_total);
        }
        self.vector_index_manager
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .mark_ready();

        // Phase 2 (post-gate): insert missing vectors with the concurrent-write
        // guard. Search during this window misses these rows (false negatives
        // only) — see pending_backfill in /api/schema for the remaining count.
        for (class, column, missing) in pending_backfill {
            let (_inserted, _skipped) = self.backfill_vector_missing(&class, &column, missing);
            if let Err(e) = self.persist_vector_graph(&class, &column) {
                tracing::warn!(
                    table = %class,
                    column = %column,
                    error = %e,
                    "Failed to persist HNSW graph snapshot after backfill"
                );
            }
        }
        Ok(())
    }

    /// True once vector indexes have been restored/reconciled and search is safe.
    pub fn vector_indexes_ready(&self) -> bool {
        self.vector_index_manager
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .is_ready()
    }

    /// Live HNSW node count for one vector index (None if no such index).
    pub fn vector_index_len(&self, class: &str, column: &str) -> Option<usize> {
        self.vector_index_manager
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .index_len(class, column)
    }

    /// Number of tombstoned vector keys currently held in memory.
    pub fn vector_deleted_keys_count(&self) -> usize {
        self.vector_index_manager
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .deleted_keys_count()
    }

    /// Vectors still waiting for background backfill (0 when idle).
    /// Non-zero means `VECTOR SEARCH` may miss those rows until it drains.
    pub fn vector_pending_backfill(&self) -> usize {
        self.vector_index_manager
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .pending_backfill_count()
    }

    /// In-place prune of `class.column`'s HNSW graph down to live document
    /// vectors, then persists the compacted snapshot. Returns nodes removed.
    pub fn prune_vector_index(&self, class: &str, column: &str) -> Result<usize> {
        let removed = self
            .vector_index_manager
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .prune_index_to_live(class, column)?;
        if removed > 0 {
            self.persist_vector_graph(class, column)?;
        }
        Ok(removed)
    }

    /// Builds the LSM key for vector index metadata.
    fn make_vec_meta_key(class: &str, column: &str) -> Vec<u8> {
        let mut key = Vec::new();
        key.extend_from_slice(b"__vec_meta__");
        key.extend_from_slice(class.as_bytes());
        key.extend_from_slice(b"__");
        key.extend_from_slice(column.as_bytes());
        key
    }

    /// Builds the LSM key for secondary index metadata.
    fn make_idx_meta_key(class: &str, column: &str) -> Vec<u8> {
        let mut key = Vec::new();
        key.extend_from_slice(b"__idx_meta__");
        key.extend_from_slice(class.as_bytes());
        key.extend_from_slice(b"__");
        key.extend_from_slice(column.as_bytes());
        key
    }

    /// Persists secondary index metadata so schema introspection can list
    /// indexes even before the in-memory index manager is rebuilt.
    fn write_idx_meta(&self, class: &str, column: &str, entries: usize) -> Result<()> {
        let meta_key = Self::make_idx_meta_key(class, column);
        let meta_json = serde_json::json!({
            "class": class,
            "column": column,
            "entries": entries,
            "format": "v2",
        });
        let meta_val = serde_json::to_vec(&meta_json)
            .map_err(|e| onto_core::CoreError::Serialization(e.to_string()))?;
        let seq = self.next_seq();
        let entry = Entry::put(meta_key.clone(), meta_val.clone(), seq);
        let mut ws = self.write_state.write();
        ws.wal.append(&entry)?;
        ws.wal.flush_buf()?;
        ws.memtable.put_with_seq(meta_key, meta_val, seq);
        Ok(())
    }

    /// Removes persisted secondary index metadata on DROP INDEX.
    fn delete_idx_meta(&self, class: &str, column: &str) {
        let meta_key = Self::make_idx_meta_key(class, column);
        let seq = self.next_seq();
        let del_entry = Entry::delete(meta_key.clone(), seq);
        let mut ws = self.write_state.write();
        if let Err(e) = ws.wal.append(&del_entry) {
            tracing::error!("Failed to write WAL entry for index meta drop: {}", e);
            return;
        }
        let _ = ws.wal.flush_buf();
        ws.memtable.delete_with_seq(meta_key, seq);
    }

    // ══════════════════════════════════════════════════════════════�?    //  MVCC Transaction API
    // ══════════════════════════════════════════════════════════════�?
    /// Begins a new transaction. Returns the transaction ID.
    /// Snapshot is taken at the last committed data point.
    pub fn begin_txn(&self) -> SeqNo {
        // seq_counter is the NEXT value to assign, so last committed = seq_counter - 1.
        // Acquire pairs with the AcqRel in fetch_add (line ~821) to ensure we see
        // all data written before the sequence number was incremented.
        let last_seq = self.seq_counter.load(Ordering::Acquire).saturating_sub(1);
        self.write_state.write().txn_manager.begin(last_seq)
    }

    /// Commits a transaction. Flushes its write buffer to WAL + MemTable.
    ///
    /// Lock strategy (3 phases to avoid nesting write_state inside index locks):
    ///  Phase 1 — brief write_state: take writes, assign seqs, read old values
    ///  Phase 2 — no write_state:    index de/re mutations (independent locks)
    ///  Phase 3 — brief write_state: WAL + memtable writes
    pub fn commit_txn(&self, txn_id: SeqNo) -> Result<()> {
        // ── Phase 1: Take writes, assign sequence numbers, read old values ──
        // Snapshot live SST handles newest-first BEFORE acquiring write_state
        // lock (lock order: levels → sst_cache, then release before write_state).
        // Only paths still listed in `levels` are included — compacted-away
        // cache leftovers must not be consulted (they can shadow fresher data).
        let sst_handles: Vec<Arc<SsTable>> = {
            let levels = self.levels.lock().unwrap_or_else(|e| e.into_inner());
            let mut cache = self.sst_cache.lock().unwrap_or_else(|e| e.into_inner());
            let mut handles = Vec::new();
            let mut seen = std::collections::HashSet::new();
            for level in levels.iter() {
                for sst_info in level.iter().rev() {
                    if !seen.insert(sst_info.path.clone()) {
                        continue;
                    }
                    match cache.get(&sst_info.path) {
                        Some(sst) => handles.push(Arc::clone(sst)),
                        None => match SsTable::open(&sst_info.path) {
                            Ok(sst) => {
                                let sst = Arc::new(sst);
                                cache.insert(sst_info.path.clone(), Arc::clone(&sst));
                                handles.push(sst);
                            }
                            // Compacted away between levels snapshot and open.
                            Err(e) if Self::is_not_found(&e) => continue,
                            Err(e) => return Err(e),
                        },
                    }
                }
            }
            handles
        };

        let (writes_with_seq, old_values, index_entries_batch) = {
            let mut ws = self.write_state.write();
            let writes = ws.txn_manager.commit(txn_id)?;

            let mut writes_with_seq: Vec<(Vec<u8>, WriteOp, SeqNo)> =
                Vec::with_capacity(writes.len());
            let mut old_values: HashMap<Vec<u8>, Vec<u8>> = HashMap::new();
            let mut index_entries_batch: Vec<(Vec<u8>, Vec<u8>, SeqNo)> = Vec::new();

            for (key, op) in writes {
                let seq = self.next_seq();

                // Check index existence (brief read locks, dropped immediately)
                let class = Self::extract_class_from_key(&key);
                let has_indexes = class.as_ref().is_some_and(|c| {
                    !self
                        .index_manager
                        .read()
                        .unwrap_or_else(|e| e.into_inner())
                        .indexes_for_class(c)
                        .is_empty()
                });
                let has_vector_indexes = class.as_ref().is_some_and(|c| {
                    self.vector_index_manager
                        .read()
                        .unwrap_or_else(|e| e.into_inner())
                        .has_any_index(c)
                });

                // Read old value for de-indexing (from locked memtable + live SSTs)
                if has_indexes || has_vector_indexes {
                    if let Some(old_val) =
                        Self::get_from_locked(&ws, &sst_handles, key.as_slice())?
                    {
                        old_values.insert(key.clone(), old_val);
                    }
                }

                // Pre-compute index entries for Put operations (need new doc to compute)
                if has_indexes {
                    if let WriteOp::Put(ref value) = op {
                        if let Some(ref c) = class {
                            if let Some(ref doc) = parse_doc_bytes(value) {
                                let entries = self
                                    .index_manager
                                    .read()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .index_document_read_only(c, &key, doc);
                                for (idx_key, idx_val) in entries {
                                    let idx_seq = self.next_seq();
                                    index_entries_batch.push((idx_key, idx_val, idx_seq));
                                }
                            }
                        }
                    }
                }

                writes_with_seq.push((key, op, seq));
            }

            (writes_with_seq, old_values, index_entries_batch)
        };

        // ── Phase 2: Index mutations (no write_state held) ──
        {
            let mut idx_mgr = self
                .index_manager
                .write()
                .unwrap_or_else(|e| e.into_inner());
            let mut vec_mgr = self
                .vector_index_manager
                .write()
                .unwrap_or_else(|e| e.into_inner());

            for (key, op, _) in &writes_with_seq {
                let class = Self::extract_class_from_key(key);

                // De-index old value (for both Put/Update and Delete)
                if let Some(old_val) = old_values.get(key) {
                    if let Some(ref old_doc) = parse_doc_bytes(old_val) {
                        if let Some(ref c) = class {
                            idx_mgr.deindex_document(c, key, old_doc);
                            vec_mgr.deindex_vectors(key);
                        }
                    }
                }

                // Re-index new value (Put only)
                if let WriteOp::Put(value) = op {
                    if let Some(ref doc) = parse_doc_bytes(value) {
                        if let Some(ref c) = class {
                            idx_mgr.index_document(c, key, doc);
                            vec_mgr.index_document_vectors(key, c, doc);
                        }
                    }
                }
            }
        }

        // ── Phase 3: WAL + MemTable writes (brief write_state) ──
        let needs_flush = {
            let mut ws = self.write_state.write();

            // Write index entries to WAL + memtable
            for (idx_key, idx_val, idx_seq) in index_entries_batch {
                let entry = Entry::put(idx_key.clone(), idx_val.clone(), idx_seq);
                ws.wal.append(&entry)?;
                ws.memtable.put_with_seq(idx_key, idx_val, idx_seq);
            }

            // Write data entries to WAL + memtable
            for (key, op, seq) in writes_with_seq {
                match op {
                    WriteOp::Put(value) => {
                        ws.wal.append_raw_put(&key, &value, seq)?;
                        ws.memtable.put_with_seq(key, value, seq);
                    }
                    WriteOp::Delete => {
                        let entry = Entry::delete(key.clone(), seq);
                        ws.wal.append(&entry)?;
                        ws.memtable.delete_with_seq(key, seq);
                    }
                }
            }

            ws.wal.flush_buf()?;

            ws.memtable.size() >= self.options.memtable_size_limit
                || ws.wal.needs_rotation()
        };
        // write_state lock released here

        // Group commit: batch sync across concurrent transactions
        // Strategy: wait up to 10µs OR until 4 transactions are ready
        if self.options.sync_wal_on_commit {
            let current_seq = self.seq_counter.load(Ordering::Relaxed);
            let is_leader = self.group_commit.register(current_seq);
            if is_leader {
                // Leader waits for batch or timeout, then syncs
                let _batch_size = self.group_commit.wait_for_batch();
                {
                    let mut ws = self.write_state.write();
                    ws.wal.sync()?;
                }
                self.group_commit.complete_sync(current_seq);
            } else {
                // Follower waits for the leader to complete sync
                self.group_commit.wait_for_sync(current_seq);
            }
        }

        if needs_flush {
            self.flush_memtable()?;
        }

        Ok(())
    }

    /// Extracts the class name from a key like "Product::00000000000123456789".
    fn extract_class_from_key(key: &[u8]) -> Option<String> {
        let key_str = std::str::from_utf8(key).ok()?;
        let parts: Vec<&str> = key_str.splitn(2, "::").collect();
        if parts.len() == 2 && !parts[0].is_empty() {
            Some(parts[0].to_string())
        } else {
            None
        }
    }

    /// Gets a value by key from already-locked write state + live SST handles.
    /// Used by `commit_txn` which holds the write_state write lock.
    ///
    /// `sst_handles` must be newest-first (L0 → L6, later flushes first) and
    /// contain only SSTs still listed in `levels` — first hit wins, matching
    /// the read path's first-seen-wins contract.
    fn get_from_locked(
        ws: &WriteState,
        sst_handles: &[Arc<SsTable>],
        key: &[u8],
    ) -> Result<Option<Value>> {
        use crate::lsm::memtable::VersionLookup;
        match ws.memtable.get_version(key) {
            Some(VersionLookup::Value(v, _)) => return Ok(Some(v)),
            Some(VersionLookup::Tombstone) => return Ok(None),
            None => {}
        }
        if let Some(ref imm) = ws.immutable_memtable {
            match imm.get_version(key) {
                Some(VersionLookup::Value(v, _)) => return Ok(Some(v)),
                Some(VersionLookup::Tombstone) => return Ok(None),
                None => {}
            }
        }
        for sst in sst_handles {
            match sst.get_full(key)? {
                Some((value, _, EntryKind::Put)) => return Ok(Some(value)),
                Some((_, _, EntryKind::Delete)) => return Ok(None),
                None => continue,
            }
        }
        Ok(None)
    }

    /// Aborts a transaction. Discards all pending writes.
    pub fn abort_txn(&self, txn_id: SeqNo) -> Result<()> {
        self.write_state.write().txn_manager.abort(txn_id)
    }

    /// Buffers a put operation in a transaction.
    pub fn txn_put(&self, txn_id: SeqNo, key: Key, value: Value) -> Result<()> {
        self.write_state
            .write()
            .txn_manager
            .get_mut(txn_id)
            .ok_or_else(|| {
                onto_core::CoreError::InvalidArgument(format!(
                    "transaction {} not found or not active",
                    txn_id
                ))
            })?
            .put(key, value);
        Ok(())
    }

    /// Batch put within a transaction: buffers multiple entries in a single lock acquisition.
    ///
    /// This is much faster than calling `txn_put()` in a loop because the write_state
    /// lock is acquired only once for all entries.
    pub fn txn_put_batch(&self, txn_id: SeqNo, entries: Vec<(Key, Value)>) -> Result<usize> {
        let count = entries.len();
        let mut ws = self.write_state.write();
        let txn = ws.txn_manager.get_mut(txn_id).ok_or_else(|| {
            onto_core::CoreError::InvalidArgument(format!(
                "transaction {} not found or not active",
                txn_id
            ))
        })?;
        for (key, value) in entries {
            txn.put(key, value);
        }
        Ok(count)
    }

    /// Buffers a delete operation in a transaction.
    pub fn txn_delete(&self, txn_id: SeqNo, key: Key) -> Result<()> {
        self.write_state
            .write()
            .txn_manager
            .get_mut(txn_id)
            .ok_or_else(|| {
                onto_core::CoreError::InvalidArgument(format!(
                    "transaction {} not found or not active",
                    txn_id
                ))
            })?
            .delete(key);
        Ok(())
    }

    /// Reads a key within a transaction context.
    ///
    /// Read path:
    /// 1. Check the transaction's own write buffer (uncommitted writes)
    /// 2. Check MemTable (with snapshot visibility)
    /// 3. Check SSTables (with snapshot visibility)
    pub fn txn_get(&self, txn_id: SeqNo, key: &[u8]) -> Result<Option<Value>> {
        // Atomically check write buffer and construct visibility snapshot
        let (buffer_result, vis) = {
            let ws = self.write_state.read();
            let buffer_result = ws.txn_manager.get(txn_id).and_then(|txn| {
                txn.write_buffer_get(key).map(|op| match op {
                    WriteOp::Put(v) => Some(v.clone()),
                    WriteOp::Delete => None,
                })
            });
            let vis = ws.txn_manager.visibility_for(txn_id);
            (buffer_result, vis)
        };

        // 1. Check transaction's own write buffer
        if let Some(result) = buffer_result {
            return Ok(result);
        }

        // 2-3. Read from storage with snapshot visibility
        self.get_with_visibility(key, &vis)
    }

    /// Scans all entries with the given prefix, respecting snapshot visibility.
    pub fn txn_scan_prefix(&self, txn_id: SeqNo, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let (vis, write_buffer) = {
            let ws = self.write_state.read();
            let vis = ws.txn_manager.visibility_for(txn_id);
            let buf: Vec<(Vec<u8>, WriteOp)> = ws
                .txn_manager
                .get(txn_id)
                .map(|t| {
                    t.write_buffer_iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect()
                })
                .unwrap_or_default();
            (vis, buf)
        };

        // Get base results from storage with visibility filtering
        let base_results = self.scan_prefix_with_visibility(prefix, &vis)?;

        // Use HashMap for O(1) lookups during write buffer overlay
        let mut results_map: std::collections::HashMap<Vec<u8>, Vec<u8>> =
            base_results.into_iter().collect();

        // Overlay the transaction's own write buffer
        for (key, op) in &write_buffer {
            if key.starts_with(prefix) {
                match op {
                    WriteOp::Put(value) => {
                        results_map.insert(key.clone(), value.clone());
                    }
                    WriteOp::Delete => {
                        results_map.remove(key);
                    }
                }
            }
        }

        // Convert to sorted Vec
        let mut results: Vec<(Vec<u8>, Vec<u8>)> = results_map.into_iter().collect();
        results.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(results)
    }

    /// Gets a value by key with snapshot visibility filtering.
    fn get_with_visibility(
        &self,
        key: &[u8],
        vis: &crate::mvcc::Visibility,
    ) -> Result<Option<Value>> {
        // Check active MemTable — use range query to find visible version
        {
            let ws = self.write_state.read();
            for entry in ws.memtable.get_versions(key) {
                if vis.is_visible(entry.seq_no) {
                    if entry.is_tombstone() {
                        return Ok(None);
                    }
                    return Ok(Some(entry.value.to_vec()));
                }
            }
            // Check immutable MemTable
            if let Some(ref imm) = ws.immutable_memtable {
                for entry in imm.get_versions(key) {
                    if vis.is_visible(entry.seq_no) {
                        if entry.is_tombstone() {
                            return Ok(None);
                        }
                        return Ok(Some(entry.value.to_vec()));
                    }
                }
            }
        }

        // Check SSTables (newest to oldest)
        let candidates: Vec<PathBuf> = {
            let levels = self.levels.lock().unwrap_or_else(|e| e.into_inner());
            let mut cands = Vec::new();
            for level in levels.iter() {
                for sst_info in level.iter().rev() {
                    if key < sst_info.min_key.as_slice() || key > sst_info.max_key.as_slice() {
                        continue;
                    }
                    cands.push(sst_info.path.clone());
                }
            }
            cands
        };

        // Snapshot SST handles from cache (brief lock)
        let sst_handles: Vec<Arc<SsTable>> = {
            let mut cache = self.sst_cache.lock().unwrap_or_else(|e| e.into_inner());
            let mut handles = Vec::with_capacity(candidates.len());
            for path in &candidates {
                if let Some(sst) = cache.get(path) {
                    handles.push(Arc::clone(sst));
                } else {
                    let sst = Arc::new(SsTable::open(path)?);
                    cache.insert(path.to_path_buf(), Arc::clone(&sst));
                    handles.push(sst);
                }
            }
            handles
        };

        // Iterate outside the lock
        for sst in &sst_handles {
            match sst.get_full(key)? {
                Some((value, seq, kind)) if vis.is_visible(seq) => {
                    if kind == EntryKind::Delete {
                        return Ok(None);
                    }
                    return Ok(Some(value));
                }
                _ => continue,
            }
        }

        Ok(None)
    }

    /// Scans entries with prefix, filtering by snapshot visibility.
    fn scan_prefix_with_visibility(
        &self,
        prefix: &[u8],
        vis: &crate::mvcc::Visibility,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        self.scan_prefix_internal(prefix, Some(vis), None)
    }

    /// Returns the number of active transactions.
    pub fn active_txn_count(&self) -> usize {
        self.write_state.read().txn_manager.active_count()
    }

    // =================================================================
    //  Index API
    // =================================================================

    /// Returns the current index creation progress.
    /// Returns (class.column, total_entries, processed_entries) if in progress, None otherwise.
    pub fn index_creation_progress(&self) -> Option<(String, usize, usize)> {
        self.index_progress.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Creates a secondary index on a class.column.
    /// Automatically backfills existing data for the class.
    pub fn create_index(&self, class: &str, column: &str) -> Result<()> {
        let start = std::time::Instant::now();
        tracing::info!(table = class, column = column, "Index creation started");

        self.index_manager
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .create_index(class, column);

        // Backfill: one streaming pass collecting (encoded_value, pk) pairs,
        // then a single bulk_load. The previous per-row path (parse + tree
        // insert + WAL put per row) ran ~150 rows/s — 40 min for 600k rows.
        let prefix = format!("{}::", class);
        let mut pairs: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
        let mut lsm_entries: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
        let mut total_estimate = 0usize;

        {
            let label = format!("{}.{}", class, column);
            let mut prog = self.index_progress.lock().unwrap_or_else(|e| e.into_inner());
            *prog = Some((label, 0, 0));
        }

        self.for_each_live_prefix(prefix.as_bytes(), None, None, |pk, val_bytes| {
            total_estimate += 1;
            if let Some(val) = extract_index_field(val_bytes, class, column) {
                let encoded = crate::IndexManager::encode_value(&val);
                lsm_entries.push((
                    crate::IndexManager::make_index_key(class, column, &encoded, pk),
                    Vec::new(),
                ));
                pairs.push((encoded, pk.to_vec()));
            }
            if total_estimate % 50_000 == 0 {
                tracing::info!(
                    rows = total_estimate,
                    pairs = pairs.len(),
                    scan_ms = start.elapsed().as_millis(),
                    "Index backfill scan progress"
                );
            }
            Ok(())
        })?;
        let scan_ms = start.elapsed().as_millis();
        tracing::info!(
            rows = total_estimate,
            pairs = pairs.len(),
            scan_ms,
            "Index backfill scan done"
        );

        {
            let mut prog = self.index_progress.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(ref mut p) = *prog {
                p.1 = total_estimate;
                p.2 = total_estimate;
            }
        }

        // One-shot sort + bottom-up page construction for both trees.
        {
            let t = std::time::Instant::now();
            let mut mgr = self
                .index_manager
                .write()
                .unwrap_or_else(|e| e.into_inner());
            mgr.bulk_load_index(class, column, &pairs)?;
            tracing::info!(
                bulk_ms = t.elapsed().as_millis(),
                pairs = pairs.len(),
                "Index bulk_load done"
            );
        }

        // Persist index entries to WAL so the index can be rebuilt after restart.
        const BATCH_SIZE: usize = 5000;
        let mut batch_start = 0usize;
        while batch_start < lsm_entries.len() {
            let batch_end = (batch_start + BATCH_SIZE).min(lsm_entries.len());
            {
                let mut ws = self.write_state.write();
                for (key, value) in &lsm_entries[batch_start..batch_end] {
                    let seq = self.next_seq();
                    ws.wal.append_raw_put(key, value, seq)?;
                    ws.memtable.put_with_seq(key.clone(), value.clone(), seq);
                }
                ws.wal.flush_buf()?;
            }
            batch_start = batch_end;
            std::thread::yield_now();
        }

        // Clear progress
        {
            let mut prog = self.index_progress.lock().unwrap_or_else(|e| e.into_inner());
            *prog = None;
        }

        // Persist index metadata for schema introspection.
        if let Err(e) = self.write_idx_meta(class, column, pairs.len()) {
            tracing::warn!(
                table = class,
                column = column,
                error = %e,
                "Failed to persist index metadata"
            );
        }

        let elapsed_ms = start.elapsed().as_millis();
        tracing::info!(
            table = class,
            column = column,
            elapsed_ms,
            entries = pairs.len(),
            total_scanned = total_estimate,
            "Index creation completed"
        );

        Ok(())
    }

    /// Drops a secondary index.
    /// Also tombstones all `__idx2__` LSM entries for this index so startup
    /// rebuild does not resurrect it from leftover index entries.
    pub fn drop_index(&self, class: &str, column: &str) -> bool {
        if let Err(e) = self.tombstone_index_entries(class, column) {
            tracing::warn!(
                table = class,
                column = column,
                error = %e,
                "Failed to tombstone LSM index entries on drop"
            );
        }
        self.delete_idx_meta(class, column);
        self.index_manager
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .drop_index(class, column)
    }

    /// Writes delete tombstones for every index entry belonging to (class, column).
    /// Without this, `rebuild_indexes` recreates the disk index on next startup.
    fn tombstone_index_entries(&self, class: &str, column: &str) -> Result<()> {
        // v2 index key format: `__idx2__` || class_len:u16 || class || col_len:u16 || col || ...
        let cb = class.as_bytes();
        let kb = column.as_bytes();
        let mut prefix = Vec::with_capacity(8 + 2 + cb.len() + 2 + kb.len());
        prefix.extend_from_slice(b"__idx2__");
        prefix.extend_from_slice(&(cb.len() as u16).to_le_bytes());
        prefix.extend_from_slice(cb);
        prefix.extend_from_slice(&(kb.len() as u16).to_le_bytes());
        prefix.extend_from_slice(kb);

        let keys: Vec<Vec<u8>> = self
            .scan_prefix(&prefix)?
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        if keys.is_empty() {
            return Ok(());
        }
        let mut ws = self.write_state.write();
        let mut cleaned = 0usize;
        for key in keys {
            let seq = self.next_seq();
            let entry = Entry::delete(key.clone(), seq);
            ws.wal.append(&entry)?;
            ws.memtable.delete_with_seq(key, seq);
            cleaned += 1;
        }
        ws.wal.flush_buf()?;
        tracing::info!(
            table = class,
            column = column,
            cleaned = cleaned,
            "Tombstoned LSM index entries on drop"
        );
        drop(ws);
        // Persist tombstones to SSTable so a crash before WAL rotation cannot
        // resurrect the index via rebuild_from_entries.
        self.flush_memtable()?;
        Ok(())
    }

    /// Returns true if an index exists on the given class.column.
    pub fn has_index(&self, class: &str, column: &str) -> bool {
        self.index_manager
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .has_index(class, column)
    }

    /// Returns all live (class, column) index pairs from the index manager.
    pub fn list_indexes(&self) -> Vec<(String, String)> {
        self.index_manager
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .all_index_keys()
    }

    /// Returns a reference to the index manager RwLock.
    pub fn index_manager(&self) -> &RwLock<IndexManager> {
        &self.index_manager
    }

    // =================================================================
    //  Vector Index API
    // =================================================================

    /// Creates a vector index on a class.column with HNSW parameters.
    pub fn create_vector_index(
        &self,
        class: &str,
        column: &str,
        dimension: usize,
        metric: crate::vector::DistanceMetric,
        m: usize,
        ef_construction: usize,
        ef_search: usize,
    ) -> Result<()> {
        self.vector_index_manager
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .create_index(
                class,
                column,
                dimension,
                metric,
                m,
                ef_construction,
                ef_search,
            )?;

        // Persist vector index metadata to LSM
        let meta_key = Self::make_vec_meta_key(class, column);
        let meta_json = serde_json::json!({
            "class": class,
            "column": column,
            "dimension": dimension,
            "metric": format!("{:?}", metric),
            "m": m,
            "ef_construction": ef_construction,
            "ef_search": ef_search,
        });
        let meta_val = serde_json::to_vec(&meta_json)
            .map_err(|e| onto_core::CoreError::Serialization(e.to_string()))?;
        let seq = self.next_seq();
        let entry = Entry::put(meta_key.clone(), meta_val.clone(), seq);
        {
            let mut ws = self.write_state.write();
            ws.wal.append(&entry)?;
            ws.wal.flush_buf()?;
            ws.memtable.put_with_seq(meta_key, meta_val, seq);
        }

        // Backfill: scan existing documents, collect vectors, then batch insert under single lock
        let prefix = format!("{}::", class);
        let entries = self.scan_prefix(prefix.as_bytes())?;

        let mut vectors: Vec<(Vec<u8>, Vec<f32>)> = Vec::new();
        for (pk, val_bytes) in entries {
            if let Some(doc) = parse_doc_bytes(&val_bytes) {
                if doc.get("__class__").and_then(|v| v.as_str()) == Some(class) {
                    if let Some(serde_json::Value::Array(arr)) = doc.get(column) {
                        let vec: Vec<f32> = arr
                            .iter()
                            .filter_map(|v| v.as_f64().map(|f| f as f32))
                            .collect();
                        if vec.len() == dimension {
                            vectors.push((pk, vec));
                        }
                    }
                }
            }
        }

        // Batch insert under single write lock (not per-document)
        if !vectors.is_empty() {
            let owned: Vec<(Vec<u8>, String, String, Vec<f32>)> = vectors
                .into_iter()
                .map(|(pk, v)| (pk, class.to_string(), column.to_string(), v))
                .collect();
            self.vector_index_manager
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .index_vector_batch_owned(owned);
        }

        // Persist the HNSW graph for fast restart (file snapshot).
        if let Err(e) = self.persist_vector_graph(class, column) {
            tracing::warn!(
                table = %class,
                column = %column,
                error = %e,
                "Failed to persist HNSW graph snapshot after create"
            );
        }

        Ok(())
    }

    /// Drops a vector index.
    pub fn drop_vector_index(&self, class: &str, column: &str) -> bool {
        let removed = self
            .vector_index_manager
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .drop_index(class, column);
        // Always tombstone persisted metadata/graph/delta keys, even when the
        // in-memory index is missing (e.g. after a failed snapshot load).
        // Otherwise every restart re-reads the stale snapshot and re-fails.
        {
            let meta_key = Self::make_vec_meta_key(class, column);
            let seq = self.next_seq();
            let del_entry = Entry::delete(meta_key.clone(), seq);
            let mut ws = self.write_state.write();
            if let Err(e) = ws.wal.append(&del_entry) {
                tracing::error!("Failed to write WAL entry for vector index drop: {}", e);
                return false;
            }
            if let Err(e) = ws.wal.flush_buf() {
                tracing::error!("Failed to flush WAL for vector index drop: {}", e);
                return false;
            }
            ws.memtable.delete_with_seq(meta_key, seq);

            let graph_key = format!("__vec_graph__{}_{}", class, column);
            let seq2 = self.next_seq();
            let del_entry2 = Entry::delete(graph_key.clone().into_bytes(), seq2);
            if let Err(e) = ws.wal.append(&del_entry2) {
                tracing::error!("Failed to write WAL entry for vector graph drop: {}", e);
            }
            ws.wal.flush_buf().ok();
            ws.memtable.delete_with_seq(graph_key.into_bytes(), seq2);

            let delta_key = format!("__vec_delta__{}_{}", class, column);
            let seq3 = self.next_seq();
            let del_entry3 = Entry::delete(delta_key.clone().into_bytes(), seq3);
            let _ = ws.wal.append(&del_entry3);
            let _ = ws.wal.flush_buf();
            ws.memtable.delete_with_seq(delta_key.into_bytes(), seq3);
        }
        // Remove the file-based snapshot too.
        let _ = std::fs::remove_file(
            self.options
                .data_dir
                .join("indexes")
                .join(format!("{}_{}.hnsw", class, column)),
        );
        removed
    }

    /// Returns true if a vector index exists on the given class.column.
    pub fn has_vector_index(&self, class: &str, column: &str) -> bool {
        self.vector_index_manager
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .has_index(class, column)
    }

    /// Returns a reference to the vector index manager RwLock.
    pub fn vector_index_manager(&self) -> &RwLock<VectorIndexManager> {
        &self.vector_index_manager
    }

    /// Returns engine statistics.
    pub fn stats(&self) -> EngineStats {
        let ws = self.write_state.read();
        let levels = self.levels.lock().unwrap_or_else(|e| e.into_inner());
        let total_sstables: usize = levels.iter().map(|l| l.len()).sum();
        let total_sst_size: u64 = levels.iter().flat_map(|l| l.iter()).map(|s| s.size).sum();

        EngineStats {
            memtable_size: ws.memtable.size(),
            memtable_entries: ws.memtable.len(),
            num_levels: levels.len(),
            total_sstables,
            total_sst_size,
        }
    }

    // =================================================================
    //  Backup & Restore
    // =================================================================

    /// Creates a full snapshot backup of the database to `backup_dir`.
    ///
    /// Steps:
    /// 1. Flush MemTable → SSTable (ensure all data persisted)
    /// 2. Flush disk indexes
    /// 3. Snapshot SSTable paths (after flush, before copy)
    /// 4. Copy all `.sst` files, `wal.log`, and `indexes/*.idx`
    /// 5. Write a manifest file listing all copied files
    ///
    /// The backup is a consistent snapshot because:
    /// - MemTable is flushed before copying
    /// - SSTable paths are snapshotted after flush
    /// - New writes after flush go to a new MemTable (not in backup)
    /// - WAL is copied after SSTables (captures any post-flush writes)
    pub fn backup(&self, backup_dir: &Path) -> Result<BackupManifest> {
        // Step 1: Flush MemTable to ensure all data is in SSTables
        self.flush()?;

        // Step 2: Flush disk indexes
        self.flush_disk_indexes()?;

        // Step 3: Create backup directory
        fs::create_dir_all(backup_dir)?;
        let idx_backup_dir = backup_dir.join("indexes");
        fs::create_dir_all(&idx_backup_dir)?;

        let mut manifest = BackupManifest {
            timestamp: chrono_timestamp(),
            files: Vec::new(),
            backup_type: "full".to_string(),
            base_timestamp: None,
        };

        // Step 4: Snapshot SSTable paths (consistent point-in-time)
        let levels = self.levels.lock().unwrap_or_else(|e| e.into_inner());
        let mut sst_paths: Vec<PathBuf> = Vec::new();
        for level in levels.iter() {
            for info in level.iter() {
                sst_paths.push(info.path.clone());
            }
        }
        drop(levels);

        for sst_path in &sst_paths {
            let fname = sst_path
                .file_name()
                .expect("should be valid")
                .to_str()
                .expect("should be valid");
            let dest = backup_dir.join(fname);
            fs::copy(sst_path, &dest)?;
            let data = fs::read(&dest)?;
            let checksum = crc32fast::hash(&data);
            manifest.files.push(BackupFile {
                name: fname.to_string(),
                size: data.len() as u64,
                file_type: BackupFileType::SSTable,
                checksum,
            });
        }

        // Step 5: Copy WAL file
        let wal_path = self.options.data_dir.join("wal.log");
        if wal_path.exists() {
            let dest = backup_dir.join("wal.log");
            fs::copy(&wal_path, &dest)?;
            let data = fs::read(&dest)?;
            let checksum = crc32fast::hash(&data);
            manifest.files.push(BackupFile {
                name: "wal.log".to_string(),
                size: data.len() as u64,
                file_type: BackupFileType::Wal,
                checksum,
            });
        }

        // Step 6: Copy index files
        let idx_dir = self.options.data_dir.join("indexes");
        if idx_dir.exists() {
            for entry in fs::read_dir(&idx_dir)? {
                let entry = entry?;
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("idx") {
                    let fname = path
                        .file_name()
                        .expect("should be valid")
                        .to_str()
                        .expect("should be valid");
                    let dest = idx_backup_dir.join(fname);
                    fs::copy(&path, &dest)?;
                    let data = fs::read(&dest)?;
                    let checksum = crc32fast::hash(&data);
                    manifest.files.push(BackupFile {
                        name: format!("indexes/{}", fname),
                        size: data.len() as u64,
                        file_type: BackupFileType::Index,
                        checksum,
                    });
                }
            }
        }

        // Step 7: Write manifest
        let manifest_json = serde_json::to_string_pretty(&manifest)
            .map_err(|e| onto_core::CoreError::Serialization(e.to_string()))?;
        let manifest_path = backup_dir.join("manifest.json");
        fs::write(&manifest_path, manifest_json)?;

        tracing::info!(
            "Backup completed: {} files, {} bytes total",
            manifest.files.len(),
            manifest.files.iter().map(|f| f.size).sum::<u64>()
        );

        Ok(manifest)
    }

    /// Restores the database from a backup created by `backup()`.
    ///
    /// This is a static method — call it before `LsmEngine::open()`.
    /// It copies all backup files into `data_dir`, then the engine's
    /// normal startup (WAL replay + index rebuild) handles recovery.
    pub fn restore(backup_dir: &Path, data_dir: &Path) -> Result<BackupManifest> {
        // Read manifest
        let manifest_path = backup_dir.join("manifest.json");
        let manifest_bytes = fs::read(&manifest_path)?;
        let manifest: BackupManifest = serde_json::from_slice(&manifest_bytes)
            .map_err(|e| onto_core::CoreError::Serialization(e.to_string()))?;

        // Create data directory
        fs::create_dir_all(data_dir)?;

        // Copy all files from backup
        for file in &manifest.files {
            let src = backup_dir.join(&file.name);
            let dest = data_dir.join(&file.name);

            // Create parent directory if needed (for indexes/)
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent)?;
            }

            if !src.exists() {
                return Err(onto_core::CoreError::Custom(format!(
                    "Backup file missing: {}",
                    file.name
                )));
            }

            fs::copy(&src, &dest)?;
        }

        tracing::info!(
            "Restore completed: {} files copied to {}",
            manifest.files.len(),
            data_dir.display()
        );

        Ok(manifest)
    }

    /// Creates an incremental backup — only copies files modified since the given timestamp.
    ///
    /// This is much faster than a full backup when only a few SSTables have changed.
    /// The `since` parameter should be the timestamp from a previous full or incremental backup.
    ///
    /// The incremental backup always includes the WAL file (for point-in-time recovery).
    pub fn backup_incremental(
        &self,
        backup_dir: &Path,
        since: &std::time::SystemTime,
    ) -> Result<BackupManifest> {
        self.flush()?;
        self.flush_disk_indexes()?;

        fs::create_dir_all(backup_dir)?;
        let idx_backup_dir = backup_dir.join("indexes");
        fs::create_dir_all(&idx_backup_dir)?;

        let since_modified = *since;

        let mut manifest = BackupManifest {
            timestamp: chrono_timestamp(),
            files: Vec::new(),
            backup_type: "incremental".to_string(),
            base_timestamp: None,
        };

        // Copy only SSTables modified since the last backup
        let levels = self.levels.lock().unwrap_or_else(|e| e.into_inner());
        let mut sst_paths: Vec<PathBuf> = Vec::new();
        for level in levels.iter() {
            for info in level.iter() {
                sst_paths.push(info.path.clone());
            }
        }
        drop(levels);

        for sst_path in &sst_paths {
            let meta = fs::metadata(sst_path)?;
            if let Ok(modified) = meta.modified() {
                if modified > since_modified {
                    let fname = sst_path
                        .file_name()
                        .expect("should be valid")
                        .to_str()
                        .expect("should be valid");
                    let dest = backup_dir.join(fname);
                    fs::copy(sst_path, &dest)?;
                    let data = fs::read(&dest)?;
                    let checksum = crc32fast::hash(&data);
                    manifest.files.push(BackupFile {
                        name: fname.to_string(),
                        size: data.len() as u64,
                        file_type: BackupFileType::SSTable,
                        checksum,
                    });
                }
            }
        }

        // Always copy WAL (needed for PITR)
        let wal_path = self.options.data_dir.join("wal.log");
        if wal_path.exists() {
            let dest = backup_dir.join("wal.log");
            fs::copy(&wal_path, &dest)?;
            let data = fs::read(&dest)?;
            let checksum = crc32fast::hash(&data);
            manifest.files.push(BackupFile {
                name: "wal.log".to_string(),
                size: data.len() as u64,
                file_type: BackupFileType::Wal,
                checksum,
            });
        }

        // Copy modified index files
        let idx_dir = self.options.data_dir.join("indexes");
        if idx_dir.exists() {
            for entry in fs::read_dir(&idx_dir)? {
                let entry = entry?;
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("idx") {
                    if let Ok(meta) = fs::metadata(&path) {
                        if let Ok(modified) = meta.modified() {
                            if modified > since_modified {
                                let fname = path
                                    .file_name()
                                    .expect("should be valid")
                                    .to_str()
                                    .expect("should be valid");
                                let dest = idx_backup_dir.join(fname);
                                fs::copy(&path, &dest)?;
                                let data = fs::read(&dest)?;
                                let checksum = crc32fast::hash(&data);
                                manifest.files.push(BackupFile {
                                    name: format!("indexes/{}", fname),
                                    size: data.len() as u64,
                                    file_type: BackupFileType::Index,
                                    checksum,
                                });
                            }
                        }
                    }
                }
            }
        }

        // Write manifest
        let manifest_json = serde_json::to_string_pretty(&manifest)
            .map_err(|e| onto_core::CoreError::Serialization(e.to_string()))?;
        fs::write(backup_dir.join("manifest.json"), manifest_json)?;

        tracing::info!(
            "Incremental backup completed: {} new/modified files, {} bytes total",
            manifest.files.len(),
            manifest.files.iter().map(|f| f.size).sum::<u64>()
        );

        Ok(manifest)
    }

    /// Verifies a backup's integrity by checking all files exist and checksums match.
    ///
    /// Returns `Ok(())` if all checks pass, or an error describing what's wrong.
    pub fn verify_backup(backup_dir: &Path) -> Result<()> {
        let manifest_path = backup_dir.join("manifest.json");
        if !manifest_path.exists() {
            return Err(onto_core::CoreError::Custom(
                "manifest.json not found in backup directory".to_string(),
            ));
        }

        let manifest_bytes = fs::read(&manifest_path)?;
        let manifest: BackupManifest = serde_json::from_slice(&manifest_bytes)
            .map_err(|e| onto_core::CoreError::Serialization(e.to_string()))?;

        let mut errors: Vec<String> = Vec::new();

        for file in &manifest.files {
            let path = backup_dir.join(&file.name);

            // Check file exists
            if !path.exists() {
                errors.push(format!("missing file: {}", file.name));
                continue;
            }

            // Check file size
            let meta = fs::metadata(&path)?;
            if meta.len() != file.size {
                errors.push(format!(
                    "size mismatch for {}: expected {} bytes, found {} bytes",
                    file.name,
                    file.size,
                    meta.len()
                ));
                continue;
            }

            // Check checksum (skip if 0 — backward compat with old manifests)
            if file.checksum != 0 {
                let data = fs::read(&path)?;
                let actual_checksum = crc32fast::hash(&data);
                if actual_checksum != file.checksum {
                    errors.push(format!(
                        "checksum mismatch for {}: expected {:#010x}, found {:#010x}",
                        file.name, file.checksum, actual_checksum
                    ));
                }
            }
        }

        if errors.is_empty() {
            tracing::info!(
                "Backup verification passed: {} files OK",
                manifest.files.len()
            );
            Ok(())
        } else {
            let msg = format!("Backup verification failed:\n  {}", errors.join("\n  "));
            Err(onto_core::CoreError::Custom(msg))
        }
    }

    /// Flushes all disk-based indexes to disk (with fsync).
    fn flush_disk_indexes(&self) -> Result<()> {
        self.index_manager
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .flush_disk_indexes();
        Ok(())
    }
}

/// Engine statistics.
#[derive(Debug)]
pub struct EngineStats {
    pub memtable_size: usize,
    pub memtable_entries: usize,
    pub num_levels: usize,
    pub total_sstables: usize,
    pub total_sst_size: u64,
}

/// Backup manifest: lists all files in a backup snapshot.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BackupManifest {
    /// ISO 8601 timestamp of when the backup was created.
    pub timestamp: String,
    /// List of files included in the backup.
    pub files: Vec<BackupFile>,
    /// Backup type: "full" or "incremental".
    #[serde(default = "default_backup_type")]
    pub backup_type: String,
    /// For incremental backups: ISO 8601 timestamp of the base backup.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_timestamp: Option<String>,
}

fn default_backup_type() -> String {
    "full".to_string()
}

/// A single file in a backup.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BackupFile {
    /// Relative path within the backup (e.g., "L0_0.sst", "indexes/Product_price.idx").
    pub name: String,
    /// File size in bytes.
    pub size: u64,
    /// Type of file.
    pub file_type: BackupFileType,
    /// CRC32 checksum of the file content.
    #[serde(default)]
    pub checksum: u32,
}

/// Type of backed-up file.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum BackupFileType {
    SSTable,
    Wal,
    Index,
}

/// Returns a simple ISO 8601 timestamp string (no external dependency).
fn chrono_timestamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = dur.as_secs();
    // Convert to approximate date/time (UTC)
    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;

    // Simplified date from days since epoch (good enough for timestamps)
    let mut y = 1970;
    let mut remaining = days;
    loop {
        let days_in_year = if is_leap_year(y) { 366 } else { 365 };
        if remaining < days_in_year {
            break;
        }
        remaining -= days_in_year;
        y += 1;
    }
    let leap = is_leap_year(y);
    let month_days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut m = 0;
    while m < 12 && remaining >= month_days[m] {
        remaining -= month_days[m];
        m += 1;
    }
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y,
        m + 1,
        remaining + 1,
        hours,
        minutes,
        seconds
    )
}

fn is_leap_year(y: u64) -> bool {
    (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_prefix_may_overlap() {
        // prefix == min_key �?true
        assert!(LsmEngine::prefix_may_overlap(b"002", b"002", b"005"));
        // prefix < min_key, but min_key starts with prefix �?true
        assert!(LsmEngine::prefix_may_overlap(b"00", b"002", b"005"));
        // prefix < min_key, min_key does NOT start with prefix �?false
        assert!(!LsmEngine::prefix_may_overlap(b"001", b"002", b"005"));
        // prefix in range �?true
        assert!(LsmEngine::prefix_may_overlap(b"003", b"002", b"005"));
        // prefix == max_key �?true
        assert!(LsmEngine::prefix_may_overlap(b"005", b"002", b"005"));
        // prefix > max_key �?false
        assert!(!LsmEngine::prefix_may_overlap(b"006", b"002", b"005"));
        // prefix much larger �?false
        assert!(!LsmEngine::prefix_may_overlap(b"Z", b"A", b"B"));
        // empty prefix matches everything �?true
        assert!(LsmEngine::prefix_may_overlap(b"", b"A", b"Z"));
        // prefix entirely before the SST range
        assert!(!LsmEngine::prefix_may_overlap(b"001", b"002", b"005"));
    }

    /// count_prefix must agree with scan_prefix().len() and ignore tombstones.
    #[test]
    fn test_count_prefix_matches_scan_and_handles_tombstones() {
        let dir = tempdir().expect("should be valid");
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            memtable_size_limit: 1024 * 1024,
            ..Default::default()
        };
        let engine = LsmEngine::open(options).expect("should be valid");

        for i in 0..20u32 {
            let key = format!("Product::{i:03}");
            engine
                .put(key.into_bytes(), format!("v{i}").into_bytes())
                .expect("should be valid");
        }
        // Tombstones: 3 deletes should reduce live count
        for i in 0..3u32 {
            let key = format!("Product::{i:03}");
            engine.delete(key.into_bytes()).expect("should be valid");
        }
        // Unrelated prefix must not be counted
        engine
            .put(b"Order::001".to_vec(), b"x".to_vec())
            .expect("should be valid");

        let (live, tombs) = engine.count_prefix(b"Product::").expect("should be valid");
        assert_eq!(live, 17, "20 puts minus 3 deletes");
        assert_eq!(tombs, 3);

        let scanned = engine.scan_prefix(b"Product::").expect("should be valid");
        assert_eq!(scanned.len(), live, "count_prefix must match scan_prefix");

        let (olive, _) = engine.count_prefix(b"Order::").expect("should be valid");
        assert_eq!(olive, 1);
    }

    /// scan_prefix_limit must return the latest version of each key and
    /// stop after `limit` live entries (newest-first early exit).
    #[test]
    fn test_scan_prefix_limit_latest_wins_and_early_exit() {
        let dir = tempdir().expect("should be valid");
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            memtable_size_limit: 256, // force multiple SSTs
            ..Default::default()
        };
        let engine = LsmEngine::open(options).expect("should be valid");

        // 50 keys; overwrite key 000 with a newer value after some flushes
        for i in 0..50u32 {
            let key = format!("T::{i:03}");
            engine
                .put(key.into_bytes(), format!("old{i}").into_bytes())
                .expect("should be valid");
        }
        engine.flush().expect("should be valid");
        engine
            .put(b"T::000".to_vec(), b"NEWEST".to_vec())
            .expect("should be valid");
        engine
            .delete(b"T::001".to_vec())
            .expect("should be valid");

        // Unlimited scan_prefix sees 49 live (50 - 1 delete)
        let all = engine.scan_prefix(b"T::").expect("should be valid");
        assert_eq!(all.len(), 49);

        // Limited: 5 entries, must include latest T::000 value if present
        let lim = engine.scan_prefix_limit(b"T::", 5).expect("should be valid");
        assert!(lim.len() <= 5, "must respect limit, got {}", lim.len());
        assert!(!lim.is_empty());
        // If 000 is in the batch (newest-first should surface it early), value must be NEWEST
        if let Some((_, v)) = lim.iter().find(|(k, _)| k.as_slice() == b"T::000") {
            assert_eq!(v.as_slice(), b"NEWEST", "must return latest version");
        }
        // T::001 is deleted — must not appear
        assert!(!lim.iter().any(|(k, _)| k.as_slice() == b"T::001"));

        // limit=1 returns exactly 1 live entry
        let one = engine.scan_prefix_limit(b"T::", 1).expect("should be valid");
        assert_eq!(one.len(), 1);
    }

    /// count_prefix_with applies a per-value predicate without materializing
    /// values into the dedup map, and only evaluates the winning version.
    #[test]
    fn test_count_prefix_with_filter_and_tombstones() {
        let dir = tempdir().expect("should be valid");
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            memtable_size_limit: 1024 * 1024,
            ..Default::default()
        };
        let engine = LsmEngine::open(options).expect("should be valid");

        // 20 puts: even indices have value "keep*", odd have "drop*"
        for i in 0..20u32 {
            let key = format!("Product::{i:03}");
            let val = if i % 2 == 0 { format!("keep{i}") } else { format!("drop{i}") };
            engine
                .put(key.into_bytes(), val.into_bytes())
                .expect("should be valid");
        }
        // Delete 3 keys: 000 (keep), 001 (drop), 002 (keep)
        for i in 0..3u32 {
            let key = format!("Product::{i:03}");
            engine.delete(key.into_bytes()).expect("should be valid");
        }
        // Overwrite one odd key to "keep" via a newer Put — filter must see the winner
        engine
            .put(b"Product::005".to_vec(), b"keep-overwritten".to_vec())
            .expect("should be valid");

        // Unfiltered: count_prefix_with(|_| true) == count_prefix
        let (all, tombs) = engine
            .count_prefix_with(b"Product::", None, |_| Ok(true))
            .expect("should be valid");
        let (live, tombs2) = engine.count_prefix(b"Product::").expect("should be valid");
        assert_eq!(all, live);
        assert_eq!(tombs, tombs2);
        assert_eq!(live, 17, "20 puts minus 3 deletes");
        assert_eq!(tombs, 3);

        // Filtered: only values starting with "keep"
        let (matched, _) = engine
            .count_prefix_with(b"Product::", None, |v| {
                Ok(v.starts_with(b"keep"))
            })
            .expect("should be valid");
        // Deletes remove 000,001,002 (loop 0..3). Surviving "keep" evens:
        // 004,006,008,010,012,014,016,018 = 8, plus 005 overwritten to keep = 9.
        assert_eq!(matched, 9, "8 surviving evens + overwritten 005");

        // Pred on the losing (older) version must not be used: 005's old value was "drop5"
        let (drops, _) = engine
            .count_prefix_with(b"Product::", None, |v| Ok(v.starts_with(b"drop")))
            .expect("should be valid");
        // Odd "drop" survivors: 003,007,009,011,013,015,017,019 (001 deleted, 005 overwritten)
        assert_eq!(drops, 8);
    }

    /// count_prefix_with after flush agrees with pre-flush results.
    #[test]
    fn test_count_prefix_with_across_flush() {
        let dir = tempdir().expect("should be valid");
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            memtable_size_limit: 256,
            ..Default::default()
        };
        let engine = LsmEngine::open(options).expect("should be valid");

        for i in 0..50u32 {
            let key = format!("Item::{i:04}");
            let val = if i % 3 == 0 { b"a".to_vec() } else { b"b".to_vec() };
            engine.put(key.into_bytes(), val).expect("should be valid");
        }

        let before = engine
            .count_prefix_with(b"Item::", None, |v| Ok(v == b"a"))
            .expect("should be valid");
        engine.flush().expect("should be valid");
        let after = engine
            .count_prefix_with(b"Item::", None, |v| Ok(v == b"a"))
            .expect("should be valid");
        assert_eq!(before.0, after.0, "matched count must survive flush");
        assert_eq!(after.0, 17, "i%3==0 for 0..50 → 17 items");
    }

    /// Version selection must follow level order (newest first), never `seq`.
    /// `clean_index` can zero `seq`, after which `w_seq == seq` matching picks
    /// the OLDER level's value. This test overwrites one key across many
    /// flushes so several SSTs hold different versions of it.
    #[test]
    fn test_version_selection_prefers_newest_across_flushes() {
        let dir = tempdir().expect("should be valid");
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            memtable_size_limit: 256,
            ..Default::default()
        };
        let engine = LsmEngine::open(options).expect("should be valid");

        // Write v0..v4 of the same key, flushing after each so each version
        // lands in its own SST. Also write filler keys to force separate SSTs.
        for i in 0..5u32 {
            let filler = format!("K::fill{i:03}");
            engine
                .put(filler.into_bytes(), b"fill".to_vec())
                .expect("should be valid");
            engine
                .put(b"K::target".to_vec(), format!("v{i}").into_bytes())
                .expect("should be valid");
            engine.flush().expect("should be valid");
        }

        let mut seen: std::collections::HashMap<Vec<u8>, Vec<u8>> = std::collections::HashMap::new();
        engine
            .for_each_live_prefix(b"K::", None, None, |k, v| {
                assert!(
                    seen.insert(k.to_vec(), v.to_vec()).is_none(),
                    "key visited twice"
                );
                Ok(())
            })
            .expect("should be valid");
        assert_eq!(
            seen.get(b"K::target".as_slice()).map(|v| v.as_slice()),
            Some(&b"v4"[..]),
            "for_each_live_prefix must see the newest version"
        );

        // count_prefix_with must agree: filter matching only v4 sees 1,
        // filter matching only v0 sees 0.
        let only_v4 = engine
            .count_prefix_with(b"K::", None, |v| Ok(v == b"v4"))
            .expect("should be valid");
        assert_eq!(only_v4.0, 1, "only the newest version counts");
        let only_v0 = engine
            .count_prefix_with(b"K::", None, |v| Ok(v == b"v0"))
            .expect("should be valid");
        assert_eq!(only_v0.0, 0, "superseded version must not count");
    }

    /// Regression: concurrent flush_memtable calls used to race on the single
    /// `immutable_memtable` slot — the loser's entries vanished from reads
    /// (lost UPDATE/DELETE under bulk import). Four writers + two flushers on
    /// a tiny memtable must never lose a write.
    #[test]
    fn test_concurrent_flushes_lose_no_writes() {
        use std::sync::Arc;
        let dir = tempdir().expect("should be valid");
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            memtable_size_limit: 4096,
            ..Default::default()
        };
        let engine = Arc::new(LsmEngine::open(options).expect("should be valid"));

        let mut handles = Vec::new();
        for t in 0..4u32 {
            let e = Arc::clone(&engine);
            handles.push(std::thread::spawn(move || {
                for i in 0..300u32 {
                    let key = format!("R::{t}::{i:04}");
                    e.put(key.into_bytes(), format!("v{i}").into_bytes())
                        .expect("put");
                }
            }));
        }
        for _ in 0..2 {
            let e = Arc::clone(&engine);
            handles.push(std::thread::spawn(move || {
                for _ in 0..80 {
                    let _ = e.flush();
                    std::thread::yield_now();
                }
            }));
        }
        for h in handles {
            h.join().expect("join");
        }

        for t in 0..4u32 {
            for i in 0..300u32 {
                let key = format!("R::{t}::{i:04}");
                let got = engine.get(key.as_bytes()).expect("get");
                assert_eq!(
                    got,
                    Some(format!("v{i}").into_bytes()),
                    "lost write {key}"
                );
            }
        }
        engine.shutdown();
    }

    /// Regression: a bulk DELETE's tombstones used to be dropped when a
    /// concurrent flush raced (production incident: DELETE reported N rows
    /// deleted, rows resurfaced). Under flush pressure tombstones must stick.
    #[test]
    fn test_concurrent_flushes_preserve_delete_tombstones() {
        use std::sync::Arc;
        let dir = tempdir().expect("should be valid");
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            memtable_size_limit: 4096,
            ..Default::default()
        };
        let engine = Arc::new(LsmEngine::open(options).expect("should be valid"));

        for i in 0..2000u32 {
            let key = format!("D::{i:04}");
            engine.put(key.into_bytes(), b"x".to_vec()).expect("put");
        }

        let mut handles = Vec::new();
        for _ in 0..2 {
            let e = Arc::clone(&engine);
            handles.push(std::thread::spawn(move || {
                for _ in 0..100 {
                    let _ = e.flush();
                    std::thread::yield_now();
                }
            }));
        }
        // Bulk-delete half the rows while flushes race.
        for i in 0..1000u32 {
            let key = format!("D::{i:04}");
            engine.delete(key.into_bytes()).expect("delete");
        }
        for h in handles {
            h.join().expect("join");
        }

        for i in 0..1000u32 {
            let key = format!("D::{i:04}");
            assert_eq!(
                engine.get(key.as_bytes()).expect("get"),
                None,
                "tombstone lost: {key}"
            );
        }
        for i in 1000..2000u32 {
            let key = format!("D::{i:04}");
            assert!(
                engine.get(key.as_bytes()).expect("get").is_some(),
                "live row lost: {key}"
            );
        }
        engine.shutdown();
    }

    /// Regression: UPDATE-style txn_put/commit must survive shutdown+reopen
    /// even when the memtable is never flushed. Mirrors the fp-refill pattern
    /// (one document rewrite per transaction) that lost 86k writes on restart.
    #[test]
    fn test_txn_updates_survive_restart_without_flush() {
        let dir = tempdir().expect("should be valid");
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            memtable_size_limit: 128 * 1024 * 1024, // never flush
            ..Default::default()
        };
        let engine = LsmEngine::open(options.clone()).expect("open");

        // Base docs (original insert path).
        for i in 0..200u32 {
            let key = format!("C::{i:04}");
            let val = format!(r#"{{"id":"{}","smiles":"CCO","fp":null}}"#, key);
            engine.put(key.into_bytes(), val.into_bytes()).expect("put");
        }

        // UPDATE-style rewrites: one txn per row (like refill_fp_v3).
        for i in 0..200u32 {
            let key = format!("C::{i:04}");
            let txn = engine.begin_txn();
            let val = format!(r#"{{"id":"{}","smiles":"CCO","fp":"REFILLED"}}"#, key);
            engine
                .txn_put(txn, key.into_bytes(), val.into_bytes())
                .expect("txn_put");
            engine.commit_txn(txn).expect("commit");
        }

        // Simulated crash: WAL synced, memtable NOT flushed.
        engine.shutdown();
        drop(engine);

        let engine2 = LsmEngine::open(options).expect("reopen");
        let mut missing = 0u32;
        for i in 0..200u32 {
            let key = format!("C::{i:04}");
            let got = engine2.get(key.as_bytes()).expect("get");
            match got {
                Some(v) => {
                    let s = String::from_utf8_lossy(&v);
                    if !s.contains("REFILLED") {
                        missing += 1;
                        if missing <= 3 {
                            eprintln!("LOST update on {key}: {s}");
                        }
                    }
                }
                None => {
                    missing += 1;
                    if missing <= 3 {
                        eprintln!("LOST row entirely: {key}");
                    }
                }
            }
        }
        assert_eq!(missing, 0, "{missing}/200 txn updates lost across restart");
        engine2.shutdown();
    }

    /// Same pattern but with concurrent flushes during the update window —
    /// exercises reset_wal_internal while updates are still landing.
    #[test]
    fn test_txn_updates_survive_restart_with_flushes() {
        use std::sync::Arc;
        let dir = tempdir().expect("should be valid");
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            memtable_size_limit: 4096, // force frequent flushes
            ..Default::default()
        };
        let engine = Arc::new(LsmEngine::open(options.clone()).expect("open"));

        for i in 0..300u32 {
            let key = format!("F::{i:04}");
            let val = format!(r#"{{"id":"{}","fp":null}}"#, key);
            engine.put(key.into_bytes(), val.into_bytes()).expect("put");
        }

        let flusher = {
            let e = Arc::clone(&engine);
            std::thread::spawn(move || {
                for _ in 0..40 {
                    let _ = e.flush();
                    std::thread::yield_now();
                }
            })
        };

        for i in 0..300u32 {
            let key = format!("F::{i:04}");
            let txn = engine.begin_txn();
            let val = format!(r#"{{"id":"{}","fp":"REFILLED"}}"#, key);
            engine
                .txn_put(txn, key.into_bytes(), val.into_bytes())
                .expect("txn_put");
            engine.commit_txn(txn).expect("commit");
        }

        flusher.join().expect("join");
        engine.shutdown();
        drop(engine);

        let engine2 = LsmEngine::open(options).expect("reopen");
        let mut missing = 0u32;
        for i in 0..300u32 {
            let key = format!("F::{i:04}");
            let got = engine2.get(key.as_bytes()).expect("get");
            let ok = got
                .as_deref()
                .map(|v| String::from_utf8_lossy(v).contains("REFILLED"))
                .unwrap_or(false);
            if !ok {
                missing += 1;
                if missing <= 3 {
                    eprintln!("LOST update on {key}: {got:?}");
                }
            }
        }
        assert_eq!(missing, 0, "{missing}/300 txn updates lost across restart");
        engine2.shutdown();
    }

    /// Explicit single flush then read back — isolates persist_immutable_to_sst.
    #[test]
    fn test_explicit_flush_preserves_keys() {
        let dir = tempdir().expect("should be valid");
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            memtable_size_limit: 256 * 1024 * 1024, // no auto flush
            ..Default::default()
        };
        let engine = LsmEngine::open(options).expect("open");
        let n = 20u32;
        for i in 0..n {
            let key = format!("sembio.Compound::CHEMBL{i}");
            engine
                .put(key.into_bytes(), format!("value-{i}").into_bytes())
                .expect("put");
        }
        engine.flush().expect("flush");
        let mut missing = Vec::new();
        for i in 0..n {
            let key = format!("sembio.Compound::CHEMBL{i}");
            match engine.get(key.as_bytes()).expect("get") {
                Some(v) if v == format!("value-{i}").into_bytes() => {}
                other => missing.push((i, other.map(|v| v.len()))),
            }
        }
        assert!(missing.is_empty(), "after explicit flush missing {missing:?}");
        engine.shutdown();
    }

    /// Same keys as the flush test but with a huge memtable (no flush).
    #[test]
    fn test_puts_stay_visible_no_flush() {
        let dir = tempdir().expect("should be valid");
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            memtable_size_limit: 256 * 1024 * 1024,
            ..Default::default()
        };
        let engine = LsmEngine::open(options).expect("open");
        let n = 200u32;
        for i in 0..n {
            let key = format!("sembio.Compound::CHEMBL{i}");
            engine
                .put(key.into_bytes(), format!("value-{i}").into_bytes())
                .expect("put");
        }
        let mut missing = Vec::new();
        for i in 0..n {
            let key = format!("sembio.Compound::CHEMBL{i}");
            if engine.get(key.as_bytes()).expect("get").is_none() {
                missing.push(i);
            }
        }
        assert!(missing.is_empty(), "no-flush missing {missing:?}");
        engine.shutdown();
    }

    /// Direct SST round-trip: build an SST with a few known keys and get_full
    /// each one back. Isolates the block-index / restart-point lookup.
    /// Keys must be added in sorted order (SST builder contract).
    #[test]
    fn test_sst_get_full_roundtrip() {
        let dir = tempdir().expect("should be valid");
        let path = dir.path().join("t.sst");
        let mut b = crate::lsm::sstable::SsTableBuilder::new();
        // Sorted order: CHEMBL0, CHEMBL1, CHEMBL10..19, CHEMBL2, CHEMBL20..29, CHEMBL3..
        let mut keys: Vec<(String, u32)> = (0..30u32).map(|i| (format!("sembio.Compound::CHEMBL{i}"), i)).collect();
        keys.sort_by(|a, b| a.0.cmp(&b.0));
        for (key, i) in &keys {
            b.add_owned(onto_core::Entry::put(
                key.clone().into_bytes(),
                format!("value-{i}").into_bytes(),
                *i as u64 + 1,
            ));
        }
        let sst = b.build(&path).expect("build");
        let mut missing = Vec::new();
        for i in 0..30u32 {
            let key = format!("sembio.Compound::CHEMBL{i}");
            match sst.get_full(key.as_bytes()).expect("get_full") {
                Some((v, _, _)) => {
                    if v != format!("value-{i}").into_bytes() {
                        missing.push((i, "wrong"));
                    }
                }
                None => missing.push((i, "absent")),
            }
        }
        assert!(
            missing.is_empty(),
            "SST get_full missing {:?}",
            &missing[..missing.len().min(10)]
        );
    }

    /// Minimal repro: plain put() with frequent flush — every key must stay
    /// visible while the engine is still open (no restart involved).
    #[test]
    fn test_puts_stay_visible_across_flush() {
        use onto_core::binary_row::map_to_binary;
        use serde_json::{Map, Value};

        let dir = tempdir().expect("should be valid");
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            memtable_size_limit: 4096,
            ..Default::default()
        };
        let engine = LsmEngine::open(options).expect("open");
        let n = 200u32;
        for i in 0..n {
            let key = format!("sembio.Compound::CHEMBL{i}");
            let mut m = Map::new();
            m.insert("id".into(), Value::String(format!("CHEMBL{i}")));
            m.insert(
                "smiles".into(),
                Value::String(
                    "Nc1nc2ccn(CCCCc3ccc(C(=O)N[C@@H](CCC(=O)O)C(=O)O)cc3)c2c(=O)[nH]1".into(),
                ),
            );
            m.insert("fp".into(), Value::String("null".into()));
            engine
                .put(key.into_bytes(), map_to_binary(&m))
                .expect("put");
        }
        let mut missing = Vec::new();
        for i in 0..n {
            let key = format!("sembio.Compound::CHEMBL{i}");
            match engine.get(key.as_bytes()).expect("get") {
                Some(v) => {
                    if !v.windows(4).any(|w| w == b"null") {
                        missing.push((i, "wrong-value"));
                    }
                }
                None => missing.push((i, "absent")),
            }
        }
        assert!(
            missing.is_empty(),
            "{} / {} puts invisible after flush: {:?}",
            missing.len(),
            n,
            &missing[..missing.len().min(10)]
        );
        engine.shutdown();
    }

    /// Simulate a hard process kill (no shutdown()): engine is dropped while
    /// WAL has been flushed to OS cache via commit_txn's flush_buf. Also uses
    /// BinaryRow-sized documents closer to real Compound rows.
    #[test]
    fn test_txn_updates_survive_hard_kill() {
        use onto_core::binary_row::map_to_binary;
        use serde_json::{Map, Value};

        let dir = tempdir().expect("should be valid");
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            memtable_size_limit: 256 * 1024, // force frequent flush / reset_wal
            ..Default::default()
        };

        let mk_doc = |i: u32, fp: &str| {
            let mut m = Map::new();
            m.insert("id".into(), Value::String(format!("CHEMBL{i}")));
            m.insert(
                "smiles".into(),
                Value::String(
                    "Nc1nc2ccn(CCCCc3ccc(C(=O)N[C@@H](CCC(=O)O)C(=O)O)cc3)c2c(=O)[nH]1".into(),
                ),
            );
            m.insert("fp".into(), Value::String(fp.to_string()));
            m.insert("molregno".into(), Value::String(format!("{}", 2857819 + i)));
            map_to_binary(&m)
        };

        {
            let engine = LsmEngine::open(options.clone()).expect("open");
            for i in 0..400u32 {
                let key = format!("sembio.Compound::CHEMBL{i}");
                engine
                    .put(key.into_bytes(), mk_doc(i, "null"))
                    .expect("put");
            }
            // Base puts must all be visible.
            let mut base_ok = 0u32;
            for i in 0..400u32 {
                let key = format!("sembio.Compound::CHEMBL{i}");
                if engine.get(key.as_bytes()).expect("get").is_some() {
                    base_ok += 1;
                }
            }
            eprintln!("post-put visible: {base_ok}/400");
            for i in 0..400u32 {
                let key = format!("sembio.Compound::CHEMBL{i}");
                let txn = engine.begin_txn();
                engine
                    .txn_put(txn, key.into_bytes(), mk_doc(i, "REFILLED"))
                    .expect("txn_put");
                engine.commit_txn(txn).expect("commit");
            }
            // Confirm updates are visible before the "crash".
            let mut live_ok = 0u32;
            let mut stale = 0u32;
            let mut gone = 0u32;
            let mut stale_idx = Vec::new();
            for i in 0..400u32 {
                let key = format!("sembio.Compound::CHEMBL{i}");
                match engine.get(key.as_bytes()).expect("get") {
                    Some(v) => {
                        if v.windows(8).any(|w| w == b"REFILLED") {
                            live_ok += 1;
                        } else {
                            stale += 1;
                            if stale_idx.len() < 10 {
                                stale_idx.push(i);
                            }
                        }
                    }
                    None => {
                        gone += 1;
                        if stale_idx.len() < 10 {
                            stale_idx.push(i);
                        }
                    }
                }
            }
            eprintln!("pre-crash: ok={live_ok} stale={stale} gone={gone} sample_bad={stale_idx:?}");
            assert_eq!(live_ok, 400, "updates not even visible before drop");
            // HARD KILL: no shutdown(), no flush(). Drop the engine.
            drop(engine);
        }

        let engine2 = LsmEngine::open(options).expect("reopen");
        let mut missing = Vec::new();
        for i in 0..400u32 {
            let key = format!("sembio.Compound::CHEMBL{i}");
            let got = engine2.get(key.as_bytes()).expect("get");
            let ok = got
                .as_deref()
                .map(|v| {
                    v.windows(8).any(|w| w == b"REFILLED")
                })
                .unwrap_or(false);
            if !ok {
                missing.push((i, got.as_ref().map(|v| v.len())));
            }
        }
        if !missing.is_empty() {
            let idxs: Vec<u32> = missing.iter().map(|(i, _)| *i).collect();
            eprintln!("lost {} rows, idxs[:20]={:?} idxs[-10:]={:?}", missing.len(), &idxs[..idxs.len().min(20)], &idxs[idxs.len().saturating_sub(10)..]);
            for (i, glen) in missing.iter().take(5) {
                eprintln!("  CHEMBL{i} get_len={glen:?}");
            }
            // Count via scan too
            let mut scan_n = 0usize;
            let mut scan_refilled = 0usize;
            let _ = engine2.for_each_live_prefix(b"sembio.Compound::", None, None, |_, v| {
                scan_n += 1;
                if v.windows(8).any(|w| w == b"REFILLED") {
                    scan_refilled += 1;
                }
                Ok(())
            });
            eprintln!("scan: total={scan_n} refilled={scan_refilled}");
        }
        assert_eq!(missing.len(), 0, "{}/400 updates lost after hard kill", missing.len());
        engine2.shutdown();
    }

    /// Regression: reset_wal_internal used to blind-replace wal.log, dropping
    /// records appended during the SST build window. Those writes stayed
    /// visible until restart but were unrecoverable after. Simulate a crash
    /// via shutdown() (syncs WAL, does NOT flush memtable) + reopen.
    #[test]
    fn test_wal_covers_writes_during_flush_windows() {
        use std::sync::Arc;
        let dir = tempdir().expect("should be valid");
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            memtable_size_limit: 4096,
            ..Default::default()
        };
        let engine = Arc::new(LsmEngine::open(options.clone()).expect("should be valid"));

        let mut handles = Vec::new();
        for t in 0..3u32 {
            let e = Arc::clone(&engine);
            handles.push(std::thread::spawn(move || {
                for i in 0..250u32 {
                    let key = format!("W::{t}::{i:04}");
                    e.put(key.into_bytes(), format!("v{i}").into_bytes())
                        .expect("put");
                }
            }));
        }
        for _ in 0..2 {
            let e = Arc::clone(&engine);
            handles.push(std::thread::spawn(move || {
                for _ in 0..60 {
                    let _ = e.flush();
                    std::thread::yield_now();
                }
            }));
        }
        for h in handles {
            h.join().expect("join");
        }
        // Simulated crash: WAL is synced, memtable is not flushed to SST.
        engine.shutdown();
        drop(engine);

        let engine2 = LsmEngine::open(options).expect("reopen");
        for t in 0..3u32 {
            for i in 0..250u32 {
                let key = format!("W::{t}::{i:04}");
                let got = engine2.get(key.as_bytes()).expect("get");
                assert_eq!(
                    got,
                    Some(format!("v{i}").into_bytes()),
                    "write lost across crash: {key}"
                );
            }
        }
        engine2.shutdown();
    }

    /// for_each_live_prefix visits each live key once with the winning value,
    /// without requiring the caller to hold the full result set.
    #[test]
    fn test_for_each_live_prefix_winners_and_overlay() {
        let dir = tempdir().expect("should be valid");
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            memtable_size_limit: 256,
            ..Default::default()
        };
        let engine = LsmEngine::open(options).expect("should be valid");

        for i in 0..30u32 {
            let key = format!("T::{i:03}");
            engine
                .put(key.into_bytes(), format!("v{i}").into_bytes())
                .expect("should be valid");
        }
        // Overwrite one key (newer wins)
        engine
            .put(b"T::005".to_vec(), b"v5-new".to_vec())
            .expect("should be valid");
        // Delete two keys
        engine.delete(b"T::001".to_vec()).expect("should be valid");
        engine.delete(b"T::002".to_vec()).expect("should be valid");
        engine.flush().expect("should be valid");

        let mut seen: std::collections::HashMap<Vec<u8>, Vec<u8>> = std::collections::HashMap::new();
        engine
            .for_each_live_prefix(b"T::", None, None, |k, v| {
                assert!(
                    seen.insert(k.to_vec(), v.to_vec()).is_none(),
                    "key {} visited twice",
                    String::from_utf8_lossy(k)
                );
                Ok(())
            })
            .expect("should be valid");
        assert_eq!(seen.len(), 28, "30 puts - 2 deletes");
        assert_eq!(seen.get(b"T::005".as_slice()).map(|v| v.as_slice()), Some(&b"v5-new"[..]));
        assert!(!seen.contains_key(b"T::001".as_slice()));
        assert!(!seen.contains_key(b"T::002".as_slice()));

        // Overlay: put one shadowed key + one new key + one delete
        let mut ov = std::collections::HashMap::new();
        ov.insert(b"T::005".to_vec(), Some(b"overlay-005".to_vec())); // shadow storage
        ov.insert(b"T::030".to_vec(), Some(b"overlay-030".to_vec())); // brand new
        ov.insert(b"T::003".to_vec(), None); // delete

        let mut seen2: std::collections::HashMap<Vec<u8>, Vec<u8>> = std::collections::HashMap::new();
        engine
            .for_each_live_prefix(b"T::", None, Some(&ov), |k, v| {
                seen2.insert(k.to_vec(), v.to_vec());
                Ok(())
            })
            .expect("should be valid");
        assert_eq!(seen2.len(), 28, "28 storage live - 003 + 030 = 28");
        assert_eq!(
            seen2.get(b"T::005".as_slice()).map(|v| v.as_slice()),
            Some(&b"overlay-005"[..])
        );
        assert_eq!(
            seen2.get(b"T::030".as_slice()).map(|v| v.as_slice()),
            Some(&b"overlay-030"[..])
        );
        assert!(!seen2.contains_key(b"T::003".as_slice()));
    }

    /// count_prefix after SST flush must see the same live rows as before flush.
    #[test]
    fn test_count_prefix_across_flush() {
        let dir = tempdir().expect("should be valid");
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            memtable_size_limit: 256, // force frequent flush
            ..Default::default()
        };
        let engine = LsmEngine::open(options).expect("should be valid");

        for i in 0..100u32 {
            let key = format!("Item::{i:04}");
            engine
                .put(key.into_bytes(), vec![b'v'; 40])
                .expect("should be valid");
        }
        for i in 0..10u32 {
            let key = format!("Item::{i:04}");
            engine.delete(key.into_bytes()).expect("should be valid");
        }
        engine.flush().expect("should be valid");

        let (live, tombs) = engine.count_prefix(b"Item::").expect("should be valid");
        assert_eq!(live, 90);
        assert_eq!(tombs, 10);
    }

    #[test]
    fn test_engine_basic_put_get() {
        let dir = tempdir().expect("should be valid");
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            memtable_size_limit: 1024 * 1024, // 1MB
            ..Default::default()
        };

        let engine = LsmEngine::open(options).expect("should be valid");

        engine
            .put(b"name".to_vec(), b"alice".to_vec())
            .expect("should be valid");
        engine
            .put(b"age".to_vec(), b"30".to_vec())
            .expect("should be valid");

        let val = engine.get(b"name").expect("should be valid");
        assert_eq!(val, Some(b"alice".to_vec()));

        let val = engine.get(b"age").expect("should be valid");
        assert_eq!(val, Some(b"30".to_vec()));

        let val = engine.get(b"missing").expect("should be valid");
        assert_eq!(val, None);
    }

    #[test]
    fn test_engine_overwrite() {
        let dir = tempdir().expect("should be valid");
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            ..Default::default()
        };

        let engine = LsmEngine::open(options).expect("should be valid");

        engine
            .put(b"key".to_vec(), b"v1".to_vec())
            .expect("should be valid");
        engine
            .put(b"key".to_vec(), b"v2".to_vec())
            .expect("should be valid");

        let val = engine.get(b"key").expect("should be valid");
        assert_eq!(val, Some(b"v2".to_vec()));
    }

    #[test]
    fn test_engine_delete() {
        let dir = tempdir().expect("should be valid");
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            ..Default::default()
        };

        let engine = LsmEngine::open(options).expect("should be valid");

        engine
            .put(b"key".to_vec(), b"value".to_vec())
            .expect("should be valid");
        assert!(engine.get(b"key").expect("should be valid").is_some());

        engine.delete(b"key".to_vec()).expect("should be valid");
        assert!(engine.get(b"key").expect("should be valid").is_none());
    }

    #[test]
    fn test_engine_flush_to_sstable() {
        let dir = tempdir().expect("should be valid");
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            memtable_size_limit: 128, // Very small to trigger flush
            ..Default::default()
        };

        let engine = LsmEngine::open(options).expect("should be valid");

        // Write enough data to trigger a flush
        for i in 0..20u32 {
            let key = format!("key_{:04}", i);
            let value = format!("value_{}", i);
            engine
                .put(key.into_bytes(), value.into_bytes())
                .expect("should be valid");
        }

        // All data should still be readable
        let val = engine.get(b"key_0005").expect("should be valid");
        assert_eq!(val, Some(b"value_5".to_vec()));

        let stats = engine.stats();
        assert!(
            stats.total_sstables > 0,
            "should have flushed at least one SSTable"
        );
    }

    #[test]
    fn test_engine_recovery() {
        let dir = tempdir().expect("should be valid");
        let data_dir = dir.path().to_path_buf();

        // Write data
        {
            let options = StorageOptions {
                data_dir: data_dir.clone(),
                ..Default::default()
            };
            let engine = LsmEngine::open(options).expect("should be valid");
            engine
                .put(b"key1".to_vec(), b"value1".to_vec())
                .expect("should be valid");
            engine
                .put(b"key2".to_vec(), b"value2".to_vec())
                .expect("should be valid");
            // Don't drop cleanly - simulate crash (WAL should persist)
        }

        // Recover
        {
            let options = StorageOptions {
                data_dir,
                ..Default::default()
            };
            let engine = LsmEngine::open(options).expect("should be valid");

            let val = engine.get(b"key1").expect("should be valid");
            assert_eq!(val, Some(b"value1".to_vec()));

            let val = engine.get(b"key2").expect("should be valid");
            assert_eq!(val, Some(b"value2".to_vec()));
        }
    }

    #[test]
    fn test_engine_compaction() {
        let dir = tempdir().expect("should be valid");
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            memtable_size_limit: 128, // Very small to trigger frequent flushes
            size_ratio: 2,            // Compact when level has > 2 SSTables
            ..Default::default()
        };

        let engine = LsmEngine::open(options).expect("should be valid");

        // Write enough data to trigger multiple flushes and compaction
        let num_keys = 100u32;
        for i in 0..num_keys {
            let key = format!("key_{:04}", i);
            let value = format!("value_{:06}", i); // Larger values to fill memtable faster
            engine
                .put(key.into_bytes(), value.into_bytes())
                .expect("should be valid");
        }

        let stats = engine.stats();
        println!(
            "After writing {} keys: {} SSTables across {} levels, total size {} bytes",
            num_keys, stats.total_sstables, stats.num_levels, stats.total_sst_size
        );

        // Verify all data is still readable after compaction
        for i in 0..num_keys {
            let key = format!("key_{:04}", i);
            let expected = format!("value_{:06}", i);
            let val = engine.get(key.as_bytes()).expect("should be valid");
            assert_eq!(
                val,
                Some(expected.into_bytes()),
                "key {} should be readable after compaction",
                key
            );
        }

        // Verify compaction actually happened (L0 should be smaller than without compaction)
        // With size_ratio=2 and 100 keys, we should have multiple levels
        assert!(
            stats.num_levels > 1 || stats.total_sstables <= 2,
            "compaction should have merged SSTables into deeper levels"
        );
    }

    /// File-count hard trigger: a crowded level must drain via bounded merge
    /// batches even when size-score stays under 1.0. Also covers deepest-level
    /// batched self-merge (L6 path) — previously file count only grew.
    #[test]
    fn test_file_count_trigger_merges_crowded_level() {
        let dir = tempdir().expect("should be valid");
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            // Tiny memtable → frequent flushes → many SSTs per level.
            memtable_size_limit: 64,
            size_ratio: 2,
            ..Default::default()
        };
        let engine = LsmEngine::open(options).expect("should be valid");

        // Enough writes to create well above FILE_COUNT_TRIGGER (50) SSTs
        // across levels once they fall through L0.
        let n = 600u32;
        for i in 0..n {
            let key = format!("k{:05}", i);
            let mut val = vec![b'v'; 40];
            val.extend_from_slice(i.to_string().as_bytes());
            engine.put(key.into_bytes(), val).expect("should be valid");
        }
        engine.flush().expect("flush");

        let before = engine.stats();
        // Drain: small-file merge + file-count trigger + size-score path.
        engine.flush_compaction().expect("compaction");
        // A second pass picks up whatever the first batch left behind.
        engine.flush_compaction().expect("compaction pass 2");

        let after = engine.stats();
        println!(
            "file-count trigger: sstables {} → {} (levels {} → {})",
            before.total_sstables, after.total_sstables, before.num_levels, after.num_levels
        );

        // Data must stay intact through batched merges.
        for i in 0..n {
            let key = format!("k{:05}", i);
            let val = engine.get(key.as_bytes()).expect("get").expect("key present");
            assert!(
                val.ends_with(i.to_string().as_bytes()),
                "value for {key} corrupted"
            );
        }

        // Crowded levels must not grow without bound. With ~600 tiny writes
        // the pre-compaction SST count is high; bounded batches should pull it
        // down or at least hold it (never a regression toward more files).
        assert!(
            after.total_sstables <= before.total_sstables,
            "file-count trigger must not increase SST count ({} → {})",
            before.total_sstables,
            after.total_sstables
        );
    }

    /// Overwrite-heavy workload: batched merge must dedup stale versions so
    /// file count shrinks and only the latest value is visible.
    #[test]
    fn test_batched_merge_dedups_stale_versions() {
        let dir = tempdir().expect("should be valid");
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            memtable_size_limit: 64,
            size_ratio: 2,
            ..Default::default()
        };
        let engine = LsmEngine::open(options).expect("should be valid");

        // Same 20 keys overwritten many times → multi-version SSTs.
        for round in 0..30u32 {
            for i in 0..20u32 {
                let key = format!("ov{:03}", i);
                let val = format!("r{round:02}_v{i}");
                engine.put(key.into_bytes(), val.into_bytes()).expect("put");
            }
        }
        engine.flush().expect("flush");
        engine.flush_compaction().expect("compaction");
        engine.flush_compaction().expect("compaction pass 2");

        // Latest version wins.
        for i in 0..20u32 {
            let key = format!("ov{:03}", i);
            let expected = format!("r29_v{i}");
            let got = engine.get(key.as_bytes()).expect("get").expect("present");
            assert_eq!(
                String::from_utf8_lossy(&got),
                expected,
                "stale version leaked for {key}"
            );
        }

        let stats = engine.stats();
        // 30 rounds × 20 keys = 600 puts on 20 distinct keys. After dedup
        // merge the SST count must be far below the raw put count.
        assert!(
            stats.total_sstables < 60,
            "dedup merge should collapse stale versions, got {} SSTs",
            stats.total_sstables
        );
    }

    #[test]
    fn test_engine_compaction_with_overwrites() {
        let dir = tempdir().expect("should be valid");
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            memtable_size_limit: 128,
            size_ratio: 2,
            ..Default::default()
        };

        let engine = LsmEngine::open(options).expect("should be valid");

        // Write initial data
        for i in 0..50u32 {
            let key = format!("key_{:04}", i);
            let value = format!("v1_{:06}", i);
            engine
                .put(key.into_bytes(), value.into_bytes())
                .expect("should be valid");
        }

        // Overwrite all keys with new values
        for i in 0..50u32 {
            let key = format!("key_{:04}", i);
            let value = format!("v2_{:06}", i);
            engine
                .put(key.into_bytes(), value.into_bytes())
                .expect("should be valid");
        }

        // Verify the latest values are returned
        for i in 0..50u32 {
            let key = format!("key_{:04}", i);
            let expected = format!("v2_{:06}", i);
            let val = engine.get(key.as_bytes()).expect("should be valid");
            assert_eq!(
                val,
                Some(expected.into_bytes()),
                "overwritten key {} should return latest value",
                key
            );
        }
    }

    #[test]
    fn test_engine_compaction_with_deletes() {
        let dir = tempdir().expect("should be valid");
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            memtable_size_limit: 128,
            size_ratio: 2,
            ..Default::default()
        };

        let engine = LsmEngine::open(options).expect("should be valid");

        // Write data
        for i in 0..50u32 {
            let key = format!("key_{:04}", i);
            let value = format!("value_{:06}", i);
            engine
                .put(key.into_bytes(), value.into_bytes())
                .expect("should be valid");
        }

        // Delete even-numbered keys
        for i in (0..50u32).step_by(2) {
            let key = format!("key_{:04}", i);
            engine.delete(key.into_bytes()).expect("should be valid");
        }

        // Flush remaining entries in memtable
        engine.flush().expect("should be valid");

        // Verify: odd keys exist, even keys are deleted
        for i in 0..50u32 {
            let key = format!("key_{:04}", i);
            let val = engine.get(key.as_bytes()).expect("should be valid");
            if i % 2 == 0 {
                assert!(
                    val.is_none(),
                    "deleted key {} should not exist, got {:?}",
                    key,
                    val
                );
            } else {
                assert!(val.is_some(), "non-deleted key {} should still exist", key);
            }
        }
    }

    #[test]
    fn test_compaction_scoring_and_tombstone_cleanup() {
        let dir = tempdir().expect("should be valid");
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            memtable_size_limit: 128,
            size_ratio: 2,
            ..Default::default()
        };

        let engine = LsmEngine::open(options).expect("should be valid");

        // Write data, then delete most of it
        for i in 0..80u32 {
            let key = format!("key_{:04}", i);
            let value = format!("value_{:06}", i);
            engine
                .put(key.into_bytes(), value.into_bytes())
                .expect("should be valid");
        }

        // Delete all but the last 10 keys
        for i in 0..70u32 {
            let key = format!("key_{:04}", i);
            engine.delete(key.into_bytes()).expect("should be valid");
        }

        // Force flush and compaction
        engine.flush().expect("should be valid");

        // Verify remaining keys
        for i in 70..80u32 {
            let key = format!("key_{:04}", i);
            let expected = format!("value_{:06}", i);
            let val = engine.get(key.as_bytes()).expect("should be valid");
            assert_eq!(val, Some(expected.into_bytes()), "key {} should exist", key);
        }

        // Verify deleted keys are gone
        for i in 0..70u32 {
            let key = format!("key_{:04}", i);
            let val = engine.get(key.as_bytes()).expect("should be valid");
            assert!(val.is_none(), "deleted key {} should not exist", key);
        }

        // Verify compaction scoring works: levels should be balanced
        let stats = engine.stats();
        assert!(stats.total_sstables > 0, "should have SSTables");
    }

    // ══════════════════════════════════════════════════════════════�?    //  MVCC Transaction Tests
    // ══════════════════════════════════════════════════════════════�?
    #[test]
    fn test_txn_basic_commit() {
        let dir = tempdir().expect("should be valid");
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            ..Default::default()
        };
        let engine = LsmEngine::open(options).expect("should be valid");

        let txn = engine.begin_txn();
        engine
            .txn_put(txn, b"name".to_vec(), b"alice".to_vec())
            .expect("should be valid");
        engine
            .txn_put(txn, b"age".to_vec(), b"30".to_vec())
            .expect("should be valid");
        engine.commit_txn(txn).expect("should be valid");

        assert_eq!(
            engine.get(b"name").expect("should be valid"),
            Some(b"alice".to_vec())
        );
        assert_eq!(
            engine.get(b"age").expect("should be valid"),
            Some(b"30".to_vec())
        );
    }

    #[test]
    fn test_txn_abort() {
        let dir = tempdir().expect("should be valid");
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            ..Default::default()
        };
        let engine = LsmEngine::open(options).expect("should be valid");

        let txn = engine.begin_txn();
        engine
            .txn_put(txn, b"name".to_vec(), b"alice".to_vec())
            .expect("should be valid");
        engine.abort_txn(txn).expect("should be valid");

        assert_eq!(engine.get(b"name").expect("should be valid"), None);
    }

    #[test]
    fn test_txn_read_own_writes() {
        let dir = tempdir().expect("should be valid");
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            ..Default::default()
        };
        let engine = LsmEngine::open(options).expect("should be valid");

        let txn = engine.begin_txn();
        engine
            .txn_put(txn, b"name".to_vec(), b"alice".to_vec())
            .expect("should be valid");
        engine
            .txn_put(txn, b"age".to_vec(), b"30".to_vec())
            .expect("should be valid");

        assert_eq!(
            engine.txn_get(txn, b"name").expect("should be valid"),
            Some(b"alice".to_vec())
        );
        assert_eq!(
            engine.txn_get(txn, b"age").expect("should be valid"),
            Some(b"30".to_vec())
        );

        engine.commit_txn(txn).expect("should be valid");
    }

    #[test]
    fn test_txn_snapshot_isolation() {
        let dir = tempdir().expect("should be valid");
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            ..Default::default()
        };
        let engine = LsmEngine::open(options).expect("should be valid");

        engine
            .put(b"key".to_vec(), b"v1".to_vec())
            .expect("should be valid");

        let txn1 = engine.begin_txn();

        engine
            .put(b"key".to_vec(), b"v2".to_vec())
            .expect("should be valid");

        // txn1 still sees v1 (snapshot isolation)
        assert_eq!(
            engine.txn_get(txn1, b"key").expect("should be valid"),
            Some(b"v1".to_vec())
        );

        let txn2 = engine.begin_txn();
        assert_eq!(
            engine.txn_get(txn2, b"key").expect("should be valid"),
            Some(b"v2".to_vec())
        );

        engine.commit_txn(txn1).expect("should be valid");
        engine.commit_txn(txn2).expect("should be valid");
    }

    #[test]
    fn test_txn_write_conflict_independence() {
        let dir = tempdir().expect("should be valid");
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            ..Default::default()
        };
        let engine = LsmEngine::open(options).expect("should be valid");

        let txn1 = engine.begin_txn();
        let txn2 = engine.begin_txn();

        engine
            .txn_put(txn1, b"a".to_vec(), b"1".to_vec())
            .expect("should be valid");
        engine
            .txn_put(txn2, b"b".to_vec(), b"2".to_vec())
            .expect("should be valid");

        engine.commit_txn(txn1).expect("should be valid");
        engine.commit_txn(txn2).expect("should be valid");

        assert_eq!(
            engine.get(b"a").expect("should be valid"),
            Some(b"1".to_vec())
        );
        assert_eq!(
            engine.get(b"b").expect("should be valid"),
            Some(b"2".to_vec())
        );
    }

    #[test]
    fn test_txn_delete_in_transaction() {
        let dir = tempdir().expect("should be valid");
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            ..Default::default()
        };
        let engine = LsmEngine::open(options).expect("should be valid");

        engine
            .put(b"key".to_vec(), b"value".to_vec())
            .expect("should be valid");

        let txn = engine.begin_txn();
        engine
            .txn_delete(txn, b"key".to_vec())
            .expect("should be valid");
        assert_eq!(engine.txn_get(txn, b"key").expect("should be valid"), None);

        engine.commit_txn(txn).expect("should be valid");
        assert_eq!(engine.get(b"key").expect("should be valid"), None);
    }

    #[test]
    fn test_txn_scan_prefix() {
        let dir = tempdir().expect("should be valid");
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            ..Default::default()
        };
        let engine = LsmEngine::open(options).expect("should be valid");

        engine
            .put(b"user:1".to_vec(), b"alice".to_vec())
            .expect("should be valid");
        engine
            .put(b"user:2".to_vec(), b"bob".to_vec())
            .expect("should be valid");
        engine
            .put(b"item:1".to_vec(), b"widget".to_vec())
            .expect("should be valid");

        let txn = engine.begin_txn();
        engine
            .txn_put(txn, b"user:3".to_vec(), b"charlie".to_vec())
            .expect("should be valid");

        let results = engine
            .txn_scan_prefix(txn, b"user:")
            .expect("should be valid");
        assert_eq!(results.len(), 3);

        engine.commit_txn(txn).expect("should be valid");
    }

    #[test]
    fn test_txn_overwrite_in_buffer() {
        let dir = tempdir().expect("should be valid");
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            ..Default::default()
        };
        let engine = LsmEngine::open(options).expect("should be valid");

        let txn = engine.begin_txn();
        engine
            .txn_put(txn, b"key".to_vec(), b"v1".to_vec())
            .expect("should be valid");
        engine
            .txn_put(txn, b"key".to_vec(), b"v2".to_vec())
            .expect("should be valid");

        assert_eq!(
            engine.txn_get(txn, b"key").expect("should be valid"),
            Some(b"v2".to_vec())
        );

        engine.commit_txn(txn).expect("should be valid");
        assert_eq!(
            engine.get(b"key").expect("should be valid"),
            Some(b"v2".to_vec())
        );
    }

    #[test]
    fn test_txn_active_count() {
        let dir = tempdir().expect("should be valid");
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            ..Default::default()
        };
        let engine = LsmEngine::open(options).expect("should be valid");

        assert_eq!(engine.active_txn_count(), 0);

        let t1 = engine.begin_txn();
        assert_eq!(engine.active_txn_count(), 1);

        let t2 = engine.begin_txn();
        assert_eq!(engine.active_txn_count(), 2);

        engine.commit_txn(t1).expect("should be valid");
        assert_eq!(engine.active_txn_count(), 1);

        engine.abort_txn(t2).expect("should be valid");
        assert_eq!(engine.active_txn_count(), 0);
    }

    #[test]
    fn test_index_persistence_across_restart() {
        let dir = tempdir().expect("should be valid");
        let data_dir = dir.path().to_path_buf();

        // Phase 1: Create index, insert data, flush to SSTable
        {
            let options = StorageOptions {
                data_dir: data_dir.clone(),
                memtable_size_limit: 1024 * 1024,
                ..Default::default()
            };
            let engine = LsmEngine::open(options).expect("should be valid");

            engine
                .create_index("Product", "price")
                .expect("should be valid");

            // Insert via transaction so indexes are maintained
            let txn = engine.begin_txn();
            let doc1 = serde_json::json!({"__class__": "Product", "name": "iPhone", "price": 999});
            let doc2 = serde_json::json!({"__class__": "Product", "name": "iPad", "price": 799});
            engine
                .txn_put(
                    txn,
                    b"Product::001".to_vec(),
                    serde_json::to_vec(&doc1).expect("should be valid"),
                )
                .expect("should be valid");
            engine
                .txn_put(
                    txn,
                    b"Product::002".to_vec(),
                    serde_json::to_vec(&doc2).expect("should be valid"),
                )
                .expect("should be valid");
            engine.commit_txn(txn).expect("should be valid");

            engine.flush().expect("should be valid");
        }

        // Phase 2: Reopen engine — index entries live in SSTs; rebuild the
        // in-memory IndexManager from them. `open()` defers this to the caller
        // (the server does it on a background thread) so startup stays fast.
        {
            let options = StorageOptions {
                data_dir,
                memtable_size_limit: 1024 * 1024,
                ..Default::default()
            };
            let engine = LsmEngine::open(options).expect("should be valid");
            engine.rebuild_indexes().expect("should be valid");

            // Index should exist after restart
            assert!(
                engine.has_index("Product", "price"),
                "index should persist across restart"
            );

            // Index should be functional: lookup by value
            let pkeys = engine
                .index_manager()
                .write()
                .expect("should be valid")
                .lookup_eq("Product", "price", &serde_json::json!(999));
            assert!(pkeys.is_some(), "index lookup should work after restart");
            assert_eq!(pkeys.expect("should be valid").len(), 1);

            // Range scan should also work
            let pkeys = engine
                .index_manager()
                .write()
                .expect("should be valid")
                .lookup_range(
                    "Product",
                    "price",
                    Some(&serde_json::json!(500)),
                    Some(&serde_json::json!(1000)),
                );
            assert!(pkeys.is_some());
            assert_eq!(pkeys.expect("should be valid").len(), 2); // both products
        }
    }

    #[test]
    fn test_backup_and_restore() {
        let dir = tempdir().expect("should be valid");
        let data_dir = dir.path().join("data");
        let backup_dir = dir.path().join("backup");

        // Phase 1: Create engine, insert data, flush, backup
        {
            let options = StorageOptions {
                data_dir: data_dir.clone(),
                memtable_size_limit: 1024 * 1024,
                ..Default::default()
            };
            let engine = LsmEngine::open(options).expect("should be valid");

            engine
                .put(b"key1".to_vec(), b"value1".to_vec())
                .expect("should be valid");
            engine
                .put(b"key2".to_vec(), b"value2".to_vec())
                .expect("should be valid");
            engine
                .put(b"key3".to_vec(), b"value3".to_vec())
                .expect("should be valid");
            engine.flush().expect("should be valid");

            // Create backup
            let manifest = engine.backup(&backup_dir).expect("should be valid");
            assert!(!manifest.files.is_empty(), "backup should have files");
            assert!(backup_dir.join("manifest.json").exists());
            assert!(backup_dir.join("wal.log").exists());

            // Verify at least one SSTable in backup
            let sst_files: Vec<_> = manifest
                .files
                .iter()
                .filter(|f| matches!(f.file_type, BackupFileType::SSTable))
                .collect();
            assert!(!sst_files.is_empty(), "backup should contain SSTable files");
        }

        // Phase 2: Restore to a new directory
        let restore_dir = dir.path().join("restored");
        {
            let manifest = LsmEngine::restore(&backup_dir, &restore_dir).expect("should be valid");
            assert!(!manifest.files.is_empty());

            // Verify files were copied
            assert!(restore_dir.join("wal.log").exists());
        }

        // Phase 3: Open restored engine and verify data
        {
            let options = StorageOptions {
                data_dir: restore_dir,
                memtable_size_limit: 1024 * 1024,
                ..Default::default()
            };
            let engine = LsmEngine::open(options).expect("should be valid");

            assert_eq!(
                engine.get(b"key1").expect("should be valid"),
                Some(b"value1".to_vec())
            );
            assert_eq!(
                engine.get(b"key2").expect("should be valid"),
                Some(b"value2".to_vec())
            );
            assert_eq!(
                engine.get(b"key3").expect("should be valid"),
                Some(b"value3".to_vec())
            );
            assert_eq!(engine.get(b"missing").expect("should be valid"), None);
        }
    }

    #[test]
    fn test_wal_archive_on_flush() {
        let dir = tempdir().expect("should be valid");
        let data_dir = dir.path().join("data");
        let archive_dir = dir.path().join("wal_archive");

        let options = StorageOptions {
            data_dir: data_dir.clone(),
            memtable_size_limit: 256, // Very small to trigger flush quickly
            wal_archive_dir: Some(archive_dir.clone()),
            wal_archive_max_files: 10,
            ..Default::default()
        };
        let engine = LsmEngine::open(options).expect("should be valid");

        // Write enough data to trigger multiple flushes
        for i in 0..20 {
            let key = format!("key_{:04}", i).into_bytes();
            let value = format!("value_{:04}", i).into_bytes();
            engine.put(key, value).expect("should be valid");
        }
        engine.flush().expect("should be valid");

        // Write more to trigger another flush
        for i in 20..40 {
            let key = format!("key_{:04}", i).into_bytes();
            let value = format!("value_{:04}", i).into_bytes();
            engine.put(key, value).expect("should be valid");
        }
        engine.flush().expect("should be valid");

        // Verify archive directory has WAL files
        assert!(archive_dir.exists(), "archive directory should exist");
        let archive_files: Vec<_> = std::fs::read_dir(&archive_dir)
            .expect("should be valid")
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path()
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with("wal_") && n.ends_with(".log"))
                    .unwrap_or(false)
            })
            .collect();

        assert!(
            !archive_files.is_empty(),
            "should have archived WAL files, found {}",
            archive_files.len()
        );

        // Verify data is still accessible
        for i in 0..40 {
            let key = format!("key_{:04}", i).into_bytes();
            let expected = format!("value_{:04}", i).into_bytes();
            assert_eq!(engine.get(&key).expect("should be valid"), Some(expected));
        }
    }

    #[test]
    fn test_wal_archive_cleanup() {
        let dir = tempdir().expect("should be valid");
        let data_dir = dir.path().join("data");
        let archive_dir = dir.path().join("wal_archive");

        let options = StorageOptions {
            data_dir: data_dir.clone(),
            memtable_size_limit: 128, // Very small
            wal_archive_dir: Some(archive_dir.clone()),
            wal_archive_max_files: 3, // Keep only 3 archives
            ..Default::default()
        };
        let engine = LsmEngine::open(options).expect("should be valid");

        // Trigger many flushes
        for batch in 0..10 {
            for i in 0..10 {
                let key = format!("key_{}_{}", batch, i).into_bytes();
                let value = format!("value_{}_{}", batch, i).into_bytes();
                engine.put(key, value).expect("should be valid");
            }
            engine.flush().expect("should be valid");
        }

        // Verify cleanup: should have at most 3 archive files
        let archive_files: Vec<_> = std::fs::read_dir(&archive_dir)
            .expect("should be valid")
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path()
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with("wal_") && n.ends_with(".log"))
                    .unwrap_or(false)
            })
            .collect();

        assert!(
            archive_files.len() <= 3,
            "should have at most 3 archive files, found {}",
            archive_files.len()
        );
    }

    // ── Value Metadata (Live Data) Tests ──

    #[test]
    fn test_value_meta_put_get() {
        let dir = tempdir().unwrap();
        let engine = LsmEngine::open(StorageOptions {
            data_dir: dir.path().to_path_buf(),
            ..Default::default()
        })
        .unwrap();

        let meta = crate::value_meta::ValueMetadata::new(0.8, crate::value_meta::LAMBDA_70D);
        engine.put_value_meta("BioTask", "001", &meta).unwrap();

        let loaded = engine.get_value_meta("BioTask", "001").unwrap().unwrap();
        assert!((loaded.base_score - 0.8).abs() < 0.001);
        assert!((loaded.value_score - 0.8).abs() < 0.001);
    }

    #[test]
    fn test_value_meta_not_found() {
        let dir = tempdir().unwrap();
        let engine = LsmEngine::open(StorageOptions {
            data_dir: dir.path().to_path_buf(),
            ..Default::default()
        })
        .unwrap();

        let result = engine.get_value_meta("Nonexistent", "999").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_value_score_defaults_to_1() {
        let dir = tempdir().unwrap();
        let engine = LsmEngine::open(StorageOptions {
            data_dir: dir.path().to_path_buf(),
            ..Default::default()
        })
        .unwrap();

        // Old data without meta should default to score 1.0
        let score = engine.get_value_score("OldClass", "001").unwrap();
        assert!((score - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_activate_creates_meta() {
        let dir = tempdir().unwrap();
        let engine = LsmEngine::open(StorageOptions {
            data_dir: dir.path().to_path_buf(),
            ..Default::default()
        })
        .unwrap();

        // Activate an entity without prior meta
        engine
            .activate("BioTask", "001", 0.5, "manual_heat")
            .unwrap();

        let meta = engine.get_value_meta("BioTask", "001").unwrap().unwrap();
        assert!(meta.activation_count == 1);
        assert!(meta.value_score > 0.0);
    }

    #[test]
    fn test_activate_existing_meta() {
        let dir = tempdir().unwrap();
        let engine = LsmEngine::open(StorageOptions {
            data_dir: dir.path().to_path_buf(),
            ..Default::default()
        })
        .unwrap();

        let mut meta = crate::value_meta::ValueMetadata::new(0.5, crate::value_meta::LAMBDA_7H);
        engine.put_value_meta("BioTask", "002", &meta).unwrap();

        // Activate
        engine.activate("BioTask", "002", 0.3, "citation").unwrap();

        let loaded = engine.get_value_meta("BioTask", "002").unwrap().unwrap();
        assert!(loaded.activation_count == 1);
        assert!((loaded.value_score - 0.8).abs() < 0.01); // 0.5 + 0.3
    }

    #[test]
    fn test_value_meta_decay_over_time() {
        let dir = tempdir().unwrap();
        let engine = LsmEngine::open(StorageOptions {
            data_dir: dir.path().to_path_buf(),
            ..Default::default()
        })
        .unwrap();

        let mut meta = crate::value_meta::ValueMetadata::new(1.0, crate::value_meta::LAMBDA_7H);
        // Simulate 7 hours ago
        meta.last_activated_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            - 25200;
        engine.put_value_meta("BioTask", "003", &meta).unwrap();

        let score = engine.get_value_score("BioTask", "003").unwrap();
        // After one half-life, score should be ~0.5
        assert!((score - 0.5).abs() < 0.05, "expected ~0.5, got {}", score);
    }

    #[test]
    fn test_storage_options_default_lambda() {
        let opts = StorageOptions::default();
        // Default should be LAMBDA_2Y (very slow decay)
        assert!((opts.default_lambda - crate::value_meta::LAMBDA_2Y).abs() < 1e-15);
        assert!(!opts.value_scorer_enabled);
    }

    /// Regression: deleted rows must NEVER resurrect through compaction.
    /// Mechanism under suspicion: level-order first-seen-wins lets a Put that
    /// survives at a SHALLOWER level beat a NEWER tombstone that compaction
    /// moved deeper (kept-tombstone placement inversion). Drive multi-level
    /// churn so tombstones end up below stale puts; reads must still see the
    /// deletion.
    #[test]
    fn test_tombstones_survive_compaction_no_resurrect() {
        let dir = tempdir().expect("should be valid");
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            memtable_size_limit: 64,
            size_ratio: 2,
            ..Default::default()
        };
        let engine = std::sync::Arc::new(LsmEngine::open(options).expect("should be valid"));
        for round in 0..8u32 {
            for i in 0..40u32 {
                let key = format!("vic{i:03}");
                let val = format!("r{round}_old");
                engine.put(key.into_bytes(), val.into_bytes()).expect("put");
            }
            for j in 0..60u32 {
                let key = format!("fill{round:02}_{j:04}");
                engine.put(key.into_bytes(), vec![b'x'; 40]).expect("put");
            }
            engine.flush().expect("flush");
            let _ = engine.flush_compaction();
        }

        // Phase 2: delete all victims.
        for i in 0..40u32 {
            let key = format!("vic{i:03}");
            engine.delete(key.into_bytes()).expect("delete");
        }
        engine.flush().expect("flush");

        // Phase 3: churn hard WITH a concurrent writer (production had a
        // multi-million-row rebuild flushing/compacting in parallel) —
        // concurrent flush/compaction windows are the suspected trigger.
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let eng2 = std::sync::Arc::clone(&engine);
        let stop2 = stop.clone();
        let bg = std::thread::spawn(move || {
            let mut j = 0u32;
            while !stop2.load(std::sync::atomic::Ordering::Relaxed) {
                let key = format!("bg_{j:06}");
                let _ = eng2.put(key.into_bytes(), vec![b'g'; 32]);
                if j % 20 == 0 {
                    let _ = eng2.flush();
                    let _ = eng2.flush_compaction();
                }
                j += 1;
            }
        });

        for round in 0..25u32 {
            for j in 0..80u32 {
                let key = format!("churn{round:02}_{j:04}");
                engine.put(key.into_bytes(), vec![b'c'; 40]).expect("put");
            }
            engine.flush().expect("flush");
            engine.flush_compaction().expect("compaction");
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        bg.join().expect("bg");
        engine.flush().expect("final flush");
        engine.flush_compaction().expect("final compaction");

        // Deleted keys must stay deleted — point reads, streaming scans AND
        // scan_prefix (the path SQL SELECT uses).
        for i in 0..40u32 {
            let key = format!("vic{i:03}");
            let got = engine.get(key.as_bytes()).expect("get");
            assert_eq!(got, None, "tombstone resurrected for {key}");
        }
        let mut seen = 0usize;
        engine
            .for_each_live_prefix(b"vic", None, None, |_k, _v| {
                seen += 1;
                Ok(())
            })
            .expect("scan");
        assert_eq!(seen, 0, "for_each resurrected deleted rows");
        let scanned = engine.scan_prefix(b"vic").expect("scan_prefix");
        assert!(
            scanned.is_empty(),
            "scan_prefix (SELECT path) resurrected {} deleted rows",
            scanned.len()
        );
    }
}
