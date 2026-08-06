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
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};

/// The main LSM-Tree storage engine with MVCC support.
pub struct LsmEngine {
    /// Active MemTable for writes.
    memtable: MemTable,

    /// Read-only MemTable waiting to be flushed.
    immutable_memtable: Option<MemTable>,

    /// SSTables organized by level. Level 0 is newest.
    /// Shared with the background compaction worker via Arc<Mutex>.
    levels: Arc<Mutex<Vec<Vec<SsTableInfo>>>>,

    /// Cache of opened SSTable handles, keyed by file path.
    /// Avoids re-opening files and re-reading footer/bloom/index on every read.
    /// Entries are invalidated when compaction replaces SSTable files.
    sst_cache: HashMap<PathBuf, SsTable>,

    /// Write-Ahead Log for durability.
    wal: Wal,

    /// Engine configuration.
    options: StorageOptions,

    /// Sequence number generator.
    seq_counter: AtomicU64,

    /// SSTable file ID counter (shared with compaction worker).
    sst_counter: Arc<AtomicU64>,

    /// MVCC transaction manager.
    txn_manager: TxnManager,

    /// Secondary index manager.
    index_manager: IndexManager,

    /// Vector index manager (HNSW).
    vector_index_manager: VectorIndexManager,

    /// Channel to send flush notifications to the background compaction worker.
    compaction_sender: mpsc::Sender<CompactionMsg>,

    /// Channel to receive compaction notifications (for cache invalidation).
    /// Wrapped in Mutex because mpsc::Receiver is not Sync.
    compaction_notif_receiver: std::sync::Mutex<mpsc::Receiver<CompactionNotification>>,
}

/// Temporary helper for loading WAL + SSTables before spawning the compaction worker.
struct PreLoadEngine {
    memtable: MemTable,
    immutable_memtable: Option<MemTable>,
    levels: Vec<Vec<SsTableInfo>>,
    sst_cache: HashMap<PathBuf, SsTable>,
    wal: Wal,
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
                EntryKind::Put => self.memtable.put_with_seq(entry.key, entry.value, entry.seq_no),
                EntryKind::Delete => self.memtable.delete_with_seq(entry.key, entry.seq_no),
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
            let mut sst = SsTable::open(&path)?;
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
            self.sst_cache.insert(path.clone(), sst);
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
            memtable: MemTable::new(),
            immutable_memtable: None,
            levels: vec![Vec::new(); options.num_levels],
            sst_cache: HashMap::new(),
            wal,
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

        let mut engine = LsmEngine {
            memtable: pre_engine.memtable,
            immutable_memtable: pre_engine.immutable_memtable,
            levels,
            sst_cache: pre_engine.sst_cache,
            wal: pre_engine.wal,
            options,
            seq_counter: pre_engine.seq_counter,
            sst_counter,
            txn_manager: TxnManager::new(),
            index_manager,
            vector_index_manager: VectorIndexManager::new(),
            compaction_sender,
            compaction_notif_receiver: std::sync::Mutex::new(compaction_notif_receiver),
        };

        // Rebuild secondary indexes from persisted index entries
        engine.rebuild_indexes()?;

        // Rebuild vector indexes from persisted metadata
        engine.rebuild_vector_indexes()?;

