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

pub mod types;
pub mod store;
pub mod persistent_store;
pub mod network;
pub mod error;
pub mod manager;
pub mod config_sync;
pub mod cluster_whitelist;
// State machine module removed - using PersistentRaftStore instead
// #[cfg(test)]
// pub mod state_machine;

pub use types::OntoRaft;
pub use store::OntoRaftStore;
pub use persistent_store::PersistentRaftStore;
pub use error::RaftError;
pub use manager::RaftNodeManager;
pub use config_sync::{SharedConfigStore, ConfigChangeResult};
pub use cluster_whitelist::{ClusterWhitelistManager, ValidationResult, ClusterNode};
