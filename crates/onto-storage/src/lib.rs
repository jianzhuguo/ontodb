#![allow(clippy::too_many_arguments)]
#![allow(clippy::type_complexity)]
#![allow(clippy::manual_strip)]
#![allow(clippy::needless_range_loop)]
#![allow(clippy::new_without_default)]
#![allow(clippy::collapsible_match)]
#![allow(clippy::if_same_then_else)]
#![allow(clippy::manual_checked_ops)]
#![allow(clippy::ptr_arg)]
#![allow(clippy::non_canonical_partial_ord_impl)]
#![allow(clippy::should_implement_trait)]
#![allow(clippy::sliced_string_as_bytes)]
#![allow(clippy::len_without_is_empty)]
#![allow(clippy::lines_filter_map_ok)]
#![allow(clippy::vec_init_then_push)]
#![allow(clippy::unnecessary_find_map)]
#![allow(clippy::unnecessary_unwrap)]
#![allow(clippy::result_large_err)]
#![allow(clippy::doc_lazy_continuation)]
// Copyright (c) 2024-2026 OntoDB Team
// Licensed under the Business Source License 1.1 (BUSL-1.1).
// See LICENSE for details. Change Date: 2031-09-15.
// On the Change Date, this file will be licensed under Apache License 2.0.
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
pub mod tiered_storage;
pub mod tsm;
pub mod value_meta;
pub mod vector;

pub use engine::{BackupFile, BackupFileType, BackupManifest, LsmEngine};
pub use index::{IndexManager, IndexStorageMode};
pub use iterator::StorageIterator;
pub use mvcc::{Transaction, TxnManager, TxnStatus, WriteOp};
pub use options::StorageOptions;
pub use value_meta::{ValueMetadata, ValueScorer, LAMBDA_2Y, LAMBDA_70D, LAMBDA_7H};
pub use vector::distance;
pub use vector::{
    DistanceMetric, HnswConfig, HnswIndex, SearchResult, VectorEntry, VectorIndexManager,
};

#[cfg(test)]
mod fuzz_tests;
