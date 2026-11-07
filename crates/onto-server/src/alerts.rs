// Copyright (c) 2024-2026 Guo Jianzhu (郭建筑)
// Licensed under the Apache License, Version 2.0.
// See LICENSE for details.
//! Basic threshold alerting for OntoDB (community edition).
//!
//! Periodically evaluates built-in metrics against configurable thresholds.
//! Firing alerts are dispatched through configured channels (log / webhook / file)
//! and exposed at `GET /api/alerts`.
//!
//! Community scope: fixed metric set, log + webhook + file output, sustained
//! threshold matching (`for_duration`).
//! Enterprise adds: custom rules, email/IM channels, alert history, silencing.

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::metrics::SharedMetrics;

// ═══════════════════════════════════════════════════════
//  Alert channels — where fired alerts are delivered
// ═══════════════════════════════════════════════════════

/// A delivery channel for alert notifications.
pub trait AlertChannel: Send + Sync {
    /// Human-readable channel name (for logs / diagnostics).
    fn name(&self) -> &str;
    /// Deliver one alert event. Return `Err` on transient failure.
    fn dispatch(&self, event: &AlertEvent) -> Result<(), String>;
}

/// A single alert delivery payload.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AlertEvent {
    pub rule: String,
    pub description: String,
    pub severity: AlertSeverity,
    /// `"fired"` or `"resolved"`.
    pub action: String,
    pub metric: String,
    pub current_value: f64,
    pub threshold: f64,
    pub message: String,
    /// Unix timestamp of the event.
    pub timestamp: u64,
}

// ── Log channel ─────────────────────────────────────────

/// Writes alerts to the structured log (`tracing`).
pub struct LogChannel;

impl AlertChannel for LogChannel {
    fn name(&self) -> &str {
        "log"
    }
    fn dispatch(&self, event: &AlertEvent) -> Result<(), String> {
        let level_prefix = match event.severity {
            AlertSeverity::Warning => "WARN",
            AlertSeverity::Critical => "CRITICAL",
        };
        match event.action.as_str() {
            "fired" => tracing::warn!(
                alert = %event.rule,
                metric = %event.metric,
                value = event.current_value,
                threshold = event.threshold,
                "ALERT [{level_prefix}] {}: {}",
                event.rule,
                event.message,
            ),
            "resolved" => tracing::info!(
                alert = %event.rule,
                "ALERT RESOLVED: {}",
                event.message,
            ),
            _ => tracing::info!(alert = %event.rule, "{}", event.message),
        }
        Ok(())
    }
}

// ── Webhook channel ─────────────────────────────────────

/// POSTs alert events as JSON to a configured HTTP endpoint.
///
/// Plain `http://` only — the basic channel has no TLS support. An `https://`
/// URL is rejected at dispatch time with an explicit error.
pub struct WebhookChannel {
    pub url: String,
    pub timeout_secs: u64,
}

