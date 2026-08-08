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

/// Response type from the Raft state machine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum OntoResponse {
    Success(Option<String>),
    Error(String),
}
