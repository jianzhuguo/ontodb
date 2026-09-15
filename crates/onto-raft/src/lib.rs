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
//! OntoDB Raft consensus layer.
//!
//! Provides distributed replication using openraft, with:
//! - Combined RaftStorage backed by in-memory KV store (OntoRaftStore)
//! - Persistent RaftStorage backed by LsmEngine (PersistentRaftStore)
//! - TCP-based Raft networking
//! - Node manager for cluster operations
//!
//! Architecture: Raft entries are write operations (Put/Delete) that get
//! replicated to all nodes and applied to the state machine.

pub mod cluster_whitelist;
pub mod config_sync;
pub mod error;
pub mod manager;
pub mod network;
pub mod persistent_store;
pub mod store;
pub mod types;
// State machine module removed - using PersistentRaftStore instead
// #[cfg(test)]
// pub mod state_machine;

pub use cluster_whitelist::{ClusterNode, ClusterWhitelistManager, ValidationResult};
pub use config_sync::{ConfigChangeResult, SharedConfigStore};
pub use error::RaftError;
pub use manager::RaftNodeManager;
pub use persistent_store::PersistentRaftStore;
pub use store::OntoRaftStore;
pub use types::OntoRaft;