impl AlertChannel for WebhookChannel {
    fn name(&self) -> &str {
        "webhook"
    }
    fn dispatch(&self, event: &AlertEvent) -> Result<(), String> {
        if self.url.starts_with("https://") {
            return Err(
                "https webhooks are not supported by the basic channel (plain http only); \
                 use an http:// endpoint or a local proxy"
                    .to_string(),
            );
        }
        let body = serde_json::to_string(event).map_err(|e| e.to_string())?;
        // Use a blocking HTTP call on a short-lived thread so we don't pull in
        // an async HTTP client just for alerts.
        let url = self.url.clone();
        let timeout = Duration::from_secs(self.timeout_secs.max(1));
        let handle = std::thread::spawn(move || -> Result<(), String> {
            let client = std::net::TcpStream::connect_timeout(
                &resolve_host(&url)?,
                timeout,
            )
            .map_err(|e| format!("webhook connect: {e}"))?;
            client
                .set_read_timeout(Some(timeout))
                .map_err(|e| e.to_string())?;
            client
                .set_write_timeout(Some(timeout))
                .map_err(|e| e.to_string())?;
            let (host, port, path) = parse_http_url(&url)?;
            let request = format!(
                "POST {path} HTTP/1.1\r\nHost: {host}:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let mut stream = client;
            stream
                .write_all(request.as_bytes())
                .map_err(|e| format!("webhook write: {e}"))?;
            Ok(())
        });
        handle.join().unwrap_or_else(|_| Err("webhook thread panicked".into()))
    }
}

fn resolve_host(url: &str) -> Result<std::net::SocketAddr, String> {
    let (_, rest) = url
        .split_once("://")
        .ok_or_else(|| "webhook URL must include scheme".to_string())?;
    let hostport = rest.split('/').next().unwrap_or(rest);
    let (host, port) = if let Some((h, p)) = hostport.rsplit_once(':') {
        (h, p.parse::<u16>().map_err(|_| "invalid port")?)
    } else if url.starts_with("https") {
        (hostport, 443)
    } else {
        (hostport, 80)
    };
    use std::net::ToSocketAddrs;
    let mut addrs = (host, port)
        .to_socket_addrs()
        .map_err(|e| format!("resolve {host}: {e}"))?;
    addrs.next().ok_or_else(|| format!("no address for {host}"))
}

fn parse_http_url(url: &str) -> Result<(String, u16, String), String> {
    let (_, rest) = url
        .split_once("://")
        .ok_or_else(|| "URL must include scheme".to_string())?;
    let (hostport, path) = match rest.find('/') {
        Some(i) => (&rest[..i], rest[i..].to_string()),
        None => (rest, "/".to_string()),
    };
    let (host, port) = if let Some((h, p)) = hostport.rsplit_once(':') {
        (
            h.to_string(),
            p.parse::<u16>().map_err(|_| "invalid port".to_string())?,
        )
    } else if url.starts_with("https") {
        (hostport.to_string(), 443)
    } else {
        (hostport.to_string(), 80)
    };
    Ok((host, port, path))
}

// ── File channel ────────────────────────────────────────

/// Appends alert events as JSONL to a local file.
pub struct FileChannel {
    pub path: PathBuf,
}

impl AlertChannel for FileChannel {
    fn name(&self) -> &str {
        "file"
    }
    fn dispatch(&self, event: &AlertEvent) -> Result<(), String> {
        let line = serde_json::to_string(event).map_err(|e| e.to_string())?;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|e| format!("open {}: {e}", self.path.display()))?;
        writeln!(f, "{line}").map_err(|e| format!("write: {e}"))
    }
}

// ── Dispatcher ──────────────────────────────────────────

/// Fans out alert events to all configured channels.
pub struct ChannelDispatcher {
    channels: Vec<Box<dyn AlertChannel>>,
}

impl ChannelDispatcher {
    pub fn new(channels: Vec<Box<dyn AlertChannel>>) -> Self {
        Self { channels }
    }

    /// Build a dispatcher from configuration. Always includes `LogChannel`.
    pub fn from_config(
        webhook_url: Option<&str>,
        file_path: Option<&str>,
    ) -> Self {
        let mut channels: Vec<Box<dyn AlertChannel>> = vec![Box::new(LogChannel)];
        if let Some(url) = webhook_url {
            if !url.is_empty() {
                channels.push(Box::new(WebhookChannel {
                    url: url.to_string(),
                    timeout_secs: 5,
                }));
            }
        }
        if let Some(path) = file_path {
            if !path.is_empty() {
                channels.push(Box::new(FileChannel {
                    path: PathBuf::from(path),
                }));
            }
        }
        Self { channels }
    }

    /// Deliver an event to every channel. Failures are logged, not propagated.
    pub fn dispatch(&self, event: &AlertEvent) {
        for ch in &self.channels {
            if let Err(e) = ch.dispatch(event) {
                tracing::warn!(
                    channel = ch.name(),
                    alert = %event.rule,
                    "alert dispatch failed: {e}"
                );
            }
        }
    }

    pub fn channel_names(&self) -> Vec<&str> {
        self.channels.iter().map(|c| c.name()).collect()
    }
}

/// Which built-in metric an alert rule watches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AlertMetric {
    /// Process memory usage as a fraction of the configured cap (0.0–1.0).
    MemoryUsagePercent,
    /// Disk usage as a fraction of the data-dir partition (0.0–1.0).
    DiskUsagePercent,
    /// Slow queries per minute (rolling window).
    SlowQueryRate,
    /// Query error rate as a fraction of total queries (0.0–1.0).
    ErrorRate,
    /// Currently active HTTP + TCP connections.
    ActiveConnections,
}

