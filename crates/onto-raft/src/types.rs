//! Raft type configuration for OntoDB.

use openraft::BasicNode;
use serde::{Deserialize, Serialize};

/// Node ID type.
pub type NodeId = u64;

// Raft type configuration for OntoDB.
openraft::declare_raft_types!(
    pub OntoRaftConfig:
        D = OntoRequest,
        R = OntoResponse,
        NodeId = NodeId,
        Node = BasicNode,
        Entry = openraft::Entry<OntoRaftConfig>,
        SnapshotData = std::io::Cursor<Vec<u8>>,
        AsyncRuntime = openraft::TokioRuntime,
);

/// The Raft type alias.
pub type OntoRaft = openraft::Raft<OntoRaftConfig>;

/// Log entry type.
pub type OntoEntry = openraft::Entry<OntoRaftConfig>;

/// Maximum nesting depth for Batch operations to prevent stack overflow.
const MAX_BATCH_DEPTH: usize = 10;

/// Request type for Raft state machine operations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum OntoRequest {
    /// PUT a key-value pair.
    Put { key: Vec<u8>, value: Vec<u8> },
    /// DELETE a key.
    Delete { key: Vec<u8> },
    /// Batch of operations.
    Batch { ops: Vec<OntoRequest> },
    /// Configuration change (API keys + IP whitelist).
    /// The full config JSON is replicated to all nodes via Raft log.
    ConfigChange { config_json: Vec<u8> },
}

impl OntoRequest {
    /// Returns the maximum nesting depth of this request (for Batch recursion).
    pub fn depth(&self) -> usize {
        match self {
            OntoRequest::Batch { ops } => {
                1 + ops.iter().map(|op| op.depth()).max().unwrap_or(0)
            }
            _ => 1,
        }
    }

    /// Returns true if this request exceeds the maximum allowed nesting depth.
    pub fn exceeds_max_depth(&self) -> bool {
        self.depth() > MAX_BATCH_DEPTH
    }

    /// Flatten nested batches into a single-level batch.
    pub fn flatten(self) -> Vec<OntoRequest> {
        match self {
            OntoRequest::Batch { ops } => {
                let mut flat = Vec::new();
                for op in ops {
                    flat.extend(op.flatten());
                }
                flat
            }
            other => vec![other],
        }
    }
}

/// Response type from the Raft state machine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum OntoResponse {
    Success(Option<String>),
    Error(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_request_depth_flat() {
        let put = OntoRequest::Put { key: b"k".to_vec(), value: b"v".to_vec() };
        assert_eq!(put.depth(), 1);
        assert!(!put.exceeds_max_depth());

        let del = OntoRequest::Delete { key: b"k".to_vec() };
        assert_eq!(del.depth(), 1);
    }

    #[test]
    fn test_request_depth_nested_batch() {
        let batch = OntoRequest::Batch {
            ops: vec![
                OntoRequest::Put { key: b"a".to_vec(), value: b"1".to_vec() },
                OntoRequest::Batch {
                    ops: vec![
                        OntoRequest::Put { key: b"b".to_vec(), value: b"2".to_vec() },
                    ],
                },
            ],
        };
        assert_eq!(batch.depth(), 2);
        assert!(!batch.exceeds_max_depth());
    }

    #[test]
    fn test_request_depth_exceeds_max() {
        // Build a deeply nested batch (depth = MAX_BATCH_DEPTH + 1)
        let mut deep = OntoRequest::Put { key: b"k".to_vec(), value: b"v".to_vec() };
        for _ in 0..MAX_BATCH_DEPTH {
            deep = OntoRequest::Batch { ops: vec![deep] };
        }
        assert!(deep.exceeds_max_depth());
    }

    #[test]
    fn test_request_flatten() {
        let nested = OntoRequest::Batch {
            ops: vec![
                OntoRequest::Put { key: b"a".to_vec(), value: b"1".to_vec() },
                OntoRequest::Batch {
                    ops: vec![
                        OntoRequest::Delete { key: b"b".to_vec() },
                        OntoRequest::Put { key: b"c".to_vec(), value: b"3".to_vec() },
                    ],
                },
            ],
        };
        let flat = nested.flatten();
        assert_eq!(flat.len(), 3);
        assert!(matches!(&flat[0], OntoRequest::Put { .. }));
        assert!(matches!(&flat[1], OntoRequest::Delete { .. }));
        assert!(matches!(&flat[2], OntoRequest::Put { .. }));
    }

    #[test]
    fn test_request_flatten_single() {
        let put = OntoRequest::Put { key: b"k".to_vec(), value: b"v".to_vec() };
        let flat = put.flatten();
        assert_eq!(flat.len(), 1);
    }

    #[test]
    fn test_request_config_change() {
        let config = OntoRequest::ConfigChange {
            config_json: b"{\"keys\":[]}".to_vec(),
        };
        assert_eq!(config.depth(), 1);
        assert!(!config.exceeds_max_depth());
    }
}
