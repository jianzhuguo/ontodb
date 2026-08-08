//! Sharding module (placeholder).
//!
//! TODO: Implement data sharding and cross-shard queries.

/// Sharding configuration placeholder.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ShardingConfig {
    pub enabled: bool,
    pub shard_count: u32,
    pub strategy: String,
}

impl Default for ShardingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            shard_count: 1,
            strategy: "hash".to_string(),
        }
    }
}