/// Alert severity — `Warning` is informational, `Critical` needs attention.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AlertSeverity {
    Warning,
    Critical,
}

/// Lifecycle state of an alert.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AlertStatus {
    /// Below threshold.
    Ok,
    /// Threshold exceeded and cooldown elapsed.
    Firing,
    /// Was firing, now back below threshold.
    Resolved,
}

/// A single alert rule definition.
#[derive(Debug, Clone)]
pub struct AlertRule {
    pub name: &'static str,
    pub description: &'static str,
    pub metric: AlertMetric,
    /// Threshold value (meaning depends on `metric`).
    pub threshold: f64,
    pub severity: AlertSeverity,
    /// Minimum seconds between repeated firings of the same rule.
    pub cooldown_secs: u64,
    /// Sustained seconds the threshold must be exceeded before firing (0 = immediate).
    pub for_duration_secs: u64,
}

/// Runtime state of one alert rule.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AlertState {
    pub rule: String,
    pub description: String,
    pub severity: AlertSeverity,
    pub status: AlertStatus,
    /// Metric identifier (e.g. `memory_usage_percent`).
    pub metric: String,
    /// Latest observed metric value.
    pub current_value: f64,
    pub threshold: f64,
    /// When the alert last transitioned to `Firing` (RFC 3339-ish).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fired_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<String>,
    pub message: String,
}

struct RuleTracker {
    status: AlertStatus,
    last_fired: Option<Instant>,
    last_resolved: Option<Instant>,
    /// When the threshold was first exceeded (for `for_duration` matching).
    exceeded_since: Option<Instant>,
}

/// Evaluates alert rules against live metrics on a fixed interval.
pub struct AlertManager {
    rules: Vec<AlertRule>,
    trackers: Mutex<HashMap<String, RuleTracker>>,
    states: Mutex<HashMap<String, AlertState>>,
    metrics: SharedMetrics,
    /// Previous cumulative slow-query count (for delta-based rate).
    prev_slow_queries: Mutex<u64>,
    /// Evaluation interval in seconds (for rate normalisation).
    check_interval_secs: u64,
    /// Channel dispatcher for alert delivery.
    dispatcher: ChannelDispatcher,
}

/// Configuration for the alert manager (populated from CLI args).
#[derive(Debug, Clone)]
pub struct AlertConfig {
    /// Memory usage fraction that triggers a warning (0.0–1.0).
    pub memory_warn: f64,
    /// Memory usage fraction that triggers critical.
    pub memory_critical: f64,
    /// Disk usage fraction that triggers a warning.
    pub disk_warn: f64,
    /// Disk usage fraction that triggers critical.
    pub disk_critical: f64,
    /// Slow queries per minute that triggers a warning.
    pub slow_query_warn: f64,
    /// Slow queries per minute that triggers critical.
    pub slow_query_critical: f64,
    /// Error rate fraction that triggers a warning.
    pub error_rate_warn: f64,
    /// Error rate fraction that triggers critical.
    pub error_rate_critical: f64,
    /// Active connections that triggers a warning.
    pub connections_warn: u64,
    /// Active connections that triggers critical.
    pub connections_critical: u64,
    /// Seconds between evaluations.
    pub check_interval_secs: u64,
    /// Sustained seconds threshold must be exceeded before firing (0 = immediate).
    pub for_duration_secs: u64,
    /// Optional webhook URL for alert delivery.
    pub webhook_url: Option<String>,
    /// Optional file path for JSONL alert log.
    pub file_path: Option<String>,
}

impl Default for AlertConfig {
    fn default() -> Self {
        Self {
            memory_warn: 0.85,
            memory_critical: 0.95,
            disk_warn: 0.90,
            disk_critical: 0.95,
            slow_query_warn: 10.0,
            slow_query_critical: 50.0,
            error_rate_warn: 0.05,
            error_rate_critical: 0.20,
            connections_warn: 1000,
            connections_critical: 5000,
            check_interval_secs: 30,
            for_duration_secs: 0,
            webhook_url: None,
            file_path: None,
        }
    }
}

