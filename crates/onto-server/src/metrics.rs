//! Runtime metrics collection and Prometheus export for OntoDB.
//!
//! Collects and exposes metrics in Prometheus exposition format.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

/// Atomic counter wrapper for lock-free metrics.
#[derive(Debug)]
pub struct AtomicCounter {
    value: AtomicU64,
}

impl AtomicCounter {
    pub const fn new() -> Self {
        Self {
            value: AtomicU64::new(0),
        }
    }

    pub fn inc(&self) {
        self.value.fetch_add(1, Ordering::Relaxed);
    }

    pub fn dec(&self) {
        self.value.fetch_sub(1, Ordering::Relaxed);
    }

    pub fn add(&self, n: u64) {
        self.value.fetch_add(n, Ordering::Relaxed);
    }

    pub fn set(&self, n: u64) {
        self.value.store(n, Ordering::Relaxed);
    }

    pub fn get(&self) -> u64 {
        self.value.load(Ordering::Relaxed)
    }
}

/// Histogram bucket for latency tracking.
#[derive(Debug)]
pub struct Histogram {
    /// Bucket boundaries in microseconds.
    buckets: Vec<f64>,
    /// Counts per bucket.
    counts: Vec<AtomicU64>,
    /// Total sum of observed values.
    sum: AtomicU64,
    /// Total number of observations.
    count: AtomicU64,
}

impl Histogram {
    /// Create a new histogram with standard latency buckets (in seconds).
    pub fn new_latency() -> Self {
        // Buckets in seconds: 1ms, 5ms, 10ms, 25ms, 50ms, 100ms, 250ms, 500ms, 1s, 2.5s, 5s, 10s
        let buckets = vec![
            0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
        ];
        let counts: Vec<AtomicU64> = buckets.iter().map(|_| AtomicU64::new(0)).collect();
        // +1 for +Inf bucket
        let mut all_counts = counts;
        all_counts.push(AtomicU64::new(0));

        Self {
            buckets,
            counts: all_counts,
            sum: AtomicU64::new(0),
            count: AtomicU64::new(0),
        }
    }

    /// Observe a value (in seconds).
    pub fn observe(&self, value: f64) {
        // Update sum (store as microseconds for precision)
        let micros = (value * 1_000_000.0) as u64;
        self.sum.fetch_add(micros, Ordering::Relaxed);
        self.count.fetch_add(1, Ordering::Relaxed);

        // Find the right bucket
        for (i, &boundary) in self.buckets.iter().enumerate() {
            if value <= boundary {
                self.counts[i].fetch_add(1, Ordering::Relaxed);
                return;
            }
        }
        // +Inf bucket
        self.counts.last().unwrap().fetch_add(1, Ordering::Relaxed);
    }

    /// Get the total sum in seconds.
    pub fn sum(&self) -> f64 {
        self.sum.load(Ordering::Relaxed) as f64 / 1_000_000.0
    }

    /// Get the total count.
    pub fn count(&self) -> u64 {
        self.count.load(Ordering::Relaxed)
    }

    /// Export in Prometheus histogram format.
    pub fn to_prometheus(&self, name: &str, help: &str) -> String {
        let mut output = String::new();
        output.push_str(&format!("# HELP {} {}\n", name, help));
        output.push_str(&format!("# TYPE {} histogram\n", name));

        let mut cumulative = 0u64;
        for (i, boundary) in self.buckets.iter().enumerate() {
            cumulative += self.counts[i].load(Ordering::Relaxed);
            output.push_str(&format!("{}{{le=\"{}\"}} {}\n", name, boundary, cumulative));
        }
        // +Inf
        cumulative += self.counts.last().unwrap().load(Ordering::Relaxed);
        output.push_str(&format!("{}{{le=\"+Inf\"}} {}\n", name, cumulative));
        output.push_str(&format!("{}_sum {}\n", name, self.sum()));
        output.push_str(&format!("{}_count {}\n", name, self.count()));

        output
    }
}

