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
use crate::vector::VectorIndexManager;
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
            // Skip index entries during WAL replay — indexes are rebuilt
            // separately by rebuild_indexes() after the engine is loaded.
            if entry.key.starts_with(b"__idx__") {
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

    fn load_sstables(&mut self) -> Result<()> {
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
        let mut max_seq = self.seq_counter.load(Ordering::Relaxed);
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
            let sst = SsTable::open(&path)?;
            let min_key = sst.first_key().unwrap_or_default();
            let max_key = sst.max_key().to_vec();
            let metadata = fs::metadata(&path)?;
            let mut iter = sst.iter()?;
            while iter.is_valid() {
                let seq = iter.seq_no();
                if seq >= max_seq {
                    max_seq = seq + 1;
                }
                iter.next();
            }
            drop(iter);
            if let Some(id_str) = fname.split('_').nth(1) {
                if let Ok(id) = id_str.parse::<u64>() {
                    let current = self.sst_counter.load(Ordering::Relaxed);
                    if id >= current {
                        self.sst_counter.store(id + 1, Ordering::Relaxed);
                    }
                }
            }
            self.sst_cache.insert(path.clone(), Arc::new(sst));
            self.levels[level].push(SsTableInfo {
                path,
                size: metadata.len(),
                min_key,
                max_key,
            });
        }
        self.seq_counter.store(max_seq, Ordering::Relaxed);
        Ok(())
    }
}

