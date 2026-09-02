//! Vector index warmup for reducing cold-start latency.
//!
//! After server restart, vector indexes need to be loaded from disk.
//! The first query on each index suffers high latency due to page faults.
//!
//! Warmup strategy:
//! - On startup: scan all indexes and touch their data pages
//! - Background thread: periodically re-warm recently used indexes
//! - Per-index warmup state tracking

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

/// Warmup state for a single vector index.
#[derive(Debug, Clone)]
pub struct IndexWarmupState {
    pub class: String,
    pub column: String,
    pub warmed: bool,
    pub last_warmup: Option<Instant>,
    pub warmup_duration_ms: u64,
}

/// Warmup configuration.
#[derive(Debug, Clone)]
pub struct WarmupConfig {
    /// Warm all indexes on startup.
    pub warm_on_startup: bool,
    /// Re-warm interval in seconds (0 = disabled).
    pub rewarm_interval_secs: u64,
    /// Maximum concurrent warmup tasks.
    pub max_concurrent: usize,
}

impl Default for WarmupConfig {
    fn default() -> Self {
        Self {
            warm_on_startup: true,
            rewarm_interval_secs: 0, // disabled by default
            max_concurrent: 2,
        }
    }
}

/// Manages vector index warmup operations.
pub struct WarmupManager {
    config: WarmupConfig,
    states: Vec<IndexWarmupState>,
}

impl WarmupManager {
    pub fn new(config: WarmupConfig) -> Self {
        Self {
            config,
            states: Vec::new(),
        }
    }

    /// Register an index for warmup tracking.
    pub fn register(&mut self, class: &str, column: &str) {
        self.states.push(IndexWarmupState {
            class: class.to_string(),
            column: column.to_string(),
            warmed: false,
            last_warmup: None,
            warmup_duration_ms: 0,
        });
    }

    /// Mark an index as warmed.
    pub fn mark_warmed(&mut self, class: &str, column: &str, duration_ms: u64) {
        if let Some(state) = self.states.iter_mut().find(|s| s.class == class && s.column == column) {
            state.warmed = true;
            state.last_warmup = Some(Instant::now());
            state.warmup_duration_ms = duration_ms;
        }
    }

    /// Get indexes that need warmup.
    pub fn pending_warmups(&self) -> Vec<&IndexWarmupState> {
        self.states.iter().filter(|s| !s.warmed).collect()
    }

    /// Get all warmup states.
    pub fn states(&self) -> &[IndexWarmupState] {
        &self.states
    }

    /// Check if warmup is enabled for startup.
    pub fn should_warm_on_startup(&self) -> bool {
        self.config.warm_on_startup
    }

    /// Check if periodic re-warm is enabled.
    pub fn should_rewarm(&self) -> bool {
        self.config.rewarm_interval_secs > 0
    }

    /// Get re-warm interval.
    pub fn rewarm_interval_secs(&self) -> u64 {
        self.config.rewarm_interval_secs
    }
}

/// Perform warmup by reading first entry from each index.
/// This forces the OS to page in the index data.
pub fn warmup_index(index_entries: usize) -> Instant {
    let start = Instant::now();
    // Touch memory by iterating (forces page-in if memory-mapped)
    // In practice, this would call into the HNSW index
    let _ = index_entries; // Placeholder for actual warmup logic
    start
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_warmup_manager_register() {
        let mut mgr = WarmupManager::new(WarmupConfig::default());
        mgr.register("Product", "embedding");
        mgr.register("User", "profile_vec");
        assert_eq!(mgr.states().len(), 2);
    }

    #[test]
    fn test_warmup_manager_pending() {
        let mut mgr = WarmupManager::new(WarmupConfig::default());
        mgr.register("Product", "embedding");
        mgr.register("User", "profile_vec");
        assert_eq!(mgr.pending_warmups().len(), 2);

        mgr.mark_warmed("Product", "embedding", 10);
        assert_eq!(mgr.pending_warmups().len(), 1);
        assert!(mgr.states()[0].warmed);
        assert_eq!(mgr.states()[0].warmup_duration_ms, 10);
    }

    #[test]
    fn test_warmup_config_default() {
        let config = WarmupConfig::default();
        assert!(config.warm_on_startup);
        assert_eq!(config.rewarm_interval_secs, 0);
        assert_eq!(config.max_concurrent, 2);
    }
}