/// Comprehensive metrics collection for OntoDB.
#[derive(Debug)]
pub struct Metrics {
    // ── Query metrics ──
    /// Total queries received.
    pub queries_total: AtomicCounter,
    /// Total queries by type (SELECT, INSERT, UPDATE, DELETE, etc.)
    pub queries_select: AtomicCounter,
    pub queries_insert: AtomicCounter,
    pub queries_update: AtomicCounter,
    pub queries_delete: AtomicCounter,
    pub queries_vector_search: AtomicCounter,
    pub queries_other: AtomicCounter,
    /// Query errors.
    pub query_errors: AtomicCounter,
    /// Parse errors.
    pub parse_errors: AtomicCounter,
    /// Query latency histogram.
    pub query_latency: Histogram,

    // ── Vector search metrics ──
    /// Vector search latency.
    pub vector_search_latency: Histogram,
    /// Total vector search results returned.
    pub vector_search_results: AtomicCounter,

    // ── Connection metrics ──
    /// Total HTTP connections.
    pub http_connections_total: AtomicCounter,
    /// Currently active HTTP connections.
    pub http_connections_active: AtomicCounter,
    /// Total TCP connections.
    pub tcp_connections_total: AtomicCounter,
    /// Currently active TCP connections.
    pub tcp_connections_active: AtomicCounter,

    // ── Authentication metrics ──
    /// Total auth attempts.
    pub auth_attempts: AtomicCounter,
    /// Auth successes.
    pub auth_successes: AtomicCounter,
    /// Auth failures.
    pub auth_failures: AtomicCounter,

    // ── Rate limiting metrics ──
    /// Total requests rate-limited.
    pub rate_limited_total: AtomicCounter,

    // ── Storage metrics ──
    /// Total SSTable count (snapshot).
    pub sstable_count: AtomicCounter,
    /// Total entries in storage.
    pub storage_entries: AtomicCounter,
    /// Compaction count.
    pub compactions_total: AtomicCounter,

    // ── Slow query metrics ──
    /// Queries exceeding the slow query threshold.
    pub slow_queries_total: AtomicCounter,

    // ── System resource metrics ──
    /// Process memory usage in bytes (RSS).
    pub memory_usage_bytes: AtomicCounter,
    /// Number of open file descriptors.
    pub open_file_descriptors: AtomicCounter,

    // ── Storage engine details ──
    /// WAL file size in bytes.
    pub wal_size_bytes: AtomicCounter,
    /// MemTable size in bytes.
    pub memtable_size_bytes: AtomicCounter,
    /// Total disk usage in bytes.
    pub disk_usage_bytes: AtomicCounter,

    // ── Cache metrics ──
    /// Block cache hits.
    pub block_cache_hits_total: AtomicCounter,
    /// Block cache misses.
    pub block_cache_misses_total: AtomicCounter,
    /// Block cache evictions.
    pub block_cache_evictions_total: AtomicCounter,

    // ── Transaction metrics ──
    /// Currently active transactions.
    pub active_transactions: AtomicCounter,

    // ── WAL metrics ──
    /// Total WAL writes.
    pub wal_writes_total: AtomicCounter,
    /// WAL sync latency histogram.
    pub wal_sync_latency: Histogram,

    // ── Compaction metrics ──
    /// Pending compaction tasks.
    pub compaction_pending: AtomicCounter,
    /// Compaction latency histogram.
    pub compaction_latency: Histogram,

    // ── Backup metrics ──
    /// Last backup timestamp (Unix seconds).
    pub last_backup_timestamp: AtomicCounter,
    /// Last backup size in bytes.
    pub last_backup_size_bytes: AtomicCounter,

    // ── Raft metrics ──
    /// Raft state: 0=follower, 1=leader, 2=candidate, 3=standalone.
    pub raft_state: AtomicCounter,
    /// Raft log lag (entries behind leader).
    pub raft_log_lag: AtomicCounter,

    // ── Server info ──
    /// Server start time.
    pub started_at: Instant,
}

