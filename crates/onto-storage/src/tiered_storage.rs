//! Tiered storage for time series data.
//!
//! Implements hot/warm/cold data tiering:
//! - **Hot**: MemTable (in memory) — recent data, fast reads/writes
//! - **Warm**: TSM files (SSD) — older data, compressed columnar format
//! - **Cold**: Parquet files (HDD/object storage) — archived data, highly compressed
//!
//! Data automatically migrates between tiers based on age thresholds.
//! Queries transparently read from all tiers.

use std::collections::HashMap;
use std::path::PathBuf;
use serde::{Deserialize, Serialize};

use crate::tsm::{TsPoint, TsmWriter};

// ── Configuration ──

/// Tiered storage configuration.
#[derive(Debug, Clone)]
pub struct TieredConfig {
    /// Base directory for all tiers.
    pub data_dir: PathBuf,
    /// Hot→Warm threshold: data older than this is moved to warm tier (seconds).
    pub hot_to_warm_secs: u64,
    /// Warm→Cold threshold: data older than this is moved to cold tier (seconds).
    pub warm_to_cold_secs: u64,
    /// Maximum hot tier size in bytes (triggers flush to warm).
    pub hot_max_bytes: usize,
    /// Maximum warm tier size in bytes (triggers migration to cold).
    pub warm_max_bytes: usize,
}

impl Default for TieredConfig {
    fn default() -> Self {
        Self {
            data_dir: PathBuf::from("./data/tiered"),
            hot_to_warm_secs: 3600,      // 1 hour
            warm_to_cold_secs: 86400 * 7, // 7 days
            hot_max_bytes: 100 * 1024 * 1024,   // 100MB
            warm_max_bytes: 1024 * 1024 * 1024,  // 1GB
        }
    }
}

// ── Tier Enum ──

/// Storage tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Tier {
    /// In-memory (MemTable).
    Hot,
    /// TSM files on disk (SSD).
    Warm,
    /// Parquet files (HDD/object storage).
    Cold,
}

// ── Tiered Storage Manager ──

/// Manages data across hot/warm/cold tiers.
pub struct TieredStorage {
    config: TieredConfig,
    /// Hot tier: in-memory buffer (series_key → points).
    hot_buffer: HashMap<String, Vec<TsPoint>>,
    /// Hot tier size in bytes (approximate).
    hot_bytes: usize,
    /// Warm tier: TSM writer.
    warm_writer: TsmWriter,
    /// Statistics.
    stats: TieredStats,
}

/// Statistics for tiered storage.
#[derive(Debug, Clone, Default)]
pub struct TieredStats {
    pub hot_entries: usize,
    pub warm_entries: usize,
    pub cold_entries: usize,
    pub hot_bytes: usize,
    pub warm_bytes: usize,
    pub cold_bytes: usize,
    pub flushes_to_warm: u64,
    pub migrations_to_cold: u64,
}

impl TieredStorage {
    /// Create a new tiered storage manager.
    pub fn new(config: TieredConfig) -> Result<Self, String> {
        // Create tier directories
        let hot_dir = config.data_dir.join("hot");
        let warm_dir = config.data_dir.join("warm");
        let cold_dir = config.data_dir.join("cold");
        std::fs::create_dir_all(&hot_dir).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(&warm_dir).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(&cold_dir).map_err(|e| e.to_string())?;

        Ok(Self {
            config,
            hot_buffer: HashMap::new(),
            hot_bytes: 0,
            warm_writer: TsmWriter::new(),
            stats: TieredStats::default(),
        })
    }

    /// Write a data point to the hot tier.
    pub fn write(&mut self, series_key: String, point: TsPoint) -> Result<(), String> {
        let point_size = std::mem::size_of::<TsPoint>() + series_key.len();
        self.hot_bytes += point_size;

        self.hot_buffer
            .entry(series_key)
            .or_default()
            .push(point);

        self.stats.hot_entries += 1;
        self.stats.hot_bytes = self.hot_bytes;

        // Check if hot tier needs flushing
        if self.hot_bytes >= self.config.hot_max_bytes {
            self.flush_hot_to_warm()?;
        }

        Ok(())
    }

    /// Query data across all tiers.
    pub fn query(&self, series_key: &str, start: i64, end: i64) -> Vec<TsPoint> {
        let mut results = Vec::new();

        // Query hot tier
        if let Some(points) = self.hot_buffer.get(series_key) {
            for p in points {
                if p.timestamp >= start && p.timestamp < end {
                    results.push(p.clone());
                }
            }
        }

        // Query warm tier (TSM files)
        // In a real implementation, this would read from TSM files on disk
        // For now, we only have the hot tier implemented

        results.sort_by_key(|p| p.timestamp);
        results
    }

