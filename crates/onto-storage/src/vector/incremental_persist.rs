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
//! - On flush: save only dirty nodes + a version counter
//! - On load: replay base snapshot + incremental deltas
//!
//! This reduces flush I/O from O(graph_size) to O(changes).

use std::collections::HashSet;

/// Tracks which HNSW nodes have been modified since last persist.
pub struct DirtyTracker {
    dirty_nodes: HashSet<usize>,
    base_version: u64,
    current_version: u64,
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

/// A single node delta for incremental persistence.
#[derive(Debug, Clone)]
pub struct NodeDelta {
    pub node_id: usize,
    pub layer: usize,
    pub neighbors: Vec<usize>,
}

/// Incremental persistence format:
/// [version: u64] [num_deltas: u32] [delta_1] [delta_2] ...
/// Each delta: [node_id: u32] [layer: u8] [num_neighbors: u16] [neighbor_0: u32] ...
pub struct IncrementalSnapshot {
    pub version: u64,
    pub deltas: Vec<NodeDelta>,
}

impl IncrementalSnapshot {
    pub fn new(version: u64, deltas: Vec<NodeDelta>) -> Self {
        Self { version, deltas }
    }

    /// Serialize to bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&self.version.to_le_bytes());
        buf.extend_from_slice(&(self.deltas.len() as u32).to_le_bytes());
        for delta in &self.deltas {
            buf.extend_from_slice(&(delta.node_id as u32).to_le_bytes());
            buf.push(delta.layer as u8);
            buf.extend_from_slice(&(delta.neighbors.len() as u16).to_le_bytes());
            for &n in &delta.neighbors {
                buf.extend_from_slice(&(n as u32).to_le_bytes());
            }
        }
        buf
    }

    /// Deserialize from bytes.
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < 12 {
            return None;
        }
        let version = u64::from_le_bytes(data[0..8].try_into().ok()?);
        let num_deltas = u32::from_le_bytes(data[8..12].try_into().ok()?) as usize;
        let mut offset = 12;
        let mut deltas = Vec::with_capacity(num_deltas);
        for _ in 0..num_deltas {
            if offset + 7 > data.len() {
                return None;
            }
            let node_id = u32::from_le_bytes(data[offset..offset + 4].try_into().ok()?) as usize;
            offset += 4;
            let layer = data[offset] as usize;
            offset += 1;
            let num_neighbors =
                u16::from_le_bytes(data[offset..offset + 2].try_into().ok()?) as usize;
            offset += 2;
            let mut neighbors = Vec::with_capacity(num_neighbors);
            for _ in 0..num_neighbors {
                if offset + 4 > data.len() {
                    return None;
                }
                neighbors
                    .push(u32::from_le_bytes(data[offset..offset + 4].try_into().ok()?) as usize);
                offset += 4;
            }
            deltas.push(NodeDelta {
                node_id,
                layer,
                neighbors,
            });
        }
        Some(IncrementalSnapshot { version, deltas })
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
            vec![
                NodeDelta {
                    node_id: 1,
                    layer: 0,
                    neighbors: vec![2, 3],
                },
                NodeDelta {
                    node_id: 2,
                    layer: 1,
                    neighbors: vec![1],
                },
            ],
        );

        let bytes = snapshot.to_bytes();
        let restored = IncrementalSnapshot::from_bytes(&bytes).unwrap();

        assert_eq!(restored.version, 42);
        assert_eq!(restored.deltas.len(), 2);
        assert_eq!(restored.deltas[0].node_id, 1);
        assert_eq!(restored.deltas[0].layer, 0);
        assert_eq!(restored.deltas[0].neighbors, vec![2, 3]);
        assert_eq!(restored.deltas[1].node_id, 2);
        assert_eq!(restored.deltas[1].layer, 1);
        assert_eq!(restored.deltas[1].neighbors, vec![1]);
    }

    #[test]
    fn test_incremental_snapshot_empty() {
        let snapshot = IncrementalSnapshot::new(0, vec![]);
        let bytes = snapshot.to_bytes();
        let restored = IncrementalSnapshot::from_bytes(&bytes).unwrap();
        assert_eq!(restored.version, 0);
        assert!(restored.deltas.is_empty());
    }

    #[test]
    fn test_incremental_snapshot_invalid_data() {
        assert!(IncrementalSnapshot::from_bytes(&[]).is_none());
        assert!(IncrementalSnapshot::from_bytes(&[1, 2, 3]).is_none());
    }
}
