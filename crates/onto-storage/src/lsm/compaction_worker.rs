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

/// Hard cap on compressed input bytes pulled into one merge. Keeps open-SST
/// handles, builder buffers and merge working set inside a predictable
/// envelope — a 20-SST / 24M-entry grab was an OOM source under load.
pub(crate) const MAX_MERGE_INPUT_BYTES: u64 = 256 * 1024 * 1024;
/// Hard cap on SST files opened by one merge (source + next-level overlap).
pub(crate) const MAX_SSTABLES_PER_MERGE: usize = 8;

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
    /// Last successful L6 self-merge, used to rate-limit rewrites.
    last_l6_merge: Option<std::time::Instant>,
    /// Last time each level was compacted. Drives a starvation boost so a
    /// permanently-busy deepest level cannot lock mid-levels (L1 sat at
    /// score 9.9 for 15+ min while L6 held 10.8 and won every round).
    last_compact: Vec<Option<std::time::Instant>>,
    /// Last memtable flush, used to defer L6 rewrites during write peaks.
    last_flush_at: Option<std::time::Instant>,
    /// Shared write-backpressure signal: `max_level_score * 100`. The engine
    /// reads this on the write path and sleeps when compaction is behind.
    debt: Arc<AtomicU64>,
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
        Arc<AtomicU64>,
    ) {
        let initial_levels_len = initial_levels.len();
        let levels = Arc::new(Mutex::new(initial_levels));
        let levels_clone = levels.clone();
        let debt = Arc::new(AtomicU64::new(0));
        let debt_clone = debt.clone();
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
                last_l6_merge: None,
                last_flush_at: None,
                last_compact: vec![None; initial_levels_len],
                debt: debt_clone,
            };
            worker.run();
        }).expect("failed to spawn compaction worker thread");

        (levels, sender, notif_receiver, handle, debt)
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
                    self.last_flush_at = Some(std::time::Instant::now());
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
                    // Keep compacting until no more is needed — but cap the
                    // number of consecutive passes to avoid memory buildup.
                    // Each merge holds SST input in memory; running N passes
                    // back-to-back can OOM before the allocator reclaims.
                    const MAX_CONSECUTIVE_COMPACTIONS: u32 = 6;
                    const PASS_COOLDOWN_MS: u64 = 200;
                    let mut consecutive: u32 = 0;
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
                            for level in 0..levels.len() {
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
                        if consecutive >= MAX_CONSECUTIVE_COMPACTIONS {
                            tracing::debug!(
                                "FlushAndNotify: reached {} consecutive compactions, yielding",
                                consecutive
                            );
                            break;
                        }
                        if let Err(e) = self.try_compact() {
                            tracing::error!("Background compaction failed: {}", e);
                            break;
                        }
                        consecutive += 1;
                        // Brief pause between passes to let the allocator reclaim.
                        thread::sleep(Duration::from_millis(PASS_COOLDOWN_MS));
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
                    // Poll faster while any level is critically crowded so a
                    // deep backlog drains in minutes, not one batch per minute.
                    let critical = {
                        let levels = self.levels.lock().unwrap_or_else(|e| e.into_inner());
                        levels.iter().any(|l| l.len() > 200)
                    };
                    let interval = if critical {
                        periodic_interval / 4
                    } else {
                        periodic_interval
                    };
                    if idle_ticks >= interval {
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

        // Cooldown: pause between compaction passes to let the allocator reclaim.
        // Without this, back-to-back merges can accumulate memory faster than
        // the OS returns it, causing OOM even with smaller batch sizes.
        thread::sleep(Duration::from_millis(100));

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
        // 0.992). When any non-deepest level exceeds FILE_COUNT_TRIGGER,
        // force one bounded merge. Deepest-level file count is driven by the
        // file-count score in `compaction_score` (see below) — it must NOT
        // return early here or L6 crowds out L0–L5 forever.
        const FILE_COUNT_TRIGGER: usize = 50;
        let crowded: Option<(usize, usize)> = {
            let levels = self.levels.lock().unwrap_or_else(|e| e.into_inner());
            let deepest = levels.len().saturating_sub(1);
            levels
                .iter()
                .enumerate()
                .map(|(i, l)| (i, l.len()))
                .filter(|(i, n)| *n > FILE_COUNT_TRIGGER && *i != deepest)
                .max_by_key(|(_, n)| *n)
        };
        if let Some((level, nfiles)) = crowded {
            if level == 0 {
                // L0: collapse in place first. L0 files mutually overlap
                // (memtable flushes), so L0→L1 tombstone-placement safety
                // defers almost every batch and the level never drains.
                tracing::info!(
                    "File-count trigger: L0 has {} files (> {}), self-merging batch",
                    nfiles,
                    FILE_COUNT_TRIGGER
                );
                self.merge_self_batch_at(0)?;
                self.compact_level(0)?;
                if let Some(slot) = self.last_compact.get_mut(0) {
                    *slot = Some(std::time::Instant::now());
                }
            } else {
                tracing::info!(
                    "File-count trigger: L{} has {} files (> {}), merging batch",
                    level,
                    nfiles,
                    FILE_COUNT_TRIGGER
                );
                self.compact_level(level)?;
                if let Some(slot) = self.last_compact.get_mut(level) {
                    *slot = Some(std::time::Instant::now());
                }
            }
            // Fall through to score-based selection so a crowded mid-level
            // cannot starve the rest of the tree (notably L0 and L6).
        }

        // Find the level with the highest compaction score — ALL levels,
        // including the deepest. L6 is scored by file count (see
        // `compaction_score`); executing it uses self-merge rather than
        // `compact_level` (which pushes into a non-existent next level).
        let (best_level, best_score, best_files) = {
            let levels = self.levels.lock().unwrap_or_else(|e| e.into_inner());
            let mut best_level = None;
            let mut best_score = 0.0f64;
            let mut best_files = 0usize;

            for level in 0..levels.len() {
                let score = self.compaction_score(&levels, level);
                // Starvation boost: a level that keeps losing the scheduler
                // grows its score until it gets a turn. 2× after 2 min idle.
                let idle = self
                    .last_compact
                    .get(level)
                    .and_then(|t| *t)
                    .map(|t| t.elapsed().as_secs_f64())
                    .unwrap_or(f64::INFINITY);
                let boost = 1.0 + (idle / 120.0).min(3.0);
                let boosted = score * boost;
                if boosted > best_score {
                    best_score = boosted;
                    best_level = Some(level);
                    best_files = levels[level].len();
                }
            }

            (best_level, best_score, best_files)
        };

        if let Some(level) = best_level {
            if best_score > 1.0 {
                let deepest = {
                    let levels = self.levels.lock().unwrap_or_else(|e| e.into_inner());
                    levels.len().saturating_sub(1)
                };
                if level == deepest {
                    if self.l6_merge_allowed(best_files) {
                        // One batch nets only -1..-2 files; against a backlog
                        // of several hundred that never converges and the
                        // score>1 trigger re-fires forever (observed: score 11
                        // at 710 files, one 4-file batch per minute). When the
                        // deepest level is critically crowded, run several
                        // consecutive bounded batches in this pass instead.
                        const CRITICAL_FILES: usize = 200;
                        let passes = if best_files > CRITICAL_FILES { 8 } else { 1 };
                        for pass in 0..passes {
                            let files_now = {
                                let levels =
                                    self.levels.lock().unwrap_or_else(|e| e.into_inner());
                                levels.get(deepest).map(|l| l.len()).unwrap_or(0)
                            };
                            if files_now <= 64 {
                                break;
                            }
                            tracing::info!(
                                "Background compaction: L{} self-merge pass {}/{} score={:.2} files={}",
                                level,
                                pass + 1,
                                passes,
                                best_score,
                                files_now
                            );
                            self.merge_last_level_batch()?;
                            self.last_l6_merge = Some(std::time::Instant::now());
                            if let Some(slot) = self.last_compact.get_mut(level) {
                                *slot = Some(std::time::Instant::now());
                            }
                            if pass + 1 < passes {
                                // Let the allocator reclaim between batches.
                                thread::sleep(Duration::from_millis(200));
                            }
                        }
                    }
                } else {
                    tracing::info!(
                        "Background compaction triggered: L{} score={:.2}",
                        level,
                        best_score
                    );
                    self.compact_level(level)?;
                    if let Some(slot) = self.last_compact.get_mut(level) {
                        *slot = Some(std::time::Instant::now());
                    }
                }
            }
        }

        self.publish_debt();
        Ok(())
    }

    /// Deepest-level rewrite is expensive (full read + compress + write of
    /// large SSTs) and has been a crash hotspot. Gate it on:
    /// - a minimum interval between rewrites,
    /// - write-peak deferral (recent memtable flush),
    /// - override when the level is critically crowded.
    fn l6_merge_allowed(&self, nfiles: usize) -> bool {
        const CRITICAL_FILES: usize = 200;
        const MIN_INTERVAL: Duration = Duration::from_secs(30);
        const WRITE_PEAK_QUIET: Duration = Duration::from_secs(5);

        if nfiles > CRITICAL_FILES {
            return true;
        }
        if let Some(t) = self.last_l6_merge {
            if t.elapsed() < MIN_INTERVAL {
                return false;
            }
        }
        if let Some(t) = self.last_flush_at {
            if t.elapsed() < WRITE_PEAK_QUIET {
                return false;
            }
        }
        true
    }

    /// Computes compaction urgency score for a level.
    ///
    /// Two independent signals, take the dominant one:
    /// - **size** vs theoretical target (`memtable * size_ratio^n`). Misses
    ///   crowding on a huge last level and sits in deadzones (L2 @ 0.992).
    /// - **file count** vs per-level watermark. Directly tracks read/iterator
    ///   amp and is what production actually feels.
    ///
    /// Score > 1.0 means "should compact now".
    fn compaction_score(&self, levels: &[Vec<SsTableInfo>], level: usize) -> f64 {
        let file_count = levels[level].len();
        if file_count == 0 {
            return 0.0;
        }
        let deepest = levels.len().saturating_sub(1);

        // Per-level healthy file-count watermarks.
        let target_files: f64 = if level == 0 {
            8.0
        } else if level == deepest {
            64.0
        } else {
            32.0
        };
        let file_score = if file_count as f64 > target_files {
            1.0 + (file_count as f64 - target_files) / target_files
        } else {
            // Soft pressure: half credit at half watermark.
            file_count as f64 / (target_files * 2.0)
        };

        // Size signal. Deepest level target is intentionally huge, so size
        // never fires there — file_score is the real driver.
        let size: u64 = levels[level].iter().map(|s| s.size).sum();
        let target = self.target_level_size(level);
        let size_score = if target == 0 {
            0.0
        } else {
            size as f64 / target as f64
        };

        let mut score = if level == deepest {
            file_score
        } else {
            size_score.max(file_score)
        };

        // L0 files mutually overlap — drain even when total size is small.
        if level == 0 && file_count > 8 {
            score = score.max(1.1);
        }
        // Severely crowded levels get a kick so they win the scheduler.
        // Do NOT apply the kick to the deepest level — L6 self-merge is
        // already expensive (full read + compress + write of large SSTs),
        // and the multiplier just causes cascading OOM.
        if file_count as f64 > target_files * 3.0 && level != deepest {
            score *= 1.5;
        }
        score
    }

    /// Publish current max score so the write path can apply backpressure.
    fn publish_debt(&self) {
        let max_score = {
            let levels = self.levels.lock().unwrap_or_else(|e| e.into_inner());
            (0..levels.len())
                .map(|i| self.compaction_score(&levels, i))
                .fold(0.0f64, f64::max)
        };
        self.debt.store((max_score * 100.0) as u64, Ordering::Relaxed);
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
                // Bound by BOTH count and compressed bytes.
                let count = (levels[0].len() / 2).max(4).min(levels[0].len()).min(8);
                let mut picked: Vec<SsTableInfo> = Vec::new();
                let mut total: u64 = 0;
                for s in levels[0].iter().take(count) {
                    if !picked.is_empty() && total + s.size > MAX_MERGE_INPUT_BYTES {
                        break;
                    }
                    total += s.size;
                    picked.push(s.clone());
                }
                picked
            } else {
                if levels[level].is_empty() {
                    return Ok(());
                }
                // Cap source SSTables by count AND compressed bytes so a merge
                // never opens more working set than MAX_MERGE_INPUT_BYTES.
                let mut sorted: Vec<SsTableInfo> = levels[level].clone();
                sorted.sort_by_key(|s| s.size);
                let mut picked: Vec<SsTableInfo> = Vec::new();
                let mut total: u64 = 0;
                for s in sorted {
                    if picked.len() >= 4 {
                        break;
                    }
                    if !picked.is_empty() && total + s.size > MAX_MERGE_INPUT_BYTES {
                        break;
                    }
                    if picked.is_empty() && s.size > MAX_MERGE_INPUT_BYTES {
                        break;
                    }
                    total += s.size;
                    picked.push(s);
                }
                picked
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
        let max_overlap = MAX_SSTABLES_PER_MERGE.saturating_sub(ssts_to_compact.len()).max(2);
        let source_bytes: u64 = ssts_to_compact.iter().map(|s| s.size).sum();
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
            // Bound next-level contribution by file count AND total bytes.
            let mut picked: Vec<SsTableInfo> = Vec::new();
            let mut total = source_bytes;
            for s in kept {
                if picked.len() >= max_overlap {
                    break;
                }
                if !picked.is_empty() && total + s.size > MAX_MERGE_INPUT_BYTES {
                    break;
                }
                total += s.size;
                picked.push(s);
            }
            (picked, empty_next)
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
                // L0 files mutually overlap, so one L1 tombstone span can be
                // shadowed by the entire L0 level. A fixed cap of 40 defers
                // every L1 batch (liveness trap — L1 stuck at score 10.57).
                // Scale with the shallow population so expansion can actually
                // pull the shadowers in; hard-capped to bound open SSTs.
                let expand_cap = if level == 0 {
                    (ssts_to_compact.len() * 4).max(EXPAND_CAP)
                } else {
                    (shallow.len() + ssts_to_compact.len() + next_level_ssts.len())
                        .max(EXPAND_CAP)
                        .min(32)
                };
                let mut expanded: std::collections::HashSet<PathBuf> =
                    std::collections::HashSet::new();
                let mut kept_bytes: u64 = 0;
                let next_level_bytes: u64 = next_level_ssts.iter().map(|s| s.size).sum();
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
                        kept_bytes += s.size;
                        kept.push(s.clone());
                        continue;
                    }
                    let projected = ssts_to_compact.len()
                        + next_level_ssts.len()
                        + expanded.len()
                        + shadowed.len();
                    // Byte budget as well as count: expand used to be
                    // count-only, so dozens of shallow L0 files blew heap
                    // (observed 897MB alloc / process death mid-merge).
                    let shadowed_bytes: u64 = shadowed.iter().map(|o| o.size).sum();
                    let projected_bytes =
                        kept_bytes + s.size + shadowed_bytes + next_level_bytes;
                    if projected > expand_cap || projected_bytes > MAX_MERGE_INPUT_BYTES {
                        excluded_inputs.push(s.path.clone());
                        continue;
                    }
                    for o in shadowed {
                        if expanded.insert(o.path.clone()) {
                            kept_bytes += o.size;
                            kept.push(o);
                        }
                    }
                    kept_bytes += s.size;
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
            // Every input was deferred by the tombstone guard. Sinking them to
            // the next level would resurrect rows (shallow puts would win), but
            // skipping entirely is a liveness trap — L1 was observed stuck at
            // score 10.57 because every round deferred its whole batch and the
            // level never drained. Fall back to a same-level self-merge of the
            // OLDEST files (position-preserving splice): tombstones stay at
            // this level, so the guard is not needed, and file count still
            // drops.
            tracing::info!(
                level = level,
                "tombstone guard deferred entire batch — same-level self-merge fallback"
            );
            return self.merge_self_batch_at(level);
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

            // Remove compacted SSTables from current level AND any expanded
            // shallow levels (tombstone guard pulls L0/L1 survivors into the
            // batch; they must leave their own level or the merge duplicates
            // them into next_level while the original still shadows on read).
            let compact_paths: std::collections::HashSet<PathBuf> =
                ssts_to_compact.iter().map(|s| s.path.clone()).collect();
            for lvl in levels.iter_mut().take(level + 1) {
                lvl.retain(|s| !compact_paths.contains(&s.path));
            }

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
    fn merge_self_batch_at(&mut self, level: usize) -> Result<()> {
        // Oldest-prefix batch: list position is the age contract (see the
        // positional-invariant note above). Non-L0 batches are half the size
        // to keep the open-SST working set bounded on larger files.
        let batch_limit: usize = if level == 0 { 16 } else { 8 };

        // Snapshot the oldest L0 SSTs (low indices = early flushes = stale).
        // Empty shells (<64 B) are discarded rather than merged.
        let (batch, empty_ssts) = {
            let mut levels = self.levels.lock().unwrap_or_else(|e| e.into_inner());
            if levels.len() <= level || levels[level].len() < 2 {
                return Ok(());
            }
            let mut ssts: Vec<SsTableInfo> = Vec::new();
            let mut empty: Vec<SsTableInfo> = Vec::new();
            for s in levels[level].iter() {
                if ssts.len() + empty.len() >= batch_limit {
                    break;
                }
                if s.size < 64 {
                    empty.push(s.clone());
                } else {
                    ssts.push(s.clone());
                }
            }
            for e in &empty {
                levels[level].retain(|s| s.path != e.path);
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
                        "L{} self-merge iterator failed: {}",
                        level,
                        e
                    ))
                })?;

        // Output cut size must beat the compression gap. `current_size`
        // counts raw key+value bytes while blocks land zstd-compressed (~3x).
        // A 64 MB raw cut regurgitates more outputs than inputs (no-win
        // discard → L1 livelocked at score 10.57: "8 inputs produced 11
        // outputs"). Cut so estimated outputs < inputs: raw_est ≈ disk*3,
        // need raw_est/cut < n. Floor 256 MB; ceiling bounds builder.data
        // (compressed ≈ raw/3, so ≤ ~170 MB resident).
        let batch_disk: u64 = batch.iter().map(|s| s.size).sum();
        let est_raw = batch_disk.saturating_mul(3);
        let max_outputs = (batch.len() as u64).saturating_sub(1).max(1);
        let target_sst_size: usize = (est_raw / max_outputs)
            .clamp(256 * 1024 * 1024, 320 * 1024 * 1024) as usize;
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
                    .join(format!("L{}_{}.sst", level, sst_id));
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
                .join(format!("L{}_{}.sst", level, sst_id));
            let sst = builder.build(&sst_path)?;
            let metadata = fs::metadata(&sst_path)?;
            new_ssts.push(SsTableInfo {
                path: sst_path,
                size: metadata.len(),
                min_key: batch_first_key.unwrap_or_default(),
                max_key: sst.max_key().to_vec(),
            });
        }

        let in_bytes: u64 = batch.iter().map(|s| s.size).sum();
        let out_bytes: u64 = new_ssts.iter().map(|s| s.size).sum();
        // Win if file count drops OR on-disk size drops ≥10%. Size wins matter
        // for size-driven scores (L1 was stuck at 10.57 on size_score); a pure
        // count check discarded real shrinkage and livelocked the level.
        let count_win = new_ssts.len() < batch.len();
        let size_win = out_bytes.saturating_mul(10) < in_bytes.saturating_mul(9);
        if !count_win && !size_win {
            // No progress. Drop the outputs — keeping them would churn.
            tracing::warn!(
                "L{} self-merge no-win: {} inputs produced {} outputs (target {} B), discarding",
                level,
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
            Self::splice_outputs(&mut levels, level, &batch, new_ssts);

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
            "L{} self-merge: {} → {} SSTables ({} entries, {} deduped, {} empty shells dropped)",
            level,
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
        // Multi-in / few-out rewrite. The old 2-in-1-out loop reduced file
        // count by one per pass and re-fired every ~60s — a permanent write
        // hotspot. Instead, absorb a batch of the smallest files and split
        // the output at TARGET_OUTPUT_SIZE so one pass drops several files.
        //
        // Batch size is capped at 4 to limit peak memory: each input SST is
        // ~100-140MB and the streaming merge holds block data in memory.
        // 4 files ≈ 400-560MB peak, vs 8 files ≈ 1GB+ which causes OOM.
        const MAX_BATCH_FILES: usize = 4;
        const MAX_INPUT_BYTES: u64 = 128 * 1024 * 1024; // 128MB cap
        // Uncompressed output target. 128MB keeps each output small enough
        // that a batch of 4 inputs collapses into 1-2 files while limiting
        // the builder's in-memory accumulation.
        // Uncompressed cut must beat the ~3x zstd gap or 4 inputs regenerate
        // 4 outputs (observed: before=4 after=4 every pass, L6 stuck at 691
        // files / score 10.8 and starving L1). Size the cut from the batch.
        const TARGET_OUTPUT_MIN: u64 = 256 * 1024 * 1024;
        const TARGET_OUTPUT_MAX: u64 = 384 * 1024 * 1024;
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
        // Smallest first: the tail of small files is what inflates file count.
        let (level_idx, batch) = {
            let levels = self.levels.lock().unwrap_or_else(|e| e.into_inner());
            let deepest = levels.len().saturating_sub(1);
            if deepest == 0 {
                return Ok(());
            }

            let mut sorted: Vec<SsTableInfo> = levels[deepest].clone();
            sorted.sort_by_key(|s| s.size);
            let mut picked: Vec<SsTableInfo> = Vec::new();
            let mut total: u64 = 0;
            for s in sorted {
                if picked.len() >= MAX_BATCH_FILES {
                    break;
                }
                if !picked.is_empty() && total + s.size > MAX_INPUT_BYTES {
                    break;
                }
                if picked.is_empty() && s.size > MAX_INPUT_BYTES {
                    // A single file already over the cap cannot form a safe batch.
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

        let batch_disk: u64 = batch.iter().map(|s| s.size).sum();
        let est_raw = batch_disk.saturating_mul(3);
        let max_outputs = (batch.len() as u64).saturating_sub(1).max(1);
        let target_output_size: u64 =
            (est_raw / max_outputs).clamp(TARGET_OUTPUT_MIN, TARGET_OUTPUT_MAX);

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
        // Size the bloom from input bytes (conservative 32-byte min entry).
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

            if current_size >= target_output_size {
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

        // No file-count win: drop the outputs instead of splicing. Keeping a
        // non-reducing rewrite is what churned L6 (before=4 after=4 forever).
        if new_ssts.len() >= batch.len() {
            tracing::warn!(
                level = level_idx,
                before = batch.len(),
                after = new_ssts.len(),
                target = target_output_size,
                "last-level batch did not reduce file count — discarding outputs"
            );
            for sst in &new_ssts {
                let _ = fs::remove_file(&sst.path);
            }
            return Ok(());
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
        Ok(())
    }

    /// Merge small SSTables into larger ones to prevent file count explosion.
    /// Uses streaming merge (StreamingMergeIterator) to avoid loading all entries
    /// into memory. Processes a BATCH of small files per call to bound I/O.
    fn merge_small_files(&mut self) -> Result<()> {
        const SMALL_FILE_THRESHOLD: u64 = 16 * 1024 * 1024; // 16MB
        const MIN_FILES_TO_MERGE: usize = 4;
        const MERGE_BATCH_SIZE: usize = 4;
        // Uncompressed cut must beat the ~3x zstd gap or N small inputs
        // regenerate N outputs (observed 4→4 churn on L5/L6). Size the cut
        // from the batch; floor 256MB raw, ceiling bounds builder.data.
        const TARGET_OUTPUT_MIN: u64 = 256 * 1024 * 1024;
        const TARGET_OUTPUT_MAX: u64 = 320 * 1024 * 1024;

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

        // Limit to one batch to bound memory usage (count AND compressed bytes).
        small_files.truncate(MERGE_BATCH_SIZE);
        let mut picked: Vec<SsTableInfo> = Vec::new();
        let mut total: u64 = 0;
        for s in small_files {
            if !picked.is_empty() && total + s.size > MAX_MERGE_INPUT_BYTES {
                break;
            }
            total += s.size;
            picked.push(s);
        }
        small_files = picked;
        let batch_size = small_files.len();
        let batch_disk: u64 = small_files.iter().map(|s| s.size).sum();
        let est_raw = batch_disk.saturating_mul(3);
        let max_outputs = (batch_size as u64).saturating_sub(1).max(1);
        let target_output_size: u64 =
            (est_raw / max_outputs).clamp(TARGET_OUTPUT_MIN, TARGET_OUTPUT_MAX);

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
            SsTableBuilder::with_expected_entries(target_output_size as usize / 32);
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
            if current_size >= target_output_size {
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

        // No file-count win: drop outputs instead of splicing 4→4 forever.
        if new_count >= batch_size {
            tracing::warn!(
                level = target_level,
                before = batch_size,
                after = new_count,
                target = target_output_size,
                "small-file merge did not reduce file count — discarding outputs"
            );
            for sst in &new_ssts {
                let _ = fs::remove_file(&sst.path);
            }
            return Ok(());
        }

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
            last_l6_merge: None,
            last_flush_at: None,
            last_compact: vec![None; 8],
            debt: Arc::new(AtomicU64::new(0)),
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
            last_l6_merge: None,
            last_flush_at: None,
            last_compact: vec![None; 8],
            debt: Arc::new(AtomicU64::new(0)),
        };

        worker.merge_self_batch_at(0).expect("self-merge");

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
            last_l6_merge: None,
            last_flush_at: None,
            last_compact: vec![None; 8],
            debt: Arc::new(AtomicU64::new(0)),
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
