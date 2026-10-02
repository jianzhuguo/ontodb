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
use std::path::PathBuf;
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
            };
            worker.run();
        }).expect("failed to spawn compaction worker thread");

        (levels, sender, notif_receiver, handle)
    }

    /// Main loop: waits for messages and performs compaction when needed.
    fn run(&mut self) {
        loop {
            match self.receiver.recv_timeout(Duration::from_millis(100)) {
                Ok(CompactionMsg::Flushed { level: _ }) => {
                    if let Err(e) = self.try_compact() {
                        tracing::error!("Background compaction failed: {}", e);
                    }
                }
                Ok(CompactionMsg::FlushAndNotify) => {
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
                    if let Err(e) = self.try_compact() {
                        tracing::error!("Background compaction failed: {}", e);
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    break;
                }
            }
        }
    }

    /// Checks all levels and performs compaction on the most urgent one.
    fn try_compact(&mut self) -> Result<()> {
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
        size_score + file_penalty
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
    fn compact_level(&mut self, level: usize) -> Result<()> {
        // Step 1: Snapshot SSTables to compact (don't remove from levels yet)
        let (ssts_to_compact, compact_min, compact_max) = {
            let levels = self.levels.lock().unwrap_or_else(|e| e.into_inner());
            if level >= levels.len() - 1 {
                return Ok(());
            }

            let ssts: Vec<SsTableInfo> = if level == 0 {
                let count = (levels[0].len() / 2).max(2).min(levels[0].len());
                levels[0].iter().take(count).cloned().collect()
            } else {
                if levels[level].is_empty() {
                    return Ok(());
                }
                // When file count exceeds penalty threshold (100), compact multiple
                // files at once to accelerate file count reduction.
                let file_count = levels[level].len();
                let excess = file_count.saturating_sub(100);
                if excess > 0 {
                    let batch = excess.min(8).max(1);
                    let mut sorted: Vec<SsTableInfo> = levels[level].clone();
                    sorted.sort_by_key(|s| s.size);
                    sorted.into_iter().take(batch).collect()
                } else {
                    vec![levels[level][0].clone()]
                }
            };

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

            (ssts, min, max)
        };

        // Step 2: Find overlapping SSTables in level N+1 (snapshot, don't remove)
        // Scale overlap cap with number of input files to avoid truncation
        let max_overlap = 16.max(ssts_to_compact.len() * 4);
        let next_level = level + 1;
        let next_level_ssts: Vec<SsTableInfo> = {
            let levels = self.levels.lock().unwrap_or_else(|e| e.into_inner());
            let mut overlapping: Vec<SsTableInfo> = levels[next_level]
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
            overlapping.truncate(max_overlap);
            overlapping
        };

        // Step 3: Streaming merge using min-heap (O(1) memory per SSTable)
        let (deepest_level, levels_snapshot) = {
            let levels = self.levels.lock().unwrap_or_else(|e| e.into_inner());
            (levels.len() - 1, levels.clone())
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
        let mut builder = SsTableBuilder::new();
        builder.set_compression_level(self.options.compression_level);
        let mut new_ssts = Vec::new();
        let mut current_size = 0usize;
        let mut last_key: Option<Vec<u8>> = None;
        let mut batch_first_key: Option<Vec<u8>> = None;
        let mut entry_count = 0u64;

        while merge_iter.has_next() {
            let entry = merge_iter.next().unwrap();
            let key = entry.key;
            let value = entry.value;
            let seq_no = entry.seq_no;
            let kind = entry.kind;

            // Skip duplicate keys (streaming merge gives newest first per key)
            if last_key.as_ref() == Some(&key) {
                continue;
            }

            // Tombstone cleanup
            if kind == EntryKind::Delete {
                let can_drop = next_level >= deepest_level
                    || Self::can_drop_tombstone_static(&key, next_level, &levels_snapshot);
                if can_drop {
                    last_key = Some(key);
                    continue;
                }
            }

            // Track first key of current batch
            if batch_first_key.is_none() {
                batch_first_key = Some(key.clone());
            }

            // Add entry to builder
            current_size += key.len() + value.len() + 16;
            builder.add(&Entry {
                key: key.clone(),
                value,
                seq_no,
                kind,
            });
            entry_count += 1;
            last_key = Some(key);

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

                builder = SsTableBuilder::new();
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

            // Add new merged SSTables
            levels[next_level].extend(new_ssts);
            levels[next_level].sort_by(|a, b| a.min_key.cmp(&b.min_key));

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

    /// Merge small SSTables into larger ones to prevent file count explosion.
    /// Uses streaming merge (StreamingMergeIterator) to avoid loading all entries
    /// into memory. Processes a BATCH of small files per call to bound I/O.
    fn merge_small_files(&mut self) -> Result<()> {
        const SMALL_FILE_THRESHOLD: u64 = 16 * 1024 * 1024; // 16MB
        const MIN_FILES_TO_MERGE: usize = 4;
        const MERGE_BATCH_SIZE: usize = 40;
        // Target output size MUST exceed SMALL_FILE_THRESHOLD to prevent infinite re-merging.
        // Set to 2x threshold so merged files are definitively "not small".
        const TARGET_OUTPUT_SIZE: u64 = 96 * 1024 * 1024; // 96MB

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
        let mut builder = SsTableBuilder::new();
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

            let sst_entry = match entry.kind {
                EntryKind::Put => Entry::put(entry.key.clone(), entry.value, entry.seq_no),
                EntryKind::Delete => Entry::delete(entry.key.clone(), entry.seq_no),
            };
            builder.add(&sst_entry);
            current_size += entry.key.len() as u64 + sst_entry.value.len() as u64 + 16;
            total_entries += 1;
            last_key = Some(entry.key);

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
                builder = SsTableBuilder::new();
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

        // Replace small files with new larger files and schedule old ones for deletion
        {
            let mut levels = self.levels.lock().unwrap_or_else(|e| e.into_inner());
            let small_paths: std::collections::HashSet<PathBuf> =
                small_files.iter().map(|s| s.path.clone()).collect();

            levels[target_level].retain(|s| !small_paths.contains(&s.path));
            levels[target_level].extend(new_ssts);
            levels[target_level].sort_by(|a, b| a.min_key.cmp(&b.min_key));

            self.pending_deletions.extend(small_paths);
        }

        tracing::info!(
            "Small file merge batch complete (L{}): {} small files → {} larger files ({}MB)",
            target_level,
            batch_size,
            new_count,
            total_output_size / 1024 / 1024
        );

        Ok(())
    }

    fn can_drop_tombstone_static(
        key: &[u8],
        from_level: usize,
        levels: &[Vec<SsTableInfo>],
    ) -> bool {
        for level in (from_level + 1)..levels.len() {
            for sst_info in &levels[level] {
                if key >= sst_info.min_key.as_slice() && key <= sst_info.max_key.as_slice() {
                    return false;
                }
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
}
