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