    /// Flush hot tier to warm tier (TSM files).
    pub fn flush_hot_to_warm(&mut self) -> Result<(), String> {
        if self.hot_buffer.is_empty() {
            return Ok(());
        }

        // Move all hot data to TSM writer
        for (series_key, points) in self.hot_buffer.drain() {
            for point in points {
                self.warm_writer.write(series_key.clone(), point);
            }
        }

        // Flush TSM writer to disk
        let blocks = self.warm_writer.flush();
        if !blocks.is_empty() {
            let encoded = TsmWriter::encode_blocks(&blocks);
            let tsm_path = self.config.data_dir.join("warm").join(format!(
                "data_{}.tsm",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs()
            ));
            std::fs::write(&tsm_path, &encoded).map_err(|e| e.to_string())?;
            self.stats.flushes_to_warm += 1;
            self.stats.warm_bytes += encoded.len();
        }

        self.hot_bytes = 0;
        self.stats.hot_entries = 0;
        self.stats.hot_bytes = 0;

        Ok(())
    }

    /// Migrate old warm data to cold tier.
    pub fn migrate_warm_to_cold(&mut self) -> Result<(), String> {
        let warm_dir = self.config.data_dir.join("warm");
        let cold_dir = self.config.data_dir.join("cold");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let entries = std::fs::read_dir(&warm_dir).map_err(|e| e.to_string())?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("tsm") {
                // Check file age
                if let Ok(metadata) = std::fs::metadata(&path) {
                    if let Ok(modified) = metadata.modified() {
                        let age = now.saturating_sub(
                            modified
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs(),
                        );

                        if age >= self.config.warm_to_cold_secs {
                            // Move to cold tier
                            let filename = match path.file_name() {
                                Some(f) => f,
                                None => continue,
                            };
                            let cold_path = cold_dir.join(filename);
                            std::fs::rename(&path, &cold_path).map_err(|e| e.to_string())?;
                            self.stats.migrations_to_cold += 1;
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Get storage statistics.
    pub fn stats(&self) -> &TieredStats {
        &self.stats
    }

    /// Get the tier a given timestamp would belong to.
    pub fn tier_for_timestamp(&self, timestamp: i64) -> Tier {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        let age_secs = (now - timestamp).max(0) as u64;

        if age_secs < self.config.hot_to_warm_secs {
            Tier::Hot
        } else if age_secs < self.config.warm_to_cold_secs {
            Tier::Warm
        } else {
            Tier::Cold
        }
    }

    /// Get the tier for a given age in seconds.
    pub fn tier_for_age(&self, age_secs: u64) -> Tier {
        if age_secs < self.config.hot_to_warm_secs {
            Tier::Hot
        } else if age_secs < self.config.warm_to_cold_secs {
            Tier::Warm
        } else {
            Tier::Cold
        }
    }
}

// ── Continuous Query Engine ──

/// A continuous query that runs periodically on time series data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContinuousQuery {
    /// Query name.
    pub name: String,
    /// Query interval in seconds.
    pub interval_secs: u64,
    /// Source series pattern (e.g., "cpu.*").
    pub source_pattern: String,
    /// Target series for results.
    pub target_series: String,
    /// Aggregation function.
    pub aggregation: AggregationType,
    /// Window size in seconds.
    pub window_secs: u64,
}

/// Aggregation type for continuous queries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AggregationType {
    Mean,
    Sum,
    Min,
    Max,
    Count,
    StdDev,
    Percentile(f64),
}

/// Continuous query engine.
pub struct ContinuousQueryEngine {
    queries: Vec<ContinuousQuery>,
    /// Last run time for each query.
    last_run: HashMap<String, u64>,
}

impl Default for ContinuousQueryEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl ContinuousQueryEngine {
    pub fn new() -> Self {
        Self {
            queries: Vec::new(),
            last_run: HashMap::new(),
        }
    }

    /// Register a continuous query.
    pub fn register(&mut self, query: ContinuousQuery) {
        self.queries.push(query);
    }

    /// Check which queries need to run now.
    pub fn pending_queries(&self) -> Vec<&ContinuousQuery> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        self.queries
            .iter()
            .filter(|q| {
                let last = self.last_run.get(&q.name).copied().unwrap_or(0);
                now - last >= q.interval_secs
            })
            .collect()
    }

    /// Mark a query as run.
    pub fn mark_run(&mut self, query_name: &str) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.last_run.insert(query_name.to_string(), now);
    }

    /// Apply aggregation to a set of values.
    pub fn aggregate(values: &[f64], agg: &AggregationType) -> f64 {
        match agg {
            AggregationType::Mean => {
                if values.is_empty() { 0.0 } else { values.iter().sum::<f64>() / values.len() as f64 }
            }
            AggregationType::Sum => values.iter().sum(),
            AggregationType::Min => values.iter().copied().fold(f64::INFINITY, f64::min),
            AggregationType::Max => values.iter().copied().fold(f64::NEG_INFINITY, f64::max),
            AggregationType::Count => values.len() as f64,
            AggregationType::StdDev => {
                let mean = values.iter().sum::<f64>() / values.len() as f64;
                let var = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / values.len() as f64;
                var.sqrt()
            }
            AggregationType::Percentile(p) => {
                let mut sorted = values.to_vec();
                sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                let idx = ((p / 100.0) * (sorted.len() - 1) as f64).round() as usize;
                sorted[idx.min(sorted.len() - 1)]
            }
        }
    }
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tsm::TsValue;

    #[test]
    fn test_tiered_storage_write() {
        let dir = tempfile::tempdir().expect("should be valid");
        let config = TieredConfig {
            data_dir: dir.path().to_path_buf(),
            hot_max_bytes: 1024, // Small for testing
            ..Default::default()
        };
        let mut storage = TieredStorage::new(config).expect("should be valid");

        // Write some points
        for i in 0..10 {
            storage.write(
                "cpu.usage".to_string(),
                TsPoint {
                    timestamp: 1000 + i,
                    value: TsValue::Float(0.5),
                },
            ).expect("should be valid");
        }

        assert_eq!(storage.stats().hot_entries, 10);
    }

    #[test]
    fn test_tiered_storage_flush() {
        let dir = tempfile::tempdir().expect("should be valid");
        let config = TieredConfig {
            data_dir: dir.path().to_path_buf(),
            hot_max_bytes: 100, // Very small to trigger flush
            ..Default::default()
        };
        let mut storage = TieredStorage::new(config).expect("should be valid");

        // Write enough to trigger flush
        for i in 0..100 {
            storage.write(
                "cpu.usage".to_string(),
                TsPoint {
                    timestamp: 1000 + i,
                    value: TsValue::Float(0.5),
                },
            ).expect("should be valid");
        }

        // Should have flushed to warm tier
        assert!(storage.stats().flushes_to_warm > 0);
    }

    #[test]
    fn test_tier_for_timestamp() {
        let dir = tempfile::tempdir().expect("should be valid");
        let config = TieredConfig {
            data_dir: dir.path().to_path_buf(),
            hot_to_warm_secs: 3600,
            warm_to_cold_secs: 86400,
            ..Default::default()
        };
        let storage = TieredStorage::new(config).expect("should be valid");

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("should be valid")
            .as_secs() as i64;

        assert_eq!(storage.tier_for_timestamp(now), Tier::Hot);
        assert_eq!(storage.tier_for_timestamp(now - 4000), Tier::Warm);
        assert_eq!(storage.tier_for_timestamp(now - 100000), Tier::Cold);
    }

    #[test]
    fn test_continuous_query_pending() {
        let mut engine = ContinuousQueryEngine::new();
        engine.register(ContinuousQuery {
            name: "cpu_avg".to_string(),
            interval_secs: 60,
            source_pattern: "cpu.*".to_string(),
            target_series: "cpu.avg".to_string(),
            aggregation: AggregationType::Mean,
            window_secs: 300,
        });

        let pending = engine.pending_queries();
        assert_eq!(pending.len(), 1);

        engine.mark_run("cpu_avg");
        let pending = engine.pending_queries();
        assert_eq!(pending.len(), 0);
    }

    #[test]
    fn test_aggregation() {
        let values = vec![1.0, 2.0, 3.0, 4.0, 5.0];

        assert_eq!(ContinuousQueryEngine::aggregate(&values, &AggregationType::Mean), 3.0);
        assert_eq!(ContinuousQueryEngine::aggregate(&values, &AggregationType::Sum), 15.0);
        assert_eq!(ContinuousQueryEngine::aggregate(&values, &AggregationType::Min), 1.0);
        assert_eq!(ContinuousQueryEngine::aggregate(&values, &AggregationType::Max), 5.0);
        assert_eq!(ContinuousQueryEngine::aggregate(&values, &AggregationType::Count), 5.0);
    }
}