fn now_iso() -> String {
    // Simple ISO-ish timestamp without pulling in chrono formatting.
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("unix:{secs}")
}

impl AlertManager {
    pub fn new(metrics: SharedMetrics, config: &AlertConfig) -> Self {
        let rules = vec![
            // ── Memory ──
            AlertRule {
                name: "memory_usage_warning",
                description: "Process memory usage is high",
                metric: AlertMetric::MemoryUsagePercent,
                threshold: config.memory_warn,
                severity: AlertSeverity::Warning,
                cooldown_secs: 300,
                for_duration_secs: config.for_duration_secs,
            },
            AlertRule {
                name: "memory_usage_critical",
                description: "Process memory usage is critically high",
                metric: AlertMetric::MemoryUsagePercent,
                threshold: config.memory_critical,
                severity: AlertSeverity::Critical,
                cooldown_secs: 300,
                for_duration_secs: config.for_duration_secs,
            },
            // ── Disk ──
            AlertRule {
                name: "disk_usage_warning",
                description: "Disk usage is high",
                metric: AlertMetric::DiskUsagePercent,
                threshold: config.disk_warn,
                severity: AlertSeverity::Warning,
                cooldown_secs: 600,
                for_duration_secs: config.for_duration_secs,
            },
            AlertRule {
                name: "disk_usage_critical",
                description: "Disk usage is critically high",
                metric: AlertMetric::DiskUsagePercent,
                threshold: config.disk_critical,
                severity: AlertSeverity::Critical,
                cooldown_secs: 600,
                for_duration_secs: config.for_duration_secs,
            },
            // ── Slow queries ──
            AlertRule {
                name: "slow_query_rate_warning",
                description: "Slow query rate is elevated",
                metric: AlertMetric::SlowQueryRate,
                threshold: config.slow_query_warn,
                severity: AlertSeverity::Warning,
                cooldown_secs: 300,
                for_duration_secs: config.for_duration_secs,
            },
            AlertRule {
                name: "slow_query_rate_critical",
                description: "Slow query rate is very high",
                metric: AlertMetric::SlowQueryRate,
                threshold: config.slow_query_critical,
                severity: AlertSeverity::Critical,
                cooldown_secs: 300,
                for_duration_secs: config.for_duration_secs,
            },
            // ── Error rate ──
            AlertRule {
                name: "error_rate_warning",
                description: "Query error rate is elevated",
                metric: AlertMetric::ErrorRate,
                threshold: config.error_rate_warn,
                severity: AlertSeverity::Warning,
                cooldown_secs: 300,
                for_duration_secs: config.for_duration_secs,
            },
            AlertRule {
                name: "error_rate_critical",
                description: "Query error rate is critically high",
                metric: AlertMetric::ErrorRate,
                threshold: config.error_rate_critical,
                severity: AlertSeverity::Critical,
                cooldown_secs: 300,
                for_duration_secs: config.for_duration_secs,
            },
            // ── Connections ──
            AlertRule {
                name: "active_connections_warning",
                description: "Active connection count is high",
                metric: AlertMetric::ActiveConnections,
                threshold: config.connections_warn as f64,
                severity: AlertSeverity::Warning,
                cooldown_secs: 300,
                for_duration_secs: config.for_duration_secs,
            },
            AlertRule {
                name: "active_connections_critical",
                description: "Active connection count is critically high",
                metric: AlertMetric::ActiveConnections,
                threshold: config.connections_critical as f64,
                severity: AlertSeverity::Critical,
                cooldown_secs: 300,
                for_duration_secs: config.for_duration_secs,
            },
        ];

        let trackers = rules
            .iter()
            .map(|r| {
                (
                    r.name.to_string(),
                    RuleTracker {
                        status: AlertStatus::Ok,
                        last_fired: None,
                        last_resolved: None,
                        exceeded_since: None,
                    },
                )
            })
            .collect();

        let states = rules
            .iter()
            .map(|r| {
                (
                    r.name.to_string(),
                    AlertState {
                        rule: r.name.to_string(),
                        description: r.description.to_string(),
                        severity: r.severity,
                        status: AlertStatus::Ok,
                        metric: metric_label(r.metric).to_string(),
                        current_value: 0.0,
                        threshold: r.threshold,
                        fired_at: None,
                        resolved_at: None,
                        message: "ok".to_string(),
                    },
                )
            })
            .collect();

        let dispatcher = ChannelDispatcher::from_config(
            config.webhook_url.as_deref(),
            config.file_path.as_deref(),
        );

        Self {
            rules,
            trackers: Mutex::new(trackers),
            states: Mutex::new(states),
            metrics,
            prev_slow_queries: Mutex::new(0),
            check_interval_secs: config.check_interval_secs.max(5),
            dispatcher,
        }
    }

