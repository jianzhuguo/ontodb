//! Snapshot visibility rules for MVCC.
//!
//! Determines which versions of a key are visible to a given transaction
//! based on snapshot isolation.

use onto_core::SeqNo;
use std::collections::BTreeSet;

/// Visibility rules for snapshot isolation.
///
/// A version with timestamp `ts` is visible to a transaction with `snapshot_ts`
/// if and only if:
/// 1. `ts <= snapshot_ts` (the version was committed before the snapshot)
/// 2. `ts` is NOT in the active transactions set (the writer has committed)
/// 3. `ts` is not the current transaction's own uncommitted writes
///    (handled separately by the write buffer)
pub struct Visibility {
    /// Timestamp of the reader's snapshot.
    pub snapshot_ts: SeqNo,

    /// Set of transaction IDs that are currently active (uncommitted).
    /// Any version written by an active transaction is invisible.
    pub active_txns: BTreeSet<SeqNo>,
}

impl Visibility {
    /// Creates a new visibility context.
    pub fn new(snapshot_ts: SeqNo, active_txns: BTreeSet<SeqNo>) -> Self {
        Self {
            snapshot_ts,
            active_txns,
        }
    }

    /// Returns true if a version with the given write timestamp is visible.
    ///
    /// Rules:
    /// - Must be <= snapshot_ts (written before or at snapshot time)
    /// - Must NOT be from an active (uncommitted) transaction
    pub fn is_visible(&self, write_ts: SeqNo) -> bool {
        write_ts <= self.snapshot_ts && !self.active_txns.contains(&write_ts)
    }

    /// Returns true if a version with the given write timestamp is the
    /// latest visible version among candidates, for dedup purposes.
    ///
    /// Used during compaction to decide which version to keep.
    pub fn is_latest_visible(&self, write_ts: SeqNo, _key: &[u8]) -> bool {
        self.is_visible(write_ts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_visibility_basic() {
        let active = BTreeSet::new();
        let vis = Visibility::new(10, active);

        assert!(vis.is_visible(5));   // committed before snapshot
        assert!(vis.is_visible(10));  // committed at snapshot time
        assert!(!vis.is_visible(15)); // committed after snapshot
    }

    #[test]
    fn test_visibility_active_txn() {
        let mut active = BTreeSet::new();
        active.insert(7); // txn 7 is still active
        let vis = Visibility::new(10, active);

        assert!(vis.is_visible(5));   // committed by finished txn
        assert!(!vis.is_visible(7));  // written by active txn, invisible
        assert!(vis.is_visible(9));   // committed by finished txn
    }

    #[test]
    fn test_visibility_multiple_active() {
        let mut active = BTreeSet::new();
        active.insert(3);
        active.insert(7);
        active.insert(12);
        let vis = Visibility::new(15, active);

        assert!(vis.is_visible(2));   // committed
        assert!(!vis.is_visible(3));  // active txn
        assert!(vis.is_visible(5));   // committed
        assert!(!vis.is_visible(7));  // active txn
        assert!(vis.is_visible(10));  // committed
        assert!(!vis.is_visible(12)); // active txn
        assert!(!vis.is_visible(16)); // after snapshot
    }
}