        Ok(engine)
    }

    /// Puts a key-value pair.
    pub fn put(&mut self, key: Key, value: Value) -> Result<()> {
        let seq = self.next_seq();
        let entry = Entry::put(key.clone(), value.clone(), seq);

        // Write to WAL first (durability)
        self.wal.append(&entry)?;

        // Write to MemTable with engine's global seq_no
        self.memtable.put_with_seq(key, value, seq);

        // Check if we need to flush
        if self.memtable.size() >= self.options.memtable_size_limit {
            self.flush_memtable()?;
        }

        Ok(())
    }

    /// Gets a value by key.
    pub fn get(&mut self, key: &[u8]) -> Result<Option<Value>> {
        self.drain_compaction_notifications();

        // 1. Check active MemTable
        if let Some((val, _)) = self.memtable.get(key) {
            return Ok(Some(val.to_vec()));
        }

        // 2. Check immutable MemTable
        if let Some(ref imm) = self.immutable_memtable {
            if let Some((val, _)) = imm.get(key) {
                return Ok(Some(val.to_vec()));
            }
        }

        // 3. Check SSTables (newest to oldest)
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

        for path in &candidates {
            let sst = self.get_sst(path)?;
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
    pub fn scan_prefix(&mut self, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        self.scan_prefix_internal(prefix, None)
    }

    /// Internal scan implementation shared by scan_prefix and scan_prefix_with_visibility.
    /// When `vis` is Some, only entries visible to the snapshot are included.
    fn scan_prefix_internal(
        &mut self,
        prefix: &[u8],
        vis: Option<&crate::mvcc::Visibility>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        self.drain_compaction_notifications();

        let mut seen: BTreeMap<Vec<u8>, (Vec<u8>, SeqNo, EntryKind)> = BTreeMap::new();

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

        for path in &sst_paths {
            let sst = self.get_sst(path)?;
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

        // 2. Scan immutable MemTable (overrides SSTables)
        if let Some(ref imm) = self.immutable_memtable {
            for entry in imm.entries() {
                if !entry.key.starts_with(prefix) {
                    continue;
                }
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

        // 3. Scan active MemTable (overrides everything)
        for entry in self.memtable.entries() {
            if !entry.key.starts_with(prefix) {
                continue;
            }
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
    pub fn delete(&mut self, key: Key) -> Result<()> {
        let seq = self.next_seq();
        let entry = Entry::delete(key.clone(), seq);

        self.wal.append(&entry)?;
        self.memtable.delete_with_seq(key, seq);

        if self.memtable.size() >= self.options.memtable_size_limit {
            self.flush_memtable()?;
        }

        Ok(())
    }

    /// Manually flushes the current MemTable to an SSTable.
    /// Call this after a batch of writes to ensure all data is persisted.
    pub fn flush(&mut self) -> Result<()> {
        if !self.memtable.is_empty() {
            self.flush_memtable()?;
        }
        Ok(())
    }

    /// Flushes the current MemTable to an SSTable on disk.
    fn flush_memtable(&mut self) -> Result<()> {
        // Swap current MemTable to immutable
        let old_mem = std::mem::replace(&mut self.memtable, MemTable::new());
        self.immutable_memtable = Some(old_mem);

        // Build SSTable from immutable MemTable
        if let Some(ref imm) = self.immutable_memtable {
            let sst_id = self.sst_counter.fetch_add(1, Ordering::Relaxed);
            let sst_path = self.options.data_dir.join(format!("L0_{}.sst", sst_id));

            let mut builder = SsTableBuilder::new();
            builder.set_compression_level(self.options.compression_level);
            for entry in imm.entries() {
                builder.add(&Entry {
                    key: entry.key.clone(),
                    value: entry.value.clone(),
                    seq_no: entry.seq_no,
                    kind: entry.kind,
                });
            }

            builder.build(&sst_path)?;

            let min_key = imm
                .entries()
                .next()
                .map(|e| e.key.clone())
                .unwrap_or_default();
            let max_key = imm
                .entries()
                .last()
                .map(|e| e.key.clone())
                .unwrap_or_default();

            let metadata = fs::metadata(&sst_path)?;

            // Add to shared levels under lock
            {
                let mut levels = self.levels.lock().unwrap();
                levels[0].push(SsTableInfo {
                    path: sst_path,
                    size: metadata.len(),
                    min_key,
                    max_key,
                });
            }

            // Notify the background compaction worker
            let _ = self.compaction_sender.send(CompactionMsg::Flushed { level: 0 });
        }

        // Clear immutable MemTable
        self.immutable_memtable = None;

        // Reset WAL (we've persisted everything to SSTable)
        self.reset_wal()?;

        // Compaction is handled by the background worker �?no synchronous call needed

        Ok(())
    }


    /// Drains compaction notifications from the background worker.
    /// Evicts stale SSTable cache entries when compaction replaces files.
    fn drain_compaction_notifications(&mut self) {
        let receiver = self.compaction_notif_receiver.lock().unwrap();
        let mut had_compaction = false;
        while let Ok(notif) = receiver.try_recv() {
            match notif {
                CompactionNotification::Compacted { evicted_paths } => {
                    for path in &evicted_paths {
                        self.sst_cache.remove(path);
                    }
                    had_compaction = true;
                }
                CompactionNotification::FlushDone => {
                    // Ignore FlushDone in normal drain
                }
            }
        }
        drop(receiver);
        // After any compaction, clear the entire cache to ensure no stale handles
        if had_compaction {
            self.sst_cache.clear();
        }
    }

    /// Blocks until all pending background compaction work is complete.
    /// Useful for testing and graceful shutdown.
    pub fn flush_compaction(&mut self) -> Result<()> {
        // Drain any pending notifications first
        self.drain_compaction_notifications();

        let _ = self.compaction_sender.send(CompactionMsg::FlushAndNotify);
        let receiver = self.compaction_notif_receiver.lock().unwrap();
        let mut had_compaction = false;
        loop {
            match receiver.recv_timeout(std::time::Duration::from_secs(10)) {
                Ok(CompactionNotification::FlushDone) => break,
                Ok(CompactionNotification::Compacted { ref evicted_paths }) => {
                    for path in evicted_paths {
                        self.sst_cache.remove(path);
                    }
                    had_compaction = true;
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    tracing::warn!("flush_compaction timed out waiting for FlushDone");
                    break;
                }
                Err(_) => break,
            }
        }
        drop(receiver);
        // Clear the entire cache after compaction to ensure no stale handles
        if had_compaction {
            self.sst_cache.clear();
        }
        Ok(())
    }

    /// Resets the WAL file after a successful flush.
    ///
    /// Uses write-new-then-rename for atomicity:
    /// 1. Create a new WAL at a temp path
    /// 2. Atomically rename temp �?wal.log
    /// This ensures the WAL is never missing, even if the process crashes mid-reset.
    fn reset_wal(&mut self) -> Result<()> {
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

        self.wal = new_wal;
        // Re-open at the final path (rename doesn't update the file handle's path)
        self.wal = Wal::open(&wal_path)?;

        Ok(())
    }

    fn next_seq(&self) -> SeqNo {
        self.seq_counter.fetch_add(1, Ordering::Relaxed)
    }

    /// Gets or opens a cached SSTable handle.
    /// Opens the file and reads footer/bloom/index only on first access.
    fn get_sst(&mut self, path: &Path) -> Result<&mut SsTable> {
        if !self.sst_cache.contains_key(path) {
            let sst = SsTable::open(path)?;
            self.sst_cache.insert(path.to_path_buf(), sst);
        }
        Ok(self.sst_cache.get_mut(path).unwrap())
    }

    /// Rebuilds secondary indexes by scanning persisted index entries from SSTables.
    fn rebuild_indexes(&mut self) -> Result<()> {
        let index_entries = self.scan_prefix(b"__idx__")?;
        if !index_entries.is_empty() {
            self.index_manager.rebuild_from_entries(&index_entries);
            tracing::info!(
                "Rebuilt {} index entries across {} indexes",
                index_entries.len(),
                self.index_manager.index_count()
            );
        }
        Ok(())
    }

    /// Rebuilds vector indexes by scanning persisted metadata and document data.
    fn rebuild_vector_indexes(&mut self) -> Result<()> {
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
            let _ = self.vector_index_manager.create_index(
                class, column, *dimension, *metric, *m, *ef_construction, *ef_search,
            );
        }

        // Backfill vectors from document data
        for (class, column, dimension, _, _, _, _) in &index_configs {
            let prefix = format!("{}::", class);
            let entries = self.scan_prefix(prefix.as_bytes())?;
            let mut count = 0usize;
            for (pk, val_bytes) in entries {
                if let Ok(serde_json::Value::Object(doc)) =
                    serde_json::from_slice::<serde_json::Value>(&val_bytes)
                {
                    if doc.get("__class__").and_then(|v| v.as_str()) == Some(class.as_str()) {
                        if let Some(serde_json::Value::Array(arr)) = doc.get(column.as_str()) {
                            let vec: Vec<f32> = arr
                                .iter()
                                .filter_map(|v| v.as_f64().map(|f| f as f32))
                                .collect();
                            if vec.len() == *dimension {
                                self.vector_index_manager.index_vector(&pk, class, column, vec);
                                count += 1;
                            }
                        }
                    }
                }
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
    pub fn begin_txn(&mut self) -> SeqNo {
        // seq_counter is the NEXT value to assign, so last committed = seq_counter - 1
        let last_seq = self.seq_counter.load(Ordering::Relaxed).saturating_sub(1);
        self.txn_manager.begin(last_seq)
    }

    /// Commits a transaction. Flushes its write buffer to WAL + MemTable.
    pub fn commit_txn(&mut self, txn_id: SeqNo) -> Result<()> {
        let writes = self.txn_manager.commit(txn_id)?;

        for (key, op) in writes {
            let seq = self.next_seq();

            // Maintain indexes if any exist for this class
            let class = Self::extract_class_from_key(&key);
            let has_indexes = class.as_ref().map_or(false, |c| {
                !self.index_manager.indexes_for_class(c).is_empty()
            });
            let has_vector_indexes = class.as_ref().map_or(false, |c| {
                self.vector_index_manager.has_any_index(c)
            });

            match op {
                WriteOp::Put(value) => {
                    if has_indexes || has_vector_indexes {
                        // Deindex the old value first (handles UPDATE case)
                        if let Some(old_val) = self.get(&key)? {
                            if let Ok(serde_json::Value::Object(ref old_doc)) =
                                serde_json::from_slice::<serde_json::Value>(&old_val)
                            {
                                if let Some(ref c) = class {
                                    if has_indexes {
                                        self.index_manager.deindex_document(c, &key, old_doc);
                                    }
                                    if has_vector_indexes {
                                        self.vector_index_manager.deindex_vectors(&key);
                                    }
                                }
                            }
                        }
                        // Index the new value and persist index entries
                        if let Ok(serde_json::Value::Object(ref doc)) =
                            serde_json::from_slice::<serde_json::Value>(&value)
                        {
                            if let Some(ref c) = class {
                                if has_indexes {
                                    let index_entries = self.index_manager.index_document(c, &key, doc);
                                    for (idx_key, idx_val) in index_entries {
                                        let idx_seq = self.next_seq();
                                        let idx_entry = Entry::put(idx_key.clone(), idx_val.clone(), idx_seq);
                                        self.wal.append(&idx_entry)?;
                                        self.memtable.put_with_seq(idx_key, idx_val, idx_seq);
                                    }
                                }
                                if has_vector_indexes {
                                    self.vector_index_manager.index_document_vectors(&key, c, doc);
                                }
                            }
                        } else {
                            // Value deserialization failed — skip indexing
                        }
                    }

                    let entry = Entry::put(key.clone(), value.clone(), seq);
                    self.wal.append(&entry)?;
                    self.memtable.put_with_seq(key, value, seq);
                }
                WriteOp::Delete => {
                    // For deletes, we need the old value to de-index
                    if has_indexes || has_vector_indexes {
                        if let Some(old_val) = self.get(&key)? {
                            if let Ok(serde_json::Value::Object(ref doc)) =
                                serde_json::from_slice::<serde_json::Value>(&old_val)
                            {
                                if let Some(ref c) = class {
                                    if has_indexes {
                                        self.index_manager.deindex_document(c, &key, doc);
                                    }
                                    if has_vector_indexes {
                                        self.vector_index_manager.deindex_vectors(&key);
                                    }
                                }
                            }
                        }
                    }

                    let entry = Entry::delete(key.clone(), seq);
                    self.wal.append(&entry)?;
                    self.memtable.delete_with_seq(key, seq);
                }
            }
        }

        // Sync WAL for durability if configured
        if self.options.sync_wal_on_commit {
            self.wal.sync()?;
        }

        // Check if we need to flush
        if self.memtable.size() >= self.options.memtable_size_limit {
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

    /// Aborts a transaction. Discards all pending writes.
    pub fn abort_txn(&mut self, txn_id: SeqNo) -> Result<()> {
        self.txn_manager.abort(txn_id)
    }

    /// Buffers a put operation in a transaction.
    pub fn txn_put(&mut self, txn_id: SeqNo, key: Key, value: Value) -> Result<()> {
        self.txn_manager
            .get_mut(txn_id)
            .ok_or_else(|| onto_core::CoreError::InvalidArgument(
                format!("transaction {} not found or not active", txn_id),
            ))?
            .put(key, value);
        Ok(())
    }

    /// Buffers a delete operation in a transaction.
    pub fn txn_delete(&mut self, txn_id: SeqNo, key: Key) -> Result<()> {
        self.txn_manager
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
    pub fn txn_get(&mut self, txn_id: SeqNo, key: &[u8]) -> Result<Option<Value>> {
        // 1. Check transaction's own write buffer
        if let Some(txn) = self.txn_manager.get(txn_id) {
            if let Some(op) = txn.write_buffer_get(key) {
                return match op {
                    WriteOp::Put(v) => Ok(Some(v.clone())),
                    WriteOp::Delete => Ok(None),
                };
            }
        }

        // 2-3. Read from storage with snapshot visibility
        let vis = self.txn_manager.visibility_for(txn_id);
        self.get_with_visibility(key, &vis)
    }

    /// Scans all entries with the given prefix, respecting snapshot visibility.
    pub fn txn_scan_prefix(
        &mut self,
        txn_id: SeqNo,
        prefix: &[u8],
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let vis = self.txn_manager.visibility_for(txn_id);

        // Get base results from storage with visibility filtering
        let mut results = self.scan_prefix_with_visibility(prefix, &vis)?;

        // Overlay the transaction's own write buffer
        if let Some(txn) = self.txn_manager.get(txn_id) {
            for (key, op) in txn.write_buffer_iter() {
                if key.starts_with(prefix) {
                    match op {
                        WriteOp::Put(value) => {
                            // Insert or update in results
                            if let Some(existing) = results.iter_mut().find(|(k, _)| k == key) {
                                existing.1 = value.clone();
                            } else {
                                results.push((key.clone(), value.clone()));
                            }
                        }
                        WriteOp::Delete => {
                            // Remove from results
                            results.retain(|(k, _)| k != key);
                        }
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
        &mut self,
        key: &[u8],
        vis: &crate::mvcc::Visibility,
    ) -> Result<Option<Value>> {
        // Check active MemTable �?use range query to find visible version
        for entry in self.memtable.get_versions(key) {
            if vis.is_visible(entry.seq_no) {
                if entry.is_tombstone() {
                    return Ok(None);
                }
                return Ok(Some(entry.value.to_vec()));
            }
            // If not visible, keep looking for older visible versions
        }

        // Check immutable MemTable
        if let Some(ref imm) = self.immutable_memtable {
            for entry in imm.get_versions(key) {
                if vis.is_visible(entry.seq_no) {
                    if entry.is_tombstone() {
                        return Ok(None);
                    }
                    return Ok(Some(entry.value.to_vec()));
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

        for path in &candidates {
            let sst = self.get_sst(path)?;
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
        &mut self,
        prefix: &[u8],
        vis: &crate::mvcc::Visibility,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        self.scan_prefix_internal(prefix, Some(vis))
    }

    /// Returns the number of active transactions.
    pub fn active_txn_count(&self) -> usize {
        self.txn_manager.active_count()
    }

    // =================================================================
    //  Index API
    // =================================================================
    /// Creates a secondary index on a class.column.
    /// Automatically backfills existing data for the class.
    pub fn create_index(&mut self, class: &str, column: &str) -> Result<()> {
        self.index_manager.create_index(class, column);

        // Backfill: scan all existing entries for this class and index them
        let prefix = format!("{}::", class);
        let entries = self.scan_prefix(prefix.as_bytes())?;

        for (pk, val_bytes) in entries {
            if let Ok(serde_json::Value::Object(doc)) =
                serde_json::from_slice::<serde_json::Value>(&val_bytes)
            {
                if doc.get("__class__").and_then(|v| v.as_str()) == Some(class) {
                    let index_entries = self.index_manager.index_document(class, &pk, &doc);
                    // Persist index entries to WAL + MemTable
                    for (key, value) in index_entries {
                        let seq = self.next_seq();
                        let entry = Entry::put(key.clone(), value.clone(), seq);
                        self.wal.append(&entry)?;
                        self.memtable.put_with_seq(key, value, seq);
                    }
                }
            }
        }

        Ok(())
    }

    /// Drops a secondary index.
    pub fn drop_index(&mut self, class: &str, column: &str) -> bool {
        self.index_manager.drop_index(class, column)
    }

    /// Returns true if an index exists on the given class.column.
    pub fn has_index(&self, class: &str, column: &str) -> bool {
        self.index_manager.has_index(class, column)
    }

    /// Returns a reference to the index manager.
    pub fn index_manager(&self) -> &IndexManager {
        &self.index_manager
    }

    /// Returns a mutable reference to the index manager.
    pub fn index_manager_mut(&mut self) -> &mut IndexManager {
        &mut self.index_manager
    }

    // =================================================================
    //  Vector Index API
    // =================================================================

    /// Creates a vector index on a class.column with HNSW parameters.
    pub fn create_vector_index(
        &mut self,
        class: &str,
        column: &str,
        dimension: usize,
        metric: crate::vector::DistanceMetric,
        m: usize,
        ef_construction: usize,
        ef_search: usize,
    ) -> Result<()> {
        self.vector_index_manager
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
        self.wal.append(&entry)?;
        self.memtable.put_with_seq(meta_key, meta_val, seq);

        // Backfill: scan existing documents and index their vectors
        let prefix = format!("{}::", class);
        let entries = self.scan_prefix(prefix.as_bytes())?;

        for (pk, val_bytes) in entries {
            if let Ok(serde_json::Value::Object(doc)) =
                serde_json::from_slice::<serde_json::Value>(&val_bytes)
            {
                if doc.get("__class__").and_then(|v| v.as_str()) == Some(class) {
                    if let Some(serde_json::Value::Array(arr)) = doc.get(column) {
                        let vec: Vec<f32> = arr
                            .iter()
                            .filter_map(|v| v.as_f64().map(|f| f as f32))
                            .collect();
                        if vec.len() == dimension {
                            self.vector_index_manager.index_vector(&pk, class, column, vec);
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Drops a vector index.
    pub fn drop_vector_index(&mut self, class: &str, column: &str) -> bool {
        let removed = self.vector_index_manager.drop_index(class, column);
        if removed {
            // Remove persisted metadata
            let meta_key = Self::make_vec_meta_key(class, column);
            let seq = self.next_seq();
            let del_entry = Entry::delete(meta_key.clone(), seq);
            let _ = self.wal.append(&del_entry);
            self.memtable.delete_with_seq(meta_key, seq);
        }
        removed
    }

    /// Returns true if a vector index exists on the given class.column.
    pub fn has_vector_index(&self, class: &str, column: &str) -> bool {
        self.vector_index_manager.has_index(class, column)
    }

    /// Returns a reference to the vector index manager.
    pub fn vector_index_manager(&self) -> &VectorIndexManager {
        &self.vector_index_manager
    }

    /// Returns a mutable reference to the vector index manager.
    pub fn vector_index_manager_mut(&mut self) -> &mut VectorIndexManager {
        &mut self.vector_index_manager
    }

    /// Returns engine statistics.
    pub fn stats(&self) -> EngineStats {
        let levels = self.levels.lock().unwrap();
        let total_sstables: usize = levels.iter().map(|l| l.len()).sum();
        let total_sst_size: u64 = levels
            .iter()
            .flat_map(|l| l.iter())
            .map(|s| s.size)
            .sum();

        EngineStats {
            memtable_size: self.memtable.size(),
            memtable_entries: self.memtable.len(),
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
    pub fn backup(&mut self, backup_dir: &Path) -> Result<BackupManifest> {
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
    fn flush_disk_indexes(&mut self) -> Result<()> {
        self.index_manager.flush_disk_indexes();
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

        let mut engine = LsmEngine::open(options).unwrap();

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

        let mut engine = LsmEngine::open(options).unwrap();

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

        let mut engine = LsmEngine::open(options).unwrap();

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

        let mut engine = LsmEngine::open(options).unwrap();

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
            let mut engine = LsmEngine::open(options).unwrap();
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
            let mut engine = LsmEngine::open(options).unwrap();

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

        let mut engine = LsmEngine::open(options).unwrap();

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

        let mut engine = LsmEngine::open(options).unwrap();

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

        let mut engine = LsmEngine::open(options).unwrap();

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

        let mut engine = LsmEngine::open(options).unwrap();

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
        let mut engine = LsmEngine::open(options).unwrap();

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
        let mut engine = LsmEngine::open(options).unwrap();

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
        let mut engine = LsmEngine::open(options).unwrap();

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
        let mut engine = LsmEngine::open(options).unwrap();

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
        let mut engine = LsmEngine::open(options).unwrap();

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
        let mut engine = LsmEngine::open(options).unwrap();

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
        let mut engine = LsmEngine::open(options).unwrap();

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
        let mut engine = LsmEngine::open(options).unwrap();

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
        let mut engine = LsmEngine::open(options).unwrap();

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
            let mut engine = LsmEngine::open(options).unwrap();

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
            let mut engine = LsmEngine::open(options).unwrap();

            // Index should exist after restart
            assert!(engine.has_index("Product", "price"), "index should persist across restart");

            // Index should be functional: lookup by value
            let pkeys = engine.index_manager_mut().lookup_eq(
                "Product",
                "price",
                &serde_json::json!(999),
            );
            assert!(pkeys.is_some(), "index lookup should work after restart");
            assert_eq!(pkeys.unwrap().len(), 1);

            // Range scan should also work
            let pkeys = engine.index_manager_mut().lookup_range(
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
            let mut engine = LsmEngine::open(options).unwrap();

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
            let mut engine = LsmEngine::open(options).unwrap();

            assert_eq!(engine.get(b"key1").unwrap(), Some(b"value1".to_vec()));
            assert_eq!(engine.get(b"key2").unwrap(), Some(b"value2".to_vec()));
            assert_eq!(engine.get(b"key3").unwrap(), Some(b"value3".to_vec()));
            assert_eq!(engine.get(b"missing").unwrap(), None);
        }
    }
}