    /// Evaluate all rules once against current metrics.
    pub fn evaluate(&self) {
        let current_values = self.snapshot_metric_values();

        for rule in &self.rules {
            let value = current_values
                .get(&rule.metric)
                .copied()
                .unwrap_or(0.0);

            let exceeded = value >= rule.threshold;

            let mut trackers = self.trackers.lock().unwrap_or_else(|e| e.into_inner());
            let mut states = self.states.lock().unwrap_or_else(|e| e.into_inner());
            let tracker = trackers.entry(rule.name.to_string()).or_insert(RuleTracker {
                status: AlertStatus::Ok,
                last_fired: None,
                last_resolved: None,
                exceeded_since: None,
            });

            let now = Instant::now();

            if exceeded {
                // Track when the threshold was first crossed.
                let since = *tracker.exceeded_since.get_or_insert(now);
                let sustained_secs = now.duration_since(since).as_secs();

                // Only fire after the sustained duration has elapsed.
                let duration_met = rule.for_duration_secs == 0
                    || sustained_secs >= rule.for_duration_secs;

                let cooldown_elapsed = tracker
                    .last_fired
                    .map(|t| now.duration_since(t).as_secs() >= rule.cooldown_secs)
                    .unwrap_or(true);

                if tracker.status != AlertStatus::Firing && duration_met && cooldown_elapsed {
                    tracker.status = AlertStatus::Firing;
                    tracker.last_fired = Some(now);

                    let msg = format!(
                        "{} = {:.4} (threshold {:.4}, sustained {}s)",
                        metric_label(rule.metric),
                        value,
                        rule.threshold,
                        sustained_secs,
                    );

                    // Update state.
                    if let Some(state) = states.get_mut(rule.name) {
                        state.status = AlertStatus::Firing;
                        state.current_value = value;
                        state.fired_at = Some(now_iso());
                        state.message = msg.clone();
                    }

                    // Dispatch through channels.
                    let event = AlertEvent {
                        rule: rule.name.to_string(),
                        description: rule.description.to_string(),
                        severity: rule.severity,
                        action: "fired".to_string(),
                        metric: metric_label(rule.metric).to_string(),
                        current_value: value,
                        threshold: rule.threshold,
                        message: msg,
                        timestamp: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs(),
                    };
                    self.dispatcher.dispatch(&event);
                } else if tracker.status == AlertStatus::Firing {
                    // Still firing — update value.
                    if let Some(state) = states.get_mut(rule.name) {
                        state.current_value = value;
                    }
                } else if !duration_met {
                    // Waiting for sustained duration.
                    if let Some(state) = states.get_mut(rule.name) {
                        state.status = AlertStatus::Ok;
                        state.current_value = value;
                        state.message = format!(
                            "pending ({} of {}s sustained)",
                            sustained_secs, rule.for_duration_secs
                        );
                    }
                } else {
                    // Exceeded and sustained, but still inside the cooldown
                    // window after a resolve — keep the value fresh, no re-fire.
                    if let Some(state) = states.get_mut(rule.name) {
                        state.current_value = value;
                    }
                }
            } else {
                // Below threshold — reset sustained timer.
                tracker.exceeded_since = None;

                if tracker.status == AlertStatus::Firing {
                    tracker.status = AlertStatus::Resolved;
                    tracker.last_resolved = Some(now);
                    let msg = format!("resolved ({value:.4} < {:.4})", rule.threshold);

                    if let Some(state) = states.get_mut(rule.name) {
                        state.status = AlertStatus::Resolved;
                        state.current_value = value;
                        state.resolved_at = Some(now_iso());
                        state.message = msg.clone();
                    }

                    let event = AlertEvent {
                        rule: rule.name.to_string(),
                        description: rule.description.to_string(),
                        severity: rule.severity,
                        action: "resolved".to_string(),
                        metric: metric_label(rule.metric).to_string(),
                        current_value: value,
                        threshold: rule.threshold,
                        message: msg,
                        timestamp: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs(),
                    };
                    self.dispatcher.dispatch(&event);
                } else {
                    if let Some(state) = states.get_mut(rule.name) {
                        state.status = AlertStatus::Ok;
                        state.current_value = value;
                    }
                }
            }
        }
    }

