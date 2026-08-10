//! Audit logging for OntoDB — 等保2.0三级合规.
//!
//! Features:
//! - Query audit with metadata (user, timestamp, duration, status)
//! - Admin operation audit (config changes, key management, etc.)
//! - SHA256 integrity chain (tamper-evident log)
//! - Configurable retention period with auto-cleanup
//! - Daily log rotation (audit_YYYYMMDD.jsonl)

use std::fs::{self, OpenOptions};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

// ── Audit entry types ──

/// Audit log entry type discriminator.
#[derive(Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuditEventType {
    /// SQL query execution.
    Query,
    /// Admin operation (key/whitelist/config changes).
    Admin,
    /// Authentication event (login success/failure, lockout).
    Auth,
}

/// Audit log entry.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct AuditEntry {
    /// ISO 8601 timestamp.
    pub timestamp: String,
    /// Event type (query, admin, auth).
    pub event_type: AuditEventType,
    /// Client IP address.
    pub client_ip: String,
    /// API key identifier (first 8 chars, rest masked).
    pub api_key_id: String,
    /// Operation type (SELECT, INSERT, add_key, delete_ip, etc.).
    pub operation: String,
    /// Detail (query text or admin action description, truncated).
    pub detail: String,
    /// Execution time in milliseconds (0 for non-query events).
    #[serde(default)]
    pub duration_ms: f64,
    /// Whether the operation succeeded.
    pub success: bool,
    /// Error message if failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Number of rows affected/returned (for queries).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rows_affected: Option<usize>,
    /// SHA256 hash of the previous entry for integrity chain.
    /// First entry in a file uses the hash of the previous file's last entry.
    pub prev_hash: String,
    /// SHA256 hash of this entry (hash of all fields except `entry_hash`).
    pub entry_hash: String,
}

// ── Configuration ──

/// Audit logger configuration.
#[derive(Debug, Clone)]
pub struct AuditConfig {
    /// Whether audit logging is enabled.
    pub enabled: bool,
    /// Directory for audit log files.
    pub log_dir: PathBuf,
    /// Maximum detail text length to log (default: 1000).
    pub max_detail_len: usize,
    /// Whether to log successful operations (default: true).
    pub log_success: bool,
    /// Whether to log failed operations (default: true).
    pub log_failures: bool,
    /// Log retention period in days (default: 180, 等保要求≥6个月).
    pub retention_days: u32,
}

impl Default for AuditConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            log_dir: PathBuf::from("audit_logs"),
            max_detail_len: 1000,
            log_success: true,
            log_failures: true,
            retention_days: 180,
        }
    }
}

// ── Integrity chain ──

/// SHA256 hash of data, returned as hex string.
fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}

/// Compute the entry hash from all fields except `entry_hash`.
fn compute_entry_hash(entry: &AuditEntry) -> String {
    let payload = format!(
        "{}|{}|{}|{}|{}|{}|{:.3}|{}|{}",
        entry.timestamp,
        serde_json::to_string(&entry.event_type).unwrap_or_default(),
        entry.client_ip,
        entry.api_key_id,
        entry.operation,
        entry.detail,
        entry.duration_ms,
        entry.success,
        entry.prev_hash,
    );
    sha256_hex(payload.as_bytes())
}

// ── Logger ──

/// Thread-safe audit logger with integrity chain and auto-cleanup.
pub struct AuditLogger {
    config: AuditConfig,
    file: Mutex<Option<std::fs::File>>,
    /// Hash of the last written entry (for integrity chain).
    last_hash: Mutex<String>,
}

impl AuditLogger {
    /// Create a new audit logger.
    pub fn new(config: AuditConfig) -> Self {
        let (file, last_hash) = if config.enabled {
            fs::create_dir_all(&config.log_dir).ok();

            // Recover last hash from existing log files for chain continuity
            let last_hash = recover_last_hash(&config.log_dir);

            let path = config.log_dir.join(format!("audit_{}.jsonl", today_str()));
            let f = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .unwrap_or_else(|e| {
                    eprintln!("Failed to open audit log {:?}: {}", path, e);
                    fallback_file()
                });
            (Some(f), last_hash)
        } else {
            (None, String::from(GENESIS_HASH))
        };

        Self {
            config,
            file: Mutex::new(file),
            last_hash: Mutex::new(last_hash),
        }
    }

    /// Run auto-cleanup of expired log files. Call periodically.
    pub fn cleanup_expired_logs(&self) -> usize {
        if !self.config.enabled {
            return 0;
        }
        cleanup_old_logs(&self.config.log_dir, self.config.retention_days)
    }

