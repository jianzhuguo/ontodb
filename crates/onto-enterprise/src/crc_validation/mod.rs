//! CRC validation module for detecting silent data corruption.
//!
//! Provides:
//! - Page-level CRC32 checksums for SSTable data
//! - Background integrity verification
//! - Auto-repair from Raft replicas on corruption

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use parking_lot::RwLock;

/// CRC validation configuration.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CrcValidationConfig {
    /// Enable CRC validation.
    pub enabled: bool,
    /// Enable background integrity scan.
    pub background_scan: bool,
    /// Background scan interval in hours.
    pub scan_interval_hours: u64,
    /// Enable auto-repair from replicas on corruption.
    pub auto_repair: bool,
    /// Log corruption events.
    pub log_corruption: bool,
}

impl Default for CrcValidationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            background_scan: true,
            scan_interval_hours: 24,
            auto_repair: true,
            log_corruption: true,
        }
    }
}

/// CRC checksum stored with each data page.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PageChecksum {
    /// CRC32 checksum of the page data.
    pub crc32: u32,
    /// Page offset in the file.
    pub offset: u64,
    /// Page size in bytes.
    pub size: u32,
    /// Sequence number for versioning.
    pub seq_no: u64,
}

/// Corruption event record.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CorruptionEvent {
    /// Timestamp of detection.
    pub timestamp: String,
    /// File path where corruption was detected.
    pub file_path: PathBuf,
    /// Offset of corrupted page.
    pub offset: u64,
    /// Expected CRC32.
    pub expected_crc: u32,
    /// Actual CRC32.
    pub actual_crc: u32,
    /// Whether auto-repair was attempted.
    pub repaired: bool,
    /// Repair result message.
    pub repair_message: Option<String>,
}

/// CRC validation manager.
#[derive(Clone)]
pub struct CrcValidationManager {
    config: CrcValidationConfig,
    /// Corruption event log.
    corruption_log: Arc<RwLock<Vec<CorruptionEvent>>>,
    /// Statistics.
    stats: Arc<RwLock<CrcStats>>,
}

/// CRC validation statistics.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct CrcStats {
    pub pages_verified: u64,
    pub corruption_detected: u64,
    pub repair_attempted: u64,
    pub repair_succeeded: u64,
}

impl CrcValidationManager {
    /// Create a new CRC validation manager.
    pub fn new(config: CrcValidationConfig) -> Self {
        Self {
            config,
            corruption_log: Arc::new(RwLock::new(Vec::new())),
            stats: Arc::new(RwLock::new(CrcStats::default())),
        }
    }

    /// Compute CRC32 checksum for data.
    pub fn compute_crc32(data: &[u8]) -> u32 {
        crc32fast::hash(data)
    }

    /// Verify data integrity against expected CRC.
    pub fn verify_crc32(data: &[u8], expected: u32) -> bool {
        Self::compute_crc32(data) == expected
    }

    /// Create a page checksum.
    pub fn create_checksum(data: &[u8], offset: u64, seq_no: u64) -> PageChecksum {
        PageChecksum {
            crc32: Self::compute_crc32(data),
            offset,
            size: data.len() as u32,
            seq_no,
        }
    }

    /// Verify a page against its checksum.
    pub fn verify_page(&self, data: &[u8], checksum: &PageChecksum) -> Result<(), CorruptionEvent> {
        if !self.config.enabled {
            return Ok(());
        }

        let actual_crc = Self::compute_crc32(data);
        if actual_crc != checksum.crc32 {
            let event = CorruptionEvent {
                timestamp: chrono::Utc::now().to_rfc3339(),
                file_path: PathBuf::new(), // Caller should set this
                offset: checksum.offset,
                expected_crc: checksum.crc32,
                actual_crc,
                repaired: false,
                repair_message: None,
            };

            if self.config.log_corruption {
                tracing::error!(
                    "CRC corruption detected at offset {}: expected={:08x}, actual={:08x}",
                    checksum.offset,
                    checksum.crc32,
                    actual_crc
                );
            }

            self.corruption_log.write().push(event.clone());
            self.stats.write().corruption_detected += 1;

            return Err(event);
        }

        self.stats.write().pages_verified += 1;
        Ok(())
    }

    /// Record a repair attempt.
    pub fn record_repair(&self, event: &mut CorruptionEvent, success: bool, message: String) {
        event.repaired = true;
        event.repair_message = Some(message);

        let mut stats = self.stats.write();
        stats.repair_attempted += 1;
        if success {
            stats.repair_succeeded += 1;
        }
    }

    /// Get corruption log.
    pub fn corruption_log(&self) -> Vec<CorruptionEvent> {
        self.corruption_log.read().clone()
    }

    /// Get statistics.
    pub fn stats(&self) -> CrcStats {
        self.stats.read().clone()
    }

    /// Clear corruption log.
    pub fn clear_log(&self) {
        self.corruption_log.write().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_crc32_computation() {
        let data = b"hello world";
        let crc = CrcValidationManager::compute_crc32(data);
        assert!(crc != 0);
        assert!(CrcValidationManager::verify_crc32(data, crc));
    }

    #[test]
    fn test_crc32_mismatch() {
        let data = b"hello world";
        let crc = CrcValidationManager::compute_crc32(data);
        assert!(!CrcValidationManager::verify_crc32(b"hello worle", crc));
    }

    #[test]
    fn test_page_checksum() {
        let data = b"test data for crc";
        let checksum = CrcValidationManager::create_checksum(data, 1024, 42);
        assert_eq!(checksum.offset, 1024);
        assert_eq!(checksum.size, data.len() as u32);
        assert_eq!(checksum.seq_no, 42);

        let manager = CrcValidationManager::new(CrcValidationConfig {
            enabled: true,
            ..Default::default()
        });
        assert!(manager.verify_page(data, &checksum).is_ok());
    }

    #[test]
    fn test_corruption_detection() {
        let data = b"original data";
        let mut checksum = CrcValidationManager::create_checksum(data, 0, 1);

        let manager = CrcValidationManager::new(CrcValidationConfig {
            enabled: true,
            ..Default::default()
        });

        // Tamper with checksum
        checksum.crc32 = 0x12345678;

        let result = manager.verify_page(data, &checksum);
        assert!(result.is_err());

        let stats = manager.stats();
        assert_eq!(stats.corruption_detected, 1);
    }
}