impl Metrics {
    pub fn new() -> Self {
        Self {
            queries_total: AtomicCounter::new(),
            queries_select: AtomicCounter::new(),
            queries_insert: AtomicCounter::new(),
            queries_update: AtomicCounter::new(),
            queries_delete: AtomicCounter::new(),
            queries_vector_search: AtomicCounter::new(),
            queries_other: AtomicCounter::new(),
            query_errors: AtomicCounter::new(),
            parse_errors: AtomicCounter::new(),
            query_latency: Histogram::new_latency(),
            vector_search_latency: Histogram::new_latency(),
            vector_search_results: AtomicCounter::new(),
            http_connections_total: AtomicCounter::new(),
            http_connections_active: AtomicCounter::new(),
            tcp_connections_total: AtomicCounter::new(),
            tcp_connections_active: AtomicCounter::new(),
            auth_attempts: AtomicCounter::new(),
            auth_successes: AtomicCounter::new(),
            auth_failures: AtomicCounter::new(),
            rate_limited_total: AtomicCounter::new(),
            sstable_count: AtomicCounter::new(),
            storage_entries: AtomicCounter::new(),
            compactions_total: AtomicCounter::new(),
            slow_queries_total: AtomicCounter::new(),
            memory_usage_bytes: AtomicCounter::new(),
            open_file_descriptors: AtomicCounter::new(),
            wal_size_bytes: AtomicCounter::new(),
            memtable_size_bytes: AtomicCounter::new(),
            disk_usage_bytes: AtomicCounter::new(),
            block_cache_hits_total: AtomicCounter::new(),
            block_cache_misses_total: AtomicCounter::new(),
            block_cache_evictions_total: AtomicCounter::new(),
            active_transactions: AtomicCounter::new(),
            wal_writes_total: AtomicCounter::new(),
            wal_sync_latency: Histogram::new_latency(),
            compaction_pending: AtomicCounter::new(),
            compaction_latency: Histogram::new_latency(),
            last_backup_timestamp: AtomicCounter::new(),
            last_backup_size_bytes: AtomicCounter::new(),
            raft_state: AtomicCounter::new(),
            raft_log_lag: AtomicCounter::new(),
            started_at: Instant::now(),
        }
    }

    /// Record a completed query.
    pub fn record_query(&self, query_type: &str, latency_secs: f64, success: bool) {
        self.queries_total.inc();
        self.query_latency.observe(latency_secs);

        if !success {
            self.query_errors.inc();
            return;
        }

        match query_type.to_uppercase().as_str() {
            "SELECT" => self.queries_select.inc(),
            "INSERT" => self.queries_insert.inc(),
            "UPDATE" => self.queries_update.inc(),
            "DELETE" => self.queries_delete.inc(),
            "VECTOR_SEARCH" => self.queries_vector_search.inc(),
            _ => self.queries_other.inc(),
        }
    }

    /// Record a parse error.
    pub fn record_parse_error(&self) {
        self.parse_errors.inc();
    }

    /// Updates storage metrics from engine stats.
    /// Call this before exporting metrics to get accurate storage numbers.
    pub fn update_storage_stats(&self, sstables: usize, entries: usize, compactions: u64) {
        self.sstable_count.set(sstables as u64);
        self.storage_entries.set(entries as u64);
        self.compactions_total.set(compactions);
    }

