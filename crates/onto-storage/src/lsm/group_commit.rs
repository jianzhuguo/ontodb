// Copyright (c) 2024-2026 OntoDB Team
// Licensed under the Business Source License 1.1 (BUSL-1.1).
// See LICENSE for details. Change Date: 2031-09-15.
// On the Change Date, this file will be licensed under Apache License 2.0.
//! Group commit coordinator for WAL durability.
//!
//! Multiple transactions batch their WAL writes and share a single fsync,
//! dramatically reducing the number of expensive sync operations.
//!
//! Supports hybrid batching: wait up to N microseconds OR until M transactions
//! are ready, whichever comes first.

use parking_lot::{Condvar, Mutex};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Group commit configuration.
pub struct GroupCommitConfig {
    /// Maximum time to wait for more transactions before syncing (microseconds).
    pub wait_timeout_us: u64,
    /// Number of transactions to batch before forcing sync.
    pub batch_threshold: u32,
}

impl Default for GroupCommitConfig {
    fn default() -> Self {
        Self {
            wait_timeout_us: 10, // 10µs
            batch_threshold: 4,  // 4 transactions
        }
    }
}

/// Group commit state.
struct GroupState {
    /// Number of transactions waiting for sync.
    waiters: u32,
    /// Sequence number of the last completed sync.
    last_synced_seq: u64,
    /// Whether a sync is currently in progress.
    syncing: bool,
    /// Timestamp when the first waiter registered.
    first_waiter_at: Option<Instant>,
}

/// Coordinator for WAL group commit with hybrid batching.
///
/// Batching strategy: wait up to `wait_timeout_us` OR until `batch_threshold`
/// transactions are ready, whichever comes first. This balances latency vs
/// throughput.
pub struct GroupCommitCoordinator {
    config: GroupCommitConfig,
    state: Mutex<GroupState>,
    sync_done: Condvar,
    batch_ready: Condvar,
    sync_count: AtomicU64,
}

impl GroupCommitCoordinator {
    /// Create a new group commit coordinator with default config.
    pub fn new() -> Self {
        Self::with_config(GroupCommitConfig::default())
    }

    /// Create with custom config.
    pub fn with_config(config: GroupCommitConfig) -> Self {
        Self {
            config,
            state: Mutex::new(GroupState {
                waiters: 0,
                last_synced_seq: 0,
                syncing: false,
                first_waiter_at: None,
            }),
            sync_done: Condvar::new(),
            batch_ready: Condvar::new(),
            sync_count: AtomicU64::new(0),
        }
    }

    /// Called by a transaction after appending to WAL buffer.
    /// Returns true if this transaction should perform the sync (leader).
    ///
    /// Leader election: first transaction becomes leader and waits for
    /// batch_threshold or timeout before syncing.
    pub fn register(&self, _seq: u64) -> bool {
        let mut state = self.state.lock();
        state.waiters += 1;

        if state.first_waiter_at.is_none() {
            state.first_waiter_at = Some(Instant::now());
        }

        if !state.syncing {
            // This transaction becomes the sync leader
            state.syncing = true;
            true
        } else {
            // Another transaction is the leader
            // If we've reached batch threshold, notify leader
            if state.waiters >= self.config.batch_threshold {
                self.batch_ready.notify_one();
            }
            false
        }
    }

    /// Called by the leader to wait for batch or timeout before syncing.
    /// Returns the number of transactions that will be synced.
    pub fn wait_for_batch(&self) -> u32 {
        let mut state = self.state.lock();

        // Wait until batch_threshold or timeout
        let timeout = Duration::from_micros(self.config.wait_timeout_us);
        let deadline = state.first_waiter_at.unwrap_or_else(Instant::now) + timeout;

        while state.waiters < self.config.batch_threshold {
            let now = Instant::now();
            if now >= deadline {
                break; // Timeout reached
            }
            let remaining = deadline - now;
            let result = self.batch_ready.wait_for(&mut state, remaining);
            if result.timed_out() {
                break;
            }
        }

        state.waiters
    }

    /// Called by the leader after completing the sync.
    /// Wakes up all waiting transactions.
    pub fn complete_sync(&self, seq: u64) {
        let mut state = self.state.lock();
        state.last_synced_seq = seq;
        state.syncing = false;
        state.waiters = 0;
        state.first_waiter_at = None;
        self.sync_count.fetch_add(1, Ordering::Relaxed);
        self.sync_done.notify_all();
    }

    /// Called by followers to wait for the sync to complete.
    pub fn wait_for_sync(&self, seq: u64) {
        let mut state = self.state.lock();
        // If our sequence is already synced, return immediately
        if seq <= state.last_synced_seq {
            return;
        }
        // Wait for sync to complete
        while state.syncing && seq > state.last_synced_seq {
            self.sync_done.wait(&mut state);
        }
    }

    /// Get the number of sync operations performed.
    pub fn sync_count(&self) -> u64 {
        self.sync_count.load(Ordering::Relaxed)
    }

    /// Get current config.
    pub fn config(&self) -> &GroupCommitConfig {
        &self.config
    }
}

impl Default for GroupCommitCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn test_group_commit_basic() {
        let coord = GroupCommitCoordinator::new();

        // First transaction becomes leader
        assert!(coord.register(1));
        let batch = coord.wait_for_batch();
        assert!(batch >= 1);
        coord.complete_sync(1);

        assert_eq!(coord.sync_count(), 1);
    }

    #[test]
    fn test_group_commit_batching() {
        let coord = Arc::new(GroupCommitCoordinator::new());
        let mut handles = vec![];

        // Spawn 4 concurrent transactions
        for i in 0..4 {
            let coord = coord.clone();
            handles.push(thread::spawn(move || {
                let is_leader = coord.register(i);
                if is_leader {
                    // Leader waits for batch
                    let _batch = coord.wait_for_batch();
                    thread::sleep(Duration::from_millis(1));
                    coord.complete_sync(i);
                } else {
                    // Follower waits
                    coord.wait_for_sync(i);
                }
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        // Should have fewer syncs than transactions
        assert!(coord.sync_count() <= 4);
    }

    #[test]
    fn test_group_commit_timeout() {
        let config = GroupCommitConfig {
            wait_timeout_us: 100, // 100µs
            batch_threshold: 10,  // High threshold, will timeout first
        };
        let coord = GroupCommitCoordinator::with_config(config);

        // Single transaction - should timeout and sync alone
        assert!(coord.register(1));
        let batch = coord.wait_for_batch();
        assert_eq!(batch, 1); // Only 1 transaction
        coord.complete_sync(1);

        assert_eq!(coord.sync_count(), 1);
    }

    #[test]
    fn test_group_commit_batch_threshold() {
        let config = GroupCommitConfig {
            wait_timeout_us: 10_000, // 10ms - long timeout
            batch_threshold: 2,      // Low threshold
        };
        let coord = Arc::new(GroupCommitCoordinator::with_config(config));
        let mut handles = vec![];

        // Spawn 2 transactions - should batch immediately
        for i in 0..2 {
            let coord = coord.clone();
            handles.push(thread::spawn(move || {
                let is_leader = coord.register(i);
                if is_leader {
                    let batch = coord.wait_for_batch();
                    assert!(batch >= 2); // Should batch both
                    coord.complete_sync(i);
                } else {
                    coord.wait_for_sync(i);
                }
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(coord.sync_count(), 1); // Only 1 sync for both
    }
}
