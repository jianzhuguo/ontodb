//! onto-core: Core types, traits, and error definitions for OntoDB.
//!
//! This crate provides the foundational abstractions used across all OntoDB components:
//! - Storage key/value types
//! - Error types
//! - Core traits for storage engines

pub mod error;
pub mod types;
pub mod value;

pub use error::CoreError;
pub use types::{Bytes, Key, SeqNo, Timestamp, Value};
pub use value::OntoValue;
