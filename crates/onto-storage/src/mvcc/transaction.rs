// Copyright (c) 2024-2026 OntoDB Team
// Licensed under the Business Source License 1.1 (BUSL-1.1).
// See LICENSE for details. Change Date: 2031-09-15.
// On the Change Date, this file will be licensed under Apache License 2.0.
//! Transaction state and write operations.

use onto_core::{Key, SeqNo, Value};
use std::collections::BTreeMap;

/// Transaction status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxnStatus {
    /// Transaction is active (can read and write).
    Active,
    /// Transaction has been committed.
    Committed,
    /// Transaction has been rolled back.
    Aborted,
}

/// A pending write operation in a transaction's write buffer.
#[derive(Debug, Clone)]
pub enum WriteOp {
    /// Put a key-value pair.
    Put(Value),
    /// Delete a key.
    Delete,
}

/// Represents a single database transaction.
///
/// Each transaction has:
/// - A unique `id` (assigned at begin time)
/// - A `snapshot_ts` that determines what data is visible
/// - A `write_buffer` that accumulates writes until commit
/// - A `status` tracking the transaction lifecycle
pub struct Transaction {
    /// Unique transaction ID (same as snapshot timestamp).
    pub id: SeqNo,

    /// Timestamp of the snapshot this transaction reads from.
    /// All committed writes with ts <= snapshot_ts are visible.
    pub snapshot_ts: SeqNo,

    /// Current status.
    pub status: TxnStatus,

    /// Buffered writes (key -> operation). Applied on commit.
    write_buffer: BTreeMap<Key, WriteOp>,
}

impl Transaction {
    /// Creates a new active transaction.
    pub fn new(id: SeqNo) -> Self {
        Self {
            id,
            snapshot_ts: id,
            status: TxnStatus::Active,
            write_buffer: BTreeMap::new(),
        }
    }

    /// Buffers a put operation. Returns the transaction's write timestamp.
    pub fn put(&mut self, key: Key, value: Value) -> SeqNo {
        self.write_buffer.insert(key, WriteOp::Put(value));
        self.id
    }

    /// Buffers a delete operation. Returns the transaction's write timestamp.
    pub fn delete(&mut self, key: Key) -> SeqNo {
        self.write_buffer.insert(key, WriteOp::Delete);
        self.id
    }

    /// Takes the write buffer (for commit). Leaves it empty.
    pub fn take_writes(&mut self) -> BTreeMap<Key, WriteOp> {
        std::mem::take(&mut self.write_buffer)
    }

    /// Discards the write buffer (for abort).
    pub fn discard_writes(&mut self) {
        self.write_buffer.clear();
    }

    /// Returns true if the transaction has pending writes.
    pub fn has_writes(&self) -> bool {
        !self.write_buffer.is_empty()
    }

    /// Returns true if the transaction is still active.
    pub fn is_active(&self) -> bool {
        self.status == TxnStatus::Active
    }

    /// Gets a value from the write buffer by key.
    pub fn write_buffer_get(&self, key: &[u8]) -> Option<&WriteOp> {
        self.write_buffer.get(key)
    }

    /// Iterates over the write buffer entries.
    pub fn write_buffer_iter(&self) -> impl Iterator<Item = (&Key, &WriteOp)> {
        self.write_buffer.iter()
    }
}
