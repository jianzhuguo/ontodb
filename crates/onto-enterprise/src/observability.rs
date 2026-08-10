//! Observability module for OntoDB Enterprise.
//!
//! Provides:
//! - Slow query logging with configurable threshold
//! - Alert rule configuration
//! - Log aggregation and export
//! - Health check aggregation

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Instant;
use parking_lot::RwLock;

/// Observability configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservabilityConfig {
    /// Enable slow query logging.
    pub slow_query_enabled: bool,
    /// Slow query threshold in milliseconds.
    pub slow_query_threshold_ms: u64,
    /// Maximum slow query entries to keep.
    pub slow_query_max_entries: usize,
    /// Enable alert rules.
    pub alerting_enabled: bool,
    /// Alert rules.
    pub alert_rules: Vec<AlertRule>,
    /// Enable log export.
    pub log_export_enabled: bool,
    /// Log export endpoint (e.g., Elasticsearch, Loki).
    pub log_export_endpoint: Option<String>,
}

/// Alert rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlertRule {
    /// Rule name.
    pub name: String,
    /// Metric to monitor.
    pub metric: AlertMetric,
    /// Condition.
    pub condition: AlertCondition,
    /// Threshold value.
    pub threshold: f64,
    /// Duration in seconds before triggering.
    pub duration_secs: u64,
    /// Severity level.
    pub severity: AlertSeverity,
    /// Webhook URL for notification.
    pub webhook_url: Option<String>,
}

/// Alert metric.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AlertMetric {
    /// Query latency P99 in milliseconds.
    QueryLatencyP99,
    /// Error rate (0-1).
    ErrorRate,
    /// Connection count.
    ConnectionCount,
    /// Memory usage in bytes.
    MemoryUsage,
    /// Disk usage in bytes.
    DiskUsage,
    /// QPS (queries per second).
    Qps,
}

/// Alert condition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AlertCondition {
    GreaterThan,
    LessThan,
    Equal,
}

/// Alert severity.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum AlertSeverity {
    Critical,
    Warning,
    Info,
}

/// Slow query entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlowQueryEntry {
    /// SQL query.
    pub query: String,
    /// Execution time in milliseconds.
    pub elapsed_ms: f64,
    /// Timestamp (Unix millis).
    pub timestamp_ms: u64,
    /// Client IP.
    pub client_ip: Option<String>,
    /// Error message (if any).
    pub error: Option<String>,
    /// Rows returned/affected.
    pub rows: usize,
}

/// Alert event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlertEvent {
    /// Rule name.
    pub rule_name: String,
    /// Severity.
    pub severity: AlertSeverity,
    /// Current metric value.
    pub current_value: f64,
    /// Threshold.
    pub threshold: f64,
    /// Timestamp (Unix millis).
    pub timestamp_ms: u64,
    /// Resolved?
    pub resolved: bool,
}

/// Log entry for export.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    /// Log level.
    pub level: LogLevel,
    /// Message.
    pub message: String,
    /// Timestamp (Unix millis).
    pub timestamp_ms: u64,
    /// Module/component.
    pub module: String,
    /// Additional fields.
    pub fields: std::collections::HashMap<String, String>,
}

/// Log level.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

/// Observability manager.
pub struct ObservabilityManager {
    config: ObservabilityConfig,
    slow_queries: Arc<RwLock<VecDeque<SlowQueryEntry>>>,
    alerts: Arc<RwLock<VecDeque<AlertEvent>>>,
    logs: Arc<RwLock<VecDeque<LogEntry>>>,
}

impl ObservabilityManager {
    /// Create a new observability manager.
    pub fn new(config: ObservabilityConfig) -> Self {
        Self {
            config,
            slow_queries: Arc::new(RwLock::new(VecDeque::new())),
            alerts: Arc::new(RwLock::new(VecDeque::new())),
            logs: Arc::new(RwLock::new(VecDeque::new())),
        }
    }

