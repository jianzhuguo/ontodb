//! OntoDB Raft consensus layer.
//!
//! Provides distributed replication using openraft, with:
//! - Combined RaftStorage backed by in-memory KV store
//! - TCP-based Raft networking
//! - Node manager for cluster operations
//!
//! Architecture: Raft entries are write operations (Put/Delete) that get
//! replicated to all nodes and applied to the state machine.

pub mod types;
pub mod store;
pub mod network;
pub mod error;
pub mod manager;

pub use types::OntoRaft;
pub use store::OntoRaftStore;
pub use error::RaftError;
pub use manager::RaftNodeManager;