    /// Export all metrics in Prometheus exposition format.
    pub fn to_prometheus(&self) -> String {
        let mut output = String::with_capacity(4096);

        // ── Server info ──
        output.push_str("# HELP ontodb_info OntoDB server information\n");
        output.push_str("# TYPE ontodb_info gauge\n");
        output.push_str(&format!(
            "ontodb_info{{version=\"{}\"}} 1\n",
            env!("CARGO_PKG_VERSION")
        ));

        let uptime = self.started_at.elapsed().as_secs();
        output.push_str("# HELP ontodb_uptime_seconds Server uptime in seconds\n");
        output.push_str("# TYPE ontodb_uptime_seconds gauge\n");
        output.push_str(&format!("ontodb_uptime_seconds {}\n", uptime));

        // ── Query counters ──
        output.push_str("# HELP ontodb_queries_total Total queries received\n");
        output.push_str("# TYPE ontodb_queries_total counter\n");
        output.push_str(&format!("ontodb_queries_total {}\n", self.queries_total.get()));

        output.push_str("# HELP ontodb_queries_by_type Total queries by type\n");
        output.push_str("# TYPE ontodb_queries_by_type counter\n");
        output.push_str(&format!(
            "ontodb_queries_by_type{{type=\"select\"}} {}\n",
            self.queries_select.get()
        ));
        output.push_str(&format!(
            "ontodb_queries_by_type{{type=\"insert\"}} {}\n",
            self.queries_insert.get()
        ));
        output.push_str(&format!(
            "ontodb_queries_by_type{{type=\"update\"}} {}\n",
            self.queries_update.get()
        ));
        output.push_str(&format!(
            "ontodb_queries_by_type{{type=\"delete\"}} {}\n",
            self.queries_delete.get()
        ));
        output.push_str(&format!(
            "ontodb_queries_by_type{{type=\"vector_search\"}} {}\n",
            self.queries_vector_search.get()
        ));
        output.push_str(&format!(
            "ontodb_queries_by_type{{type=\"other\"}} {}\n",
            self.queries_other.get()
        ));

        // ── Errors ──
        output.push_str("# HELP ontodb_query_errors_total Total query execution errors\n");
        output.push_str("# TYPE ontodb_query_errors_total counter\n");
        output.push_str(&format!(
            "ontodb_query_errors_total {}\n",
            self.query_errors.get()
        ));

        output.push_str("# HELP ontodb_parse_errors_total Total query parse errors\n");
        output.push_str("# TYPE ontodb_parse_errors_total counter\n");
        output.push_str(&format!(
            "ontodb_parse_errors_total {}\n",
            self.parse_errors.get()
        ));

        // ── Latency histograms ──
        output.push_str(&self.query_latency.to_prometheus(
            "ontodb_query_duration_seconds",
            "Query execution latency in seconds",
        ));

        output.push_str(&self.vector_search_latency.to_prometheus(
            "ontodb_vector_search_duration_seconds",
            "Vector search latency in seconds",
        ));

        // ── Vector search ──
        output.push_str(
            "# HELP ontodb_vector_search_results_total Total vector search results returned\n",
        );
        output.push_str("# TYPE ontodb_vector_search_results_total counter\n");
        output.push_str(&format!(
            "ontodb_vector_search_results_total {}\n",
            self.vector_search_results.get()
        ));

        // ── Connections ──
        output.push_str("# HELP ontodb_http_connections_total Total HTTP connections\n");
        output.push_str("# TYPE ontodb_http_connections_total counter\n");
        output.push_str(&format!(
            "ontodb_http_connections_total {}\n",
            self.http_connections_total.get()
        ));

        output.push_str("# HELP ontodb_http_connections_active Current active HTTP connections\n");
        output.push_str("# TYPE ontodb_http_connections_active gauge\n");
        output.push_str(&format!(
            "ontodb_http_connections_active {}\n",
            self.http_connections_active.get()
        ));

        output.push_str("# HELP ontodb_tcp_connections_total Total TCP connections\n");
        output.push_str("# TYPE ontodb_tcp_connections_total counter\n");
        output.push_str(&format!(
            "ontodb_tcp_connections_total {}\n",
            self.tcp_connections_total.get()
        ));

        output.push_str("# HELP ontodb_tcp_connections_active Current active TCP connections\n");
        output.push_str("# TYPE ontodb_tcp_connections_active gauge\n");
        output.push_str(&format!(
            "ontodb_tcp_connections_active {}\n",
            self.tcp_connections_active.get()
        ));

        // ── Authentication ──
        output.push_str("# HELP ontodb_auth_attempts_total Total authentication attempts\n");
        output.push_str("# TYPE ontodb_auth_attempts_total counter\n");
        output.push_str(&format!(
            "ontodb_auth_attempts_total {}\n",
            self.auth_attempts.get()
        ));

        output.push_str("# HELP ontodb_auth_successes_total Successful authentications\n");
        output.push_str("# TYPE ontodb_auth_successes_total counter\n");
        output.push_str(&format!(
            "ontodb_auth_successes_total {}\n",
            self.auth_successes.get()
        ));

        output.push_str("# HELP ontodb_auth_failures_total Failed authentications\n");
        output.push_str("# TYPE ontodb_auth_failures_total counter\n");
        output.push_str(&format!(
            "ontodb_auth_failures_total {}\n",
            self.auth_failures.get()
        ));

        // ── Rate limiting ──
        output.push_str("# HELP ontodb_rate_limited_total Total rate-limited requests\n");
        output.push_str("# TYPE ontodb_rate_limited_total counter\n");
        output.push_str(&format!(
            "ontodb_rate_limited_total {}\n",
            self.rate_limited_total.get()
        ));

        // ── Storage ──
        output.push_str("# HELP ontodb_sstable_count Current number of SSTables\n");
        output.push_str("# TYPE ontodb_sstable_count gauge\n");
        output.push_str(&format!(
            "ontodb_sstable_count {}\n",
            self.sstable_count.get()
        ));

        output.push_str("# HELP ontodb_storage_entries Total entries in storage\n");
        output.push_str("# TYPE ontodb_storage_entries gauge\n");
        output.push_str(&format!(
            "ontodb_storage_entries {}\n",
            self.storage_entries.get()
        ));

        output.push_str("# HELP ontodb_compactions_total Total compactions performed\n");
        output.push_str("# TYPE ontodb_compactions_total counter\n");
        output.push_str(&format!(
            "ontodb_compactions_total {}\n",
            self.compactions_total.get()
        ));

        // ── System resources ──
        output.push_str("# HELP ontodb_memory_usage_bytes Process memory usage (RSS)\n");
        output.push_str("# TYPE ontodb_memory_usage_bytes gauge\n");
        output.push_str(&format!("ontodb_memory_usage_bytes {}\n", self.memory_usage_bytes.get()));

        output.push_str("# HELP ontodb_open_file_descriptors Open file descriptors\n");
        output.push_str("# TYPE ontodb_open_file_descriptors gauge\n");
        output.push_str(&format!("ontodb_open_file_descriptors {}\n", self.open_file_descriptors.get()));

        // ── Storage engine details ──
        output.push_str("# HELP ontodb_wal_size_bytes WAL file size\n");
        output.push_str("# TYPE ontodb_wal_size_bytes gauge\n");
        output.push_str(&format!("ontodb_wal_size_bytes {}\n", self.wal_size_bytes.get()));

        output.push_str("# HELP ontodb_memtable_size_bytes MemTable size\n");
        output.push_str("# TYPE ontodb_memtable_size_bytes gauge\n");
        output.push_str(&format!("ontodb_memtable_size_bytes {}\n", self.memtable_size_bytes.get()));

        output.push_str("# HELP ontodb_disk_usage_bytes Total disk usage\n");
        output.push_str("# TYPE ontodb_disk_usage_bytes gauge\n");
        output.push_str(&format!("ontodb_disk_usage_bytes {}\n", self.disk_usage_bytes.get()));

        // ── Cache ──
        output.push_str("# HELP ontodb_block_cache_hits_total Block cache hits\n");
        output.push_str("# TYPE ontodb_block_cache_hits_total counter\n");
        output.push_str(&format!("ontodb_block_cache_hits_total {}\n", self.block_cache_hits_total.get()));

        output.push_str("# HELP ontodb_block_cache_misses_total Block cache misses\n");
        output.push_str("# TYPE ontodb_block_cache_misses_total counter\n");
        output.push_str(&format!("ontodb_block_cache_misses_total {}\n", self.block_cache_misses_total.get()));

        output.push_str("# HELP ontodb_block_cache_evictions_total Block cache evictions\n");
        output.push_str("# TYPE ontodb_block_cache_evictions_total counter\n");
        output.push_str(&format!("ontodb_block_cache_evictions_total {}\n", self.block_cache_evictions_total.get()));

        // ── Transactions ──
        output.push_str("# HELP ontodb_active_transactions Active MVCC transactions\n");
        output.push_str("# TYPE ontodb_active_transactions gauge\n");
        output.push_str(&format!("ontodb_active_transactions {}\n", self.active_transactions.get()));

        // ── WAL ──
        output.push_str("# HELP ontodb_wal_writes_total Total WAL writes\n");
        output.push_str("# TYPE ontodb_wal_writes_total counter\n");
        output.push_str(&format!("ontodb_wal_writes_total {}\n", self.wal_writes_total.get()));

        output.push_str(&self.wal_sync_latency.to_prometheus(
            "ontodb_wal_sync_duration_seconds",
            "WAL fsync latency",
        ));

        // ── Compaction ──
        output.push_str("# HELP ontodb_compaction_pending Pending compaction tasks\n");
        output.push_str("# TYPE ontodb_compaction_pending gauge\n");
        output.push_str(&format!("ontodb_compaction_pending {}\n", self.compaction_pending.get()));

        output.push_str(&self.compaction_latency.to_prometheus(
            "ontodb_compaction_duration_seconds",
            "Compaction latency",
        ));

        // ── Backup ──
        output.push_str("# HELP ontodb_last_backup_timestamp Last backup Unix timestamp\n");
        output.push_str("# TYPE ontodb_last_backup_timestamp gauge\n");
        output.push_str(&format!("ontodb_last_backup_timestamp {}\n", self.last_backup_timestamp.get()));

        output.push_str("# HELP ontodb_last_backup_size_bytes Last backup size\n");
        output.push_str("# TYPE ontodb_last_backup_size_bytes gauge\n");
        output.push_str(&format!("ontodb_last_backup_size_bytes {}\n", self.last_backup_size_bytes.get()));

        // ── Raft ──
        output.push_str("# HELP ontodb_raft_state Raft state (0=follower, 1=leader, 2=candidate, 3=standalone)\n");
        output.push_str("# TYPE ontodb_raft_state gauge\n");
        output.push_str(&format!("ontodb_raft_state {}\n", self.raft_state.get()));

        output.push_str("# HELP ontodb_raft_log_lag Raft log entries behind leader\n");
        output.push_str("# TYPE ontodb_raft_log_lag gauge\n");
        output.push_str(&format!("ontodb_raft_log_lag {}\n", self.raft_log_lag.get()));

        output
    }