    /// Record a query execution.
    pub fn record_query(
        &self,
        query: &str,
        elapsed_ms: f64,
        rows: usize,
        error: Option<&str>,
        client_ip: Option<&str>,
    ) {
        // Log slow queries
        if self.config.slow_query_enabled && elapsed_ms >= self.config.slow_query_threshold_ms as f64 {
            let entry = SlowQueryEntry {
                query: query.to_string(),
                elapsed_ms,
                timestamp_ms: current_timestamp_ms(),
                client_ip: client_ip.map(|s| s.to_string()),
                error: error.map(|s| s.to_string()),
                rows,
            };

            let mut slow_queries = self.slow_queries.write();
            if slow_queries.len() >= self.config.slow_query_max_entries {
                slow_queries.pop_front();
            }
            slow_queries.push_back(entry);

            tracing::warn!(
                "Slow query ({:.1}ms): {}",
                elapsed_ms,
                if query.len() > 100 { &query[..100] } else { query }
            );
        }

        // Log to aggregation
        let level = if error.is_some() {
            LogLevel::Error
        } else if elapsed_ms >= self.config.slow_query_threshold_ms as f64 {
            LogLevel::Warn
        } else {
            LogLevel::Info
        };

        self.log(level, &format!("Query executed in {:.1}ms", elapsed_ms), "query", {
            let mut fields = std::collections::HashMap::new();
            fields.insert("query".to_string(), query.to_string());
            fields.insert("elapsed_ms".to_string(), format!("{:.1}", elapsed_ms));
            fields.insert("rows".to_string(), rows.to_string());
            if let Some(err) = error {
                fields.insert("error".to_string(), err.to_string());
            }
            fields
        });
    }

    /// Get slow query log.
    pub fn get_slow_queries(&self, limit: usize) -> Vec<SlowQueryEntry> {
        let slow_queries = self.slow_queries.read();
        let start = if slow_queries.len() > limit { slow_queries.len() - limit } else { 0 };
        slow_queries.iter().skip(start).cloned().collect()
    }

    /// Clear slow query log.
    pub fn clear_slow_queries(&self) {
        self.slow_queries.write().clear();
    }

    /// Log a message.
    pub fn log(
        &self,
        level: LogLevel,
        message: &str,
        module: &str,
        fields: std::collections::HashMap<String, String>,
    ) {
        let entry = LogEntry {
            level,
            message: message.to_string(),
            timestamp_ms: current_timestamp_ms(),
            module: module.to_string(),
            fields,
        };

        let mut logs = self.logs.write();
        if logs.len() >= 10000 {
            logs.pop_front();
        }
        logs.push_back(entry);
    }

    /// Get recent logs.
    pub fn get_logs(&self, limit: usize) -> Vec<LogEntry> {
        let logs = self.logs.read();
        let start = if logs.len() > limit { logs.len() - limit } else { 0 };
        logs.iter().skip(start).cloned().collect()
    }

    /// Check alert rules against current metrics.
    pub fn check_alerts(&self, metrics: &AlertMetrics) -> Vec<AlertEvent> {
        if !self.config.alerting_enabled {
            return Vec::new();
        }

        let mut new_alerts = Vec::new();

        for rule in &self.config.alert_rules {
            let current_value = match rule.metric {
                AlertMetric::QueryLatencyP99 => metrics.query_latency_p99_ms,
                AlertMetric::ErrorRate => metrics.error_rate,
                AlertMetric::ConnectionCount => metrics.connection_count as f64,
                AlertMetric::MemoryUsage => metrics.memory_usage_bytes as f64,
                AlertMetric::DiskUsage => metrics.disk_usage_bytes as f64,
                AlertMetric::Qps => metrics.qps,
            };

            let triggered = match rule.condition {
                AlertCondition::GreaterThan => current_value > rule.threshold,
                AlertCondition::LessThan => current_value < rule.threshold,
                AlertCondition::Equal => (current_value - rule.threshold).abs() < 0.001,
            };

            if triggered {
                let event = AlertEvent {
                    rule_name: rule.name.clone(),
                    severity: rule.severity.clone(),
                    current_value,
                    threshold: rule.threshold,
                    timestamp_ms: current_timestamp_ms(),
                    resolved: false,
                };
                new_alerts.push(event);
            }
        }

        // Store alerts
        if !new_alerts.is_empty() {
            let mut alerts = self.alerts.write();
            for alert in &new_alerts {
                alerts.push_back(alert.clone());
            }
            // Keep last 1000 alerts
            while alerts.len() > 1000 {
                alerts.pop_front();
            }
        }

        new_alerts
    }

    /// Get recent alerts.
    pub fn get_alerts(&self, limit: usize) -> Vec<AlertEvent> {
        let alerts = self.alerts.read();
        let start = if alerts.len() > limit { alerts.len() - limit } else { 0 };
        alerts.iter().skip(start).cloned().collect()
    }