impl LsmEngine {
    /// Opens or creates an LSM engine at the given directory.
    pub fn open(options: StorageOptions) -> Result<Self> {
        fs::create_dir_all(&options.data_dir)?;

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
        pre_engine.load_sstables()?;

        // Spawn the background compaction worker with the loaded levels
        let (levels, compaction_sender, compaction_notif_receiver, _worker_handle) =
            CompactionWorker::spawn(options.clone(), pre_engine.levels, sst_counter.clone());

        // Create index manager with disk storage if configured
        let index_manager = match &options.index_storage_mode {
            None | Some(crate::index::IndexStorageMode::InMemory) => IndexManager::new(),
            Some(mode) => {
                let idx_dir = options.data_dir.join("indexes");
                fs::create_dir_all(&idx_dir)?;
                let mut mgr = IndexManager::with_disk_storage(&idx_dir, mode.clone());
                if let Err(e) = mgr.open_disk_indexes() {
                    tracing::warn!("Failed to open some disk indexes: {}", e);
                }
                mgr
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

        // Rebuild secondary indexes from persisted index entries
        engine.rebuild_indexes()?;

        // Validate rebuilt indexes — remove any corrupted ones
        engine.validate_indexes();

        // Rebuild vector indexes from persisted metadata
        engine.rebuild_vector_indexes()?;

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
                ws.memtable.size() >= self.options.memtable_size_limit,
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
        self.drain_compaction_notifications();

        // 1. Collect candidate SST paths from levels (brief lock).
        let candidates = {
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

        // 2. Check MemTables under read lock (concurrent with other readers).
        {
            let ws = self.write_state.read();
            if let Some((val, _)) = ws.memtable.get(key) {
                return Ok(Some(val.to_vec()));
            }
            if let Some(ref imm) = ws.immutable_memtable {
                if let Some((val, _)) = imm.get(key) {
                    return Ok(Some(val.to_vec()));
                }
            }
        }

        // 3. Snapshot SST handles from cache (may lazily open files).
        let sst_handles: Vec<Arc<SsTable>> = {
            let mut cache = self.sst_cache.lock().unwrap_or_else(|e| e.into_inner());
            candidates
                .iter()
                .filter_map(|path| {
                    if !cache.contains_key(path) {
                        let sst = SsTable::open(path).ok()?;
                        cache.insert(path.clone(), Arc::new(sst));
                    }
                    cache.get(path).map(Arc::clone)
                })
                .collect()
        };

        // 4. Iterate SST handles outside the lock (I/O-heavy).
        for sst in &sst_handles {
            match sst.get_full(key)? {
                Some((value, _, EntryKind::Put)) => return Ok(Some(value)),
                Some((_, _, EntryKind::Delete)) => return Ok(None),
                None => continue,
            }
        }

        Ok(None)
    }

    /// Scans all entries whose key starts with the given prefix.
    /// Returns a sorted Vec of (key, value) pairs.
    ///
    /// This leverages the LSM-Tree's sorted key structure:
    /// entries with `{class}::` prefix are contiguous in sorted order.
    pub fn scan_prefix(&self, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        eprintln!("[STORAGE scan_prefix] prefix='{}', len={}", String::from_utf8_lossy(prefix), prefix.len());
        let result = self.scan_prefix_internal(prefix, None, None)?;
        eprintln!("[STORAGE scan_prefix] returned {} entries", result.len());
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

    /// Scan with prefix, returning at most `limit` entries.
    /// Stops scanning SSTs once limit is reached — avoids full table scan for LIMIT queries.
    pub fn scan_prefix_limit(&self, prefix: &[u8], limit: usize) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        eprintln!("[STORAGE scan_prefix_limit] prefix='{}', limit={}", String::from_utf8_lossy(prefix), limit);
        let result = self.scan_prefix_internal(prefix, None, Some(limit))?;
        eprintln!("[STORAGE scan_prefix_limit] returned {} entries", result.len());
        Ok(result)
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

        // Collect SSTable paths under lock, then release
        let sst_paths: Vec<PathBuf> = {
            let levels = self.levels.lock().unwrap_or_else(|e| e.into_inner());
            let mut paths = Vec::new();
            let mut skipped = 0usize;
            for level in levels.iter().rev() {
                for sst_info in level.iter() {
                    if !prefix.is_empty() {
                        let sst_max = sst_info.max_key.as_slice();
                        if sst_max < prefix {
                            skipped += 1;
                            continue;
                        }
                        let sst_min = sst_info.min_key.as_slice();
                        if !Self::prefix_may_overlap(prefix, sst_min, sst_max) {
                            skipped += 1;
                            continue;
                        }
                    }
                    paths.push(sst_info.path.clone());
                }
            }
            eprintln!("[STORAGE internal] prefix='{}', limit={:?}, selected={} SSTs, skipped={} SSTs, total_levels={}",
                String::from_utf8_lossy(prefix), limit, paths.len(), skipped, levels.len());
            paths
        };

        // Step 1: Snapshot SST handles from cache (brief lock — open missing files outside)
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

        // Step 2: Scan SSTables OUTSIDE the lock (I/O-heavy)
        for (sst_idx, sst) in sst_handles.iter().enumerate() {
            let mut iter = sst.iter()?;

            // Use seek to jump directly to the prefix start instead of linear scan
            iter.seek(prefix);

            let mut sst_count = 0usize;
            let mut sst_put_count = 0usize;
            let mut sst_del_count = 0usize;
            let mut overridden_count = 0usize;
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

                if is_visible(seq) {
                    let should_update = match seen.get(&key) {
                        Some((_, existing_seq, existing_kind)) => {
                            if seq > *existing_seq {
                                if *existing_kind == EntryKind::Put && kind == EntryKind::Delete {
                                    overridden_count += 1;
                                }
                                true
                            } else {
                                false
                            }
                        }
                        None => true,
                    };
                    if should_update {
                        seen.insert(key, (value, seq, kind));
                    }
                }

                sst_count += 1;
                iter.next();
            }
            if sst_count > 0 || prefix.len() < 20 {
                eprintln!("[STORAGE sst_scan] sst_idx={}, prefix='{}', entries={}, puts={}, dels={}, overridden={}, seen_total={}",
                    sst_idx, String::from_utf8_lossy(prefix), sst_count, sst_put_count, sst_del_count, overridden_count, seen.len());
            }

            // NOTE: Do NOT early-exit based on put_count here.
            // Newer SSTs (higher sst_idx) may contain tombstones that override
            // Put entries from older SSTs. Exiting early would return "phantom
            // live" entries that are actually deleted. The SST list is already
            // pruned by prefix range, so scanning all of them is cheap.
        }

        // Step 3: Scan MemTables (read lock — concurrent with other readers)
        // Uses BTreeMap range query via scan_prefix() — O(log n + matches)
        // instead of O(total_entries) full iteration + starts_with filter.
        {
            let ws = self.write_state.read();

            // Scan immutable MemTable (overrides SSTables)
            if let Some(ref imm) = ws.immutable_memtable {
                for entry in imm.scan_prefix(prefix) {
                    if is_visible(entry.seq_no) {
                        let should_update = match seen.get(&entry.key) {
                            Some((_, existing_seq, _)) => entry.seq_no > *existing_seq,
                            None => true,
                        };
                        if should_update {
                            seen.insert(
                                entry.key.clone(),
                                (entry.value.clone(), entry.seq_no, entry.kind),
                            );
                        }
                    }
                }
            }

            // Scan active MemTable (overrides everything)
            for entry in ws.memtable.scan_prefix(prefix) {
                if is_visible(entry.seq_no) {
                    let should_update = match seen.get(&entry.key) {
                        Some((_, existing_seq, _)) => entry.seq_no > *existing_seq,
                        None => true,
                    };
                    if should_update {
                        seen.insert(
                            entry.key.clone(),
                            (entry.value.clone(), entry.seq_no, entry.kind),
                        );
                    }
                }
            }
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
            eprintln!("[STORAGE filter] ALL TOMBSTONES! prefix='{}', seen={}, tombstones={}, sample_keys={:?}",
                String::from_utf8_lossy(prefix), total_seen, tombstone_count, sample);
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

        eprintln!("[STORAGE filter] prefix='{}', limit={:?}, seen={}, puts={}, tombstones={}, after_filter={}",
            String::from_utf8_lossy(prefix), limit, total_seen, put_count, tombstone_count, result.len());

        Ok(result)
    }

    /// Count live entries matching a prefix. More efficient than scan_prefix().len()
    /// because it uses a lightweight HashSet for keys instead of storing values.
    /// Returns (live_count, tombstone_count).
    pub fn count_prefix(&self, prefix: &[u8]) -> Result<(usize, usize)> {
        let mut seen: HashMap<Vec<u8>, (SeqNo, EntryKind)> = HashMap::new();

        // Scan SSTables
        let sst_paths: Vec<PathBuf> = {
            let levels = self.levels.lock().unwrap_or_else(|e| e.into_inner());
            let mut paths = Vec::new();
            for level in levels.iter().rev() {
                for sst_info in level.iter() {
                    if !prefix.is_empty() {
                        if sst_info.max_key.as_slice() < prefix {
                            continue;
                        }
                        if !Self::prefix_may_overlap(prefix, sst_info.min_key.as_slice(), sst_info.max_key.as_slice()) {
                            continue;
                        }
                    }
                    paths.push(sst_info.path.clone());
                }
            }
            paths
        };

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

        for sst in &sst_handles {
            let mut iter = sst.iter()?;
            iter.seek(prefix);
            while iter.is_valid() {
                if !iter.key().starts_with(prefix) {
                    break;
                }
                let key = iter.key().to_vec();
                let seq = iter.seq_no();
                let kind = iter.kind();
                let should_update = match seen.get(&key) {
                    Some((existing_seq, _)) => seq > *existing_seq,
                    None => true,
                };
                if should_update {
                    seen.insert(key, (seq, kind));
                }
                iter.next();
            }
        }

        // Scan MemTables
        {
            let ws = self.write_state.read();
            if let Some(ref imm) = ws.immutable_memtable {
                for entry in imm.scan_prefix(prefix) {
                    let should_update = match seen.get(&entry.key) {
                        Some((existing_seq, _)) => entry.seq_no > *existing_seq,
                        None => true,
                    };
                    if should_update {
                        seen.insert(entry.key.clone(), (entry.seq_no, entry.kind));
                    }
                }
            }
            for entry in ws.memtable.scan_prefix(prefix) {
                let should_update = match seen.get(&entry.key) {
                    Some((existing_seq, _)) => entry.seq_no > *existing_seq,
                    None => true,
                };
                if should_update {
                    seen.insert(entry.key.clone(), (entry.seq_no, entry.kind));
                }
            }
        }

        let tombstones = seen.values().filter(|(_, kind)| *kind != EntryKind::Put).count();
        let live = seen.len() - tombstones;
        Ok((live, tombstones))
    }

    /// Checks if a prefix could match any key in the range [min_key, max_key].
    ///
    /// A key starting with `prefix` exists in [min_key, max_key] iff:
    /// 1. prefix <= max_key (some prefixed key is not past the end), AND
    /// 2. min_key starts with prefix (min_key itself has this prefix), OR
    ///    min_key < prefix (the smallest prefixed key is after min_key).
    fn prefix_may_overlap(prefix: &[u8], min_key: &[u8], max_key: &[u8]) -> bool {
        if prefix > max_key {
            return false;
        }
        min_key.starts_with(prefix) || min_key < prefix
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
        };

        if needs_flush {
            self.flush_memtable()?;
        }

        Ok(())
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

        // Step 2: Snapshot immutable MemTable entries (brief lock), then build SSTable outside lock
        let sst_id = self.sst_counter.fetch_add(1, Ordering::Relaxed);
        let sst_path = self.options.data_dir.join(format!("L0_{}.sst", sst_id));
        let (entries_snapshot, min_key, max_key) = {
            let ws = self.write_state.read();
            if let Some(ref imm) = ws.immutable_memtable {
                let entries: Vec<(Vec<u8>, Vec<u8>, SeqNo, EntryKind)> = imm
                    .entries()
                    .map(|e| (e.key.clone(), e.value.clone(), e.seq_no, e.kind))
                    .collect();
                let min_key = entries
                    .first()
                    .map(|(k, _, _, _)| k.clone())
                    .unwrap_or_default();
                let max_key = entries
                    .last()
                    .map(|(k, _, _, _)| k.clone())
                    .unwrap_or_default();
                (entries, min_key, max_key)
            } else {
                return Ok(());
            }
        };

        // Build SSTable from snapshot OUTSIDE the lock (disk I/O)
        // Use add_owned + into_iter to avoid cloning key/value a second time.
        let mut builder = SsTableBuilder::new();
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

        // Notify the background compaction worker
        let _ = self
            .compaction_sender
            .send(CompactionMsg::Flushed { level: 0 });

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

        // Create new WAL at temp path
        let new_wal = Wal::open(&tmp_path)?;

        // Atomically replace the old WAL
        fs::rename(&tmp_path, &wal_path)?;

        ws.wal = new_wal;
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

    /// Rebuilds secondary indexes by scanning persisted index entries from SSTables.
    fn rebuild_indexes(&self) -> Result<()> {
        let index_entries = self.scan_prefix(b"__idx__")?;
        if !index_entries.is_empty() {
            let mut mgr = self
                .index_manager
                .write()
                .unwrap_or_else(|e| e.into_inner());
            let active_keys: std::collections::HashSet<(String, String)> =
                mgr.all_index_keys().into_iter().collect();
            let mut orphaned = 0usize;

            for (key, _value) in &index_entries {
                if let Some((class, column, _, _)) =
                    crate::index::IndexManager::parse_index_key(key)
                {
                    if !active_keys.contains(&(class, column)) {
                        orphaned += 1;
                    }
                }
            }

            // Rebuild the actual in-memory index from entries
            mgr.rebuild_from_entries(&index_entries);

            if orphaned > 0 {
                tracing::warn!(
                    orphaned_index_entries = orphaned,
                    "Found orphaned index entries from interrupted CREATE INDEX. \
                     These will be cleaned up during compaction."
                );
                drop(mgr); // Release index_manager lock before acquiring write_state
                self.cleanup_orphaned_index_entries(&index_entries, &active_keys)?;
            }

            tracing::info!(
                "Rebuilt {} index entries across {} indexes ({} orphaned skipped)",
                index_entries.len() - orphaned,
                self.index_manager.read().unwrap_or_else(|e| e.into_inner()).index_count(),
                orphaned
            );
        }
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
    fn rebuild_vector_indexes(&self) -> Result<()> {
        let meta_entries = self.scan_prefix(b"__vec_meta__")?;
        if meta_entries.is_empty() {
            return Ok(());
        }

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
                    _ => crate::vector::DistanceMetric::Cosine,
                };
                let m = meta["m"].as_u64().unwrap_or(16) as usize;
                let ef_construction = meta["ef_construction"].as_u64().unwrap_or(200) as usize;
                let ef_search = meta["ef_search"].as_u64().unwrap_or(100) as usize;

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

        // Try to load persisted HNSW graph structures first (fast path)
        let graph_entries = self.scan_prefix(b"__vec_graph__")?;
        let mut loaded_graphs: std::collections::HashMap<String, &[u8]> =
            std::collections::HashMap::new();
        for (key, val_bytes) in &graph_entries {
            if let Ok(key_str) = std::str::from_utf8(key) {
                if let Some(graph_key) = key_str.strip_prefix("__vec_graph__") {
                    loaded_graphs.insert(graph_key.to_string(), val_bytes.as_slice());
                }
            }
        }

        // Create the indexes and try to load graph structures
        for (class, column, dimension, metric, m, ef_construction, ef_search) in &index_configs {
            let graph_key = format!("{}_{}", class, column);

            if let Some(graph_data) = loaded_graphs.get(&graph_key) {
                // Fast path: load HNSW graph structure directly
                match self
                    .vector_index_manager
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .load_graph(class, column, graph_data)
                {
                    Ok(()) => {
                        tracing::info!(
                            "Loaded vector index graph for {}.{} ({} vectors, fast restore)",
                            class,
                            column,
                            self.vector_index_manager
                                .read()
                                .unwrap_or_else(|e| e.into_inner())
                                .index_meta(class, column)
                                .map(|m| m.dimension)
                                .unwrap_or(0)
                        );
                        continue;
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Failed to load HNSW graph for {}.{}, falling back to rebuild: {}",
                            class,
                            column,
                            e
                        );
                    }
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
            let entries = self.scan_prefix(prefix.as_bytes())?;
            let mut vectors: Vec<(Vec<u8>, String, String, Vec<f32>)> = Vec::new();
            for (pk, val_bytes) in entries {
                if let Some(doc) = parse_doc_bytes(&val_bytes) {
                    if doc.get("__class__").and_then(|v| v.as_str()) == Some(class.as_str()) {
                        if let Some(serde_json::Value::Array(arr)) = doc.get(column.as_str()) {
                            let vec: Vec<f32> = arr
                                .iter()
                                .filter_map(|v| v.as_f64().map(|f| f as f32))
                                .collect();
                            if vec.len() == *dimension {
                                vectors.push((pk, class.clone(), column.clone(), vec));
                            }
                        }
                    }
                }
            }
            let count = vectors.len();
            if !vectors.is_empty() {
                let batch_refs: Vec<(Vec<u8>, &str, &str, Vec<f32>)> = vectors
                    .iter()
                    .map(|(pk, c, col, v)| (pk.clone(), c.as_str(), col.as_str(), v.clone()))
                    .collect();
                self.vector_index_manager
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .index_vector_batch(&batch_refs);
            }

            // Persist the rebuilt HNSW graph structure for next restart
            if let Some(graph_bytes) = self
                .vector_index_manager
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .save_graph(class, column)
            {
                let graph_key = format!("__vec_graph__{}_{}", class, column);
                let seq = self.next_seq();
                let entry = Entry::put(graph_key.clone().into_bytes(), graph_bytes, seq);
                let mut ws = self.write_state.write();
                let _ = ws.wal.append(&entry);
                let _ = ws.wal.flush_buf();
                ws.memtable
                    .put_with_seq(graph_key.into_bytes(), Vec::new(), seq);
            }

            tracing::info!(
                "Rebuilt vector index on {}.{} ({} vectors)",
                class,
                column,
                count
            );
        }

        Ok(())
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
        // Snapshot SST cache BEFORE acquiring write_state lock to avoid nested lock deadlock.
        let sst_cache_snapshot: HashMap<PathBuf, Arc<SsTable>> = {
            let cache = self.sst_cache.lock().unwrap_or_else(|e| e.into_inner());
            cache.clone()
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

                // Read old value for de-indexing (from locked memtable + SST cache snapshot)
                if has_indexes || has_vector_indexes {
                    if let Some(old_val) =
                        Self::get_from_locked(&ws, &sst_cache_snapshot, key.as_slice())?
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

    /// Gets a value by key from already-locked write state + SST cache.
    /// Used by `commit_txn` which holds the write_state write lock.
    fn get_from_locked(
        ws: &WriteState,
        sst_cache: &HashMap<PathBuf, Arc<SsTable>>,
        key: &[u8],
    ) -> Result<Option<Value>> {
        if let Some((val, _)) = ws.memtable.get(key) {
            return Ok(Some(val.to_vec()));
        }
        if let Some(ref imm) = ws.immutable_memtable {
            if let Some((val, _)) = imm.get(key) {
                return Ok(Some(val.to_vec()));
            }
        }
        for sst in sst_cache.values() {
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

        // Backfill: scan all existing entries for this class and index them
        let prefix = format!("{}::", class);
        let entries = self.scan_prefix(prefix.as_bytes())?;
        let total_estimate = entries.len();

        // Track progress
        {
            let label = format!("{}.{}", class, column);
            let mut prog = self.index_progress.lock().unwrap_or_else(|e| e.into_inner());
            *prog = Some((label, total_estimate, 0));
        }

        // Phase 1+2: Process in batches, releasing locks between batches
        // to avoid blocking the query engine for the entire duration.
        const BATCH_SIZE: usize = 5000;
        let mut processed = 0usize;
        let mut batch_start = 0usize;

        while batch_start < entries.len() {
            let batch_end = (batch_start + BATCH_SIZE).min(entries.len());

            // Index batch under index_manager write lock (short hold)
            let batch_entries: Vec<(Vec<u8>, Vec<u8>)> = {
                let mut mgr = self
                    .index_manager
                    .write()
                    .unwrap_or_else(|e| e.into_inner());
                let mut batch = Vec::new();
                for (pk, val_bytes) in &entries[batch_start..batch_end] {
                    if let Some(doc) = parse_doc_bytes(val_bytes) {
                        if doc.get("__class__").and_then(|v| v.as_str()) == Some(class) {
                            batch.extend(mgr.index_document(class, pk, &doc));
                        }
                    }
                }
                batch
            };

            // Persist batch to WAL + MemTable under write_state lock (short hold)
            if !batch_entries.is_empty() {
                let mut ws = self.write_state.write();
                for (key, value) in batch_entries {
                    let seq = self.next_seq();
                    ws.wal.append_raw_put(&key, &value, seq)?;
                    ws.memtable.put_with_seq(key, value, seq);
                }
                ws.wal.flush_buf()?;
            }

            processed += batch_end - batch_start;

            // Update progress tracker
            {
                let mut prog = self.index_progress.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(ref mut p) = *prog {
                    p.2 = processed;
                }
            }

            if processed % 10000 == 0 || batch_end >= entries.len() {
                tracing::info!(
                    rows_processed = processed,
                    total_estimate,
                    "Index creation progress"
                );
            }

            batch_start = batch_end;

            // Yield to let queries through between batches
            std::thread::yield_now();
        }

        // Clear progress
        {
            let mut prog = self.index_progress.lock().unwrap_or_else(|e| e.into_inner());
            *prog = None;
        }

        let elapsed_ms = start.elapsed().as_millis();
        let entries_count = total_estimate;
        tracing::info!(
            table = class,
            column = column,
            elapsed_ms,
            entries = entries_count,
            "Index creation completed"
        );

        Ok(())
    }

    /// Drops a secondary index.
    pub fn drop_index(&self, class: &str, column: &str) -> bool {
        self.index_manager
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .drop_index(class, column)
    }

    /// Returns true if an index exists on the given class.column.
    pub fn has_index(&self, class: &str, column: &str) -> bool {
        self.index_manager
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .has_index(class, column)
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
            let batch_refs: Vec<(Vec<u8>, &str, &str, Vec<f32>)> = vectors
                .iter()
                .map(|(pk, v)| (pk.clone(), class, column, v.clone()))
                .collect();
            self.vector_index_manager
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .index_vector_batch(&batch_refs);
        }

        // Persist the HNSW graph structure for fast restart
        if let Some(graph_bytes) = self
            .vector_index_manager
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .save_graph(class, column)
        {
            let graph_key = format!("__vec_graph__{}_{}", class, column);
            let seq = self.next_seq();
            let entry = Entry::put(graph_key.clone().into_bytes(), graph_bytes, seq);
            let mut ws = self.write_state.write();
            let _ = ws.wal.append(&entry);
            let _ = ws.wal.flush_buf();
            ws.memtable
                .put_with_seq(graph_key.into_bytes(), Vec::new(), seq);
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
        if removed {
            // Remove persisted metadata
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

            // Remove persisted graph structure
            let graph_key = format!("__vec_graph__{}_{}", class, column);
            let seq2 = self.next_seq();
            let del_entry2 = Entry::delete(graph_key.clone().into_bytes(), seq2);
            if let Err(e) = ws.wal.append(&del_entry2) {
                tracing::error!("Failed to write WAL entry for vector graph drop: {}", e);
            }
            ws.wal.flush_buf().ok();
            ws.memtable.delete_with_seq(graph_key.into_bytes(), seq2);
        }
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

        // Phase 2: Reopen engine �?indexes should be rebuilt automatically
        {
            let options = StorageOptions {
                data_dir,
                memtable_size_limit: 1024 * 1024,
                ..Default::default()
            };
            let engine = LsmEngine::open(options).expect("should be valid");

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
}
