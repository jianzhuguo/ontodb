//! Query audit logging for OntoDB.
//!
//! Logs all queries with metadata (user, timestamp, duration, status)
//! for security compliance and forensic analysis.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::SystemTime;

/// Audit log entry.
#[derive(Debug, serde::Serialize)]
pub struct AuditEntry {
    /// ISO 8601 timestamp.
    pub timestamp: String,
    /// Client IP address.
    pub client_ip: String,
    /// API key identifier (first 8 chars, rest masked).
    pub api_key_id: String,
    /// Query type (SELECT, INSERT, UPDATE, DELETE, SPARQL, etc.).
    pub query_type: String,
    /// Query text (truncated to max_query_log_len).
    pub query: String,
    /// Execution time in milliseconds.
    pub duration_ms: f64,
    /// Whether the query succeeded.
    pub success: bool,
    /// Error message if failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Number of rows affected/returned.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rows_affected: Option<usize>,
}

/// Audit logger configuration.
#[derive(Debug, Clone)]
pub struct AuditConfig {
    /// Whether audit logging is enabled.
    pub enabled: bool,
    /// Directory for audit log files.
    pub log_dir: PathBuf,
    /// Maximum query text length to log (default: 1000).
    pub max_query_log_len: usize,
    /// Whether to log successful queries (default: true).
    pub log_success: bool,
    /// Whether to log failed queries (default: true).
    pub log_failures: bool,
}

impl Default for AuditConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            log_dir: PathBuf::from("audit_logs"),
            max_query_log_len: 1000,
            log_success: true,
            log_failures: true,
        }
    }
}

/// Thread-safe audit logger.
pub struct AuditLogger {
    config: AuditConfig,
    file: Mutex<Option<std::fs::File>>,
}

impl AuditLogger {
    /// Create a new audit logger.
    pub fn new(config: AuditConfig) -> Self {
        let file = if config.enabled {
            fs::create_dir_all(&config.log_dir).ok();
            let path = config.log_dir.join(format!("audit_{}.jsonl", today_str()));
            Some(OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .unwrap_or_else(|e| {
                    eprintln!("Failed to open audit log: {}", e);
                    // Fallback to stderr
                    return OpenOptions::new().write(true).open("/dev/null").unwrap_or_else(|_| {
                        // On Windows, create a temp file
                        let tmp = std::env::temp_dir().join("ontodb_audit_fallback.jsonl");
                        OpenOptions::new().create(true).append(true).open(tmp).unwrap()
                    });
                }))
        } else {
            None
        };

        Self {
            config,
            file: Mutex::new(file),
        }
    }

    /// Log an audit entry.
    pub fn log(&self, entry: &AuditEntry) {
        if !self.config.enabled {
            return;
        }

        if !entry.success && !self.config.log_failures {
            return;
        }
        if entry.success && !self.config.log_success {
            return;
        }

        if let Ok(json) = serde_json::to_string(entry) {
            if let Ok(mut file) = self.file.lock() {
                if let Some(f) = file.as_mut() {
                    let _ = writeln!(f, "{}", json);
                    let _ = f.flush();
                }
            }
        }
    }

    /// Create an audit entry from query execution metadata.
    pub fn create_entry(
        &self,
        client_ip: &str,
        api_key: Option<&str>,
        query_type: &str,
        query: &str,
        duration_ms: f64,
        success: bool,
        error: Option<String>,
    ) -> AuditEntry {
        let truncated_query = if query.len() > self.config.max_query_log_len {
            format!("{}...", &query[..self.config.max_query_log_len])
        } else {
            query.to_string()
        };

        let key_id = api_key
            .map(|k| {
                if k.len() > 8 {
                    format!("{}...", &k[..8])
                } else {
                    k.to_string()
                }
            })
            .unwrap_or_else(|| "anonymous".to_string());

        AuditEntry {
            timestamp: iso_timestamp(),
            client_ip: client_ip.to_string(),
            api_key_id: key_id,
            query_type: query_type.to_string(),
            query: truncated_query,
            duration_ms,
            success,
            error,
            rows_affected: None,
        }
    }
}

fn iso_timestamp() -> String {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let days = secs / 86400;
    let remaining = secs % 86400;
    let hours = remaining / 3600;
    let minutes = (remaining % 3600) / 60;
    let seconds = remaining % 60;

    // Simplified date calculation
    let (year, month, day) = days_to_ymd(days);
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", year, month, day, hours, minutes, seconds)
}

fn today_str() -> String {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let days = now.as_secs() / 86400;
    let (year, month, day) = days_to_ymd(days);
    format!("{:04}{:02}{:02}", year, month, day)
}

fn days_to_ymd(mut days: u64) -> (u16, u8, u8) {
    let mut year = 1970u16;
    loop {
        let days_in_year = if is_leap(year) { 366 } else { 365 };
        if days < days_in_year {
            break;
        }
        days -= days_in_year;
        year += 1;
    }
    let leap = is_leap(year);
    let month_days = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut month = 1u8;
    for &md in &month_days {
        if days < md as u64 {
            break;
        }
        days -= md as u64;
        month += 1;
    }
    (year, month, (days + 1) as u8)
}

fn is_leap(year: u16) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}
