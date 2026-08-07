//! OntoDB data sharding layer.
//!
//! Provides three sharding strategies:
//! - **Class-based**: different Classes (tables) assigned to different shards
//! - **Range-based**: large Classes split by primary key range
//! - **Hash-based**: primary key hash determines shard assignment
//!
//! The `ShardRouter` sits in the query execution path and routes
//! operations to the correct shard.

pub mod types;
pub mod strategy;
pub mod router;
pub mod manager;

pub use types::{ShardId, ShardConfig, ShardMap};
pub use router::ShardRouter;
pub use manager::ShardManager;
