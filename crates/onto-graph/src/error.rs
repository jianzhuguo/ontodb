// Copyright (c) 2024-2026 OntoDB Team
// Licensed under the Business Source License 1.1 (BUSL-1.1).
// See LICENSE for details. Change Date: 2031-09-15.
// On the Change Date, this file will be licensed under Apache License 2.0.
//! Error types for the graph module.

use thiserror::Error;

#[derive(Error, Debug)]
pub enum GraphError {
    #[error("Vertex not found: {0}")]
    VertexNotFound(String),

    #[error("Edge not found: {0}")]
    EdgeNotFound(String),

    #[error("Invalid edge: source or target vertex missing")]
    InvalidEdge,

    #[error("Duplicate vertex ID: {0}")]
    DuplicateVertex(String),

    #[error("Invalid property: {0}")]
    InvalidProperty(String),

    #[error("Traversal error: {0}")]
    TraversalError(String),

    #[error("Storage error: {0}")]
    StorageError(String),

    #[error("Serialization error: {0}")]
    SerializationError(String),
}

impl From<serde_json::Error> for GraphError {
    fn from(e: serde_json::Error) -> Self {
        GraphError::SerializationError(e.to_string())
    }
}

impl From<onto_core::CoreError> for GraphError {
    fn from(e: onto_core::CoreError) -> Self {
        GraphError::StorageError(e.to_string())
    }
}