    /// Collect current values for all alert metrics.
    fn snapshot_metric_values(&self) -> HashMap<AlertMetric, f64> {
        let mut map = HashMap::new();

        // Memory: use the reported RSS. On platforms where RSS is 0 (Windows/macOS
        // placeholder), fall back to 0 so alerts stay quiet rather than firing
        // spuriously.
        let mem_bytes = self.metrics.memory_usage_bytes.get();
        // We don't know total RAM from metrics alone; treat the configured
        // memtable + a heuristic as a soft cap. For the basic alert we compare
        // RSS against 4 GiB as a pragmatic ceiling for "high memory".
        let mem_cap: f64 = 4.0 * 1024.0 * 1024.0 * 1024.0; // 4 GiB
        let mem_pct = if mem_bytes > 0 {
            (mem_bytes as f64 / mem_cap).min(1.0)
        } else {
            0.0
        };
        map.insert(AlertMetric::MemoryUsagePercent, mem_pct);

        // Disk: use disk_usage_bytes / wal + memtable as a rough indicator.
        // For basic alerting, compare disk_usage_bytes against 100 GiB soft cap.
        let disk_bytes = self.metrics.disk_usage_bytes.get();
        let disk_cap: f64 = 100.0 * 1024.0 * 1024.0 * 1024.0; // 100 GiB
        let disk_pct = if disk_bytes > 0 {
            (disk_bytes as f64 / disk_cap).min(1.0)
        } else {
            0.0
        };
        map.insert(AlertMetric::DiskUsagePercent, disk_pct);

        // Slow query rate: delta of cumulative counter, normalised to per-minute.
        let current_slow = self.metrics.slow_queries_total.get();
        let delta = {
            let mut prev = self.prev_slow_queries.lock().unwrap_or_else(|e| e.into_inner());
            let d = current_slow.saturating_sub(*prev);
            *prev = current_slow;
            d
        };
        let slow_rate = delta as f64 * (60.0 / self.check_interval_secs as f64);
        map.insert(AlertMetric::SlowQueryRate, slow_rate);

        // Error rate: query_errors / queries_total.
        let total = self.metrics.queries_total.get();
        let errors = self.metrics.query_errors.get();
        let err_rate = if total > 0 {
            errors as f64 / total as f64
        } else {
            0.0
        };
        map.insert(AlertMetric::ErrorRate, err_rate);

        // Active connections.
        let conns = self.metrics.http_connections_active.get()
            + self.metrics.tcp_connections_active.get();
        map.insert(AlertMetric::ActiveConnections, conns as f64);

        map
    }

    /// Get a snapshot of all alert states (for the HTTP endpoint).
    pub fn states_snapshot(&self) -> Vec<AlertState> {
        let states = self.states.lock().unwrap_or_else(|e| e.into_inner());
        states.values().cloned().collect()
    }

    /// Count of currently firing alerts.
    pub fn firing_count(&self) -> usize {
        let states = self.states.lock().unwrap_or_else(|e| e.into_inner());
        states.values().filter(|s| s.status == AlertStatus::Firing).count()
    }

    /// Names of the delivery channels this manager dispatches to.
    pub fn channel_names(&self) -> Vec<String> {
        self.dispatcher.channel_names().iter().map(|s| s.to_string()).collect()
    }
}

