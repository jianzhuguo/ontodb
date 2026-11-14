// Copyright (c) 2024-2026 Guo Jianzhu (郭建筑)
// Licensed under the Apache License, Version 2.0.
// See LICENSE for details.
//! B+Tree secondary index module for OntoDB.
//!
//! Indexes are stored as key-value entries in the LSM-Tree:
//!   key:   __idx2__{class_len:u16}{class}{col_len:u16}{col}{val_len:u16}{value}{pk_len:u16}{pk}
//!   value: empty (just the key matters)
//!
//! This allows efficient prefix scanning for range queries
//! and point lookups for equality queries.

mod btree;
pub mod disk;
mod manager;
pub mod ngram;

pub use btree::{BPlusTree, ValuePkCursor, ValuePkRevCursor};
pub use manager::{IndexManager, IndexStorageMode};
pub use ngram::NgramManager;
