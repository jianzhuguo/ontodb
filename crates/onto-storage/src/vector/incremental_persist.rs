// Copyright (c) 2024-2026 OntoDB Team
// Licensed under the Business Source License 1.1 (BUSL-1.1).
// See LICENSE for details. Change Date: 2031-09-15.
// On the Change Date, this file will be licensed under Apache License 2.0.
//! Incremental persistence for HNSW indexes.
//!
//! Instead of saving the entire graph on every flush, we track
//! dirty nodes and only persist the changes.
//!
//! Design:
//! - On insert: mark node as dirty
//! - On flush/shutdown: serialize dirty nodes into an `IncrementalSnapshot`
//! - On load: replay base snapshot (`__vec_graph__`) + incremental deltas
//!   (`__vec_delta__`) in version order
//!
//! This reduces flush I/O from O(graph_size) to O(changes).
//!
//! Node payload is the bincode-serialized `HnswNode` (id + vector data +
//! neighbor lists), so a delta is self-contained: applying it can insert
//! brand-new nodes or patch existing ones.

use std::collections::HashSet;

/// Tracks which HNSW nodes have been modified since last persist.
pub struct DirtyTracker {
    dirty_nodes: HashSet<usize>,
    base_version: u64,
    current_version: u64,
}

impl Default for DirtyTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl DirtyTracker {
    pub fn new() -> Self {
        Self {
            dirty_nodes: HashSet::new(),
            base_version: 0,
            current_version: 0,
        }
    }

    /// Mark a node as dirty (needs persistence).
    pub fn mark_dirty(&mut self, node_id: usize) {
        self.dirty_nodes.insert(node_id);
        self.current_version += 1;
    }

    /// Get all dirty node IDs and clear the dirty set.
    pub fn drain_dirty(&mut self) -> HashSet<usize> {
        let dirty = std::mem::take(&mut self.dirty_nodes);
        self.base_version = self.current_version;
        dirty
    }

    /// Check if there are dirty nodes.
    pub fn has_dirty(&self) -> bool {
        !self.dirty_nodes.is_empty()
    }

    /// Number of dirty nodes.
    pub fn dirty_count(&self) -> usize {
        self.dirty_nodes.len()
    }

    /// Current version counter.
    pub fn version(&self) -> u64 {
        self.current_version
    }

    /// Base version (last persisted).
    pub fn base_version(&self) -> u64 {
        self.base_version
    }
}

/// Incremental persistence format:
/// `[version: u64] [num_nodes: u32] [node_1] [node_2] ...`
/// Each node: `[node_id: u32] [blob_len: u32] [bincode(HnswNode)]`
#[derive(Debug, Clone)]
pub struct IncrementalSnapshot {
    pub version: u64,
    /// (node_id, bincode-serialized HnswNode)
    pub nodes: Vec<(u32, Vec<u8>)>,
}

impl IncrementalSnapshot {
    pub fn new(version: u64, nodes: Vec<(u32, Vec<u8>)>) -> Self {
        Self { version, nodes }
    }

    /// Serialize to bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&self.version.to_le_bytes());
        buf.extend_from_slice(&(self.nodes.len() as u32).to_le_bytes());
        for (node_id, blob) in &self.nodes {
            buf.extend_from_slice(&node_id.to_le_bytes());
            buf.extend_from_slice(&(blob.len() as u32).to_le_bytes());
            buf.extend_from_slice(blob);
        }
        buf
    }

    /// Deserialize from bytes.
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < 12 {
            return None;
        }
        let version = u64::from_le_bytes(data[0..8].try_into().ok()?);
        let num_nodes = u32::from_le_bytes(data[8..12].try_into().ok()?) as usize;
        let mut offset = 12;
        let mut nodes = Vec::with_capacity(num_nodes);
        for _ in 0..num_nodes {
            if offset + 8 > data.len() {
                return None;
            }
            let node_id = u32::from_le_bytes(data[offset..offset + 4].try_into().ok()?);
            offset += 4;
            let blob_len = u32::from_le_bytes(data[offset..offset + 4].try_into().ok()?) as usize;
            offset += 4;
            if offset + blob_len > data.len() {
                return None;
            }
            nodes.push((node_id, data[offset..offset + blob_len].to_vec()));
            offset += blob_len;
        }
        Some(IncrementalSnapshot { version, nodes })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dirty_tracker_basic() {
        let mut tracker = DirtyTracker::new();
        assert!(!tracker.has_dirty());
        assert_eq!(tracker.dirty_count(), 0);

        tracker.mark_dirty(1);
        tracker.mark_dirty(2);
        tracker.mark_dirty(3);
        assert!(tracker.has_dirty());
        assert_eq!(tracker.dirty_count(), 3);
        assert_eq!(tracker.version(), 3);

        let dirty = tracker.drain_dirty();
        assert_eq!(dirty.len(), 3);
        assert!(dirty.contains(&1));
        assert!(dirty.contains(&2));
        assert!(dirty.contains(&3));
        assert!(!tracker.has_dirty());
        assert_eq!(tracker.version(), 3);
        assert_eq!(tracker.base_version(), 3);
    }

    #[test]
    fn test_dirty_tracker_idempotent() {
        let mut tracker = DirtyTracker::new();
        tracker.mark_dirty(1);
        tracker.mark_dirty(1); // Same node, should not increase count
        assert_eq!(tracker.dirty_count(), 1);
    }

    #[test]
    fn test_incremental_snapshot_serialize() {
        let snapshot = IncrementalSnapshot::new(
            42,
            vec![(1, vec![0x01, 0x02, 0x03]), (2, vec![0xAA; 64])],
        );

        let bytes = snapshot.to_bytes();
        let restored = IncrementalSnapshot::from_bytes(&bytes).unwrap();

        assert_eq!(restored.version, 42);
        assert_eq!(restored.nodes.len(), 2);
        assert_eq!(restored.nodes[0].0, 1);
        assert_eq!(restored.nodes[0].1, vec![0x01, 0x02, 0x03]);
        assert_eq!(restored.nodes[1].0, 2);
        assert_eq!(restored.nodes[1].1, vec![0xAA; 64]);
    }

    #[test]
    fn test_incremental_snapshot_empty() {
        let snapshot = IncrementalSnapshot::new(0, vec![]);
        let bytes = snapshot.to_bytes();
        let restored = IncrementalSnapshot::from_bytes(&bytes).unwrap();
        assert_eq!(restored.version, 0);
        assert!(restored.nodes.is_empty());
    }

    #[test]
    fn test_incremental_snapshot_invalid_data() {
        assert!(IncrementalSnapshot::from_bytes(&[]).is_none());
        assert!(IncrementalSnapshot::from_bytes(&[1, 2, 3]).is_none());
    }
}