    /// Export metrics as JSON (for /api/metrics endpoint).
    #[allow(dead_code)]
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "server": {
                "version": env!("CARGO_PKG_VERSION"),
                "uptime_seconds": self.started_at.elapsed().as_secs(),
            },
            "queries": {
                "total": self.queries_total.get(),
                "by_type": {
                    "select": self.queries_select.get(),
                    "insert": self.queries_insert.get(),
                    "update": self.queries_update.get(),
                    "delete": self.queries_delete.get(),
                    "vector_search": self.queries_vector_search.get(),
                    "other": self.queries_other.get(),
                },
                "errors": self.query_errors.get(),
                "parse_errors": self.parse_errors.get(),
            },
            "vector_search": {
                "results_total": self.vector_search_results.get(),
            },
            "connections": {
                "http_total": self.http_connections_total.get(),
                "http_active": self.http_connections_active.get(),
                "tcp_total": self.tcp_connections_total.get(),
                "tcp_active": self.tcp_connections_active.get(),
            },
            "auth": {
                "attempts": self.auth_attempts.get(),
                "successes": self.auth_successes.get(),
                "failures": self.auth_failures.get(),
            },
            "rate_limiting": {
                "limited_total": self.rate_limited_total.get(),
            },
            "storage": {
                "sstable_count": self.sstable_count.get(),
                "entries": self.storage_entries.get(),
                "compactions": self.compactions_total.get(),
            }
        })
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

/// Shared metrics state.
pub type SharedMetrics = Arc<Metrics>;