    /// Get observability status.
    pub fn status(&self) -> ObservabilityStatus {
        ObservabilityStatus {
            slow_query_count: self.slow_queries.read().len(),
            alert_count: self.alerts.read().len(),
            log_count: self.logs.read().len(),
            slow_query_enabled: self.config.slow_query_enabled,
            alerting_enabled: self.config.alerting_enabled,
        }
    }
}

/// Current metrics for alert checking.
#[derive(Debug, Clone)]
pub struct AlertMetrics {
    pub query_latency_p99_ms: f64,
    pub error_rate: f64,
    pub connection_count: u64,
    pub memory_usage_bytes: u64,
    pub disk_usage_bytes: u64,
    pub qps: f64,
}

/// Observability status.
#[derive(Debug, Clone, Serialize)]
pub struct ObservabilityStatus {
    pub slow_query_count: usize,
    pub alert_count: usize,
    pub log_count: usize,
    pub slow_query_enabled: bool,
    pub alerting_enabled: bool,
}

impl Default for ObservabilityConfig {
    fn default() -> Self {
        Self {
            slow_query_enabled: true,
            slow_query_threshold_ms: 1000,
            slow_query_max_entries: 1000,
            alerting_enabled: false,
            alert_rules: Vec::new(),
            log_export_enabled: false,
            log_export_endpoint: None,
        }
    }
}

fn current_timestamp_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_observability_manager_new() {
        let config = ObservabilityConfig::default();
        let manager = ObservabilityManager::new(config);
        let status = manager.status();
        assert_eq!(status.slow_query_count, 0);
        assert_eq!(status.alert_count, 0);
    }

    #[test]
    fn test_record_slow_query() {
        let config = ObservabilityConfig {
            slow_query_enabled: true,
            slow_query_threshold_ms: 100,
            ..Default::default()
        };
        let manager = ObservabilityManager::new(config);

        // Fast query - should not be recorded
        manager.record_query("SELECT 1", 10.0, 1, None, None);
        assert_eq!(manager.get_slow_queries(10).len(), 0);

        // Slow query - should be recorded
        manager.record_query("SELECT * FROM large_table", 500.0, 1000, None, Some("127.0.0.1"));
        assert_eq!(manager.get_slow_queries(10).len(), 1);
    }

    #[test]
    fn test_record_error_query() {
        let config = ObservabilityConfig::default();
        let manager = ObservabilityManager::new(config);

        manager.record_query("SELECT * FROM nonexistent", 5.0, 0, Some("table not found"), None);
        let logs = manager.get_logs(10);
        assert!(logs.iter().any(|l| matches!(l.level, LogLevel::Error)));
    }

    #[test]
    fn test_slow_query_max_entries() {
        let config = ObservabilityConfig {
            slow_query_enabled: true,
            slow_query_threshold_ms: 10,
            slow_query_max_entries: 3,
            ..Default::default()
        };
        let manager = ObservabilityManager::new(config);

        for i in 0..5 {
            manager.record_query(&format!("query {}", i), 100.0, 0, None, None);
        }

        assert_eq!(manager.get_slow_queries(10).len(), 3);
    }

    #[test]
    fn test_alert_check() {
        let config = ObservabilityConfig {
            alerting_enabled: true,
            alert_rules: vec![AlertRule {
                name: "high_latency".to_string(),
                metric: AlertMetric::QueryLatencyP99,
                condition: AlertCondition::GreaterThan,
                threshold: 500.0,
                duration_secs: 0,
                severity: AlertSeverity::Warning,
                webhook_url: None,
            }],
            ..Default::default()
        };
        let manager = ObservabilityManager::new(config);

        let metrics = AlertMetrics {
            query_latency_p99_ms: 600.0,
            error_rate: 0.01,
            connection_count: 10,
            memory_usage_bytes: 1024 * 1024 * 100,
            disk_usage_bytes: 1024 * 1024 * 1000,
            qps: 1000.0,
        };

        let alerts = manager.check_alerts(&metrics);
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].rule_name, "high_latency");
    }

    #[test]
    fn test_log_export() {
        let config = ObservabilityConfig::default();
        let manager = ObservabilityManager::new(config);

        manager.log(LogLevel::Info, "test message", "test", std::collections::HashMap::new());
        let logs = manager.get_logs(10);
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].message, "test message");
    }
}
