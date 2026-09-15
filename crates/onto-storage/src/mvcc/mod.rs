// Copyright (c) 2024-2026 OntoDB Team
// Licensed under the Business Source License 1.1 (BUSL-1.1).
// See LICENSE for details. Change Date: 2031-09-15.
// On the Change Date, this file will be licensed under Apache License 2.0.
//! Multi-Version Concurrency Control (MVCC) for OntoDB.
//!
//! Provides snapshot isolation: each transaction sees a consistent view
//! of the database as of its start time. Writers don't block readers.

mod manager;
mod transaction;
mod visibility;

pub use manager::TxnManager;
pub use transaction::{Transaction, TxnStatus, WriteOp};
pub use visibility::Visibility;
