//! B+Tree secondary index module for OntoDB.
//!
//! Indexes are stored as key-value entries in the LSM-Tree:
//!   key:   __idx__{class}__{column}::{value}::{primary_key}
//!   value: empty (just the key matters)
//!
//! This allows efficient prefix scanning for range queries
//! and point lookups for equality queries.

mod btree;
pub mod disk;
mod manager;

pub use btree::BPlusTree;
pub use manager::{IndexManager, IndexStorageMode};
