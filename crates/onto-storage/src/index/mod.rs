// Copyright (c) 2024-2026 OntoDB Team
// Licensed under the Business Source License 1.1 (BUSL-1.1).
// See LICENSE for details. Change Date: 2031-09-15.
// On the Change Date, this file will be licensed under Apache License 2.0.
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

pub use btree::{BPlusTree, ValuePkCursor, ValuePkRevCursor};
pub use manager::{IndexManager, IndexStorageMode};
