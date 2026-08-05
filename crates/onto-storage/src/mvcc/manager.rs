//! Transaction manager: coordinates all active transactions.

use crate::mvcc::transaction::{Transaction, TxnStatus, WriteOp};
use crate::mvcc::visibility::Visibility;
use onto_core::{Key, Result, SeqNo, Value};
use std::collections::{BTreeMap, BTreeSet};

/// Manages all transactions and their lifecycle.
///
/// Responsibilities:
/// - Assign unique transaction IDs
/// - Track active transactions for visibility checks
/// - Commit: flush write buffer to the storage engine
/// - Abort: discard write buffer
/// - Garbage collect old versions (future)
pub struct TxnManager {
    /// Monotonically increasing transaction ID counter.
    next_txn_id: SeqNo,

    /// All active transactions, keyed by txn ID.
    active: BTreeMap<SeqNo, Transaction>,

    /// Set of active transaction IDs (for quick visibility checks).
    active_set: BTreeSet<SeqNo>,

    /// The highest committed transaction ID.
    /// New transactions snapshot at this value.
    last_committed: SeqNo,
}

impl TxnManager {
    /// Creates a new transaction manager.
    pub fn new() -> Self {
        Self {
            next_txn_id: 1, // Start from 1, 0 is reserved
            active: BTreeMap::new(),
            active_set: BTreeSet::new(),
            last_committed: 0,
        }
    }

    /// Begins a new transaction. Takes the current engine seq_no as snapshot timestamp.
    pub fn begin(&mut self, current_seq: SeqNo) -> SeqNo {
        let txn_id = self.next_txn_id;
        self.next_txn_id += 1;

        let mut txn = Transaction::new(txn_id);
        txn.snapshot_ts = current_seq; // Snapshot at current engine state
        self.active.insert(txn_id, txn);
        self.active_set.insert(txn_id);

        txn_id
    }

    /// Gets a reference to a transaction by ID.
    pub fn get(&self, txn_id: SeqNo) -> Option<&Transaction> {
        self.active.get(&txn_id)
    }

    /// Gets a mutable reference to a transaction by ID.
    pub fn get_mut(&mut self, txn_id: SeqNo) -> Option<&mut Transaction> {
        self.active.get_mut(&txn_id)
    }

    /// Commits a transaction. Returns the write operations to apply.
    ///
    /// The caller (storage engine) is responsible for actually writing
    /// the operations to the WAL and MemTable.
    pub fn commit(&mut self, txn_id: SeqNo) -> Result<BTreeMap<Key, WriteOp>> {
        let txn = self
            .active
            .get_mut(&txn_id)
            .ok_or_else(|| onto_core::CoreError::InvalidArgument(
                format!("transaction {} not found", txn_id),
            ))?;

        if !txn.is_active() {
            return Err(onto_core::CoreError::InvalidArgument(
                format!("transaction {} is not active (status: {:?})", txn_id, txn.status),
            ));
        }

        txn.status = TxnStatus::Committed;
        let writes = txn.take_writes();

        // Update last_committed
        if txn_id > self.last_committed {
            self.last_committed = txn_id;
        }

        // Remove from active set
        self.active_set.remove(&txn_id);
        self.active.remove(&txn_id);

        Ok(writes)
    }

    /// Aborts a transaction. Discards all pending writes.
    pub fn abort(&mut self, txn_id: SeqNo) -> Result<()> {
        let txn = self
            .active
            .get_mut(&txn_id)
            .ok_or_else(|| onto_core::CoreError::InvalidArgument(
                format!("transaction {} not found", txn_id),
            ))?;

        if !txn.is_active() {
            return Err(onto_core::CoreError::InvalidArgument(
                format!("transaction {} is not active (status: {:?})", txn_id, txn.status),
            ));
        }

        txn.status = TxnStatus::Aborted;
        txn.discard_writes();

        // Remove from active set
        self.active_set.remove(&txn_id);
        self.active.remove(&txn_id);

        Ok(())
    }

    /// Creates a visibility context for the given transaction.
    /// Used by the read path to determine which versions are visible.
    pub fn visibility_for(&self, txn_id: SeqNo) -> Visibility {
        let snapshot_ts = self
            .active
            .get(&txn_id)
            .map(|t| t.snapshot_ts)
            .unwrap_or(self.last_committed);

        // Active set excludes the reader's own transaction
        let active_set: BTreeSet<SeqNo> = self
            .active_set
            .iter()
            .copied()
            .filter(|&id| id != txn_id)
            .collect();

        Visibility::new(snapshot_ts, active_set)
    }

    /// Returns the number of active transactions.
    pub fn active_count(&self) -> usize {
        self.active.len()
    }

    /// Returns the last committed transaction ID.
    pub fn last_committed(&self) -> SeqNo {
        self.last_committed
    }
}

impl Default for TxnManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_begin_commit() {
        let mut mgr = TxnManager::new();

        let txn_id = mgr.begin(0); // engine at seq 0
        assert_eq!(txn_id, 1);
        assert_eq!(mgr.active_count(), 1);

        // Buffer a write
        let txn = mgr.get_mut(txn_id).unwrap();
        txn.put(b"key1".to_vec(), b"val1".to_vec());

        // Commit
        let writes = mgr.commit(txn_id).unwrap();
        assert_eq!(writes.len(), 1);
        assert!(writes.contains_key(&b"key1".to_vec()));
        assert_eq!(mgr.active_count(), 0);
        assert_eq!(mgr.last_committed(), 1);
    }

    #[test]
    fn test_begin_abort() {
        let mut mgr = TxnManager::new();

        let txn_id = mgr.begin(0);
        let txn = mgr.get_mut(txn_id).unwrap();
        txn.put(b"key1".to_vec(), b"val1".to_vec());

        mgr.abort(txn_id).unwrap();
        assert_eq!(mgr.active_count(), 0);
    }

    #[test]
    fn test_multiple_concurrent_txns() {
        let mut mgr = TxnManager::new();

        let t1 = mgr.begin(0);
        let t2 = mgr.begin(0);
        let t3 = mgr.begin(0);

        assert_eq!(mgr.active_count(), 3);

        assert_ne!(t1, t2);
        assert_ne!(t2, t3);

        mgr.commit(t2).unwrap();
        assert_eq!(mgr.active_count(), 2);

        mgr.abort(t1).unwrap();
        assert_eq!(mgr.active_count(), 1);

        assert!(mgr.get(t3).unwrap().is_active());
    }

    #[test]
    fn test_visibility() {
        let mut mgr = TxnManager::new();

        // Pre-existing data at engine seq=5
        let t1 = mgr.begin(5); // snapshot_ts = 5

        // A write happens at seq=8, then another txn commits
        let t2 = mgr.begin(8);
        mgr.commit(t2).unwrap();

        let vis = mgr.visibility_for(t1);
        assert!(vis.is_visible(3));  // seq=3 <= snapshot_ts=5
        assert!(vis.is_visible(5));  // seq=5 <= snapshot_ts=5
        assert!(!vis.is_visible(8)); // seq=8 > snapshot_ts=5
    }

    #[test]
    fn test_visibility_after_commit() {
        let mut mgr = TxnManager::new();

        let t1 = mgr.begin(10); // snapshot_ts = 10
        mgr.commit(t1).unwrap();

        let t2 = mgr.begin(15); // snapshot_ts = 15
        let vis = mgr.visibility_for(t2);
        assert!(vis.is_visible(10)); // t1 committed, seq=10 <= 15
        assert!(vis.is_visible(5));  // old data
        assert!(!vis.is_visible(16)); // future
    }
}
