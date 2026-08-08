//! onto-storage: LSM-Tree storage engine for OntoDB.
//!
//! Architecture:
//! - WAL (Write-Ahead Log): Durability guarantee
//! - MemTable: In-memory sorted structure (skip list)
//! - SSTable: Sorted string table on disk
//! - LSM Engine: Orchestrates all components
//! - Vector: HNSW vector index for similarity search

pub mod engine;
pub mod index;
pub mod iterator;
pub mod lsm;
pub mod mvcc;
pub mod options;
pub mod vector;

pub use engine::{LsmEngine, BackupManifest, BackupFile, BackupFileType};
pub use index::{IndexManager, IndexStorageMode};
pub use iterator::StorageIterator;
pub use mvcc::{TxnManager, Transaction, TxnStatus, WriteOp};
pub use options::StorageOptions;
pub use vector::{DistanceMetric, HnswConfig, HnswIndex, VectorEntry, SearchResult, VectorIndexManager};
pub use vector::distance;

#[cfg(test)]
mod fuzz_tests;