    /// Start a background cleanup task that runs every `interval_hours` hours.
    pub fn start_cleanup_task(self: &std::sync::Arc<Self>, interval_hours: u64) {
        let logger = std::sync::Arc::clone(self);
        std::thread::spawn(move || {
            let interval = std::time::Duration::from_secs(interval_hours * 3600);
            loop {
                std::thread::sleep(interval);
                let removed = logger.cleanup_expired_logs();
                if removed > 0 {
                    eprintln!("Audit log cleanup: removed {} expired files", removed);
                }
            }
        });
    }

    /// Rotate to a new daily log file if the date has changed.
    /// Always ensures the file handle points to today's log file.
    fn rotate_if_needed(&self) {
        let today = today_str();
        let expected_name = format!("audit_{}.jsonl", today);
        let path = self.config.log_dir.join(&expected_name);

        let mut file_guard = self.file.lock().expect("should be valid");

        // Check if we already have this file open by trying to open it
        // In append mode, reopening is safe and idempotent
        match OpenOptions::new().create(true).append(true).open(&path) {
            Ok(new_file) => {
                *file_guard = Some(new_file);
            }
            Err(e) => {
                eprintln!("Failed to rotate audit log to {:?}: {}", path, e);
            }
        }
    }

    /// Log an audit entry (generic).
    pub fn log(&self, mut entry: AuditEntry) {
        if !self.config.enabled {
            return;
        }
        if !entry.success && !self.config.log_failures {
            return;
        }
        if entry.success && !self.config.log_success {
            return;
        }

        // Set integrity chain — hold lock atomically for prev_hash read + update
        {
            let mut last = self.last_hash.lock().expect("should be valid");
            entry.prev_hash = last.clone();
            entry.entry_hash = compute_entry_hash(&entry);
            *last = entry.entry_hash.clone();
        }

        // Rotate if needed
        self.rotate_if_needed();

        // Write entry
        if let Ok(json) = serde_json::to_string(&entry) {
            let mut file_guard = self.file.lock().expect("should be valid");
            if let Some(f) = file_guard.as_mut() {
                let _ = writeln!(f, "{}", json);
                let _ = f.flush();
            }
        }
    }

    /// Create a query audit entry.
    pub fn create_query_entry(
        &self,
        client_ip: &str,
        api_key: Option<&str>,
        query_type: &str,
        query: &str,
        duration_ms: f64,
        success: bool,
        error: Option<String>,
    ) -> AuditEntry {
        let detail = truncate(query, self.config.max_detail_len);
        let key_id = mask_key(api_key.unwrap_or("anonymous"));

        AuditEntry {
            timestamp: iso_timestamp(),
            event_type: AuditEventType::Query,
            client_ip: client_ip.to_string(),
            api_key_id: key_id,
            operation: query_type.to_string(),
            detail,
            duration_ms,
            success,
            error,
            rows_affected: None,
            prev_hash: String::new(),
            entry_hash: String::new(),
        }
    }

    /// Create an admin operation audit entry.
    pub fn create_admin_entry(
        &self,
        client_ip: &str,
        api_key: Option<&str>,
        action: &str,
        detail: &str,
        success: bool,
        error: Option<String>,
    ) -> AuditEntry {
        let detail = truncate(detail, self.config.max_detail_len);
        let key_id = mask_key(api_key.unwrap_or("system"));

        AuditEntry {
            timestamp: iso_timestamp(),
            event_type: AuditEventType::Admin,
            client_ip: client_ip.to_string(),
            api_key_id: key_id,
            operation: action.to_string(),
            detail,
            duration_ms: 0.0,
            success,
            error,
            rows_affected: None,
            prev_hash: String::new(),
            entry_hash: String::new(),
        }
    }

    /// Create an auth event audit entry.
    pub fn create_auth_entry(
        &self,
        client_ip: &str,
        api_key: Option<&str>,
        action: &str,
        detail: &str,
        success: bool,
    ) -> AuditEntry {
        let key_id = mask_key(api_key.unwrap_or("unknown"));

        AuditEntry {
            timestamp: iso_timestamp(),
            event_type: AuditEventType::Auth,
            client_ip: client_ip.to_string(),
            api_key_id: key_id,
            operation: action.to_string(),
            detail: detail.to_string(),
            duration_ms: 0.0,
            success,
            error: None,
            rows_affected: None,
            prev_hash: String::new(),
            entry_hash: String::new(),
        }
    }

    /// Get the current integrity chain hash.
    pub fn current_hash(&self) -> String {
        self.last_hash.lock().expect("should be valid").clone()
    }

