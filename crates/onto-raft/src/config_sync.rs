//! Shared configuration store for cross-node config synchronization.
//!
//! When Raft is active, config changes are replicated via the Raft log.
//! All nodes share the same config through this store.

use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

/// The shared configuration store.
/// Holds the current auth config and persists to disk.
#[derive(Clone)]
pub struct SharedConfigStore {
    /// Current config JSON (canonical form).
    config_json: Arc<RwLock<Vec<u8>>>,
    /// Path to the config file on disk.
    file_path: Option<PathBuf>,
    /// Callbacks to notify when config changes.
    /// In practice, this updates the AuthState in-memory.
    on_change: Arc<RwLock<Vec<Box<dyn Fn(&[u8]) + Send + Sync>>>>,
}

impl SharedConfigStore {
    /// Create a new shared config store.
    pub fn new(file_path: Option<PathBuf>) -> Self {
        Self {
            config_json: Arc::new(RwLock::new(Vec::new())),
            file_path,
            on_change: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Initialize from disk. Returns true if config was loaded.
    pub fn load_from_disk(&self) -> bool {
        let path = match &self.file_path {
            Some(p) => p,
            None => return false,
        };

        match std::fs::read_to_string(path) {
            Ok(data) => {
                *self.config_json.write() = data.into_bytes();
                true
            }
            Err(_) => false,
        }
    }

    /// Get the current config as JSON bytes.
    pub fn get_json(&self) -> Vec<u8> {
        self.config_json.read().clone()
    }

    /// Apply a new config (from Raft replication or local change).
    /// Writes to disk first, then updates in-memory and notifies listeners.
    pub fn apply(&self, config_json: &[u8]) -> Result<(), String> {
        // Validate JSON
        serde_json::from_slice::<serde_json::Value>(config_json)
            .map_err(|e| format!("invalid config JSON: {}", e))?;

        // Persist to disk first (fail-fast before updating in-memory state)
        if let Some(ref path) = self.file_path {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            std::fs::write(path, config_json)
                .map_err(|e| format!("failed to write config: {}", e))?;
        }

        // Update in-memory (only after successful disk write)
        *self.config_json.write() = config_json.to_vec();

        // Notify listeners
        for cb in self.on_change.read().iter() {
            cb(config_json);
        }

        Ok(())
    }

    /// Register a callback for config changes.
    pub fn on_change(&self, cb: Box<dyn Fn(&[u8]) + Send + Sync>) {
        self.on_change.write().push(cb);
    }

    /// Get the file path.
    pub fn file_path(&self) -> Option<&PathBuf> {
        self.file_path.as_ref()
    }
}

/// Result of a config change operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigChangeResult {
    pub success: bool,
    pub message: String,
    pub replicated_via_raft: bool,
    pub node_count: usize,
}
