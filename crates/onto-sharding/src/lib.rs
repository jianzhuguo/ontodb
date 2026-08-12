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

pub use types::{ShardId, ShardConfig, ShardMap, ShardStrategy, RangeShard};
pub use router::ShardRouter;
pub use manager::{
    ShardManager, ShardStatus, MigrationStatus, MigrationTask, MigrationResult,
    RebalanceRecord, RebalanceResult, SplitStrategy, ShardStatistics,
};
