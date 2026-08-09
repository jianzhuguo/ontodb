//! onto-core: Core types, traits, and error definitions for OntoDB.
//!
//! This crate provides the foundational abstractions used across all OntoDB components:
//! - Storage key/value types
//! - Error types
//! - Core traits for storage engines
//! - Unified entity identity (EntityId)

pub mod binary_row;
pub mod entity;
pub mod error;
pub mod geo;
pub mod geohash_index;
pub mod rtree;
pub mod spatiotemporal;
pub mod time_series;
pub mod types;
pub mod value;

pub use entity::EntityId;
pub use error::{CoreError, Result};
pub use types::{Bytes, Entry, EntryKind, Key, SeqNo, Timestamp, Value};
pub use value::OntoValue;
