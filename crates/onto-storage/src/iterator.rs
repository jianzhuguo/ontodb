// Copyright (c) 2024-2026 OntoDB Team
// Licensed under the Business Source License 1.1 (BUSL-1.1).
// See LICENSE for details. Change Date: 2031-09-15.
// On the Change Date, this file will be licensed under Apache License 2.0.
//! Storage iterator trait - unified interface for traversing key-value pairs.

use onto_core::SeqNo;

/// Trait for iterating over sorted key-value entries in the storage engine.
pub trait StorageIterator {
    /// Returns true if the iterator is valid (points to a valid entry).
    fn is_valid(&self) -> bool;

    /// Returns the current key. Only valid when `is_valid()` is true.
    fn key(&self) -> &[u8];

    /// Returns the current value. Only valid when `is_valid()` is true.
    fn value(&self) -> &[u8];

    /// Returns the sequence number of the current entry.
    fn seq_no(&self) -> SeqNo;

    /// Advances the iterator to the next entry.
    fn next(&mut self) -> bool;

    /// Seeks to the first entry with key >= `target`.
    fn seek(&mut self, target: &[u8]);

    /// Seeks to the first entry.
    fn seek_to_first(&mut self);

    /// Seeks to the last entry.
    fn seek_to_last(&mut self);
}
