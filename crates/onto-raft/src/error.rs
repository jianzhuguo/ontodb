//! Error types for the Raft layer.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum RaftError {
    #[error("storage error: {0}")]
    Storage(String),

    #[error("network error: {0}")]
    Network(String),

    #[error("serialization error: {0}")]
    Serialization(String),

    #[error("raft protocol error: {0}")]
    Protocol(String),

    #[error("node not found: {0}")]
    NodeNotFound(u64),
}

impl From<serde_json::Error> for RaftError {
    fn from(e: serde_json::Error) -> Self {
        RaftError::Serialization(e.to_string())
    }
}

impl From<onto_core::CoreError> for RaftError {
    fn from(e: onto_core::CoreError) -> Self {
        RaftError::Storage(e.to_string())
    }
}
