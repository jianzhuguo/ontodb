//! Group commit coordinator for WAL durability.
//!
//! Multiple transactions batch their WAL writes and share a single fsync,
//! dramatically reducing the number of expensive sync operations.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use parking_lot::{Condvar, Mutex};

/// Group commit state.
struct GroupState {
    /// Number of transactions waiting for sync.
    waiters: u32,
    /// Sequence number of the last completed sync.
    last_synced_seq: u64,
    /// Whether a sync is currently in progress.
    syncing: bool,
}

/// Coordinator for WAL group commit.
///
/// Instead of each transaction calling fsync independently,
/// multiple transactions batch into a single sync operation.
pub struct GroupCommitCoordinator {
    state: Mutex<GroupState>,
    sync_done: Condvar,
    sync_count: AtomicU64,
}

impl GroupCommitCoordinator {
    /// Create a new group commit coordinator.
    pub fn new() -> Self {
        Self {
            state: Mutex::new(GroupState {
                waiters: 0,
                last_synced_seq: 0,
                syncing: false,
            }),
            sync_done: Condvar::new(),
            sync_count: AtomicU64::new(0),
        }
    }

    /// Called by a transaction after appending to WAL buffer.
    /// Returns true if this transaction should perform the sync (leader).
    pub fn register_and_maybe_leader(&self, seq: u64) -> bool {
        let mut state = self.state.lock();
        state.waiters += 1;

        if !state.syncing {
            // This transaction becomes the sync leader
            state.syncing = true;
            true
        } else {
            // Another transaction is already syncing; wait for it
            false
        }
    }

    /// Called by the leader after completing the sync.
    /// Wakes up all waiting transactions.
    pub fn complete_sync(&self, seq: u64) {
        let mut state = self.state.lock();
        state.last_synced_seq = seq;
        state.syncing = false;
        state.waiters = 0;
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
}

impl Default for GroupCommitCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn test_group_commit_basic() {
        let coord = GroupCommitCoordinator::new();
        
        // First transaction becomes leader
        assert!(coord.register_and_maybe_leader(1));
        coord.complete_sync(1);
        
        // Second transaction after sync
        assert!(coord.register_and_maybe_leader(2));
        coord.complete_sync(2);
        
        assert_eq!(coord.sync_count(), 2);
    }

    #[test]
    fn test_group_commit_batching() {
        let coord = Arc::new(GroupCommitCoordinator::new());
        let mut handles = vec![];

        // Spawn 4 concurrent transactions
        for i in 0..4 {
            let coord = coord.clone();
            handles.push(thread::spawn(move || {
                let is_leader = coord.register_and_maybe_leader(i);
                if is_leader {
                    // Leader does the sync
                    thread::sleep(Duration::from_millis(10));
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
        assert!(coord.sync_count() < 4);
    }
}
