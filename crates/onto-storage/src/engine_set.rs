// Copyright (c) 2024-2026 Guo Jianzhu (郭建筑)
// Licensed under the Apache License, Version 2.0.
// See LICENSE for details.
//! Multi-engine router: one [`LsmEngine`] per namespace plus a default engine.
//!
//! OntoDB keeps one engine per namespace under `data/{ns}/` and a default
//! engine at the data root. Every key is stored in exactly one engine, chosen
//! by the namespace of the class it belongs to:
//!
//! * class-scoped keys (documents `{ns.Class}::{id}`, index keys `__idx2__…`,
//!   value metadata `__val_meta__::{class}::{pk}`, index metadata
//!   `__idx_meta__{class}__{col}`, vector keys `__vec_*{class}…`) live in the
//!   engine of their class's namespace;
//! * namespace-agnostic system keys (`__ontology__`, `__ns__`, `__graph_v__`,
//!   `__graph_e__`, `__triple__*`, `__mv_*`, `__cte_*`, `__raft_*`, ...) live
//!   in the default engine. DDL and catalog writes therefore share one engine
//!   while document writes are isolated per namespace.
//!
//! A class name is namespace-qualified when it contains a dot
//! (`sempet.Drug`); the namespace is the segment before the first dot. Bare
//! class names (`Product`) belong to the default engine. Keys whose namespace
//! is not registered with the set fall back to the default engine, so a
//! namespace must be registered before its keys are written (data for
//! unregistered namespaces lands in the default engine and must be migrated
//! when the namespace appears).
//!
//! Transactions: [`EngineSet::begin_txn`] hands out a router-level handle. The
//! binding to a concrete engine is created lazily on the first **write** (the
//! engine's `begin_txn` snapshot is taken at that point). All writes of one
//! transaction must target the same engine — cross-namespace transactions are
//! rejected. Reads never bind: they use the transaction view only when the
//! transaction is already bound to the key's engine (read-your-own-writes) and
//! otherwise read committed state from the key's engine, which keeps
//! cross-namespace read-only access (e.g. `sempet` queries reading
//! `sembio.Compound`) working inside a write transaction.

