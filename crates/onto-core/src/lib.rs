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

//! onto-core: Core types, traits, and error definitions for OntoDB.
//!
//! This crate provides the foundational abstractions used across all OntoDB components:
//! - Storage key/value types
//! - Error types
//! - Core traits for storage engines
//! - Unified entity identity (EntityId)

pub mod binary_row;
pub mod entity;
pub mod error;
pub mod geo;
pub mod geohash_index;
pub mod rtree;
pub mod spatiotemporal;
pub mod sttrl;
pub mod time_series;
pub mod types;
pub mod value;

pub use entity::EntityId;
pub use error::{CoreError, Result};
pub use types::{Bytes, Entry, EntryKind, Key, SeqNo, Timestamp, Value};
pub use value::OntoValue;