fn metric_label(m: AlertMetric) -> &'static str {
    match m {
        AlertMetric::MemoryUsagePercent => "memory_usage_percent",
        AlertMetric::DiskUsagePercent => "disk_usage_percent",
        AlertMetric::SlowQueryRate => "slow_query_rate",
        AlertMetric::ErrorRate => "error_rate",
        AlertMetric::ActiveConnections => "active_connections",
    }
}

/// Shared alert manager.
pub type SharedAlertManager = Arc<AlertManager>;

/// Spawn the background evaluation loop. Returns the shared manager.
pub fn spawn_alert_loop(
    metrics: SharedMetrics,
    config: AlertConfig,
) -> SharedAlertManager {
    let manager = Arc::new(AlertManager::new(metrics, &config));
    let mgr = manager.clone();
    let interval = Duration::from_secs(config.check_interval_secs.max(5));

    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            mgr.evaluate();
        }
    });

    manager
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::Metrics;

    fn manager_with(config: AlertConfig) -> (AlertManager, Arc<Metrics>) {
        let metrics = Arc::new(Metrics::new());
        let mgr = AlertManager::new(metrics.clone(), &config);
        (mgr, metrics)
    }

    fn state_of(mgr: &AlertManager, rule: &str) -> AlertState {
        mgr.states_snapshot()
            .into_iter()
            .find(|s| s.rule == rule)
            .unwrap_or_else(|| panic!("no state for rule {rule}"))
    }

    fn sample_event(action: &str) -> AlertEvent {
        AlertEvent {
            rule: "test_rule".to_string(),
            description: "test description".to_string(),
            severity: AlertSeverity::Warning,
            action: action.to_string(),
            metric: "test_metric".to_string(),
            current_value: 1.5,
            threshold: 1.0,
            message: "test message".to_string(),
            timestamp: 42,
        }
    }

    // ── Rule matching ────────────────────────────────────

    #[test]
    fn fires_immediately_when_for_duration_zero() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("alerts.jsonl");
        let config = AlertConfig {
            connections_warn: 100,
            connections_critical: 500,
            for_duration_secs: 0,
            file_path: Some(file.to_string_lossy().into_owned()),
            ..Default::default()
        };
        let (mgr, metrics) = manager_with(config);
        metrics.http_connections_active.set(150);
        mgr.evaluate();

        let st = state_of(&mgr, "active_connections_warning");
        assert_eq!(st.status, AlertStatus::Firing);
        assert_eq!(st.current_value, 150.0);
        assert!(st.fired_at.is_some());
        assert_eq!(mgr.firing_count(), 1);

        // Critical threshold (500) not crossed.
        let st = state_of(&mgr, "active_connections_critical");
        assert_eq!(st.status, AlertStatus::Ok);

        // The fired event reached the file channel as JSONL.
        let content = std::fs::read_to_string(&file).unwrap();
        assert!(content.contains("\"rule\":\"active_connections_warning\""));
        assert!(content.contains("\"action\":\"fired\""));
    }

    #[test]
    fn for_duration_requires_sustained_exceedance() {
        let config = AlertConfig {
            connections_warn: 100,
            connections_critical: 500,
            for_duration_secs: 1,
            ..Default::default()
        };
        let (mgr, metrics) = manager_with(config);
        metrics.tcp_connections_active.set(150);

        // First crossing: must wait out the sustained window.
        mgr.evaluate();
        let st = state_of(&mgr, "active_connections_warning");
        assert_ne!(st.status, AlertStatus::Firing);
        assert!(st.message.contains("pending"), "message: {}", st.message);

        // A dip below threshold resets the sustained timer.
        metrics.tcp_connections_active.set(0);
        mgr.evaluate();
        metrics.tcp_connections_active.set(150);
        mgr.evaluate();
        let st = state_of(&mgr, "active_connections_warning");
        assert_ne!(st.status, AlertStatus::Firing);

        // Sustained long enough → fires.
        std::thread::sleep(Duration::from_millis(1200));
        mgr.evaluate();
        let st = state_of(&mgr, "active_connections_warning");
        assert_eq!(st.status, AlertStatus::Firing);
    }

    #[test]
    fn resolves_when_value_drops_below_threshold() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("alerts.jsonl");
        let config = AlertConfig {
            connections_warn: 100,
            connections_critical: 500,
            for_duration_secs: 0,
            file_path: Some(file.to_string_lossy().into_owned()),
            ..Default::default()
        };
        let (mgr, metrics) = manager_with(config);
        metrics.http_connections_active.set(150);
        mgr.evaluate();
        assert_eq!(state_of(&mgr, "active_connections_warning").status, AlertStatus::Firing);

        metrics.http_connections_active.set(0);
        mgr.evaluate();
        let st = state_of(&mgr, "active_connections_warning");
        assert_eq!(st.status, AlertStatus::Resolved);
        assert!(st.resolved_at.is_some());
        assert_eq!(mgr.firing_count(), 0);

        let content = std::fs::read_to_string(&file).unwrap();
        assert!(content.contains("\"action\":\"resolved\""));
    }

    #[test]
    fn cooldown_blocks_immediate_refire() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("alerts.jsonl");
        let config = AlertConfig {
            connections_warn: 100,
            connections_critical: 500,
            for_duration_secs: 0,
            file_path: Some(file.to_string_lossy().into_owned()),
            ..Default::default()
        };
        let (mgr, metrics) = manager_with(config);

        metrics.http_connections_active.set(150);
        mgr.evaluate(); // fire
        metrics.http_connections_active.set(0);
        mgr.evaluate(); // resolve
        metrics.http_connections_active.set(150);
        mgr.evaluate(); // re-exceed inside cooldown → no re-fire

        let st = state_of(&mgr, "active_connections_warning");
        assert_ne!(st.status, AlertStatus::Firing, "cooldown must block re-fire");
        assert_eq!(st.current_value, 150.0, "value still tracked during cooldown");

        let content = std::fs::read_to_string(&file).unwrap();
        let fired = content.lines().filter(|l| l.contains("\"action\":\"fired\"")).count();
        assert_eq!(fired, 1, "exactly one fired event despite two exceedances");
    }

    // ── Channels ─────────────────────────────────────────

    #[test]
    fn file_channel_appends_jsonl() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let ch = FileChannel { path: path.clone() };
        ch.dispatch(&sample_event("fired")).unwrap();
        ch.dispatch(&sample_event("resolved")).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content.lines().count(), 2);
        let v: serde_json::Value = serde_json::from_str(content.lines().next().unwrap()).unwrap();
        assert_eq!(v["rule"], "test_rule");
        assert_eq!(v["action"], "fired");
        assert_eq!(v["severity"], "warning");
    }

    #[test]
    fn webhook_rejects_https() {
        let ch = WebhookChannel {
            url: "https://example.com/hook".to_string(),
            timeout_secs: 1,
        };
        let err = ch.dispatch(&sample_event("fired")).unwrap_err();
        assert!(err.contains("https"), "error should mention https: {err}");
    }

    #[test]
    fn dispatcher_fans_out_to_all_channels() {
        struct Mock {
            seen: Arc<Mutex<Vec<String>>>,
        }
        impl AlertChannel for Mock {
            fn name(&self) -> &str {
                "mock"
            }
            fn dispatch(&self, event: &AlertEvent) -> Result<(), String> {
                self.seen.lock().unwrap().push(event.rule.clone());
                Ok(())
            }
        }

        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fanout.jsonl");
        let dispatcher = ChannelDispatcher::new(vec![
            Box::new(Mock { seen: seen.clone() }),
            Box::new(FileChannel { path: path.clone() }),
        ]);

        dispatcher.dispatch(&sample_event("fired"));

        assert_eq!(*seen.lock().unwrap(), vec!["test_rule".to_string()]);
        assert!(std::fs::read_to_string(&path).unwrap().contains("test_rule"));
    }

    #[test]
    fn dispatcher_builds_channels_from_config() {
        let d = ChannelDispatcher::from_config(None, None);
        assert_eq!(d.channel_names(), vec!["log"]);

        let d = ChannelDispatcher::from_config(Some("http://example.com/hook"), Some("/tmp/a.jsonl"));
        assert_eq!(d.channel_names(), vec!["log", "webhook", "file"]);

        // Empty strings are ignored, log channel always present.
        let d = ChannelDispatcher::from_config(Some(""), Some(""));
        assert_eq!(d.channel_names(), vec!["log"]);
    }
}