use crate::engine::{EngineStats, LsmEngine};
use crate::index::IndexManager;
use crate::mvcc::Visibility;
use crate::options::StorageOptions;
use crate::value_meta::ValueMetadata;
use onto_core::{CoreError, Key, Result, SeqNo, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

/// v2 length-prefixed index key marker (`IndexManager::make_index_key`).
const IDX2_PREFIX: &[u8] = b"__idx2__";
/// v1 `::`-delimited index key marker (legacy, migration only).
const IDX_V1_PREFIX: &[u8] = b"__idx__";
/// Secondary-index metadata key marker: `__idx_meta__{class}__{column}`.
const IDX_META_PREFIX: &[u8] = b"__idx_meta__";
/// Dropped-index marker: `__idx_drop__{class}__{column}`.
const IDX_DROP_PREFIX: &[u8] = b"__idx_drop__";
/// Value-metadata key marker: `__val_meta__::{class}::{pk}`.
const VAL_META_PREFIX: &[u8] = b"__val_meta__::";
/// Vector-index metadata key marker: `__vec_meta__{class}__{column}`.
const VEC_META_PREFIX: &[u8] = b"__vec_meta__";
/// Vector graph snapshot key marker: `__vec_graph__{class}_{column}`.
const VEC_GRAPH_PREFIX: &[u8] = b"__vec_graph__";
/// Vector delta key marker: `__vec_delta__{class}_{column}`.
const VEC_DELTA_PREFIX: &[u8] = b"__vec_delta__";

/// Key prefixes that always live in the default engine: catalog, graph cache,
/// triples, materialized views, CTE staging, raft state and engine probes.
const GLOBAL_PREFIXES: &[&[u8]] = &[
    b"__ontology__",
    b"__ns__",
    b"__graph_v__",
    b"__graph_e__",
    b"__triple__",
    b"__triple_pos__",
    b"__triple_osp__",
    b"__mv_",
    b"__cte_",
    b"__raft_",
    b"__config__",
    b"__probe_pk__",
    b"__validate_probe__",
];

/// Marker families whose keys embed their owning class and therefore follow
/// the class's namespace engine instead of the default engine.
const CLASS_EMBEDDED_MARKERS: &[&[u8]] = &[
    IDX_META_PREFIX,
    IDX_DROP_PREFIX,
    VEC_META_PREFIX,
    VEC_GRAPH_PREFIX,
    VEC_DELTA_PREFIX,
];

/// Routes key/class-addressed operations across namespace engines.
///
/// See the module docs for the routing rules and transaction semantics.
pub struct EngineSet {
    default: Arc<LsmEngine>,
    namespaces: RwLock<HashMap<String, Arc<LsmEngine>>>,
    txns: Mutex<HashMap<SeqNo, TxnBinding>>,
    next_txn: AtomicU64,
}

/// A router transaction: unbound until its first write picks an engine.
enum TxnBinding {
    Unbound,
    Bound { engine: Arc<LsmEngine>, engine_seq: SeqNo },
}

impl EngineSet {
    /// Wraps an already-open engine as the default engine.
    pub fn new(default: Arc<LsmEngine>) -> Self {
        Self {
            default,
            namespaces: RwLock::new(HashMap::new()),
            txns: Mutex::new(HashMap::new()),
            next_txn: AtomicU64::new(1),
        }
    }

    /// Opens the default engine at `options.data_dir`.
    pub fn open(options: StorageOptions) -> Result<Self> {
        Ok(Self::new(Arc::new(LsmEngine::open(options)?)))
    }

    /// Opens the default engine plus one engine per namespace, each under
    /// `{data_dir}/{ns}/`.
    pub fn open_with_namespaces(options: StorageOptions, namespaces: &[&str]) -> Result<Self> {
        let set = Self::open(options)?;
        for ns in namespaces {
            let base = set.default.options().clone();
            set.open_namespace(ns, &base)?;
        }
        Ok(set)
    }

    /// Opens the default engine at `options.data_dir` and registers one engine
    /// per existing `{data_dir}/{ns}/` namespace subdirectory.
    ///
    /// Startup-safe cutover: before data migration the data root has no
    /// namespace subdirectories, so every key routes to the default engine
    /// (identical to the historical single-engine layout). After migration
    /// drops keys into `data/{ns}/`, those engines are picked up automatically.
    /// A subdirectory counts as a namespace engine when it looks like one
    /// (holds `wal.log` or `*.sst`); `indexes/` and other bookkeeping
    /// directories are ignored.
    pub fn open_with_discovery(options: StorageOptions) -> Result<Self> {
        let root = options.data_dir.clone();
        let mut namespaces: Vec<String> = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&root) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                    continue;
                };
                if validate_namespace(&name).is_err() {
                    continue;
                }
                let is_engine_dir = path.join("wal.log").exists()
                    || std::fs::read_dir(&path)
                        .map(|mut d| {
                            d.any(|e| {
                                e.map(|e| e.path().extension().is_some_and(|x| x == "sst"))
                                    .unwrap_or(false)
                            })
                        })
                        .unwrap_or(false);
                if is_engine_dir {
                    namespaces.push(name);
                }
            }
        }
        namespaces.sort();
        let set = Self::open(options)?;
        for ns in &namespaces {
            let base = set.default.options().clone();
            set.open_namespace(ns, &base)?;
        }
        Ok(set)
    }

    /// Opens and registers a namespace engine under `{base.data_dir}/{ns}/`.
    pub fn open_namespace(&self, ns: &str, base: &StorageOptions) -> Result<Arc<LsmEngine>> {
        let opts = namespace_options(base, ns)?;
        let engine = Arc::new(LsmEngine::open(opts)?);
        self.add_namespace(ns, Arc::clone(&engine))?;
        Ok(engine)
    }

    /// [`Self::open_namespace`] with the default engine's options
    /// (`{root}/{ns}/`), i.e. a namespace engine born under the set root.
    pub fn open_namespace_from_default(&self, ns: &str) -> Result<Arc<LsmEngine>> {
        let base = self.default.options().clone();
        self.open_namespace(ns, &base)
    }

    /// Registers an already-open engine for `ns`.
    pub fn add_namespace(&self, ns: &str, engine: Arc<LsmEngine>) -> Result<()> {
        validate_namespace(ns)?;
        let mut namespaces = self.namespaces.write().unwrap_or_else(|e| e.into_inner());
        if namespaces.contains_key(ns) {
            return Err(CoreError::custom(format!(
                "namespace '{ns}' is already registered"
            )));
        }
        namespaces.insert(ns.to_string(), engine);
        Ok(())
    }

    /// Drops `ns` from the registry and returns its engine, if present.
    pub fn remove_namespace(&self, ns: &str) -> Option<Arc<LsmEngine>> {
        self.namespaces
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(ns)
    }

    /// Registered namespace names, sorted.
    pub fn namespaces(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .namespaces
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .cloned()
            .collect();
        names.sort();
        names
    }

    /// The engine that holds namespace-agnostic keys and bare-class keys.
    pub fn default_engine(&self) -> Arc<LsmEngine> {
        Arc::clone(&self.default)
    }

    /// The engine registered for `ns`, if any.
    pub fn namespace_engine(&self, ns: &str) -> Option<Arc<LsmEngine>> {
        self.namespaces
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(ns)
            .cloned()
    }

    /// All engines: default first, then namespaces sorted by name.
    pub fn all_engines(&self) -> Vec<Arc<LsmEngine>> {
        let mut engines = vec![self.default_engine()];
        let namespaces = self.namespaces.read().unwrap_or_else(|e| e.into_inner());
        let mut names: Vec<&String> = namespaces.keys().collect();
        names.sort();
        for ns in names {
            engines.push(Arc::clone(&namespaces[ns]));
        }
        engines
    }

    // ── Routing ────────────────────────────────────────────────────────────

    /// Namespace of a class name: the segment before the first dot.
    ///
    /// `Some("sempet")` for `sempet.Drug`, `None` for bare `Product`.
    pub fn class_namespace(class: &str) -> Option<&str> {
        class.split_once('.').map(|(ns, _)| ns)
    }

    /// Namespace a key belongs to, before the registration check.
    ///
    /// `None` for global system keys and bare-class keys (both live in the
    /// default engine). Class-embedded key families (index keys, value
    /// metadata, index metadata, vector keys) are routed by the namespace of
    /// the class stored inside the key — they are written by the class's
    /// engine and must be read back from it.
    pub fn key_namespace(key: &[u8]) -> Option<String> {
        // Value metadata: `__val_meta__::{class}::{pk}`.
        if let Some(rest) = key.strip_prefix(VAL_META_PREFIX) {
            let class = split_first(rest, b"::").map(|(c, _)| c).unwrap_or(rest);
            return bytes_namespace(class);
        }
        // Index keys carry a length-prefixed (v2) or delimiter-separated (v1)
        // class; parse it exactly.
        if key.starts_with(IDX2_PREFIX) {
            if let Some((class, _, _, _)) = IndexManager::parse_index_key(key) {
                return Self::class_namespace(&class).map(str::to_string);
            }
        } else if key.starts_with(IDX_V1_PREFIX) {
            if let Some((class, _, _, _)) = IndexManager::parse_index_key_v1(key) {
                return Self::class_namespace(&class).map(str::to_string);
            }
        } else if let Some(rest) = CLASS_EMBEDDED_MARKERS
            .iter()
            .find_map(|marker| key.strip_prefix(*marker))
        {
            // `__idx_meta__{class}__{col}` and friends: the class starts the
            // region but its exact end is ambiguous (`__`/`_` may appear in
            // class or column names), so route by the namespace head only.
            return bytes_namespace(rest);
        }
        // Global system keys are engine-agnostic.
        if is_global_key(key) {
            return None;
        }
        // Document key `{class}::{id}` (class before the first `::`).
        let class = split_first(key, b"::").map(|(c, _)| c).unwrap_or(key);
        bytes_namespace(class)
    }

    /// Namespace a key is routed to under this set: like
    /// [`Self::key_namespace`] but `None` when the namespace is not
    /// registered (the key falls back to the default engine).
    pub fn target_namespace(&self, key: &[u8]) -> Option<String> {
        let ns = Self::key_namespace(key)?;
        if self.namespace_engine(&ns).is_some() {
            Some(ns)
        } else {
            None
        }
    }

    /// Engine that owns documents of `class`.
    pub fn engine_for_class(&self, class: &str) -> Arc<LsmEngine> {
        match Self::class_namespace(class) {
            Some(ns) => self.engine_for_namespace(ns),
            None => self.default_engine(),
        }
    }

    /// Engine that owns `key`.
    pub fn engine_for_key(&self, key: &[u8]) -> Arc<LsmEngine> {
        match Self::key_namespace(key) {
            Some(ns) => self.engine_for_namespace(&ns),
            None => self.default_engine(),
        }
    }

    /// Engines that may hold keys starting with `prefix`.
    ///
    /// A single engine is returned when the prefix pins down one namespace
    /// (or the default engine); otherwise every engine must be scanned and
    /// the results merged.
    pub fn engines_for_prefix(&self, prefix: &[u8]) -> Vec<Arc<LsmEngine>> {
        // Class-embedded families: narrow when the prefix carries a class
        // region with a dot, i.e. a determinable namespace.
        if let Some(engine) = self.class_embedded_prefix_engine(prefix) {
            return vec![engine];
        }
        // Global system keys only ever live in the default engine.
        if GLOBAL_PREFIXES.iter().any(|m| prefix.starts_with(m)) {
            return vec![self.default_engine()];
        }
        // `{class}::…` prefix: the class is fully known before `::`, and keys
        // of a class never share a prefix across engines.
        if let Some((class, _)) = split_first(prefix, b"::") {
            return vec![self.engine_for_class_bytes(class)];
        }
        // `{ns}.…` prefix (dot present): every matching document key belongs
        // to `ns`, and no `__`-prefixed key starts with `{ns}.`.
        if let Some(ns) = bytes_namespace(prefix) {
            return vec![self.engine_for_namespace(&ns)];
        }
        // Ambiguous (`sem`, `Product`, empty, ...): scan everything.
        self.all_engines()
    }

    pub fn engine_for_namespace(&self, ns: &str) -> Arc<LsmEngine> {
        self.namespaces
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(ns)
            .cloned()
            .unwrap_or_else(|| self.default_engine())
    }

    fn engine_for_class_bytes(&self, class: &[u8]) -> Arc<LsmEngine> {
        match bytes_namespace(class) {
            Some(ns) => self.engine_for_namespace(&ns),
            None => self.default_engine(),
        }
    }

    /// Resolves the engine for a class-embedded prefix, or `None` when the
    /// prefix is too short to pin down a namespace (the caller must fan out).
    fn class_embedded_prefix_engine(&self, prefix: &[u8]) -> Option<Arc<LsmEngine>> {
        if let Some(rest) = prefix.strip_prefix(IDX2_PREFIX) {
            // `__idx2__` || class_len:u16 || class ...
            if rest.len() < 3 {
                return None;
            }
            let class_len = u16::from_le_bytes([rest[0], rest[1]]) as usize;
            let have = rest.len() - 2;
            let take = have.min(class_len);
            let class = &rest[2..2 + take];
            // A dot pins the namespace down even for a partial class.
            return Some(self.engine_for_class_bytes(class));
        }
        if prefix.starts_with(IDX_V1_PREFIX) {
            let rest = &prefix[IDX_V1_PREFIX.len()..];
            return Some(self.engine_for_class_bytes(rest));
        }
        if let Some(rest) = prefix.strip_prefix(VAL_META_PREFIX) {
            return Some(self.engine_for_class_bytes(rest));
        }
        if let Some(rest) = CLASS_EMBEDDED_MARKERS
            .iter()
            .find_map(|marker| prefix.strip_prefix(*marker))
        {
            return Some(self.engine_for_class_bytes(rest));
        }
        None
    }

    // ── Non-transactional data plane (routed) ──────────────────────────────

    pub fn put(&self, key: Key, value: Value) -> Result<()> {
        self.engine_for_key(&key).put(key, value)
    }

    /// Writes a batch, splitting entries across engines as needed. The batch
    /// is not atomic across engines.
    pub fn put_batch(&self, entries: Vec<(Key, Value)>) -> Result<usize> {
        let mut groups: Vec<(Arc<LsmEngine>, Vec<(Key, Value)>)> = Vec::new();
        for (key, value) in entries {
            let engine = self.engine_for_key(&key);
            match groups.iter_mut().find(|(e, _)| Arc::ptr_eq(e, &engine)) {
                Some((_, list)) => list.push((key, value)),
                None => groups.push((engine, vec![(key, value)])),
            }
        }
        let mut total = 0usize;
        for (engine, list) in groups {
            total += engine.put_batch(list)?;
        }
        Ok(total)
    }

    pub fn get(&self, key: &[u8]) -> Result<Option<Value>> {
        self.engine_for_key(key).get(key)
    }

    pub fn multi_get(&self, keys: &[Vec<u8>]) -> Result<Vec<Option<Value>>> {
        let mut out: Vec<Option<Value>> = std::iter::repeat_with(|| None).take(keys.len()).collect();
        let mut groups: Vec<(Arc<LsmEngine>, Vec<usize>)> = Vec::new();
        for (i, key) in keys.iter().enumerate() {
            let engine = self.engine_for_key(key);
            match groups.iter_mut().find(|(e, _)| Arc::ptr_eq(e, &engine)) {
                Some((_, idxs)) => idxs.push(i),
                None => groups.push((engine, vec![i])),
            }
        }
        for (engine, idxs) in groups {
            let sub: Vec<Vec<u8>> = idxs.iter().map(|&i| keys[i].clone()).collect();
            let vals = engine.multi_get(&sub)?;
            for (i, val) in idxs.into_iter().zip(vals) {
                out[i] = val;
            }
        }
        Ok(out)
    }

    pub fn delete(&self, key: Key) -> Result<()> {
        self.engine_for_key(&key).delete(key)
    }

    pub fn delete_with_deindex(&self, key: Key, value: &[u8]) -> Result<()> {
        self.engine_for_key(&key).delete_with_deindex(key, value)
    }

    /// Prefix scan, merged in key order across engines when the prefix is
    /// ambiguous.
    pub fn scan_prefix(&self, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let engines = self.engines_for_prefix(prefix);
        let single = engines.len() == 1;
        let mut merged = Vec::new();
        for engine in engines {
            merged.extend(engine.scan_prefix(prefix)?);
        }
        if !single {
            merged.sort_by(|a, b| a.0.cmp(&b.0));
        }
        Ok(merged)
    }

    pub fn scan_prefix_limit(
        &self,
        prefix: &[u8],
        limit: usize,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let engines = self.engines_for_prefix(prefix);
        let single = engines.len() == 1;
        let mut merged = Vec::new();
        for engine in engines {
            merged.extend(engine.scan_prefix_limit(prefix, limit)?);
        }
        if !single {
            merged.sort_by(|a, b| a.0.cmp(&b.0));
        }
        merged.truncate(limit);
        Ok(merged)
    }

    pub fn scan_prefix_after(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let engines = self.engines_for_prefix(prefix);
        let single = engines.len() == 1;
        let mut merged = Vec::new();
        for engine in engines {
            merged.extend(engine.scan_prefix_after(prefix, after, limit)?);
        }
        if !single {
            merged.sort_by(|a, b| a.0.cmp(&b.0));
        }
        merged.truncate(limit);
        Ok(merged)
    }

    /// Counts live/tombstone entries under `prefix` across all engines that
    /// may hold it.
    pub fn count_prefix(&self, prefix: &[u8]) -> Result<(usize, usize)> {
        let mut live = 0usize;
        let mut tombs = 0usize;
        for engine in self.engines_for_prefix(prefix) {
            let (l, t) = engine.count_prefix(prefix)?;
            live += l;
            tombs += t;
        }
        Ok((live, tombs))
    }

    /// Iterates live entries under `prefix` in engine order (default first,
    /// then namespaces by name). Entries are key-ordered within one engine.
    pub fn for_each_live_prefix<F>(
        &self,
        prefix: &[u8],
        vis: Option<&Visibility>,
        overlay: Option<&HashMap<Vec<u8>, Option<Vec<u8>>>>,
        mut f: F,
    ) -> Result<()>
    where
        F: FnMut(&[u8], &[u8]) -> Result<()>,
    {
        for engine in self.engines_for_prefix(prefix) {
            engine.for_each_live_prefix(prefix, vis, overlay, &mut f)?;
        }
        Ok(())
    }

    /// Like [`Self::for_each_live_prefix`] but the callback returns
    /// `Ok(false)` to stop early across all engines.
    pub fn for_each_live_prefix_until<F>(
        &self,
        prefix: &[u8],
        vis: Option<&Visibility>,
        overlay: Option<&HashMap<Vec<u8>, Option<Vec<u8>>>>,
        mut f: F,
    ) -> Result<()>
    where
        F: FnMut(&[u8], &[u8]) -> Result<bool>,
    {
        for engine in self.engines_for_prefix(prefix) {
            let mut stop = false;
            engine.for_each_live_prefix_until(prefix, vis, overlay, |k, v| {
                let cont = f(k, v)?;
                stop = !cont;
                Ok(cont)
            })?;
            if stop {
                break;
            }
        }
        Ok(())
    }

    /// Flushes every engine's memtable to disk.
    pub fn flush_all(&self) -> Result<()> {
        for engine in self.all_engines() {
            engine.flush()?;
        }
        Ok(())
    }

    /// Shuts every engine down.
    pub fn shutdown_all(&self) {
        for engine in self.all_engines() {
            engine.shutdown();
        }
    }

    /// Runs one compaction round on every engine.
    pub fn flush_compaction_all(&self) -> Result<()> {
        for engine in self.all_engines() {
            engine.flush_compaction()?;
        }
        Ok(())
    }

    /// Data directory of the default engine (the set root).
    pub fn data_dir(&self) -> std::path::PathBuf {
        self.default.data_dir()
    }

    /// Aggregates engine statistics across the set.
    pub fn stats(&self) -> EngineStats {
        let mut total = EngineStats {
            memtable_size: 0,
            memtable_entries: 0,
            num_levels: 0,
            total_sstables: 0,
            total_sst_size: 0,
        };
        for engine in self.all_engines() {
            let s = engine.stats();
            total.memtable_size += s.memtable_size;
            total.memtable_entries += s.memtable_entries;
            total.num_levels = total.num_levels.max(s.num_levels);
            total.total_sstables += s.total_sstables;
            total.total_sst_size += s.total_sst_size;
        }
        total
    }

    /// Recovery scan (all Put entries regardless of tombstones), merged in key
    /// order across engines.
    pub fn scan_prefix_recovery(&self, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let engines = self.engines_for_prefix(prefix);
        let single = engines.len() == 1;
        let mut merged = Vec::new();
        for engine in engines {
            merged.extend(engine.scan_prefix_recovery(prefix)?);
        }
        if !single {
            merged.sort_by(|a, b| a.0.cmp(&b.0));
        }
        Ok(merged)
    }

    // ── Class-scoped forwarding (routes to the class's engine) ─────────────

    pub fn has_index(&self, class: &str, column: &str) -> bool {
        self.engine_for_class(class).has_index(class, column)
    }

    pub fn create_index(&self, class: &str, column: &str) -> Result<()> {
        self.engine_for_class(class).create_index(class, column)
    }

    pub fn drop_index(&self, class: &str, column: &str) -> bool {
        self.engine_for_class(class).drop_index(class, column)
    }

    pub fn get_value_meta(&self, class: &str, pk: &str) -> Result<Option<ValueMetadata>> {
        self.engine_for_class(class).get_value_meta(class, pk)
    }

    pub fn put_value_meta(&self, class: &str, pk: &str, meta: &ValueMetadata) -> Result<()> {
        self.engine_for_class(class).put_value_meta(class, pk, meta)
    }

    pub fn get_value_score(&self, class: &str, pk: &str) -> Result<f64> {
        self.engine_for_class(class).get_value_score(class, pk)
    }

    pub fn activate(&self, class: &str, pk: &str, delta: f64, reason: &str) -> Result<()> {
        self.engine_for_class(class).activate(class, pk, delta, reason)
    }

    pub fn has_vector_index(&self, class: &str, column: &str) -> bool {
        self.engine_for_class(class).has_vector_index(class, column)
    }

    pub fn vector_index_len(&self, class: &str, column: &str) -> Option<usize> {
        self.engine_for_class(class).vector_index_len(class, column)
    }

    pub fn list_indexes(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for engine in self.all_engines() {
            out.extend(engine.list_indexes());
        }
        out
    }

    // ── Transactions ───────────────────────────────────────────────────────

    /// Allocates a router transaction handle. The engine binding is created
    /// lazily at the first write (see module docs).
    pub fn begin_txn(&self) -> SeqNo {
        let handle = self.next_txn.fetch_add(1, Ordering::Relaxed);
        self.txns
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(handle, TxnBinding::Unbound);
        handle
    }

    /// Returns the engine and engine-local sequence of a bound transaction.
    pub fn txn_binding(&self, txn: SeqNo) -> Option<(Arc<LsmEngine>, SeqNo)> {
        match self
            .txns
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&txn)
        {
            Some(TxnBinding::Bound { engine, engine_seq }) => {
                Some((Arc::clone(engine), *engine_seq))
            }
            _ => None,
        }
    }

    /// Binds a transaction to the engine that owns `class`, or verifies an
    /// existing binding. Returns the engine and its engine-local sequence for
    /// call sites that need to hand `(&LsmEngine, SeqNo)` to helpers.
    pub fn txn_bind_class(&self, txn: SeqNo, class: &str) -> Result<(Arc<LsmEngine>, SeqNo)> {
        self.bind_txn(txn, self.engine_for_class(class))
    }

    /// [`Self::txn_bind_class`] keyed by a storage key.
    pub fn txn_bind_key(&self, txn: SeqNo, key: &[u8]) -> Result<(Arc<LsmEngine>, SeqNo)> {
        self.bind_txn(txn, self.engine_for_key(key))
    }

    fn bind_txn(&self, txn: SeqNo, engine: Arc<LsmEngine>) -> Result<(Arc<LsmEngine>, SeqNo)> {
        let mut txns = self.txns.lock().unwrap_or_else(|e| e.into_inner());
        match txns.get(&txn) {
            Some(TxnBinding::Bound { engine: bound, engine_seq }) => {
                if Arc::ptr_eq(bound, &engine) {
                    Ok((Arc::clone(bound), *engine_seq))
                } else {
                    Err(cross_ns_error(txn))
                }
            }
            Some(TxnBinding::Unbound) => {
                let engine_seq = engine.begin_txn();
                txns.insert(
                    txn,
                    TxnBinding::Bound {
                        engine: Arc::clone(&engine),
                        engine_seq,
                    },
                );
                Ok((engine, engine_seq))
            }
            None => Err(CoreError::custom(format!("unknown transaction {txn}"))),
        }
    }

    /// Routes and forwards `txn_put`, binding the transaction on first write.
    pub fn txn_put(&self, txn: SeqNo, key: Key, value: Value) -> Result<()> {
        let (engine, engine_seq) = self.txn_bind_key(txn, &key)?;
        engine.txn_put(engine_seq, key, value)
    }

    /// Routes and forwards a batch write. All entries must land on the same
    /// engine (cross-namespace transactions are not supported).
    pub fn txn_put_batch(&self, txn: SeqNo, entries: Vec<(Key, Value)>) -> Result<usize> {
        let first = entries
            .first()
            .ok_or_else(|| CoreError::custom("txn_put_batch: empty batch"))?;
        let (engine, engine_seq) = self.txn_bind_key(txn, &first.0)?;
        for (key, _) in &entries[1..] {
            if !Arc::ptr_eq(&engine, &self.engine_for_key(key)) {
                return Err(cross_ns_error(txn));
            }
        }
        engine.txn_put_batch(engine_seq, entries)
    }

    /// Routes and forwards `txn_delete`, binding the transaction on first write.
    pub fn txn_delete(&self, txn: SeqNo, key: Key) -> Result<()> {
        let (engine, engine_seq) = self.txn_bind_key(txn, &key)?;
        engine.txn_delete(engine_seq, key)
    }

    /// Reads through the transaction when it is bound to the key's engine
    /// (read-your-own-writes); otherwise reads committed state. Reads never
    /// bind the transaction.
    pub fn txn_get(&self, txn: SeqNo, key: &[u8]) -> Result<Option<Value>> {
        let engine = self.engine_for_key(key);
        if let Some((bound, engine_seq)) = self.txn_binding(txn) {
            if Arc::ptr_eq(&bound, &engine) {
                return engine.txn_get(engine_seq, key);
            }
        }
        engine.get(key)
    }

    /// Transaction-aware prefix scan, merged in key order across engines.
    /// The bound engine contributes its transaction view; other engines
    /// contribute committed state (cross-namespace writes cannot exist).
    pub fn txn_scan_prefix(
        &self,
        txn: SeqNo,
        prefix: &[u8],
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let binding = self.txn_binding(txn);
        let engines = self.engines_for_prefix(prefix);
        let single = engines.len() == 1;
        let mut merged = Vec::new();
        for engine in engines {
            let entries = match &binding {
                Some((bound, engine_seq)) if Arc::ptr_eq(bound, &engine) => {
                    engine.txn_scan_prefix(*engine_seq, prefix)?
                }
                _ => engine.scan_prefix(prefix)?,
            };
            merged.extend(entries);
        }
        if !single {
            merged.sort_by(|a, b| a.0.cmp(&b.0));
        }
        Ok(merged)
    }

    /// Transaction-aware prefix iteration. The bound engine contributes its
    /// transaction view; other engines iterate committed state.
    pub fn txn_for_each_live_prefix<F>(
        &self,
        txn: SeqNo,
        prefix: &[u8],
        mut f: F,
    ) -> Result<()>
    where
        F: FnMut(&[u8], &[u8]) -> Result<()>,
    {
        let binding = self.txn_binding(txn);
        for engine in self.engines_for_prefix(prefix) {
            match &binding {
                Some((bound, engine_seq)) if Arc::ptr_eq(bound, &engine) => {
                    engine.txn_for_each_live_prefix(*engine_seq, prefix, &mut f)?;
                }
                _ => {
                    engine.for_each_live_prefix(prefix, None, None, &mut f)?;
                }
            }
        }
        Ok(())
    }

    /// Commits a bound transaction. An unbound transaction (no writes) is a
    /// no-op; an unknown handle is an error.
    pub fn commit_txn(&self, txn: SeqNo) -> Result<()> {
        match self
            .txns
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&txn)
        {
            Some(TxnBinding::Bound { engine, engine_seq }) => engine.commit_txn(engine_seq),
            Some(TxnBinding::Unbound) => Ok(()),
            None => Err(CoreError::custom(format!("unknown transaction {txn}"))),
        }
    }

    /// Aborts a bound transaction. An unbound transaction is a no-op; an
    /// unknown handle is an error.
    pub fn abort_txn(&self, txn: SeqNo) -> Result<()> {
        match self
            .txns
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&txn)
        {
            Some(TxnBinding::Bound { engine, engine_seq }) => engine.abort_txn(engine_seq),
            Some(TxnBinding::Unbound) => Ok(()),
            None => Err(CoreError::custom(format!("unknown transaction {txn}"))),
        }
    }
}

