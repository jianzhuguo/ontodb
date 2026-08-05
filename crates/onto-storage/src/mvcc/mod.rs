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
