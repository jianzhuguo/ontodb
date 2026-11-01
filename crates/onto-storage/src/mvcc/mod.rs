// Copyright (c) 2024-2026 Guo Jianzhu (郭建筑)
// Licensed under the Apache License, Version 2.0.
// See LICENSE for details.
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
