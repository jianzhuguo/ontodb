// Copyright (c) 2024-2026 OntoDB Team
// Licensed under the Business Source License 1.1 (BUSL-1.1).
// See LICENSE for details. Change Date: 2031-09-15.
// On the Change Date, this file will be licensed under Apache License 2.0.
//! Background compaction worker.
//!
//! Runs compaction in a separate thread to avoid blocking the write path.
//! The main engine sends flush notifications; the worker decides when and
//! how to compact, performs the I/O-heavy merge, and reports results back.
//!
//! Design:
//! - `levels` metadata is shared via `Arc<Mutex<Vec<Vec<SsTableInfo>>>>`
//! - The worker holds a reference to the shared levels
//! - The worker owns its own SSTable handles for reading during compaction
//! - The main engine adds flush results to levels[0] under the lock
//! - The worker acquires the lock briefly to read metadata and write results

use crate::lsm::sstable::{SsTable, SsTableBuilder};
use crate::options::StorageOptions;
use onto_core::{Entry, EntryKind, Result, SeqNo};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::sync::{mpsc, Arc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// Metadata about an SSTable file, kept in memory.
#[derive(Clone, Debug)]
pub struct SsTableInfo {
    /// File path.
    pub path: PathBuf,
    /// Approximate size in bytes.
    pub size: u64,
    /// Min key in this SSTable.
    pub min_key: Vec<u8>,
    /// Max key in this SSTable.
    pub max_key: Vec<u8>,
}

/// Messages sent from the engine to the compaction worker.
pub enum CompactionMsg {
    /// A new SSTable was flushed to the given level.
    Flushed { level: usize },
    /// Perform all pending compaction and signal completion.
    FlushAndNotify,
    /// Shut down the worker.
    Shutdown,
}

/// Notifications sent from the compaction worker back to the engine.
pub enum CompactionNotification {
    /// Compaction completed. The engine should evict these SSTable paths from its cache.
    Compacted { evicted_paths: Vec<PathBuf> },
    /// All pending compaction work is done (response to FlushAndNotify).
    FlushDone,
}

/// Background compaction worker.
pub struct CompactionWorker {
    /// Shared level metadata (same data the main engine uses for reads).
    levels: Arc<Mutex<Vec<Vec<SsTableInfo>>>>,
    /// Engine configuration.
    options: StorageOptions,
    /// SSTable file ID counter (shared with engine).
    sst_counter: Arc<AtomicU64>,
    /// Channel receiver for messages from the engine.
    receiver: mpsc::Receiver<CompactionMsg>,
    /// Channel sender for notifications back to the engine.
    notif_sender: Option<mpsc::Sender<CompactionNotification>>,
    /// Cache of opened SSTable handles for reading during compaction.
    sst_cache: HashMap<PathBuf, SsTable>,
    /// Files pending deletion (deferred until next compaction cycle).
    pending_deletions: Vec<PathBuf>,
    /// Skip compaction until this instant (backoff after merge timeout).
    cooldown_until: Option<std::time::Instant>,
}

impl CompactionWorker {
    /// Spawns the compaction worker in a background thread.
    ///
    /// Returns:
    /// - The shared levels (Arc<Mutex>) for the engine to use
    /// - A sender to communicate with the worker
    /// - A notification receiver for compaction results (cache invalidation)
    /// - The join handle for the worker thread
    pub fn spawn(
        options: StorageOptions,
        initial_levels: Vec<Vec<SsTableInfo>>,
        sst_counter: Arc<AtomicU64>,
    ) -> (
        Arc<Mutex<Vec<Vec<SsTableInfo>>>>,
        mpsc::Sender<CompactionMsg>,
        mpsc::Receiver<CompactionNotification>,
        JoinHandle<()>,
    ) {
        let levels = Arc::new(Mutex::new(initial_levels));
        let levels_clone = levels.clone();
        let (sender, receiver) = mpsc::channel();
        let (notif_sender, notif_receiver) = mpsc::channel();

        let handle = thread::Builder::new()
            .name("compaction-worker".into())
            .stack_size(32 * 1024 * 1024) // 32MB stack for large sort operations
            .spawn(move || {
            let mut worker = Self {
                levels: levels_clone,
                options,
                sst_counter,
                receiver,
                notif_sender: Some(notif_sender),
                sst_cache: HashMap::new(),
                pending_deletions: Vec::new(),
                cooldown_until: None,
            };
            worker.run();
        }).expect("failed to spawn compaction worker thread");

        (levels, sender, notif_receiver, handle)
    }

    /// Main loop: waits for messages and performs compaction when needed.
    fn run(&mut self) {
        // Periodic compaction check: even without flush messages, check every
        // 60s if any level needs compaction. This is essential when the
        // memtable is large (256MB+) — flushes are rare, and without a
        // periodic trigger the SST backlog never gets compacted.
        let mut idle_ticks: u64 = 0;
        let periodic_interval: u64 = 600; // 600 × 100ms = 60s
        loop {
            match self.receiver.recv_timeout(Duration::from_millis(100)) {
                Ok(CompactionMsg::Flushed { level: _ }) => {
                    idle_ticks = 0;
                    if let Err(e) = self.try_compact() {
                        tracing::error!("Background compaction failed: {}", e);
                    }
                }
                Ok(CompactionMsg::FlushAndNotify) => {
                    idle_ticks = 0;
                    // Perform all pending compaction (includes small file merge)
                    if let Err(e) = self.try_compact() {
                        tracing::error!("Background compaction failed: {}", e);
                    }
                    // Keep compacting until no more is needed
                    loop {
                        let (score, excessive) = {
                            let levels = self.levels.lock().unwrap_or_else(|e| e.into_inner());
                            if levels.len() < 2 {
                                break;
                            }
                            let mut best = 0.0f64;
                            let mut excessive = false;
                            for (_i, level) in levels.iter().enumerate().skip(1) {
                                if level.len() > 1000 {
                                    excessive = true;
                                }
                            }
                            for level in 0..levels.len() - 1 {
                                let s = self.compaction_score(&levels, level);
                                if s > best {
                                    best = s;
                                }
                            }
                            (best, excessive)
                        };
                        // If levels have too many small files, let merge_small_files
                        // handle it across multiple ticks instead of spinning here
                        if excessive {
                            break;
                        }
                        if score <= 1.0 {
                            break;
                        }
                        if let Err(e) = self.try_compact() {
                            tracing::error!("Background compaction failed: {}", e);
                            break;
                        }
                    }
                    // Signal completion
                    if let Some(ref sender) = self.notif_sender {
                        let _ = sender.send(CompactionNotification::FlushDone);
                    }
                }
                Ok(CompactionMsg::Shutdown) => {
                    tracing::info!("Compaction worker shutting down");
                    break;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    idle_ticks += 1;
                    if idle_ticks >= periodic_interval {
                        idle_ticks = 0;
                        if let Err(e) = self.try_compact() {
                            tracing::error!("Background compaction failed: {}", e);
                        }
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    break;
                }
            }
        }
    }

    /// Tells the engine to drop `sst_cache` entries for paths this worker
    /// deleted. Without this the engine keeps stale `Arc<SsTable>` handles
    /// for files that are already gone from disk and from `levels`.
    fn notify_evicted(&self, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        if let Some(ref sender) = self.notif_sender {
            let _ = sender.send(CompactionNotification::Compacted { evicted_paths: paths });
        }
    }

    /// Checks all levels and performs compaction on the most urgent one.
    fn try_compact(&mut self) -> Result<()> {
        // Respect cooldown after a timed-out merge — prevents immediately
        // re-attempting the same oversized compaction.
        if let Some(until) = self.cooldown_until {
            if std::time::Instant::now() < until {
                return Ok(());
            }
            self.cooldown_until = None;
        }

        // Delete files from previous compaction cycles (deferred to avoid race with engine)
        for path in self.pending_deletions.drain(..) {
            self.sst_cache.remove(&path);
            let _ = fs::remove_file(&path);
        }

        // Run small file merge FIRST — this reduces file count before normal
        // compaction, preventing OOM when normal compaction tries to open
        // thousands of tiny overlapping SSTables.
        if let Err(e) = self.merge_small_files() {
            tracing::warn!("Small file merge failed (non-fatal): {}", e);
        }

        // Check if any level has too many files — if so, skip normal compaction
        // and let merge_small_files reduce the count over multiple ticks.
        const MAX_FILES_PER_LEVEL: usize = 1000;
        let excessive_files = {
            let levels = self.levels.lock().unwrap_or_else(|e| e.into_inner());
            levels.iter().enumerate().any(|(i, l)| i > 0 && l.len() > MAX_FILES_PER_LEVEL)
        };

        if excessive_files {
            // Skip normal compaction; merge_small_files will handle it
            return Ok(());
        }

        // ── File-count hard trigger (B) ──────────────────────────────────
        // Size-score alone can sit just under 1.0 forever (we observed L2 at
        // 0.992), and the deepest level is never a size-score source. When any
        // level exceeds FILE_COUNT_TRIGGER files, drain it with a bounded
        // batch regardless of score. This is what unblocks L6.
        const FILE_COUNT_TRIGGER: usize = 50;
        let crowded: Option<(usize, usize)> = {
            let levels = self.levels.lock().unwrap_or_else(|e| e.into_inner());
            levels
                .iter()
                .enumerate()
                .map(|(i, l)| (i, l.len()))
                .filter(|(_, n)| *n > FILE_COUNT_TRIGGER)
                .max_by_key(|(_, n)| *n)
        };
        if let Some((level, nfiles)) = crowded {
            let deepest = {
                let levels = self.levels.lock().unwrap_or_else(|e| e.into_inner());
                levels.len().saturating_sub(1)
            };
            if level == deepest {
                // Deepest level: bounded self-merge. Quiet when no size-safe
                // pair exists (otherwise the trigger logs every tick).
                self.merge_last_level_batch()?;
            } else if level == 0 {
                // L0: collapse in place first. L0 files mutually overlap
                // (memtable flushes), so L0→L1 tombstone-placement safety
                // defers almost every batch and the level never drains.
                // Self-merge cuts file count without sinking tombstones;
                // once L0 is small, the shadowing set fits under EXPAND_CAP
                // and normal L0→L1 can proceed.
                tracing::info!(
                    "File-count trigger: L0 has {} files (> {}), self-merging batch",
                    nfiles,
                    FILE_COUNT_TRIGGER
                );
                self.merge_l0_self_batch()?;
                self.compact_level(0)?;
            } else {
                tracing::info!(
                    "File-count trigger: L{} has {} files (> {}), merging batch",
                    level,
                    nfiles,
                    FILE_COUNT_TRIGGER
                );
                self.compact_level(level)?;
            }
            return Ok(());
        }

        // Find the level with the highest compaction score
        let (best_level, best_score) = {
            let levels = self.levels.lock().unwrap_or_else(|e| e.into_inner());
            let mut best_level = None;
            let mut best_score = 0.0f64;

            for level in 0..levels.len() - 1 {
                let score = self.compaction_score(&levels, level);
                if score > best_score {
                    best_score = score;
                    best_level = Some(level);
                }
            }

            (best_level, best_score)
        };

        if let Some(level) = best_level {
            if best_score > 1.0 {
                tracing::info!(
                    "Background compaction triggered: L{:.2} score={:.2}",
                    level,
                    best_score
                );
                self.compact_level(level)?;
            }
        }

        // Deepest-level file count is handled by the file-count trigger above
        // (bounded self-merge batch). Full-level self-merge stays disabled:
        // rewriting ~24M entries in one shot OOMs and never reduced file count.

        Ok(())
    }

    /// Computes compaction urgency score for a level.
    fn compaction_score(&self, levels: &[Vec<SsTableInfo>], level: usize) -> f64 {
        let size: u64 = levels[level].iter().map(|s| s.size).sum();
        let target = self.target_level_size(level);
        if target == 0 {
            return 0.0;
        }
        let size_score = size as f64 / target as f64;
        // Penalize levels with too many files — push compaction to drain them faster.
        // Each file above 100 adds 0.01 to the score.
        let file_count = levels[level].len();
        let file_penalty = if file_count > 100 {
            (file_count - 100) as f64 * 0.01
        } else {
            0.0
        };
        let score = size_score + file_penalty;
        // L0 must drain into L1 even when total size sits under target —
        // many small overlapping L0 SSTs multiply read amp without ever
        // tripping the size score. Floor the score so the L0→L1 path runs
        // once the file-count trigger has thinned the level out.
        if level == 0 && file_count > 16 {
            score.max(1.1)
        } else {
            score
        }
    }

    /// Returns the target size for a level.
    fn target_level_size(&self, level: usize) -> u64 {
        if level == 0 {
            (self.options.memtable_size_limit as u64) * 4
        } else {
            let l1_base =
                (self.options.memtable_size_limit as u64) * (self.options.size_ratio as u64);
            let mut target = l1_base;
            for _ in 1..level {
                target *= self.options.size_ratio as u64;
            }
            target
        }
    }

    /// Performs leveled compaction: merges SSTables from level N into level N+1.
    /// Span (min,max) of Delete-entry keys in one SST, or None when the file
    /// holds no tombstones. Used for tombstone placement safety.
    fn tombstone_span(path: &Path) -> Option<(Vec<u8>, Vec<u8>)> {
        let mut iter = crate::lsm::streaming_merge::StreamingMergeIterator::new(&[path]).ok()?;
        let mut span: Option<(Vec<u8>, Vec<u8>)> = None;
        while iter.has_next() {
            let entry = match iter.next() {
                Some(e) => e,
                None => break,
            };
            if entry.kind == EntryKind::Delete {
                match &mut span {
                    None => span = Some((entry.key.clone(), entry.key.clone())),
                    Some((lo, hi)) => {
                        if entry.key < *lo {
                            *lo = entry.key.clone();
                        }
                        if entry.key > *hi {
                            *hi = entry.key.clone();
                        }
                    }
                }
            }
        }
        span
    }

    fn compact_level(&mut self, level: usize) -> Result<()> {
        // Step 1: Snapshot SSTables to compact (don't remove from levels yet)
        let (mut ssts_to_compact, compact_min, compact_max, empty_ssts) = {
            let mut levels = self.levels.lock().unwrap_or_else(|e| e.into_inner());
            if level >= levels.len() - 1 {
                return Ok(());
            }

            let ssts: Vec<SsTableInfo> = if level == 0 {
                // L0 files can be large (memtable-sized); take a wider batch so
                // the level drains instead of trickling 2 files at a time.
                let count = (levels[0].len() / 2).max(4).min(levels[0].len()).min(8);
                levels[0].iter().take(count).cloned().collect()
            } else {
                if levels[level].is_empty() {
                    return Ok(());
                }
                // Cap source SSTables to reduce memory pressure
                let file_count = levels[level].len();
                let excess = file_count.saturating_sub(100);
                let batch = if excess > 0 {
                    excess.min(4).max(1)
                } else {
                    1
                };
                let mut sorted: Vec<SsTableInfo> = levels[level].clone();
                sorted.sort_by_key(|s| s.size);
                sorted.into_iter().take(batch).collect()
            };

            // Drop 0-entry empty SSTs (57-byte shells). They carry no data and
            // only pollute the "old SST" footer-upgrade counter.
            let (empty_ssts, ssts): (Vec<SsTableInfo>, Vec<SsTableInfo>) =
                ssts.into_iter().partition(|s| s.size < 64);
            for e in &empty_ssts {
                levels[level].retain(|s| s.path != e.path);
            }

            if ssts.is_empty() {
                return Ok(());
            }

            let min = ssts
                .iter()
                .map(|s| s.min_key.as_slice())
                .min()
                .unwrap_or(b"")
                .to_vec();
            let max = ssts
                .iter()
                .map(|s| s.max_key.as_slice())
                .max()
                .unwrap_or(b"")
                .to_vec();

            (ssts, min, max, empty_ssts)
        };

        // Delete empty-shell SSTs outside the levels lock
        if !empty_ssts.is_empty() {
            let mut evicted = Vec::new();
            for e in &empty_ssts {
                self.sst_cache.remove(&e.path);
                self.pending_deletions.push(e.path.clone());
                evicted.push(e.path.clone());
            }
            self.notify_evicted(evicted);
            tracing::info!(
                count = empty_ssts.len(),
                "Discarded empty-shell SSTs during compaction pickup"
            );
        }

        // Step 2: Find overlapping SSTables in level N+1 (snapshot, don't remove)
        // Cap total SSTables opened simultaneously to bound memory usage.
        // Each open SSTable holds bloom filter + index + block cache in memory.
        const MAX_SSTABLES_PER_MERGE: usize = 20;
        let max_overlap = MAX_SSTABLES_PER_MERGE.saturating_sub(ssts_to_compact.len()).max(4);
        let next_level = level + 1;
        let (next_level_ssts, empty_next): (Vec<SsTableInfo>, Vec<SsTableInfo>) = {
            let mut levels = self.levels.lock().unwrap_or_else(|e| e.into_inner());
            let overlapping: Vec<SsTableInfo> = levels[next_level]
                .iter()
                .filter(|sst_info| {
                    Self::ranges_overlap(
                        &compact_min,
                        &compact_max,
                        &sst_info.min_key,
                        &sst_info.max_key,
                    )
                })
                .cloned()
                .collect();
            let (empty_next, mut kept): (Vec<SsTableInfo>, Vec<SsTableInfo>) =
                overlapping.into_iter().partition(|s| s.size < 64);
            for e in &empty_next {
                levels[next_level].retain(|s| s.path != e.path);
            }
            kept.truncate(max_overlap);
            (kept, empty_next)
        };
        for e in &empty_next {
            self.sst_cache.remove(&e.path);
            self.pending_deletions.push(e.path.clone());
        }
        if !empty_next.is_empty() {
            self.notify_evicted(empty_next.iter().map(|e| e.path.clone()).collect());
        }

        // Step 2.5: Tombstone placement safety. L1+ selection merges the
        // SMALLEST files first, so a small tombstone file sinks below a big
        // same-key Put file — level-order reads then let the shallower put win
        // and resurrect the row. When an input file's tombstones are shadowed
        // by a SHALLOW survivor (levels 0..=level outside the merge), pull the
        // shadowing file(s) into the merge so the versions resolve together
        // (newest = tombstone wins). If the expansion would exceed the safety
        // cap, keep the affected input file OUT of this round — its tombstones
        // stay at the source level, which is safe (shallow beats deep on read).
        {
            // L0 files mutually overlap, so the shadowing set can be the whole
            // level. Cap scales with level size: a fixed 40 would defer every
            // L0 batch (liveness trap) while L1+ has disjoint ranges and needs
            // no scaling.
            const EXPAND_CAP: usize = 40;
            let expand_cap = if level == 0 {
                (ssts_to_compact.len() * 4).max(EXPAND_CAP)
            } else {
                EXPAND_CAP
            };
            let mut excluded_inputs: Vec<PathBuf> = Vec::new();
            let mut kept: Vec<SsTableInfo> = Vec::with_capacity(ssts_to_compact.len());
            {
                let levels = self.levels.lock().unwrap_or_else(|e| e.into_inner());
                let merged_paths: std::collections::HashSet<PathBuf> = ssts_to_compact
                    .iter()
                    .chain(next_level_ssts.iter())
                    .map(|s| s.path.clone())
                    .collect();
                let shallow: Vec<SsTableInfo> = levels
                    .iter()
                    .take(level + 1)
                    .flat_map(|l| l.iter())
                    .filter(|s| !merged_paths.contains(&s.path))
                    .cloned()
                    .collect();
                let mut expanded: std::collections::HashSet<PathBuf> =
                    std::collections::HashSet::new();
                for s in &ssts_to_compact {
                    let shadowed = match Self::tombstone_span(&s.path) {
                        Some((lo, hi)) => shallow
                            .iter()
                            .filter(|o| {
                                Self::ranges_overlap(&lo, &hi, &o.min_key, &o.max_key)
                            })
                            .cloned()
                            .collect::<Vec<_>>(),
                        None => Vec::new(),
                    };
                    if shadowed.is_empty() {
                        kept.push(s.clone());
                        continue;
                    }
                    let projected = ssts_to_compact.len()
                        + next_level_ssts.len()
                        + expanded.len()
                        + shadowed.len();
                    if projected > expand_cap {
                        excluded_inputs.push(s.path.clone());
                        continue;
                    }
                    for o in shadowed {
                        if expanded.insert(o.path.clone()) {
                            kept.push(o);
                        }
                    }
                    kept.push(s.clone());
                }
            }
            if !excluded_inputs.is_empty() {
                tracing::info!(
                    excluded = excluded_inputs.len(),
                    "tombstone placement: deferring input files shadowed by shallow survivors"
                );
            }
            ssts_to_compact = kept;
        }
        if ssts_to_compact.is_empty() {
            return Ok(());
        }

        // Step 3: Streaming merge using min-heap (O(1) memory per SSTable)
        // Tombstone-drop safety: a tombstone may only be dropped when no SST
        // that SURVIVES outside this merge could still hold an older version
        // of the key — at ANY level (shallower survivors included: dropping a
        // tombstone while a put lives above it resurrects the row; and even
        // keeping it, moving it below a shallow survivor inverts level-order
        // reads). Checking only deeper levels missed same-level siblings,
        // truncated next-level files AND shallower files.
        let survivor_ranges: Vec<(Vec<u8>, Vec<u8>)> = {
            let levels = self.levels.lock().unwrap_or_else(|e| e.into_inner());
            let merged: std::collections::HashSet<PathBuf> = ssts_to_compact
                .iter()
                .chain(next_level_ssts.iter())
                .map(|s| s.path.clone())
                .collect();
            levels
                .iter()
                .flat_map(|ssts| ssts.iter())
                .filter(|s| !merged.contains(&s.path))
                .map(|s| (s.min_key.clone(), s.max_key.clone()))
                .collect()
        };

        // Collect all SSTable paths for streaming merge
        let mut all_sst_paths: Vec<PathBuf> = Vec::new();
        for sst_info in &ssts_to_compact {
            all_sst_paths.push(sst_info.path.clone());
        }
        for sst_info in &next_level_ssts {
            all_sst_paths.push(sst_info.path.clone());
        }

        // Create streaming merge iterator
        let mut merge_iter = crate::lsm::streaming_merge::StreamingMergeIterator::new(&all_sst_paths)
            .map_err(|e| onto_core::CoreError::Custom(format!("Failed to create merge iterator: {}", e)))?;

        // Step 4: Stream entries with dedup + tombstone cleanup, write directly to new SSTables
        let target_sst_size = 64 * 1024 * 1024; // 64MB per output SST
        let merge_start = std::time::Instant::now();
        // 10 minutes: a single L1 merge can legitimately process millions of
        // entries (post-cleanup rebalance); 120s aborted those merges and the
        // LSM could never converge.
        const MERGE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);
        let mut builder =
            SsTableBuilder::with_expected_entries(target_sst_size as usize / 32);
        builder.set_compression_level(self.options.compression_level);
        let mut new_ssts: Vec<SsTableInfo> = Vec::new();
        let mut current_size = 0usize;
        let mut last_key: Option<Vec<u8>> = None;
        let mut batch_first_key: Option<Vec<u8>> = None;
        let mut entry_count = 0u64;

        while merge_iter.has_next() {
            // Timeout check: abort if merge takes too long
            if merge_start.elapsed() > MERGE_TIMEOUT {
                tracing::warn!(
                    "Compaction merge timed out after {}s, aborting (processed {} entries)",
                    merge_start.elapsed().as_secs(),
                    entry_count
                );
                // Delete partial output SSTs so we don't leak files.
                for sst in &new_ssts {
                    let _ = fs::remove_file(&sst.path);
                }
                // Back off before the next compaction attempt — prevents
                // immediately re-triggering the same oversized merge.
                self.cooldown_until =
                    Some(std::time::Instant::now() + std::time::Duration::from_secs(60));
                // Return Err so the caller's retry loop breaks instead of
                // immediately re-triggering the same oversized compaction.
                return Err(onto_core::CoreError::Custom(format!(
                    "compaction merge timed out after {}s ({} entries)",
                    MERGE_TIMEOUT.as_secs(),
                    entry_count
                )));
            }
            let entry = merge_iter.next().unwrap();
            let key = entry.key;
            let value = entry.value;
            let seq_no = entry.seq_no;
            let kind = entry.kind;

            // Skip duplicate keys (streaming merge gives newest first per key)
            if last_key.as_ref() == Some(&key) {
                continue;
            }

            // Tombstone cleanup: drop only when no surviving SST outside this
            // merge can hold an older version of the key. The old
            // `next_level >= deepest_level` shortcut dropped tombstones even
            // when sibling files were excluded from the batch (truncate/limit),
            // resurrecting deleted rows.
            if kind == EntryKind::Delete {
                if Self::tombstone_safe_to_drop(&key, &survivor_ranges) {
                    last_key = Some(key);
                    continue;
                }
            }

            // Track first key of current batch
            if batch_first_key.is_none() {
                batch_first_key = Some(key.clone());
            }

            // Add entry to builder. One clone kept for next-iteration dedup;
            // the rest moves into the builder (bloom insert is by-reference).
            current_size += key.len() + value.len() + 16;
            let dedup_key = key.clone();
            builder.add_owned(Entry {
                key,
                value,
                seq_no,
                kind,
            });
            entry_count += 1;
            last_key = Some(dedup_key);

            // Flush SST when target size reached
            if current_size >= target_sst_size {
                // Throttle: yield between SST flushes to avoid starving queries
                std::thread::sleep(std::time::Duration::from_millis(10));
                let sst_id = self.sst_counter.fetch_add(1, Ordering::Relaxed);
                let sst_path = self
                    .options
                    .data_dir
                    .join(format!("L{}_{}.sst", next_level, sst_id));
                let sst = builder.build(&sst_path)?;

                let metadata = fs::metadata(&sst_path)?;
                new_ssts.push(SsTableInfo {
                    path: sst_path,
                    size: metadata.len(),
                    min_key: batch_first_key.take().unwrap_or_default(),
                    max_key: sst.max_key().to_vec(),
                });

                builder = SsTableBuilder::with_expected_entries(1 << 21);
                builder.set_compression_level(self.options.compression_level);
                current_size = 0;
            }
        }

        // Flush remaining entries
        if current_size > 0 {
            let sst_id = self.sst_counter.fetch_add(1, Ordering::Relaxed);
            let sst_path = self
                .options
                .data_dir
                .join(format!("L{}_{}.sst", next_level, sst_id));
            let sst = builder.build(&sst_path)?;

            let metadata = fs::metadata(&sst_path)?;
            new_ssts.push(SsTableInfo {
                path: sst_path,
                size: metadata.len(),
                min_key: batch_first_key.unwrap_or_default(),
                max_key: sst.max_key().to_vec(),
            });
        }

        // Step 5: Atomically update levels, evict cache, and schedule deletions under lock
        let evicted_paths = {
            let mut levels = self.levels.lock().unwrap_or_else(|e| e.into_inner());

            // Remove compacted SSTables from current level
            let compact_paths: std::collections::HashSet<PathBuf> =
                ssts_to_compact.iter().map(|s| s.path.clone()).collect();
            levels[level].retain(|s| !compact_paths.contains(&s.path));

            // Remove overlapping SSTables from next level
            let overlap_paths: std::collections::HashSet<PathBuf> =
                next_level_ssts.iter().map(|s| s.path.clone()).collect();
            levels[next_level].retain(|s| !overlap_paths.contains(&s.path));

            // Add new merged SSTables. Cross-level: level-N inputs are newer
            // than any level-(N+1) file, so the output is the newest thing in
            // next_level — append at the end (read via `iter().rev()` first).
            // Do NOT `sort_by(min_key)`: that breaks the insertion-order-is-age
            // contract and lets a stale Put beat a newer Tombstone.
            levels[next_level].extend(new_ssts);

            // Evict cache entries and schedule deletions inside the same critical section
            let mut evicted = Vec::new();
            for sst_info in &ssts_to_compact {
                self.sst_cache.remove(&sst_info.path);
                evicted.push(sst_info.path.clone());
                self.pending_deletions.push(sst_info.path.clone());
            }
            for sst_info in &next_level_ssts {
                self.sst_cache.remove(&sst_info.path);
                evicted.push(sst_info.path.clone());
                self.pending_deletions.push(sst_info.path.clone());
            }
            evicted
        };

        // Notify the engine about evicted SSTable paths for cache invalidation
        if let Some(ref sender) = self.notif_sender {
            let _ = sender.send(CompactionNotification::Compacted { evicted_paths });
        }

        tracing::info!(
            "Background compaction L{}→L{}: merged {} + {} SSTables ({} entries)",
            level,
            next_level,
            ssts_to_compact.len(),
            next_level_ssts.len(),
            entry_count
        );

        Ok(())
    }

    /// L0 in-place self-merge (no sink to L1).
    ///
    /// L0 SSTs are memtable flushes with mutually overlapping key ranges, so
    /// the L0→L1 tombstone-placement guard (Step 2.5 in `compact_level`)
    /// defers almost every input and the level never drains — a pure liveness
    /// trap under bulk load. Merging L0 files *among themselves* keeps
    /// tombstones at L0 (shallow-beats-deep reads stay correct) while cutting
    /// file count, which is the only goal of the file-count trigger.
    ///
    /// Positional invariant: `levels[0]` is flush-ordered (append = newest) and
    /// the read path does first-seen-wins over `iter().rev()` — list position,
    /// NOT seq_no, decides which SST wins. So the batch must be the OLDEST
    /// prefix (low indices) and the output must splice back at the same
    /// position. Appending merged output at the end would promote stale
    /// versions over newer non-batched flushes and resurrect deleted rows.
    fn merge_l0_self_batch(&mut self) -> Result<()> {
        const L0_SELF_BATCH: usize = 16;

        // Snapshot the oldest L0 SSTs (low indices = early flushes = stale).
        // Empty shells (<64 B) are discarded rather than merged.
        let (batch, empty_ssts) = {
            let mut levels = self.levels.lock().unwrap_or_else(|e| e.into_inner());
            if levels.is_empty() || levels[0].len() < 2 {
                return Ok(());
            }
            let mut ssts: Vec<SsTableInfo> = Vec::new();
            let mut empty: Vec<SsTableInfo> = Vec::new();
            for s in levels[0].iter() {
                if ssts.len() + empty.len() >= L0_SELF_BATCH {
                    break;
                }
                if s.size < 64 {
                    empty.push(s.clone());
                } else {
                    ssts.push(s.clone());
                }
            }
            for e in &empty {
                levels[0].retain(|s| s.path != e.path);
            }
            (ssts, empty)
        };
        for e in &empty_ssts {
            self.sst_cache.remove(&e.path);
            self.pending_deletions.push(e.path.clone());
        }
        if batch.len() < 2 {
            return Ok(());
        }

        // Tombstone-drop safety: a tombstone may only be dropped when no SST
        // surviving outside this merge can hold an older version of the key.
        // Same contract as `compact_level` — self-merge must not resurrect
        // rows whose only remaining Put lives in an L0/L1 file we left out.
        let survivor_ranges: Vec<(Vec<u8>, Vec<u8>)> = {
            let levels = self.levels.lock().unwrap_or_else(|e| e.into_inner());
            let merged: std::collections::HashSet<PathBuf> =
                batch.iter().map(|s| s.path.clone()).collect();
            levels
                .iter()
                .flat_map(|ssts| ssts.iter())
                .filter(|s| !merged.contains(&s.path))
                .map(|s| (s.min_key.clone(), s.max_key.clone()))
                .collect()
        };

        let paths: Vec<PathBuf> = batch.iter().map(|s| s.path.clone()).collect();
        let mut merge_iter =
            crate::lsm::streaming_merge::StreamingMergeIterator::new(&paths)
                .map_err(|e| {
                    onto_core::CoreError::Custom(format!(
                        "L0 self-merge iterator failed: {}",
                        e
                    ))
                })?;

        // Output cut size must beat the compression gap. `batch_disk` is
        // on-disk (compressed) but `current_size` counts raw key+value bytes.
        // L0 flushes run ~3× compression: 16 × 22 MB ≈ 352 MB disk ≈ 1 GB raw.
        // A 64 MB cut regurgitates 16+ outputs (no win, merge discarded);
        // 256 MB yields ~4 outputs — a real 16→4 reduction.
        let target_sst_size = 256 * 1024 * 1024;
        let mut builder =
            SsTableBuilder::with_expected_entries(target_sst_size as usize / 32);
        builder.set_compression_level(self.options.compression_level);
        let mut new_ssts: Vec<SsTableInfo> = Vec::new();
        let mut current_size = 0usize;
        let mut last_key: Option<Vec<u8>> = None;
        let mut batch_first_key: Option<Vec<u8>> = None;
        let mut entry_count = 0u64;
        let mut deduped = 0u64;

        while merge_iter.has_next() {
            let entry = merge_iter.next().unwrap();
            let key = entry.key;
            let value = entry.value;
            let seq_no = entry.seq_no;
            let kind = entry.kind;

            // Streaming merge yields newest-first per key: keep one version.
            if last_key.as_ref() == Some(&key) {
                deduped += 1;
                continue;
            }

            if kind == EntryKind::Delete
                && Self::tombstone_safe_to_drop(&key, &survivor_ranges)
            {
                last_key = Some(key);
                continue;
            }

            if batch_first_key.is_none() {
                batch_first_key = Some(key.clone());
            }
            current_size += key.len() + value.len() + 16;
            let dedup_key = key.clone();
            builder.add_owned(Entry {
                key,
                value,
                seq_no,
                kind,
            });
            entry_count += 1;
            last_key = Some(dedup_key);

            if current_size >= target_sst_size {
                let sst_id = self.sst_counter.fetch_add(1, Ordering::Relaxed);
                let sst_path = self
                    .options
                    .data_dir
                    .join(format!("L0_{}.sst", sst_id));
                let sst = builder.build(&sst_path)?;
                let metadata = fs::metadata(&sst_path)?;
                new_ssts.push(SsTableInfo {
                    path: sst_path,
                    size: metadata.len(),
                    min_key: batch_first_key.take().unwrap_or_default(),
                    max_key: sst.max_key().to_vec(),
                });
                builder = SsTableBuilder::with_expected_entries(1 << 21);
                builder.set_compression_level(self.options.compression_level);
                current_size = 0;
            }
        }
        if current_size > 0 {
            let sst_id = self.sst_counter.fetch_add(1, Ordering::Relaxed);
            let sst_path = self
                .options
                .data_dir
                .join(format!("L0_{}.sst", sst_id));
            let sst = builder.build(&sst_path)?;
            let metadata = fs::metadata(&sst_path)?;
            new_ssts.push(SsTableInfo {
                path: sst_path,
                size: metadata.len(),
                min_key: batch_first_key.unwrap_or_default(),
                max_key: sst.max_key().to_vec(),
            });
        }

        if new_ssts.len() >= batch.len() {
            // No file-count win (e.g. everything deduped to the same number of
            // outputs). Drop the outputs — keeping them would churn.
            tracing::warn!(
                "L0 self-merge no-win: {} inputs produced {} outputs (target {} B), discarding",
                batch.len(),
                new_ssts.len(),
                target_sst_size
            );
            for sst in &new_ssts {
                let _ = fs::remove_file(&sst.path);
            }
            return Ok(());
        }
        let out_count = new_ssts.len();

        // Splice the output back at the batch's newest slot. Non-batched L0
        // files sit at higher indices and stay read-first (`iter().rev()`),
        // so their newer versions keep winning over the merged output.
        let evicted = {
            let mut levels = self.levels.lock().unwrap_or_else(|e| e.into_inner());
            Self::splice_outputs(&mut levels, 0, &batch, new_ssts);

            let mut evicted = Vec::new();
            for s in &batch {
                self.sst_cache.remove(&s.path);
                evicted.push(s.path.clone());
                self.pending_deletions.push(s.path.clone());
            }
            evicted
        };

        if let Some(ref sender) = self.notif_sender {
            let _ = sender.send(CompactionNotification::Compacted { evicted_paths: evicted });
        }

        tracing::info!(
            "L0 self-merge: {} → {} SSTables ({} entries, {} deduped, {} empty shells dropped)",
            batch.len(),
            out_count,
            entry_count,
            deduped,
            empty_ssts.len()
        );

        Ok(())
    }

    /// Bounded self-merge for the deepest level (C).
    ///
    /// The deepest level is never a size-score compaction source, and a full
    /// self-merge of ~24M entries OOMs / churns without reducing file count.
    ///
    /// **2-in-1-out with a size cap** is the only safe file-count reduction
    /// once L6 files are large:
    /// - a bigger batch re-splits into N outputs and churns
    ///   (observed: 8 in → 8 out, 0 deduped)
    /// - unlimited 2-in-1-out grows files without bound (multi-GB SSTs:
    ///   rewrite amplification, huge open handles, latency spikes)
    ///
    /// So: merge two of the smallest files *only when their combined size
    /// stays under `MAX_PAIR_BYTES`*. Output is a single SST (no split), so
    /// each call nets -1 file. Oversized pairs are left alone — L5→L6
    /// normal compaction is what absorbs those.
    fn merge_last_level_batch(&mut self) -> Result<()> {
        const BATCH: usize = 2;
        // Cap the *pair* so the single output stays near industry norms
        // (RocksDB 64MB, Cassandra 160MB; 64–256MB is the common range).
        // 320MB lets a 258MB pair merge (the case that left the file-count
        // trigger spinning at a 256MB cap) without welding two large SSTs
        // into a ~500MB file.
        const MAX_PAIR_BYTES: u64 = 320 * 1024 * 1024;
        // Do not split the output: 2 inputs must become 1 file or the level
        // cannot shrink. Safe because the pair is size-capped above.
        const TARGET_OUTPUT_SIZE: u64 = u64::MAX;
        const MIN_FILES: usize = 2;

        // Drop 0-entry empty shells first — they pollute the batch budget.
        let empties: Vec<PathBuf> = {
            let levels = self.levels.lock().unwrap_or_else(|e| e.into_inner());
            let deepest = levels.len().saturating_sub(1);
            if deepest == 0 {
                return Ok(());
            }
            levels[deepest]
                .iter()
                .filter(|s| s.size < 64)
                .map(|s| s.path.clone())
                .collect()
        };
        if !empties.is_empty() {
            {
                let mut levels = self.levels.lock().unwrap_or_else(|e| e.into_inner());
                let deepest = levels.len().saturating_sub(1);
                let empty_set: std::collections::HashSet<PathBuf> = empties.iter().cloned().collect();
                levels[deepest].retain(|s| !empty_set.contains(&s.path));
            }
            for p in &empties {
                self.sst_cache.remove(p);
                self.pending_deletions.push(p.clone());
            }
            self.notify_evicted(empties.clone());
            tracing::info!(
                count = empties.len(),
                "Discarded empty-shell SSTs in merge_last_level_batch"
            );
        }

        // Pick the batch under the lock, then release before I/O.
        let (level_idx, batch) = {
            let levels = self.levels.lock().unwrap_or_else(|e| e.into_inner());
            let deepest = levels.len().saturating_sub(1);
            if deepest == 0 {
                return Ok(());
            }

            // Smallest first: steady file-count progress without a huge read.
            // Only form a pair that stays under MAX_PAIR_BYTES — merging two
            // already-huge files would just mint one giant SST.
            let mut sorted: Vec<SsTableInfo> = levels[deepest].clone();
            sorted.sort_by_key(|s| s.size);
            let mut picked: Vec<SsTableInfo> = Vec::new();
            let mut total: u64 = 0;
            for s in sorted {
                if picked.len() >= BATCH {
                    break;
                }
                if !picked.is_empty() && total + s.size > MAX_PAIR_BYTES {
                    break;
                }
                if picked.is_empty() && s.size > MAX_PAIR_BYTES {
                    // A single file already over the cap cannot form a safe pair.
                    break;
                }
                total += s.size;
                picked.push(s);
            }
            if picked.len() < MIN_FILES {
                return Ok(());
            }
            (deepest, picked)
        };

        let paths: Vec<PathBuf> = batch.iter().map(|s| s.path.clone()).collect();
        let input_bytes: u64 = batch.iter().map(|s| s.size).sum();
        tracing::info!(
            level = level_idx,
            files = paths.len(),
            input_mb = input_bytes / (1024 * 1024),
            "Last-level batched self-merge starting"
        );

        let mut merge_iter =
            match crate::lsm::streaming_merge::StreamingMergeIterator::new(&paths) {
                Ok(iter) => iter,
                Err(e) => {
                    tracing::warn!("Failed to create last-level merge iterator: {}", e);
                    return Ok(());
                }
            };
        if !merge_iter.has_next() {
            return Ok(());
        }

        let mut new_ssts: Vec<SsTableInfo> = Vec::new();
        // Last-level self-merge writes one unbounded SST; size the bloom
        // from the input bytes (conservative 32-byte min entry).
        let input_bytes: u64 = batch.iter().map(|s| s.size).sum();
        let mut builder =
            SsTableBuilder::with_expected_entries((input_bytes as usize / 32).max(65536));
        builder.set_compression_level(self.options.compression_level);
        let mut current_size: u64 = 0;
        let mut batch_min_key: Vec<u8> = Vec::new();
        let mut last_key: Option<Vec<u8>> = None;
        let mut total_entries: u64 = 0;
        let mut deduped: u64 = 0;

        while merge_iter.has_next() {
            let entry = merge_iter.next().unwrap();
            // StreamingMergeIterator yields newest-first per key; keep one.
            if last_key.as_ref() == Some(&entry.key) {
                deduped += 1;
                continue;
            }
            if batch_min_key.is_empty() {
                batch_min_key = entry.key.clone();
            }

            current_size += entry.key.len() as u64 + entry.value.len() as u64 + 16;
            // One clone kept for next-iteration dedup; the rest moves into the builder.
            let dedup_key = entry.key.clone();
            let sst_entry = match entry.kind {
                EntryKind::Put => Entry::put(entry.key, entry.value, entry.seq_no),
                EntryKind::Delete => Entry::delete(entry.key, entry.seq_no),
            };
            builder.add_owned(sst_entry);
            total_entries += 1;
            last_key = Some(dedup_key);

            if current_size >= TARGET_OUTPUT_SIZE {
                let id = self.sst_counter.fetch_add(1, Ordering::Relaxed);
                let path = self
                    .options
                    .data_dir
                    .join(format!("L{}_{}.sst", level_idx, id));
                let sst = builder.build(&path)?;
                let metadata = fs::metadata(&path)?;
                new_ssts.push(SsTableInfo {
                    path,
                    size: metadata.len(),
                    min_key: batch_min_key.clone(),
                    max_key: sst.max_key().to_vec(),
                });
                builder = SsTableBuilder::with_expected_entries(1 << 21);
                builder.set_compression_level(self.options.compression_level);
                current_size = 0;
                batch_min_key = Vec::new();
            }
        }

        if !batch_min_key.is_empty() {
            let id = self.sst_counter.fetch_add(1, Ordering::Relaxed);
            let path = self
                .options
                .data_dir
                .join(format!("L{}_{}.sst", level_idx, id));
            let sst = builder.build(&path)?;
            let metadata = fs::metadata(&path)?;
            new_ssts.push(SsTableInfo {
                path,
                size: metadata.len(),
                min_key: batch_min_key,
                max_key: sst.max_key().to_vec(),
            });
        }

        // Swap batch → outputs inside one critical section, keeping age order.
        {
            let mut levels = self.levels.lock().unwrap_or_else(|e| e.into_inner());
            Self::splice_outputs(&mut levels, level_idx, &batch, new_ssts.clone());
        }
        for s in &batch {
            self.sst_cache.remove(&s.path);
            self.pending_deletions.push(s.path.clone());
        }
        if let Some(ref sender) = self.notif_sender {
            let evicted: Vec<PathBuf> = batch.iter().map(|s| s.path.clone()).collect();
            let _ = sender.send(CompactionNotification::Compacted { evicted_paths: evicted });
        }

        tracing::info!(
            level = level_idx,
            before = batch.len(),
            after = new_ssts.len(),
            entries = total_entries,
            deduped,
            "Last-level batched self-merge complete"
        );
        if new_ssts.len() >= batch.len() {
            // Non-reducing batch would churn forever at FILE_COUNT_TRIGGER.
            tracing::warn!(
                level = level_idx,
                before = batch.len(),
                after = new_ssts.len(),
                "last-level batch did not reduce file count — check TARGET_OUTPUT_SIZE"
            );
        }
        Ok(())
    }

    /// Merge small SSTables into larger ones to prevent file count explosion.
    /// Uses streaming merge (StreamingMergeIterator) to avoid loading all entries
    /// into memory. Processes a BATCH of small files per call to bound I/O.
    fn merge_small_files(&mut self) -> Result<()> {
        const SMALL_FILE_THRESHOLD: u64 = 16 * 1024 * 1024; // 16MB
        const MIN_FILES_TO_MERGE: usize = 4;
        const MERGE_BATCH_SIZE: usize = 5;
        // Target must be large enough so compressed output exceeds SMALL_FILE_THRESHOLD.
        // With ~4x compression ratio, 256MB uncompressed → ~64MB compressed output.
        const TARGET_OUTPUT_SIZE: u64 = 256 * 1024 * 1024; // 256MB uncompressed target

        // Find the level with the most small files
        let (target_level, mut small_files) = {
            let levels = self.levels.lock().unwrap_or_else(|e| e.into_inner());
            let mut best_level = None;
            let mut best_files = Vec::new();

            for (level_idx, level) in levels.iter().enumerate() {
                let small: Vec<SsTableInfo> = level
                    .iter()
                    .filter(|s| s.size < SMALL_FILE_THRESHOLD)
                    .cloned()
                    .collect();
                if small.len() >= MIN_FILES_TO_MERGE && small.len() > best_files.len() {
                    best_level = Some(level_idx);
                    best_files = small;
                }
            }

            match best_level {
                Some(l) => (l, best_files),
                None => return Ok(()),
            }
        };

        // Discard empty-shell SSTs (57-byte, 0-entry) instead of merging them.
        let (empty_shells, non_empty): (Vec<SsTableInfo>, Vec<SsTableInfo>) =
            small_files.into_iter().partition(|s| s.size < 64);
        small_files = non_empty;
        if !empty_shells.is_empty() {
            {
                let mut levels = self.levels.lock().unwrap_or_else(|e| e.into_inner());
                for e in &empty_shells {
                    levels[target_level].retain(|s| s.path != e.path);
                }
            }
            for e in &empty_shells {
                self.sst_cache.remove(&e.path);
                self.pending_deletions.push(e.path.clone());
            }
            self.notify_evicted(empty_shells.iter().map(|e| e.path.clone()).collect());
            tracing::info!(
                count = empty_shells.len(),
                "Discarded empty-shell SSTs in merge_small_files"
            );
        }

        // Limit to one batch to bound memory usage
        small_files.truncate(MERGE_BATCH_SIZE);
        let batch_size = small_files.len();

        tracing::info!(
            "Merging batch of {} small L{} SSTables (< 32MB each)",
            batch_size,
            target_level
        );

        // Collect paths for streaming merge
        let sst_paths: Vec<PathBuf> = small_files.iter().map(|s| s.path.clone()).collect();

        // Use streaming merge iterator - entries come out sorted by key, with
        // newest seq_no first for each key (dedup happens on-the-fly)
        let mut merge_iter = match crate::lsm::streaming_merge::StreamingMergeIterator::new(&sst_paths) {
            Ok(iter) => iter,
            Err(e) => {
                tracing::warn!("Failed to create streaming merge iterator: {}", e);
                return Ok(());
            }
        };

        if !merge_iter.has_next() {
            return Ok(());
        }

        // Stream entries directly into new SSTables with on-the-fly dedup.
        // StreamingMergeIterator returns entries sorted by key, with largest seq_no
        // first for duplicate keys. We keep only the first occurrence of each key.
        let mut new_ssts: Vec<SsTableInfo> = Vec::new();
        let mut builder =
            SsTableBuilder::with_expected_entries(TARGET_OUTPUT_SIZE as usize / 32);
        builder.set_compression_level(self.options.compression_level);
        let mut current_size: u64 = 0;
        let mut batch_min_key: Vec<u8> = Vec::new();
        let mut last_key: Option<Vec<u8>> = None;
        let mut total_entries: u64 = 0;
        let mut deduped_count: u64 = 0;

        while merge_iter.has_next() {
            let entry = merge_iter.next().unwrap();

            // Dedup: StreamingMergeIterator returns newest first per key,
            // so skip subsequent entries with the same key
            if last_key.as_ref() == Some(&entry.key) {
                deduped_count += 1;
                continue;
            }

            if batch_min_key.is_empty() {
                batch_min_key = entry.key.clone();
            }

            current_size += entry.key.len() as u64 + entry.value.len() as u64 + 16;
            // One clone kept for next-iteration dedup; the rest moves into the builder.
            let dedup_key = entry.key.clone();
            let sst_entry = match entry.kind {
                EntryKind::Put => Entry::put(entry.key, entry.value, entry.seq_no),
                EntryKind::Delete => Entry::delete(entry.key, entry.seq_no),
            };
            builder.add_owned(sst_entry);
            total_entries += 1;
            last_key = Some(dedup_key);

            // Flush SST when target size reached
            if current_size >= TARGET_OUTPUT_SIZE {
                // Throttle: yield between SST flushes to avoid starving queries
                std::thread::sleep(std::time::Duration::from_millis(10));
                let id = self.sst_counter.fetch_add(1, Ordering::Relaxed);
                let path = self.options.data_dir.join(format!("L{}_{}.sst", target_level, id));
                tracing::info!(
                    "Merge flushing SST: current_size={} ({}MB)",
                    current_size,
                    current_size / 1024 / 1024
                );
                let sst = builder.build(&path)?;
                let metadata = fs::metadata(&path)?;
                new_ssts.push(SsTableInfo {
                    path,
                    size: metadata.len(),
                    min_key: batch_min_key.clone(),
                    max_key: sst.max_key().to_vec(),
                });
                builder = SsTableBuilder::with_expected_entries(1 << 21);
                builder.set_compression_level(self.options.compression_level);
                current_size = 0;
                batch_min_key = Vec::new();
            }
        }

        // Flush remaining entries
        if !batch_min_key.is_empty() {
            let id = self.sst_counter.fetch_add(1, Ordering::Relaxed);
            let path = self.options.data_dir.join(format!("L{}_{}.sst", target_level, id));
            let sst = builder.build(&path)?;
            let metadata = fs::metadata(&path)?;
            new_ssts.push(SsTableInfo {
                path,
                size: metadata.len(),
                min_key: batch_min_key,
                max_key: sst.max_key().to_vec(),
            });
        }

        let new_count = new_ssts.len();
        let total_output_size: u64 = new_ssts.iter().map(|s| s.size).sum();

        tracing::info!(
            "Merge write complete: {} entries ({} deduped) → {} SSTs ({}MB total)",
            total_entries,
            deduped_count,
            new_count,
            total_output_size / 1024 / 1024
        );

        // Replace small files with new larger files and schedule old ones for
        // deletion. Splice in place — never `sort_by(min_key)`, which would
        // destroy the insertion-order-is-age contract the read path needs.
        let evicted_small: Vec<PathBuf> = {
            let mut levels = self.levels.lock().unwrap_or_else(|e| e.into_inner());
            let small_paths: std::collections::HashSet<PathBuf> =
                small_files.iter().map(|s| s.path.clone()).collect();

            Self::splice_outputs(&mut levels, target_level, &small_files, new_ssts);

            self.pending_deletions.extend(small_paths.iter().cloned());
            small_paths.into_iter().collect()
        };
        self.notify_evicted(evicted_small);

        tracing::info!(
            "Small file merge batch complete (L{}): {} small files → {} larger files ({}MB)",
            target_level,
            batch_size,
            new_count,
            total_output_size / 1024 / 1024
        );

        Ok(())
    }

    /// A tombstone may be dropped only if no SST that survives outside the
    /// current merge can contain an older version of the key. Checking only
    /// deeper levels (the old behavior) resurrects deleted rows from
    /// same-level siblings and truncated next-level files.
    fn tombstone_safe_to_drop(key: &[u8], survivor_ranges: &[(Vec<u8>, Vec<u8>)]) -> bool {
        for (min_key, max_key) in survivor_ranges {
            if min_key.is_empty() && max_key.is_empty() {
                // Unknown bounds — be conservative.
                return false;
            }
            if key >= min_key.as_slice() && key <= max_key.as_slice() {
                return false;
            }
        }
        true
    }

    fn ranges_overlap(min_a: &[u8], max_a: &[u8], min_b: &[u8], max_b: &[u8]) -> bool {
        if min_a.is_empty() || max_a.is_empty() || min_b.is_empty() || max_b.is_empty() {
            return true;
        }
        min_a <= max_b && min_b <= max_a
    }

    /// Replace `batch` with `new_ssts` inside `levels[level_idx]`, preserving
    /// **insertion order = age order** (the read path's first-hit-wins contract).
    ///
    /// The merged output carries the newest versions of the batch's keys, so it
    /// belongs at the position of the batch's NEWEST member:
    /// - files inserted later (newer than the whole batch) stay after it and are
    ///   read first — correct, their versions win;
    /// - files inserted earlier (older than the whole batch) stay before it and
    ///   are read after — correct, the output's versions win;
    /// - files interleaved between batch members are older than the batch's
    ///   newest member, so the output must still be read before them.
    ///
    /// Never `sort_by(min_key)`: that rewires the list into key order and lets
    /// a stale Put file whose `min_key` sorts later beat a newer Tombstone.
    fn splice_outputs(
        levels: &mut Vec<Vec<SsTableInfo>>,
        level_idx: usize,
        batch: &[SsTableInfo],
        new_ssts: Vec<SsTableInfo>,
    ) {
        let batch_paths: std::collections::HashSet<&Path> =
            batch.iter().map(|s| s.path.as_path()).collect();
        // Position of the newest batch member in the current list.
        let mut newest_idx = 0usize;
        for (i, s) in levels[level_idx].iter().enumerate() {
            if batch_paths.contains(s.path.as_path()) {
                newest_idx = i;
            }
        }
        // Remove batch members; adjust the splice point by how many of them
        // sat at or before `newest_idx` (they will be gone when we insert).
        let mut insert_at = newest_idx;
        for (i, s) in levels[level_idx].iter().enumerate() {
            if i < newest_idx && batch_paths.contains(s.path.as_path()) {
                insert_at -= 1;
            }
        }
        levels[level_idx].retain(|s| !batch_paths.contains(s.path.as_path()));
        let insert_at = insert_at.min(levels[level_idx].len());
        for (i, sst) in new_ssts.into_iter().enumerate() {
            levels[level_idx].insert(insert_at + i, sst);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use onto_core::Entry;
    use std::path::Path;
    use std::sync::mpsc;
    use tempfile::tempdir;

    fn build_sst(dir: &Path, name: &str, entries: Vec<Entry>) -> SsTableInfo {
        let path = dir.join(name);
        let mut builder = SsTableBuilder::new();
        let mut lo: Vec<u8> = Vec::new();
        let mut hi: Vec<u8> = Vec::new();
        for e in &entries {
            if lo.is_empty() || e.key < lo {
                lo = e.key.clone();
            }
            if hi.is_empty() || e.key > hi {
                hi = e.key.clone();
            }
        }
        for e in entries {
            builder.add_owned(e);
        }
        builder.build(&path).expect("build sst");
        let meta = std::fs::metadata(&path).expect("meta");
        SsTableInfo {
            path,
            size: meta.len(),
            min_key: lo,
            max_key: hi,
        }
    }

    fn read_winner(levels: &Arc<Mutex<Vec<Vec<SsTableInfo>>>>, key: &[u8]) -> Option<Option<Vec<u8>>> {
        let lv = levels.lock().unwrap();
        for l in lv.iter() {
            for s in l.iter().rev() {
                let sst = SsTable::open(&s.path).expect("open");
                match sst.get_full(key).expect("get") {
                    Some((val, _, EntryKind::Put)) => return Some(Some(val)),
                    Some((_, _, EntryKind::Delete)) => return Some(None),
                    None => {}
                }
            }
        }
        None
    }

    #[test]
    fn test_tombstone_span_detects_deletes() {
        let dir = tempdir().expect("tempdir");
        let t = build_sst(
            dir.path(),
            "T.sst",
            vec![
                Entry::put(b"a".to_vec(), b"1".to_vec(), 1),
                Entry::delete(b"k000".to_vec(), 1000),
                Entry::put(b"z".to_vec(), b"2".to_vec(), 2),
            ],
        );
        let span = CompactionWorker::tombstone_span(&t.path);
        assert_eq!(
            span,
            Some((b"k000".to_vec(), b"k000".to_vec())),
            "must detect delete keys"
        );
        let clean = build_sst(
            dir.path(),
            "C.sst",
            vec![Entry::put(b"a".to_vec(), b"1".to_vec(), 1)],
        );
        assert_eq!(CompactionWorker::tombstone_span(&clean.path), None);
    }

    /// Deterministic resurrection repro: a SMALL tombstone file and a BIG    /// same-key put file at the same level. Size-first selection merges the
    /// tombstone file alone, moving the tombstone DEEPER than the surviving
    /// put — level-order reads then resurrect the row. The placement fix
    /// must pull the shadowing put file into the merge instead.
    #[test]
    fn test_tombstone_not_placed_below_shadowing_put() {
        let dir = tempdir().expect("tempdir");

        // L1 file A: 20 puts incl. k000 (large file).
        let mut put_entries = Vec::new();
        for i in 0..20u32 {
            put_entries.push(Entry::put(
                format!("k{i:03}").into_bytes(),
                vec![b'v'; 500],
                10 + i as u64,
            ));
        }
        let a = build_sst(dir.path(), "L1_A.sst", put_entries);

        // L1 file T: one tombstone for k000 (tiny file — merges first).
        let t = build_sst(
            dir.path(),
            "L1_T.sst",
            vec![Entry::delete(b"k000".to_vec(), 1000)],
        );
        assert!(t.size < a.size, "test needs T smaller than A (size-first)");

        let levels: Arc<Mutex<Vec<Vec<SsTableInfo>>>> = Arc::new(Mutex::new(vec![
            Vec::new(),
            vec![t, a],
            Vec::new(),
        ]));

        let (_tx, rx) = mpsc::channel();
        let (ntx, _nrx) = mpsc::channel();
        let mut worker = CompactionWorker {
            levels: levels.clone(),
            options: StorageOptions {
                data_dir: dir.path().to_path_buf(),
                memtable_size_limit: 1024 * 1024,
                size_ratio: 2,
                ..Default::default()
            },
            sst_counter: Arc::new(AtomicU64::new(1000)),
            receiver: rx,
            notif_sender: Some(ntx),
            sst_cache: HashMap::new(),
            pending_deletions: Vec::new(),
            cooldown_until: None,
        };
        worker.compact_level(1).expect("compact");

        // The shadowing put file A must have been pulled INTO the merge:
        // L1 ends empty and the merged output lives at L2. Without the
        // placement fix A lingers at L1 and k000 resurrects.
        {
            let lv = levels.lock().unwrap();
            assert!(
                lv[1].is_empty(),
                "shadowing put file must join the tombstone merge; L1 still has {} files",
                lv[1].len()
            );
        }

        // k000 must NOT read back as a live Put. Two correct end states:
        // tombstone visible (Some(None)) or key fully absent (None).
        // Resurrection looks like Some(Some(_)).
        let w = read_winner(&levels, b"k000");
        assert!(
            !matches!(w, Some(Some(_))),
            "tombstone landed below a surviving put — row resurrected: {:?}",
            w
        );
        // Sibling puts from A must survive somewhere in the levels.
        assert_eq!(
            read_winner(&levels, b"k010"),
            Some(Some(vec![b'v'; 500])),
            "merge must not lose unrelated puts"
        );
    }

    /// L0 liveness: many mutually-overlapping L0 SSTs whose tombstones are
    /// shadowed by sibling L0 files. `compact_level(0)` defers every input
    /// (placement safety) and the level never drains. `merge_l0_self_batch`
    /// must cut file count in place without resurrecting deleted keys.
    #[test]
    fn test_l0_self_merge_thins_overcrowded_level() {
        let dir = tempdir().expect("tempdir");

        // 20 L0 files, same overlapping key span, each with a tombstone + puts.
        // The tombstone in file i is shadowed by puts in every other file, so
        // the L0→L1 placement guard would exclude all of them.
        let mut l0: Vec<SsTableInfo> = Vec::new();
        for f in 0..20u32 {
            let mut entries = Vec::new();
            // Shared tombstone first (d < u) so entries stay key-sorted —
            // SsTableBuilder requires sorted keys for get() to work.
            entries.push(Entry::delete(format!("d{f:03}").into_bytes(), 5 + f as u64));
            // Unique put per file so dedup can't erase the file's payload.
            entries.push(Entry::put(
                format!("u{f:03}").into_bytes(),
                vec![b'v'; 200],
                10 + f as u64,
            ));
            l0.push(build_sst(dir.path(), &format!("L0_{f}.sst"), entries));
        }
        let start_files = l0.len();
        assert!(start_files > 16, "test needs a crowded L0");

        let levels: Arc<Mutex<Vec<Vec<SsTableInfo>>>> =
            Arc::new(Mutex::new(vec![l0, Vec::new(), Vec::new()]));
        let (_tx, rx) = mpsc::channel();
        let (ntx, _nrx) = mpsc::channel();
        let mut worker = CompactionWorker {
            levels: levels.clone(),
            options: StorageOptions {
                data_dir: dir.path().to_path_buf(),
                memtable_size_limit: 1024 * 1024,
                size_ratio: 2,
                ..Default::default()
            },
            sst_counter: Arc::new(AtomicU64::new(1000)),
            receiver: rx,
            notif_sender: Some(ntx),
            sst_cache: HashMap::new(),
            pending_deletions: Vec::new(),
            cooldown_until: None,
        };

        worker.merge_l0_self_batch().expect("self-merge");

        let after = levels.lock().unwrap()[0].len();
        assert!(
            after < start_files,
            "L0 self-merge must reduce file count: {} → {}",
            start_files,
            after
        );

        // No resurrection: every dXXX was deleted and stays deleted.
        for f in 0..20u32 {
            let w = read_winner(&levels, format!("d{f:03}").as_bytes());
            assert!(
                !matches!(w, Some(Some(_))),
                "deleted key d{:03} resurrected: {:?}",
                f,
                w
            );
        }
        // Unique puts must survive.
        for f in 0..20u32 {
            assert_eq!(
                read_winner(&levels, format!("u{f:03}").as_bytes()),
                Some(Some(vec![b'v'; 200])),
                "put u{:03} lost in self-merge",
                f
            );
        }
    }

    /// Deterministic repro of the flaky `test_tombstones_survive_compaction_no_resurrect`.
    ///
    /// Root cause chain:
    /// 1. `merge_small_files` picks the first N small SSTs by list order and
    ///    merges ONLY those — same-level siblings outside the batch survive.
    /// 2. It then `sort_by(min_key)`, rewiring the level into key order and
    ///    destroying the "insertion order = age order" contract the read path
    ///    relies on (`engine.rs:1505`: `iter().rev()` = newest first).
    /// 3. `get()` is first-hit-wins over `iter().rev()`, so after the sort an
    ///    older Put file whose `min_key` sorts later is read BEFORE a newer
    ///    Tombstone file whose `min_key` sorts earlier → deleted row resurrects.
    ///
    /// The flaky test hits this when the concurrent writer produces a small
    /// tombstone SST outside `merge_small_files`'s batch; whether it lands
    /// before/after the merged output in min_key order is timing-dependent.
    #[test]
    fn test_merge_small_files_must_not_resurrect_tombstone() {
        let dir = tempdir().expect("tempdir");

        // 5 small files (the merge batch). One holds the OLD Put k000.
        let mut l1: Vec<SsTableInfo> = Vec::new();
        for i in 0..4u32 {
            l1.push(build_sst(
                dir.path(),
                &format!("S{i}.sst"),
                vec![Entry::put(
                    format!("k1{i:03}").into_bytes(),
                    vec![b'v'; 400],
                    5 + i as u64,
                )],
            ));
        }
        l1.push(build_sst(
            dir.path(),
            "SP.sst",
            vec![Entry::put(b"k000".to_vec(), b"old".to_vec(), 10)],
        ));
        // 6th file — NOT in the batch (truncate(5) drops it). NEWER tombstone
        // for k000. min_key = a000 so it sorts BEFORE the merged output.
        l1.push(build_sst(
            dir.path(),
            "TD.sst",
            vec![
                Entry::put(b"a000".to_vec(), b"x".to_vec(), 20),
                Entry::delete(b"k000".to_vec(), 20),
            ],
        ));

        let levels: Arc<Mutex<Vec<Vec<SsTableInfo>>>> =
            Arc::new(Mutex::new(vec![Vec::new(), l1, Vec::new()]));
        let (_tx, rx) = mpsc::channel();
        let (ntx, _nrx) = mpsc::channel();
        let mut worker = CompactionWorker {
            levels: levels.clone(),
            options: StorageOptions {
                data_dir: dir.path().to_path_buf(),
                memtable_size_limit: 1024 * 1024,
                size_ratio: 2,
                ..Default::default()
            },
            sst_counter: Arc::new(AtomicU64::new(1000)),
            receiver: rx,
            notif_sender: Some(ntx),
            sst_cache: HashMap::new(),
            pending_deletions: Vec::new(),
            cooldown_until: None,
        };

        worker.merge_small_files().expect("merge_small_files");

        let w = read_winner(&levels, b"k000");
        assert!(
            !matches!(w, Some(Some(_))),
            "newer tombstone must beat older put after merge_small_files (got {:?})",
            w
        );
    }
}