/// Builds per-namespace [`StorageOptions`] from a base: same tuning, but
/// `data_dir` (and `wal_archive_dir`, when set) moves under `{ns}/`.
fn namespace_options(base: &StorageOptions, ns: &str) -> Result<StorageOptions> {
    validate_namespace(ns)?;
    let mut opts = base.clone();
    opts.data_dir = base.data_dir.join(ns);
    if let Some(archive) = base.wal_archive_dir.clone() {
        opts.wal_archive_dir = Some(archive.join(ns));
    }
    Ok(opts)
}

/// Rejects namespace names that could escape the data directory.
fn validate_namespace(ns: &str) -> Result<()> {
    let bad = ns.is_empty()
        || ns == "."
        || ns == ".."
        || ns.contains('/')
        || ns.contains('\\')
        || ns.contains('\0');
    if bad {
        return Err(CoreError::InvalidArgument(format!(
            "invalid namespace name: {ns:?}"
        )));
    }
    Ok(())
}

fn cross_ns_error(txn: SeqNo) -> CoreError {
    CoreError::custom(format!(
        "cross-namespace transaction {txn}: all writes must target one namespace"
    ))
}

fn is_global_key(key: &[u8]) -> bool {
    GLOBAL_PREFIXES.iter().any(|m| key.starts_with(m))
}