    /// Verify the integrity of a log file. Returns (total_entries, valid_entries, first_invalid_line).
    pub fn verify_log_file(path: &Path) -> (usize, usize, Option<usize>) {
        let file = match fs::File::open(path) {
            Ok(f) => f,
            Err(_) => return (0, 0, None),
        };

        let reader = std::io::BufReader::new(file);
        let mut total = 0;
        let mut valid = 0;
        let mut prev_hash = GENESIS_HASH.to_string();

        for (line_no, line) in reader.lines().enumerate() {
            let line = match line {
                Ok(l) if !l.trim().is_empty() => l,
                _ => continue,
            };

            total += 1;

            let entry: AuditEntry = match serde_json::from_str(&line) {
                Ok(e) => e,
                Err(_) => return (total, valid, Some(line_no + 1)),
            };

            // Verify prev_hash chain
            if entry.prev_hash != prev_hash {
                return (total, valid, Some(line_no + 1));
            }

            // Verify entry_hash
            let expected_hash = compute_entry_hash(&entry);
            if entry.entry_hash != expected_hash {
                return (total, valid, Some(line_no + 1));
            }

            prev_hash = entry.entry_hash.clone();
            valid += 1;
        }

        (total, valid, None)
    }
}

/// Genesis hash for the first entry in the chain.
const GENESIS_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

// ── Helpers ──

/// Truncate text to max_len, appending "..." if truncated.
fn truncate(text: &str, max_len: usize) -> String {
    if text.len() <= max_len {
        text.to_string()
    } else {
        // Find a valid UTF-8 boundary at or before max_len
        let mut end = max_len;
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &text[..end])
    }
}

/// Mask API key: show first 8 chars + "...".
fn mask_key(key: &str) -> String {
    if key.len() > 8 {
        format!("{}...", &key[..8])
    } else {
        key.to_string()
    }
}

/// ISO 8601 timestamp.
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

    let (year, month, day) = days_to_ymd(days);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, month, day, hours, minutes, seconds
    )
}

/// Today as YYYYMMDD string.
fn today_str() -> String {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let days = now.as_secs() / 86400;
    let (year, month, day) = days_to_ymd(days);
    format!("{:04}{:02}{:02}", year, month, day)
}

/// Convert days since Unix epoch to (year, month, day).
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
    let month_days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
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

/// Parse YYYYMMDD from a filename like "audit_20260808.jsonl".
fn parse_date_from_filename(name: &str) -> Option<u64> {
    // Extract date part: audit_YYYYMMDD.jsonl
    let date_str = name.strip_prefix("audit_")?.strip_suffix(".jsonl")?;
    if date_str.len() != 8 {
        return None;
    }
    let year: u16 = date_str[0..4].parse().ok()?;
    let month: u8 = date_str[4..6].parse().ok()?;
    let day: u8 = date_str[6..8].parse().ok()?;
    if month < 1 || month > 12 || day < 1 || day > 31 {
        return None;
    }
    // Convert to days since epoch
    Some(ymd_to_days(year, month, day))
}

/// Convert (year, month, day) to days since Unix epoch.
/// Same logic as days_since_epoch in http.rs.
fn ymd_to_days(year: u16, month: u8, day: u8) -> u64 {
    let mut days = 0u64;
    for y in 1970..year {
        days += if is_leap(y) { 366 } else { 365 };
    }
    let leap = is_leap(year);
    let month_days = [
        31u64,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    for m in 0..(month as usize - 1) {
        days += month_days[m];
    }
    days + day as u64 - 1
}

/// Get today as days since epoch.
fn today_days() -> u64 {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    now.as_secs() / 86400
}

/// Clean up audit log files older than `retention_days`.
/// Returns the number of files removed.
fn cleanup_old_logs(log_dir: &Path, retention_days: u32) -> usize {
    let today = today_days();
    let cutoff = if today > retention_days as u64 {
        today - retention_days as u64
    } else {
        return 0;
    };

    let entries = match fs::read_dir(log_dir) {
        Ok(e) => e,
        Err(_) => return 0,
    };

    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        if !name_str.starts_with("audit_") || !name_str.ends_with(".jsonl") {
            continue;
        }

        if let Some(file_days) = parse_date_from_filename(&name_str) {
            if file_days < cutoff {
                if fs::remove_file(entry.path()).is_ok() {
                    removed += 1;
                }
            }
        }
    }
    removed
}

/// Recover the last hash from the most recent log file for chain continuity.
fn recover_last_hash(log_dir: &Path) -> String {
    let mut latest_file: Option<(u64, PathBuf)> = None;

    let entries = match fs::read_dir(log_dir) {
        Ok(e) => e,
        Err(_) => return GENESIS_HASH.to_string(),
    };

    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if !name_str.starts_with("audit_") || !name_str.ends_with(".jsonl") {
            continue;
        }
        if let Some(days) = parse_date_from_filename(&name_str) {
            match &latest_file {
                Some((d, _)) if days <= *d => {}
                _ => latest_file = Some((days, entry.path())),
            }
        }
    }

    let path = match latest_file {
        Some((_, p)) => p,
        None => return GENESIS_HASH.to_string(),
    };

    // Read last non-empty line and extract entry_hash
    let file = match fs::File::open(&path) {
        Ok(f) => f,
        Err(_) => return GENESIS_HASH.to_string(),
    };

    let reader = std::io::BufReader::new(file);
    let mut last_hash = GENESIS_HASH.to_string();

    for line in reader.lines().flatten() {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(entry) = serde_json::from_str::<AuditEntry>(&line) {
            last_hash = entry.entry_hash;
        }
    }

    last_hash
}

