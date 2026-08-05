//! onto-storage: LSM-Tree storage engine for OntoDB.
//!
//! Architecture:
//! - WAL (Write-Ahead Log): Durability guarantee
//! - MemTable: In-memory sorted structure (skip list)
//! - SSTable: Sorted string table on disk
//! - LSM Engine: Orchestrates all components

pub mod engine;
pub mod iterator;
pub mod lsm;
pub mod options;

pub use engine::LsmEngine;
pub use iterator::StorageIterator;
pub use options::StorageOptions;