/// Splits `hay` at the first occurrence of `delim`.
fn split_first<'a>(hay: &'a [u8], delim: &[u8]) -> Option<(&'a [u8], &'a [u8])> {
    let pos = hay.windows(delim.len()).position(|w| w == delim)?;
    Some((&hay[..pos], &hay[pos + delim.len()..]))
}

/// Namespace head of a class region: the bytes before the first dot.
fn bytes_namespace(region: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(region);
    text.split_once('.')
        .map(|(ns, _)| ns.to_string())
        .filter(|ns| !ns.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_set() -> (tempfile::TempDir, EngineSet) {
        let root = tempfile::tempdir().expect("tempdir");
        let mut opts = StorageOptions::default();
        opts.data_dir = root.path().to_path_buf();
        let set = EngineSet::open_with_namespaces(opts, &["sembio", "sempet"]).expect("open set");
        (root, set)
    }

    #[test]
    fn class_routing() {
        let (_root, set) = test_set();
        assert!(Arc::ptr_eq(
            &set.engine_for_class("sembio.Compound"),
            &set.namespace_engine("sembio").unwrap()
        ));
        assert!(Arc::ptr_eq(
            &set.engine_for_class("sempet.Drug"),
            &set.namespace_engine("sempet").unwrap()
        ));
        // Bare and unknown-namespace classes fall back to the default engine.
        assert!(Arc::ptr_eq(&set.engine_for_class("Product"), &set.default_engine()));
        assert!(Arc::ptr_eq(
            &set.engine_for_class("vet.PetDrug"),
            &set.default_engine()
        ));
    }

    #[test]
    fn key_namespace_extraction() {
        // Document keys.
        assert_eq!(
            EngineSet::key_namespace(b"sempet.Drug::001").as_deref(),
            Some("sempet")
        );
        assert_eq!(EngineSet::key_namespace(b"Product::001"), None);
        // Value metadata carries its class.
        assert_eq!(
            EngineSet::key_namespace(b"__val_meta__::sempet.Drug::001").as_deref(),
            Some("sempet")
        );
        assert_eq!(
            EngineSet::key_namespace(b"__val_meta__::Product::001"),
            None
        );
        // Index metadata: class starts the region.
        assert_eq!(
            EngineSet::key_namespace(b"__idx_meta__sempet.Drug__price").as_deref(),
            Some("sempet")
        );
        assert_eq!(EngineSet::key_namespace(b"__idx_meta__Product__price"), None);
        // Global system keys.
        assert_eq!(EngineSet::key_namespace(b"__ontology__sembio::Drug"), None);
        assert_eq!(EngineSet::key_namespace(b"__ns__sembio"), None);
        assert_eq!(EngineSet::key_namespace(b"__graph_v__v1"), None);
        assert_eq!(EngineSet::key_namespace(b"__mv_view::row"), None);
    }

    #[test]
    fn index_key_routes_by_class() {
        let (_root, set) = test_set();
        let key = IndexManager::make_index_key("sempet.Drug", "price", b"10", b"p1");
        assert!(Arc::ptr_eq(&set.engine_for_key(&key), &set.namespace_engine("sempet").unwrap()));
        let key = IndexManager::make_index_key("Product", "price", b"10", b"p1");
        assert!(Arc::ptr_eq(&set.engine_for_key(&key), &set.default_engine()));
    }

    #[test]
    fn prefix_routing() {
        let (_root, set) = test_set();
        // Single-namespace prefixes.
        let e = set.engines_for_prefix(b"sempet.Drug::");
        assert_eq!(e.len(), 1);
        assert!(Arc::ptr_eq(&e[0], &set.namespace_engine("sempet").unwrap()));
        let e = set.engines_for_prefix(b"sempet.");
        assert_eq!(e.len(), 1);
        assert!(Arc::ptr_eq(&e[0], &set.namespace_engine("sempet").unwrap()));
        // Global system prefixes stay in the default engine.
        let e = set.engines_for_prefix(b"__ontology__");
        assert_eq!(e.len(), 1);
        assert!(Arc::ptr_eq(&e[0], &set.default_engine()));
        // Bare-class document prefix.
        let e = set.engines_for_prefix(b"Product::");
        assert_eq!(e.len(), 1);
        assert!(Arc::ptr_eq(&e[0], &set.default_engine()));
        // Ambiguous prefixes fan out.
        assert_eq!(set.engines_for_prefix(b"sem").len(), 3);
        assert_eq!(set.engines_for_prefix(b"__idx2__").len(), 3);
        assert_eq!(set.engines_for_prefix(b"").len(), 3);
    }

    #[test]
    fn routed_put_get_scan() {
        let (_root, set) = test_set();
        set.put(b"sempet.Drug::001".to_vec(), b"alpha".to_vec()).unwrap();
        set.put(b"Product::001".to_vec(), b"beta".to_vec()).unwrap();
        set.put(b"sembio.Compound::001".to_vec(), b"gamma".to_vec()).unwrap();

        assert_eq!(set.get(b"sempet.Drug::001").unwrap().unwrap(), b"alpha");
        assert_eq!(set.get(b"Product::001").unwrap().unwrap(), b"beta");
        assert_eq!(set.get(b"sembio.Compound::001").unwrap().unwrap(), b"gamma");
        assert!(set.get(b"sempet.Drug::404").unwrap().is_none());

        // Class-scoped scan hits one engine; ambiguous scan merges all.
        let rows = set.scan_prefix(b"sempet.Drug::").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, b"sempet.Drug::001");
        let rows = set.scan_prefix(b"").unwrap();
        assert_eq!(rows.len(), 3);
        let keys: Vec<&[u8]> = rows.iter().map(|(k, _)| k.as_slice()).collect();
        assert_eq!(
            keys,
            vec![
                &b"Product::001"[..],
                &b"sembio.Compound::001"[..],
                &b"sempet.Drug::001"[..],
            ]
        );

        assert_eq!(set.count_prefix(b"").unwrap(), (3, 0));
        assert_eq!(set.count_prefix(b"sempet.Drug::").unwrap(), (1, 0));
    }

    #[test]
    fn txn_write_binds_and_commits() {
        let (_root, set) = test_set();
        let txn = set.begin_txn();
        assert!(set.txn_binding(txn).is_none());
        set.txn_put(txn, b"sempet.Drug::001".to_vec(), b"v1".to_vec())
            .unwrap();
        let (engine, _) = set.txn_binding(txn).unwrap();
        assert!(Arc::ptr_eq(&engine, &set.namespace_engine("sempet").unwrap()));

        // Read-your-own-writes on the bound engine.
        assert_eq!(set.txn_get(txn, b"sempet.Drug::001").unwrap().unwrap(), b"v1");
        // Not visible outside the transaction before commit.
        assert!(set.get(b"sempet.Drug::001").unwrap().is_none());

        set.commit_txn(txn).unwrap();
        assert_eq!(set.get(b"sempet.Drug::001").unwrap().unwrap(), b"v1");
    }

    #[test]
    fn txn_cross_namespace_write_rejected() {
        let (_root, set) = test_set();
        let txn = set.begin_txn();
        set.txn_put(txn, b"sempet.Drug::001".to_vec(), b"v".to_vec())
            .unwrap();
        let err = set
            .txn_put(txn, b"sembio.Compound::001".to_vec(), b"v".to_vec())
            .unwrap_err();
        assert!(err.to_string().contains("cross-namespace"), "{err}");
        set.abort_txn(txn).unwrap();
    }

    #[test]
    fn txn_cross_namespace_read_falls_back() {
        let (_root, set) = test_set();
        set.put(b"sembio.Compound::001".to_vec(), b"committed".to_vec())
            .unwrap();
        let txn = set.begin_txn();
        set.txn_put(txn, b"sempet.Drug::001".to_vec(), b"pending".to_vec())
            .unwrap();
        // Cross-namespace read sees committed state, not the pending write.
        assert_eq!(
            set.txn_get(txn, b"sembio.Compound::001").unwrap().unwrap(),
            b"committed"
        );
        // Same-namespace read sees the pending write.
        assert_eq!(
            set.txn_get(txn, b"sempet.Drug::001").unwrap().unwrap(),
            b"pending"
        );
        set.abort_txn(txn).unwrap();
    }

    #[test]
    fn txn_unbound_commit_is_noop() {
        let (_root, set) = test_set();
        let txn = set.begin_txn();
        set.commit_txn(txn).unwrap();
        let txn = set.begin_txn();
        set.abort_txn(txn).unwrap();
    }

    #[test]
    fn multi_get_preserves_order() {
        let (_root, set) = test_set();
        set.put(b"sempet.Drug::001".to_vec(), b"a".to_vec()).unwrap();
        set.put(b"Product::001".to_vec(), b"b".to_vec()).unwrap();
        let keys = vec![b"Product::001".to_vec(), b"sempet.Drug::001".to_vec()];
        let vals = set.multi_get(&keys).unwrap();
        assert_eq!(vals[0].as_deref(), Some(&b"b"[..]));
        assert_eq!(vals[1].as_deref(), Some(&b"a"[..]));
    }

    #[test]
    fn namespace_name_validation() {
        let (_root, set) = test_set();
        let engine = set.default_engine();
        assert!(set.add_namespace("", engine.clone()).is_err());
        assert!(set.add_namespace("..", engine.clone()).is_err());
        assert!(set.add_namespace("a/b", engine.clone()).is_err());
        assert!(set.add_namespace("sempet", engine).is_err()); // duplicate
    }

    #[test]
    fn discovery_ignores_non_engine_dirs() {
        let root = tempfile::tempdir().expect("tempdir");
        let mut opts = StorageOptions::default();
        opts.data_dir = root.path().to_path_buf();
        // A namespace engine from a previous run…
        std::fs::create_dir_all(root.path().join("sempet")).unwrap();
        std::fs::write(root.path().join("sempet").join("wal.log"), b"").unwrap();
        // …and bookkeeping directories that must not become engines.
        std::fs::create_dir_all(root.path().join("indexes")).unwrap();
        std::fs::create_dir_all(root.path().join("empty")).unwrap();
        let set = EngineSet::open_with_discovery(opts).expect("open set");
        assert_eq!(set.namespaces(), vec!["sempet".to_string()]);
    }
}