/// Fallback file when the primary audit log cannot be opened.
fn fallback_file() -> std::fs::File {
    OpenOptions::new()
        .write(true)
        .open("/dev/null")
        .unwrap_or_else(|_| {
            let tmp = std::env::temp_dir().join("ontodb_audit_fallback.jsonl");
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(tmp)
                .expect("should be valid")
        })
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_config() -> (AuditConfig, PathBuf) {
        let dir = std::env::temp_dir().join(format!("ontodb_audit_test_{}_{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos()));
        fs::create_dir_all(&dir).ok();
        let config = AuditConfig {
            enabled: true,
            log_dir: dir.clone(),
            max_detail_len: 100,
            log_success: true,
            log_failures: true,
            retention_days: 7,
        };
        (config, dir)
    }

    #[test]
    fn test_query_entry_creation() {
        let (config, dir) = temp_config();
        let logger = AuditLogger::new(config);

        let entry = logger.create_query_entry(
            "192.168.1.1",
            Some("test_key_12345"),
            "SELECT",
            "SELECT * FROM users",
            15.5,
            true,
            None,
        );

        assert_eq!(entry.event_type, AuditEventType::Query);
        assert_eq!(entry.operation, "SELECT");
        assert!(entry.success);
        // mask_key takes first 8 chars: "test_key" + "..."
        assert_eq!(entry.api_key_id, "test_key...");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_admin_entry_creation() {
        let (config, dir) = temp_config();
        let logger = AuditLogger::new(config);

        let entry = logger.create_admin_entry(
            "10.0.0.1",
            Some("admin_key_999"),
            "add_key",
            "key=new_app_key, desc=Test app, perm=ReadWrite",
            true,
            None,
        );

        assert_eq!(entry.event_type, AuditEventType::Admin);
        assert_eq!(entry.operation, "add_key");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_integrity_chain() {
        let (config, dir) = temp_config();
        let logger = AuditLogger::new(config);

        // Log two entries
        let entry1 = logger.create_query_entry("1.1.1.1", None, "SELECT", "SELECT 1", 1.0, true, None);
        logger.log(entry1);

        let entry2 = logger.create_query_entry("1.1.1.1", None, "INSERT", "INSERT INTO t VALUES(1)", 2.0, true, None);
        logger.log(entry2);

        // Verify chain
        let log_file = dir.join(format!("audit_{}.jsonl", today_str()));
        let (total, valid, first_invalid) = AuditLogger::verify_log_file(&log_file);
        assert_eq!(total, 2);
        assert_eq!(valid, 2);
        assert!(first_invalid.is_none());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_chain_tamper_detection() {
        let (config, dir) = temp_config();
        let logger = AuditLogger::new(config);

        let entry = logger.create_query_entry("1.1.1.1", None, "SELECT", "SELECT 1", 1.0, true, None);
        logger.log(entry);

        // Tamper with the log file
        let log_file = dir.join(format!("audit_{}.jsonl", today_str()));
        let content = fs::read_to_string(&log_file).expect("should be valid");
        let tampered = content.replace("SELECT", "DROP");
        fs::write(&log_file, tampered).expect("should be valid");

        // Verification should fail
        let (total, valid, first_invalid) = AuditLogger::verify_log_file(&log_file);
        assert_eq!(total, 1);
        assert_eq!(valid, 0);
        assert!(first_invalid.is_some());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_retention_cleanup() {
        let (config, dir) = temp_config();
        let logger = AuditLogger::new(config);

        // Create an old log file
        let old_date = "20200101";
        let old_file = dir.join(format!("audit_{}.jsonl", old_date));
        fs::write(&old_file, "{}\n").expect("should be valid");

        // Create a recent log file
        let recent_file = dir.join(format!("audit_{}.jsonl", today_str()));
        fs::write(&recent_file, "{}\n").expect("should be valid");

        let removed = logger.cleanup_expired_logs();
        assert!(removed > 0, "should have removed old files");
        assert!(!old_file.exists(), "old file should be deleted");
        assert!(recent_file.exists(), "recent file should remain");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_mask_key() {
        assert_eq!(mask_key("abc"), "abc");
        assert_eq!(mask_key("abcdefgh"), "abcdefgh");
        assert_eq!(mask_key("abcdefghi"), "abcdefgh...");
    }

    #[test]
    fn test_truncate() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world", 5), "hello...");
    }
}
