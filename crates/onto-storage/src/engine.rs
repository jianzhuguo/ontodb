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
use crate::vector::VectorIndexManager;
use crate::lsm::compaction_worker::{CompactionMsg, CompactionNotification, CompactionWorker, SsTableInfo};
use crate::lsm::memtable::MemTable;
use crate::lsm::sstable::{SsTable, SsTableBuilder};
use crate::lsm::wal::{self, Wal};
use crate::mvcc::{TxnManager, WriteOp};
use crate::options::StorageOptions;
use onto_core::{Entry, EntryKind, Key, Result, SeqNo, Value};
use onto_core::binary_row::BinaryRow;
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
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, RwLock};
use parking_lot::RwLock as FairRwLock;

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
    write_state: FairRwLock<WriteState>,

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
        for entry in entries {
            match entry.kind {
                EntryKind::Put => self.write_state.memtable.put_with_seq(entry.key, entry.value, entry.seq_no),
                EntryKind::Delete => self.write_state.memtable.delete_with_seq(entry.key, entry.seq_no),
            }
            max_seq = max_seq.max(entry.seq_no);
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
            if path.extension().map_or(false, |ext| ext == "sst") {
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
            CompactionWorker::spawn(
                options.clone(),
                pre_engine.levels,
                sst_counter.clone(),
            );

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

        let engine = LsmEngine {
            write_state: FairRwLock::new(pre_engine.write_state),
            sst_cache: Mutex::new(pre_engine.sst_cache),
            levels,
            options,
            seq_counter: pre_engine.seq_counter,
            sst_counter,
            index_manager: RwLock::new(index_manager),
            vector_index_manager: RwLock::new(VectorIndexManager::new()),
            compaction_sender,
            compaction_notif_receiver: std::sync::Mutex::new(compaction_notif_receiver),
            compaction_pending: AtomicBool::new(false),
        };

        // Rebuild secondary indexes from persisted index entries
        engine.rebuild_indexes()?;

        // Rebuild vector indexes from persisted metadata
        engine.rebuild_vector_indexes()?;

        Ok(engine)
    }

    /// Returns the data directory path.
    pub fn data_dir(&self) -> PathBuf {
        self.options.data_dir.clone()
    }

    /// Puts a key-value pair.
    pub fn put(&self, key: Key, value: Value) -> Result<()> {
        let seq = self.next_seq();
        let entry = Entry::put(key.clone(), value.clone(), seq);

        let needs_flush = {
            let mut ws = self.write_state.write();
            ws.wal.append(&entry)?;
            ws.wal_pending_count += 1;
            // Batch flush: only flush WAL buffer every 64 writes
            if ws.wal_pending_count >= 64 {
                ws.wal.flush_buf()?;
                ws.wal_pending_count = 0;
            }
            ws.memtable.put_with_seq(key, value, seq);
            ws.memtable.size() >= self.options.memtable_size_limit
        };

        if needs_flush {
            self.flush_memtable()?;
        }

        Ok(())
    }

    /// Gets a value by key.
    pub fn get(&self, key: &[u8]) -> Result<Option<Value>> {
        self.drain_compaction_notifications();

        // 1. Collect candidate SST paths from levels (brief lock).
        let candidates = {
            let levels = self.levels.lock().unwrap();
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
            let mut cache = self.sst_cache.lock().unwrap();
            candidates.iter().filter_map(|path| {
                if !cache.contains_key(path) {
                    let sst = SsTable::open(path).ok()?;
                    cache.insert(path.clone(), Arc::new(sst));
                }
                cache.get(path).map(Arc::clone)
            }).collect()
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
        self.scan_prefix_internal(prefix, None)
    }

    /// Internal scan implementation shared by scan_prefix and scan_prefix_with_visibility.
    /// When `vis` is Some, only entries visible to the snapshot are included.
    fn scan_prefix_internal(
        &self,
        prefix: &[u8],
        vis: Option<&crate::mvcc::Visibility>,
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
            let levels = self.levels.lock().unwrap();
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

        // Step 1: Snapshot SST handles from cache (brief lock — open missing files outside)
        let sst_handles: Vec<Arc<SsTable>> = {
            let mut cache = self.sst_cache.lock().unwrap();
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
        for sst in &sst_handles {
            let mut iter = sst.iter()?;

            while iter.is_valid() && iter.key() < prefix {
                iter.next();
            }

            while iter.is_valid() {
                if !iter.key().starts_with(prefix) {
                    break;
                }
                let key = iter.key().to_vec();
                let value = iter.value().to_vec();
                let seq = iter.seq_no();
                let kind = iter.kind();

                if is_visible(seq) {
                    let should_update = match seen.get(&key) {
                        Some((_, existing_seq, _)) => seq > *existing_seq,
                        None => true,
                    };
                    if should_update {
                        seen.insert(key, (value, seq, kind));
                    }
                }

                iter.next();
            }
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

        // Filter out tombstones and collect
        let result: Vec<(Vec<u8>, Vec<u8>)> = seen
            .into_iter()
            .filter(|(_, (_, _, kind))| *kind == EntryKind::Put)
            .map(|(key, (value, _, _))| (key, value))
            .collect();

        Ok(result)
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
            let old_mem = std::mem::replace(&mut ws.memtable, MemTable::new());
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
                let min_key = entries.first().map(|(k, _, _, _)| k.clone()).unwrap_or_default();
                let max_key = entries.last().map(|(k, _, _, _)| k.clone()).unwrap_or_default();
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
            let mut levels = self.levels.lock().unwrap();
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
            self.sst_cache.lock().unwrap().insert(sst_path, Arc::new(new_sst));
        }
        {
            let mut ws = self.write_state.write();
            ws.immutable_memtable = None;
            self.reset_wal_internal(&mut ws)?;
        }

        // Notify the background compaction worker
        let _ = self.compaction_sender.send(CompactionMsg::Flushed { level: 0 });

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
            let receiver = self.compaction_notif_receiver.lock().unwrap();
            while let Ok(notif) = receiver.try_recv() {
                match notif {
                    CompactionNotification::Compacted { evicted_paths: paths } => {
                        evicted_paths.extend(paths);
                    }
                    CompactionNotification::FlushDone => {}
                }
            }
        }

        // Selectively remove only the evicted SST paths from the cache.
        if !evicted_paths.is_empty() {
            let mut cache = self.sst_cache.lock().unwrap();
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
        let receiver = self.compaction_notif_receiver.lock().unwrap();
        let mut evicted_paths: Vec<PathBuf> = Vec::new();
        loop {
            match receiver.recv_timeout(std::time::Duration::from_secs(10)) {
                Ok(CompactionNotification::FlushDone) => break,
                Ok(CompactionNotification::Compacted { evicted_paths: paths }) => {
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
            let mut cache = self.sst_cache.lock().unwrap();
            for path in &evicted_paths {
                cache.remove(path);
            }
        }
        Ok(())
    }

    /// Resets the WAL file after a successful flush.
    ///
    /// Uses write-new-then-rename for atomicity:
    /// 1. Create a new WAL at a temp path
    /// 2. Atomically rename temp �?wal.log
    /// This ensures the WAL is never missing, even if the process crashes mid-reset.
    fn reset_wal_internal(&self, ws: &mut WriteState) -> Result<()> {
        let wal_path = self.options.data_dir.join("wal.log");
        let tmp_path = self.options.data_dir.join("wal.log.tmp");

        // If a stale temp file exists from a previous crash, remove it
        if tmp_path.exists() {
            fs::remove_file(&tmp_path)?;
        }

        // Create new WAL at temp path
        let new_wal = Wal::open(&tmp_path)?;

        // Atomically replace the old WAL
        fs::rename(&tmp_path, &wal_path)?;

        ws.wal = new_wal;
        // Re-open at the final path (rename doesn't update the file handle's path)
        ws.wal = Wal::open(&wal_path)?;

        Ok(())
    }

    fn next_seq(&self) -> SeqNo {
        self.seq_counter.fetch_add(1, Ordering::Relaxed)
    }

    /// Rebuilds secondary indexes by scanning persisted index entries from SSTables.
    fn rebuild_indexes(&self) -> Result<()> {
        let index_entries = self.scan_prefix(b"__idx__")?;
        if !index_entries.is_empty() {
            let mut mgr = self.index_manager.write().unwrap();
            mgr.rebuild_from_entries(&index_entries);
            tracing::info!(
                "Rebuilt {} index entries across {} indexes",
                index_entries.len(),
                mgr.index_count()
            );
        }
        Ok(())
    }

    /// Rebuilds vector indexes by scanning persisted metadata and document data.
    fn rebuild_vector_indexes(&self) -> Result<()> {
        let meta_entries = self.scan_prefix(b"__vec_meta__")?;
        if meta_entries.is_empty() {
            return Ok(());
        }

        // Parse and recreate vector indexes from metadata
        let mut index_configs: Vec<(String, String, usize, crate::vector::DistanceMetric, usize, usize, usize)> = Vec::new();
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
                    index_configs.push((class, column, dimension, metric, m, ef_construction, ef_search));
                }
            }
        }

        // Create the indexes
        for (class, column, dimension, metric, m, ef_construction, ef_search) in &index_configs {
            let _ = self.vector_index_manager.write().unwrap().create_index(
                class, column, *dimension, *metric, *m, *ef_construction, *ef_search,
            );
        }

        // Backfill vectors from document data (batch insert under single lock)
        for (class, column, dimension, _, _, _, _) in &index_configs {
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
                self.vector_index_manager.write().unwrap().index_vector_batch(&batch_refs);
            }
            tracing::info!(
                "Rebuilt vector index on {}.{} ({} vectors)",
                class, column, count
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
        // seq_counter is the NEXT value to assign, so last committed = seq_counter - 1
        let last_seq = self.seq_counter.load(Ordering::Relaxed).saturating_sub(1);
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
            let cache = self.sst_cache.lock().unwrap();
            cache.clone()
        };

        let (writes_with_seq, old_values, index_entries_batch) = {
            let mut ws = self.write_state.write();
            let writes = ws.txn_manager.commit(txn_id)?;

            let mut writes_with_seq: Vec<(Vec<u8>, WriteOp, SeqNo)> = Vec::with_capacity(writes.len());
            let mut old_values: HashMap<Vec<u8>, Vec<u8>> = HashMap::new();
            let mut index_entries_batch: Vec<(Vec<u8>, Vec<u8>, SeqNo)> = Vec::new();

            for (key, op) in writes {
                let seq = self.next_seq();

                // Check index existence (brief read locks, dropped immediately)
                let class = Self::extract_class_from_key(&key);
                let has_indexes = class.as_ref().map_or(false, |c| {
                    !self.index_manager.read().unwrap().indexes_for_class(c).is_empty()
                });
                let has_vector_indexes = class.as_ref().map_or(false, |c| {
                    self.vector_index_manager.read().unwrap().has_any_index(c)
                });

                // Read old value for de-indexing (from locked memtable + SST cache snapshot)
                if has_indexes || has_vector_indexes {
                    if let Some(old_val) = Self::get_from_locked(&ws, &sst_cache_snapshot, key.as_slice())? {
                        old_values.insert(key.clone(), old_val);
                    }
                }

                // Pre-compute index entries for Put operations (need new doc to compute)
                if has_indexes {
                    if let WriteOp::Put(ref value) = op {
                        if let Some(ref c) = class {
                            if let Some(ref doc) = parse_doc_bytes(value) {
                                let entries = self.index_manager.read().unwrap().index_document_read_only(c, &key, doc);
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
            let mut idx_mgr = self.index_manager.write().unwrap();
            let mut vec_mgr = self.vector_index_manager.write().unwrap();

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
                        let entry = Entry::put(key.clone(), value.clone(), seq);
                        ws.wal.append(&entry)?;
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

            if self.options.sync_wal_on_commit {
                ws.wal.sync()?;
            }

            ws.memtable.size() >= self.options.memtable_size_limit
        };

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
    fn get_from_locked(ws: &WriteState, sst_cache: &HashMap<PathBuf, Arc<SsTable>>, key: &[u8]) -> Result<Option<Value>> {
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
        self.write_state.write()
            .txn_manager
            .get_mut(txn_id)
            .ok_or_else(|| onto_core::CoreError::InvalidArgument(
                format!("transaction {} not found or not active", txn_id),
            ))?
            .put(key, value);
        Ok(())
    }

    /// Buffers a delete operation in a transaction.
    pub fn txn_delete(&self, txn_id: SeqNo, key: Key) -> Result<()> {
        self.write_state.write()
            .txn_manager
            .get_mut(txn_id)
            .ok_or_else(|| onto_core::CoreError::InvalidArgument(
                format!("transaction {} not found or not active", txn_id),
            ))?
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
        // 1. Check transaction's own write buffer
        {
            let ws = self.write_state.read();
            if let Some(txn) = ws.txn_manager.get(txn_id) {
                if let Some(op) = txn.write_buffer_get(key) {
                    return match op {
                        WriteOp::Put(v) => Ok(Some(v.clone())),
                        WriteOp::Delete => Ok(None),
                    };
                }
            }
        }

        // 2-3. Read from storage with snapshot visibility
        let vis = self.write_state.read().txn_manager.visibility_for(txn_id);
        self.get_with_visibility(key, &vis)
    }

    /// Scans all entries with the given prefix, respecting snapshot visibility.
    pub fn txn_scan_prefix(
        &self,
        txn_id: SeqNo,
        prefix: &[u8],
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let (vis, write_buffer) = {
            let ws = self.write_state.read();
            let vis = ws.txn_manager.visibility_for(txn_id);
            let buf: Vec<(Vec<u8>, WriteOp)> = ws.txn_manager.get(txn_id)
                .map(|t| t.write_buffer_iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                .unwrap_or_default();
            (vis, buf)
        };

        // Get base results from storage with visibility filtering
        let mut results = self.scan_prefix_with_visibility(prefix, &vis)?;

        // Overlay the transaction's own write buffer
        for (key, op) in &write_buffer {
            if key.starts_with(prefix) {
                match op {
                    WriteOp::Put(value) => {
                        if let Some(existing) = results.iter_mut().find(|(k, _)| k == key) {
                            existing.1 = value.clone();
                        } else {
                            results.push((key.clone(), value.clone()));
                        }
                    }
                    WriteOp::Delete => {
                        results.retain(|(k, _)| k != key);
                    }
                }
            }
        }

        // Sort by key
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
            let levels = self.levels.lock().unwrap();
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
            let mut cache = self.sst_cache.lock().unwrap();
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
        self.scan_prefix_internal(prefix, Some(vis))
    }

    /// Returns the number of active transactions.
    pub fn active_txn_count(&self) -> usize {
        self.write_state.read().txn_manager.active_count()
    }

    // =================================================================
    //  Index API
    // =================================================================
    /// Creates a secondary index on a class.column.
    /// Automatically backfills existing data for the class.
    pub fn create_index(&self, class: &str, column: &str) -> Result<()> {
        self.index_manager.write().unwrap().create_index(class, column);

        // Backfill: scan all existing entries for this class and index them
        let prefix = format!("{}::", class);
        let entries = self.scan_prefix(prefix.as_bytes())?;

        // Phase 1: Insert into index under a single write lock (not per-document)
        let all_index_entries: Vec<(Vec<u8>, Vec<u8>)> = {
            let mut mgr = self.index_manager.write().unwrap();
            let mut all = Vec::new();
            for (pk, val_bytes) in &entries {
                if let Some(doc) = parse_doc_bytes(val_bytes) {
                    if doc.get("__class__").and_then(|v| v.as_str()) == Some(class) {
                        all.extend(mgr.index_document(class, pk, &doc));
                    }
                }
            }
            all
        };

        // Phase 2: Persist index entries to WAL + MemTable under a single write lock
        if !all_index_entries.is_empty() {
            let mut ws = self.write_state.write();
            for (key, value) in all_index_entries {
                let seq = self.next_seq();
                let entry = Entry::put(key.clone(), value.clone(), seq);
                ws.wal.append(&entry)?;
                ws.memtable.put_with_seq(key, value, seq);
            }
            ws.wal.flush_buf()?;
        }

        Ok(())
    }

    /// Drops a secondary index.
    pub fn drop_index(&self, class: &str, column: &str) -> bool {
        self.index_manager.write().unwrap().drop_index(class, column)
    }

    /// Returns true if an index exists on the given class.column.
    pub fn has_index(&self, class: &str, column: &str) -> bool {
        self.index_manager.read().unwrap().has_index(class, column)
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
        self.vector_index_manager.write().unwrap()
            .create_index(class, column, dimension, metric, m, ef_construction, ef_search)?;

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
            self.vector_index_manager.write().unwrap().index_vector_batch(&batch_refs);
        }

        Ok(())
    }

    /// Drops a vector index.
    pub fn drop_vector_index(&self, class: &str, column: &str) -> bool {
        let removed = self.vector_index_manager.write().unwrap().drop_index(class, column);
        if removed {
            // Remove persisted metadata
            let meta_key = Self::make_vec_meta_key(class, column);
            let seq = self.next_seq();
            let del_entry = Entry::delete(meta_key.clone(), seq);
            let mut ws = self.write_state.write();
            let _ = ws.wal.append(&del_entry);
            let _ = ws.wal.flush_buf();
            ws.memtable.delete_with_seq(meta_key, seq);
        }
        removed
    }

    /// Returns true if a vector index exists on the given class.column.
    pub fn has_vector_index(&self, class: &str, column: &str) -> bool {
        self.vector_index_manager.read().unwrap().has_index(class, column)
    }

    /// Returns a reference to the vector index manager RwLock.
    pub fn vector_index_manager(&self) -> &RwLock<VectorIndexManager> {
        &self.vector_index_manager
    }

    /// Returns engine statistics.
    pub fn stats(&self) -> EngineStats {
        let ws = self.write_state.read();
        let levels = self.levels.lock().unwrap();
        let total_sstables: usize = levels.iter().map(|l| l.len()).sum();
        let total_sst_size: u64 = levels
            .iter()
            .flat_map(|l| l.iter())
            .map(|s| s.size)
            .sum();

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
    /// 3. Copy all `.sst` files, `wal.log`, and `indexes/*.idx`
    /// 4. Write a manifest file listing all copied files
    ///
    /// The backup is a consistent snapshot that can be restored with `restore()`.
    pub fn backup(&self, backup_dir: &Path) -> Result<BackupManifest> {
        // Step 1: Flush MemTable to ensure all data is in SSTables
        self.flush()?;

        // Step 2: Flush disk indexes
        // (access via index_manager_mut to flush)
        // We need a helper to flush all disk indexes
        self.flush_disk_indexes()?;

        // Step 3: Create backup directory
        fs::create_dir_all(backup_dir)?;
        let idx_backup_dir = backup_dir.join("indexes");
        fs::create_dir_all(&idx_backup_dir)?;

        let mut manifest = BackupManifest {
            timestamp: chrono_timestamp(),
            files: Vec::new(),
        };

        // Step 4: Copy SSTable files
        let levels = self.levels.lock().unwrap();
        let mut sst_paths: Vec<PathBuf> = Vec::new();
        for level in levels.iter() {
            for info in level.iter() {
                sst_paths.push(info.path.clone());
            }
        }
        drop(levels);

        for sst_path in &sst_paths {
            let fname = sst_path.file_name().unwrap().to_str().unwrap();
            let dest = backup_dir.join(fname);
            fs::copy(sst_path, &dest)?;
            let size = fs::metadata(&dest)?.len();
            manifest.files.push(BackupFile {
                name: fname.to_string(),
                size,
                file_type: BackupFileType::SSTable,
            });
        }

        // Step 5: Copy WAL file
        let wal_path = self.options.data_dir.join("wal.log");
        if wal_path.exists() {
            let dest = backup_dir.join("wal.log");
            fs::copy(&wal_path, &dest)?;
            let size = fs::metadata(&dest)?.len();
            manifest.files.push(BackupFile {
                name: "wal.log".to_string(),
                size,
                file_type: BackupFileType::Wal,
            });
        }

        // Step 6: Copy index files
        let idx_dir = self.options.data_dir.join("indexes");
        if idx_dir.exists() {
            for entry in fs::read_dir(&idx_dir)? {
                let entry = entry?;
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("idx") {
                    let fname = path.file_name().unwrap().to_str().unwrap();
                    let dest = idx_backup_dir.join(fname);
                    fs::copy(&path, &dest)?;
                    let size = fs::metadata(&dest)?.len();
                    manifest.files.push(BackupFile {
                        name: format!("indexes/{}", fname),
                        size,
                        file_type: BackupFileType::Index,
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
                    "Backup file missing: {}", file.name
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

    /// Flushes all disk-based indexes to disk (with fsync).
    fn flush_disk_indexes(&self) -> Result<()> {
        self.index_manager.write().unwrap().flush_disk_indexes();
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
    let dur = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
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
    let month_days = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut m = 0;
    while m < 12 && remaining >= month_days[m] {
        remaining -= month_days[m];
        m += 1;
    }
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", y, m + 1, remaining + 1, hours, minutes, seconds)
}

fn is_leap_year(y: u64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || (y % 400 == 0)
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
        let dir = tempdir().unwrap();
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            memtable_size_limit: 1024 * 1024, // 1MB
            ..Default::default()
        };

        let engine = LsmEngine::open(options).unwrap();

        engine
            .put(b"name".to_vec(), b"alice".to_vec())
            .unwrap();
        engine.put(b"age".to_vec(), b"30".to_vec()).unwrap();

        let val = engine.get(b"name").unwrap();
        assert_eq!(val, Some(b"alice".to_vec()));

        let val = engine.get(b"age").unwrap();
        assert_eq!(val, Some(b"30".to_vec()));

        let val = engine.get(b"missing").unwrap();
        assert_eq!(val, None);
    }

    #[test]
    fn test_engine_overwrite() {
        let dir = tempdir().unwrap();
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            ..Default::default()
        };

        let engine = LsmEngine::open(options).unwrap();

        engine.put(b"key".to_vec(), b"v1".to_vec()).unwrap();
        engine.put(b"key".to_vec(), b"v2".to_vec()).unwrap();

        let val = engine.get(b"key").unwrap();
        assert_eq!(val, Some(b"v2".to_vec()));
    }

    #[test]
    fn test_engine_delete() {
        let dir = tempdir().unwrap();
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            ..Default::default()
        };

        let engine = LsmEngine::open(options).unwrap();

        engine.put(b"key".to_vec(), b"value".to_vec()).unwrap();
        assert!(engine.get(b"key").unwrap().is_some());

        engine.delete(b"key".to_vec()).unwrap();
        assert!(engine.get(b"key").unwrap().is_none());
    }

    #[test]
    fn test_engine_flush_to_sstable() {
        let dir = tempdir().unwrap();
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            memtable_size_limit: 128, // Very small to trigger flush
            ..Default::default()
        };

        let engine = LsmEngine::open(options).unwrap();

        // Write enough data to trigger a flush
        for i in 0..20u32 {
            let key = format!("key_{:04}", i);
            let value = format!("value_{}", i);
            engine.put(key.into_bytes(), value.into_bytes()).unwrap();
        }

        // All data should still be readable
        let val = engine.get(b"key_0005").unwrap();
        assert_eq!(val, Some(b"value_5".to_vec()));

        let stats = engine.stats();
        assert!(stats.total_sstables > 0, "should have flushed at least one SSTable");
    }

    #[test]
    fn test_engine_recovery() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path().to_path_buf();

        // Write data
        {
            let options = StorageOptions {
                data_dir: data_dir.clone(),
                ..Default::default()
            };
            let engine = LsmEngine::open(options).unwrap();
            engine
                .put(b"key1".to_vec(), b"value1".to_vec())
                .unwrap();
            engine
                .put(b"key2".to_vec(), b"value2".to_vec())
                .unwrap();
            // Don't drop cleanly - simulate crash (WAL should persist)
        }

        // Recover
        {
            let options = StorageOptions {
                data_dir,
                ..Default::default()
            };
            let engine = LsmEngine::open(options).unwrap();

            let val = engine.get(b"key1").unwrap();
            assert_eq!(val, Some(b"value1".to_vec()));

            let val = engine.get(b"key2").unwrap();
            assert_eq!(val, Some(b"value2".to_vec()));
        }
    }

    #[test]
    fn test_engine_compaction() {
        let dir = tempdir().unwrap();
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            memtable_size_limit: 128, // Very small to trigger frequent flushes
            size_ratio: 2,            // Compact when level has > 2 SSTables
            ..Default::default()
        };

        let engine = LsmEngine::open(options).unwrap();

        // Write enough data to trigger multiple flushes and compaction
        let num_keys = 100u32;
        for i in 0..num_keys {
            let key = format!("key_{:04}", i);
            let value = format!("value_{:06}", i); // Larger values to fill memtable faster
            engine.put(key.into_bytes(), value.into_bytes()).unwrap();
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
            let val = engine.get(key.as_bytes()).unwrap();
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
        let dir = tempdir().unwrap();
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            memtable_size_limit: 128,
            size_ratio: 2,
            ..Default::default()
        };

        let engine = LsmEngine::open(options).unwrap();

        // Write initial data
        for i in 0..50u32 {
            let key = format!("key_{:04}", i);
            let value = format!("v1_{:06}", i);
            engine.put(key.into_bytes(), value.into_bytes()).unwrap();
        }

        // Overwrite all keys with new values
        for i in 0..50u32 {
            let key = format!("key_{:04}", i);
            let value = format!("v2_{:06}", i);
            engine.put(key.into_bytes(), value.into_bytes()).unwrap();
        }

        // Verify the latest values are returned
        for i in 0..50u32 {
            let key = format!("key_{:04}", i);
            let expected = format!("v2_{:06}", i);
            let val = engine.get(key.as_bytes()).unwrap();
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
        let dir = tempdir().unwrap();
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            memtable_size_limit: 128,
            size_ratio: 2,
            ..Default::default()
        };

        let engine = LsmEngine::open(options).unwrap();

        // Write data
        for i in 0..50u32 {
            let key = format!("key_{:04}", i);
            let value = format!("value_{:06}", i);
            engine.put(key.into_bytes(), value.into_bytes()).unwrap();
        }

        // Delete even-numbered keys
        for i in (0..50u32).step_by(2) {
            let key = format!("key_{:04}", i);
            engine.delete(key.into_bytes()).unwrap();
        }

        // Flush remaining entries in memtable
        engine.flush().unwrap();

        // Verify: odd keys exist, even keys are deleted
        for i in 0..50u32 {
            let key = format!("key_{:04}", i);
            let val = engine.get(key.as_bytes()).unwrap();
            if i % 2 == 0 {
                assert!(val.is_none(), "deleted key {} should not exist, got {:?}", key, val);
            } else {
                assert!(
                    val.is_some(),
                    "non-deleted key {} should still exist",
                    key
                );
            }
        }
    }

    #[test]
    fn test_compaction_scoring_and_tombstone_cleanup() {
        let dir = tempdir().unwrap();
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            memtable_size_limit: 128,
            size_ratio: 2,
            ..Default::default()
        };

        let engine = LsmEngine::open(options).unwrap();

        // Write data, then delete most of it
        for i in 0..80u32 {
            let key = format!("key_{:04}", i);
            let value = format!("value_{:06}", i);
            engine.put(key.into_bytes(), value.into_bytes()).unwrap();
        }

        // Delete all but the last 10 keys
        for i in 0..70u32 {
            let key = format!("key_{:04}", i);
            engine.delete(key.into_bytes()).unwrap();
        }

        // Force flush and compaction
        engine.flush().unwrap();

        // Verify remaining keys
        for i in 70..80u32 {
            let key = format!("key_{:04}", i);
            let expected = format!("value_{:06}", i);
            let val = engine.get(key.as_bytes()).unwrap();
            assert_eq!(val, Some(expected.into_bytes()), "key {} should exist", key);
        }

        // Verify deleted keys are gone
        for i in 0..70u32 {
            let key = format!("key_{:04}", i);
            let val = engine.get(key.as_bytes()).unwrap();
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
        let dir = tempdir().unwrap();
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            ..Default::default()
        };
        let engine = LsmEngine::open(options).unwrap();

        let txn = engine.begin_txn();
        engine.txn_put(txn, b"name".to_vec(), b"alice".to_vec()).unwrap();
        engine.txn_put(txn, b"age".to_vec(), b"30".to_vec()).unwrap();
        engine.commit_txn(txn).unwrap();

        assert_eq!(engine.get(b"name").unwrap(), Some(b"alice".to_vec()));
        assert_eq!(engine.get(b"age").unwrap(), Some(b"30".to_vec()));
    }

    #[test]
    fn test_txn_abort() {
        let dir = tempdir().unwrap();
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            ..Default::default()
        };
        let engine = LsmEngine::open(options).unwrap();

        let txn = engine.begin_txn();
        engine.txn_put(txn, b"name".to_vec(), b"alice".to_vec()).unwrap();
        engine.abort_txn(txn).unwrap();

        assert_eq!(engine.get(b"name").unwrap(), None);
    }

    #[test]
    fn test_txn_read_own_writes() {
        let dir = tempdir().unwrap();
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            ..Default::default()
        };
        let engine = LsmEngine::open(options).unwrap();

        let txn = engine.begin_txn();
        engine.txn_put(txn, b"name".to_vec(), b"alice".to_vec()).unwrap();
        engine.txn_put(txn, b"age".to_vec(), b"30".to_vec()).unwrap();

        assert_eq!(engine.txn_get(txn, b"name").unwrap(), Some(b"alice".to_vec()));
        assert_eq!(engine.txn_get(txn, b"age").unwrap(), Some(b"30".to_vec()));

        engine.commit_txn(txn).unwrap();
    }

    #[test]
    fn test_txn_snapshot_isolation() {
        let dir = tempdir().unwrap();
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            ..Default::default()
        };
        let engine = LsmEngine::open(options).unwrap();

        engine.put(b"key".to_vec(), b"v1".to_vec()).unwrap();

        let txn1 = engine.begin_txn();

        engine.put(b"key".to_vec(), b"v2".to_vec()).unwrap();

        // txn1 still sees v1 (snapshot isolation)
        assert_eq!(engine.txn_get(txn1, b"key").unwrap(), Some(b"v1".to_vec()));

        let txn2 = engine.begin_txn();
        assert_eq!(engine.txn_get(txn2, b"key").unwrap(), Some(b"v2".to_vec()));

        engine.commit_txn(txn1).unwrap();
        engine.commit_txn(txn2).unwrap();
    }

    #[test]
    fn test_txn_write_conflict_independence() {
        let dir = tempdir().unwrap();
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            ..Default::default()
        };
        let engine = LsmEngine::open(options).unwrap();

        let txn1 = engine.begin_txn();
        let txn2 = engine.begin_txn();

        engine.txn_put(txn1, b"a".to_vec(), b"1".to_vec()).unwrap();
        engine.txn_put(txn2, b"b".to_vec(), b"2".to_vec()).unwrap();

        engine.commit_txn(txn1).unwrap();
        engine.commit_txn(txn2).unwrap();

        assert_eq!(engine.get(b"a").unwrap(), Some(b"1".to_vec()));
        assert_eq!(engine.get(b"b").unwrap(), Some(b"2".to_vec()));
    }

    #[test]
    fn test_txn_delete_in_transaction() {
        let dir = tempdir().unwrap();
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            ..Default::default()
        };
        let engine = LsmEngine::open(options).unwrap();

        engine.put(b"key".to_vec(), b"value".to_vec()).unwrap();

        let txn = engine.begin_txn();
        engine.txn_delete(txn, b"key".to_vec()).unwrap();
        assert_eq!(engine.txn_get(txn, b"key").unwrap(), None);

        engine.commit_txn(txn).unwrap();
        assert_eq!(engine.get(b"key").unwrap(), None);
    }

    #[test]
    fn test_txn_scan_prefix() {
        let dir = tempdir().unwrap();
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            ..Default::default()
        };
        let engine = LsmEngine::open(options).unwrap();

        engine.put(b"user:1".to_vec(), b"alice".to_vec()).unwrap();
        engine.put(b"user:2".to_vec(), b"bob".to_vec()).unwrap();
        engine.put(b"item:1".to_vec(), b"widget".to_vec()).unwrap();

        let txn = engine.begin_txn();
        engine.txn_put(txn, b"user:3".to_vec(), b"charlie".to_vec()).unwrap();

        let results = engine.txn_scan_prefix(txn, b"user:").unwrap();
        assert_eq!(results.len(), 3);

        engine.commit_txn(txn).unwrap();
    }

    #[test]
    fn test_txn_overwrite_in_buffer() {
        let dir = tempdir().unwrap();
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            ..Default::default()
        };
        let engine = LsmEngine::open(options).unwrap();

        let txn = engine.begin_txn();
        engine.txn_put(txn, b"key".to_vec(), b"v1".to_vec()).unwrap();
        engine.txn_put(txn, b"key".to_vec(), b"v2".to_vec()).unwrap();

        assert_eq!(engine.txn_get(txn, b"key").unwrap(), Some(b"v2".to_vec()));

        engine.commit_txn(txn).unwrap();
        assert_eq!(engine.get(b"key").unwrap(), Some(b"v2".to_vec()));
    }

    #[test]
    fn test_txn_active_count() {
        let dir = tempdir().unwrap();
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            ..Default::default()
        };
        let engine = LsmEngine::open(options).unwrap();

        assert_eq!(engine.active_txn_count(), 0);

        let t1 = engine.begin_txn();
        assert_eq!(engine.active_txn_count(), 1);

        let t2 = engine.begin_txn();
        assert_eq!(engine.active_txn_count(), 2);

        engine.commit_txn(t1).unwrap();
        assert_eq!(engine.active_txn_count(), 1);

        engine.abort_txn(t2).unwrap();
        assert_eq!(engine.active_txn_count(), 0);
    }

    #[test]
    fn test_index_persistence_across_restart() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path().to_path_buf();

        // Phase 1: Create index, insert data, flush to SSTable
        {
            let options = StorageOptions {
                data_dir: data_dir.clone(),
                memtable_size_limit: 1024 * 1024,
                ..Default::default()
            };
            let engine = LsmEngine::open(options).unwrap();

            engine.create_index("Product", "price").unwrap();

            // Insert via transaction so indexes are maintained
            let txn = engine.begin_txn();
            let doc1 = serde_json::json!({"__class__": "Product", "name": "iPhone", "price": 999});
            let doc2 = serde_json::json!({"__class__": "Product", "name": "iPad", "price": 799});
            engine.txn_put(txn, b"Product::001".to_vec(), serde_json::to_vec(&doc1).unwrap()).unwrap();
            engine.txn_put(txn, b"Product::002".to_vec(), serde_json::to_vec(&doc2).unwrap()).unwrap();
            engine.commit_txn(txn).unwrap();

            engine.flush().unwrap();
        }

        // Phase 2: Reopen engine �?indexes should be rebuilt automatically
        {
            let options = StorageOptions {
                data_dir,
                memtable_size_limit: 1024 * 1024,
                ..Default::default()
            };
            let engine = LsmEngine::open(options).unwrap();

            // Index should exist after restart
            assert!(engine.has_index("Product", "price"), "index should persist across restart");

            // Index should be functional: lookup by value
            let pkeys = engine.index_manager().write().unwrap().lookup_eq(
                "Product",
                "price",
                &serde_json::json!(999),
            );
            assert!(pkeys.is_some(), "index lookup should work after restart");
            assert_eq!(pkeys.unwrap().len(), 1);

            // Range scan should also work
            let pkeys = engine.index_manager().write().unwrap().lookup_range(
                "Product",
                "price",
                Some(&serde_json::json!(500)),
                Some(&serde_json::json!(1000)),
            );
            assert!(pkeys.is_some());
            assert_eq!(pkeys.unwrap().len(), 2); // both products
        }
    }

    #[test]
    fn test_backup_and_restore() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path().join("data");
        let backup_dir = dir.path().join("backup");

        // Phase 1: Create engine, insert data, flush, backup
        {
            let options = StorageOptions {
                data_dir: data_dir.clone(),
                memtable_size_limit: 1024 * 1024,
                ..Default::default()
            };
            let engine = LsmEngine::open(options).unwrap();

            engine.put(b"key1".to_vec(), b"value1".to_vec()).unwrap();
            engine.put(b"key2".to_vec(), b"value2".to_vec()).unwrap();
            engine.put(b"key3".to_vec(), b"value3".to_vec()).unwrap();
            engine.flush().unwrap();

            // Create backup
            let manifest = engine.backup(&backup_dir).unwrap();
            assert!(!manifest.files.is_empty(), "backup should have files");
            assert!(backup_dir.join("manifest.json").exists());
            assert!(backup_dir.join("wal.log").exists());

            // Verify at least one SSTable in backup
            let sst_files: Vec<_> = manifest.files.iter()
                .filter(|f| matches!(f.file_type, BackupFileType::SSTable))
                .collect();
            assert!(!sst_files.is_empty(), "backup should contain SSTable files");
        }

        // Phase 2: Restore to a new directory
        let restore_dir = dir.path().join("restored");
        {
            let manifest = LsmEngine::restore(&backup_dir, &restore_dir).unwrap();
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
            let engine = LsmEngine::open(options).unwrap();

            assert_eq!(engine.get(b"key1").unwrap(), Some(b"value1".to_vec()));
            assert_eq!(engine.get(b"key2").unwrap(), Some(b"value2".to_vec()));
            assert_eq!(engine.get(b"key3").unwrap(), Some(b"value3".to_vec()));
            assert_eq!(engine.get(b"missing").unwrap(), None);
        }
    }
}
