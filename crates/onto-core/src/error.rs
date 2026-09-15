// Copyright (c) 2024-2026 OntoDB Team
// Licensed under the Business Source License 1.1 (BUSL-1.1).
// See LICENSE for details. Change Date: 2031-09-15.
// On the Change Date, this file will be licensed under Apache License 2.0.
//! Core error types for OntoDB.

use thiserror::Error;

/// Core error type used across all OntoDB components.
#[derive(Error, Debug)]
pub enum CoreError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("serialization error: {0}")]
    Serialization(String),

    #[error("corruption detected: {0}")]
    Corruption(String),

    #[error("key not found: {key:?}")]
    KeyNotFound { key: Vec<u8> },

    #[error("invalid argument: {0}")]
    InvalidArgument(String),

    #[error("storage full")]
    StorageFull,

    #[error("checksum mismatch: expected {expected:#010x}, got {actual:#010x}")]
    ChecksumMismatch { expected: u32, actual: u32 },

    #[error("inference error: {0}")]
    Inference(String),

    #[error("ontology inconsistent: {0}")]
    OntologyInconsistent(String),

    #[error("transaction conflict: {0}")]
    TransactionConflict(String),

    #[error("{0}")]
    Custom(String),
}

impl CoreError {
    pub fn custom(msg: impl Into<String>) -> Self {
        CoreError::Custom(msg.into())
    }

    pub fn corruption(msg: impl Into<String>) -> Self {
        CoreError::Corruption(msg.into())
    }
}

/// Result type alias for OntoDB operations.
pub type Result<T> = std::result::Result<T, CoreError>;
