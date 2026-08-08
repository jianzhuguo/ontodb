//! Backup and recovery module for OntoDB Enterprise.
//!
//! Provides:
//! - Full backup: Complete snapshot of all data
//! - Incremental backup: Only changes since last backup
//! - Backup metadata and catalog
//! - Point-in-time recovery (PITR) support
//! - Backup integrity verification
//!
//! Backup strategy:
//! 1. Full backup periodically (e.g., weekly)
//! 2. Incremental backups between full backups (e.g., hourly)
//! 3. WAL archiving for PITR

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use parking_lot::RwLock;

/// Backup configuration.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BackupConfig {
    /// Enable backup functionality.
    pub enabled: bool,
    /// Directory for storing backups.
    pub backup_dir: PathBuf,
    /// Enable incremental backups.
    pub incremental_enabled: bool,
    /// Full backup interval in hours (0 = manual only).
    pub full_backup_interval_hours: u64,
    /// Incremental backup interval in hours.
    pub incremental_interval_hours: u64,
    /// Maximum number of full backups to retain.
    pub max_full_backups: usize,
    /// Maximum number of incremental backups per full backup.
    pub max_incremental_backups: usize,
    /// Enable backup compression.
    pub compress: bool,
    /// Enable backup encryption.
    pub encrypt: bool,
    /// Enable PITR (Point-in-Time Recovery).
    pub pitr_enabled: bool,
    /// WAL archive directory for PITR.
    pub wal_archive_dir: Option<PathBuf>,
}

impl Default for BackupConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            backup_dir: PathBuf::from("./backups"),
            incremental_enabled: true,
            full_backup_interval_hours: 168, // Weekly
            incremental_interval_hours: 1,   // Hourly
            max_full_backups: 4,
            max_incremental_backups: 168,    // 7 days * 24 hours
            compress: true,
            encrypt: false,
            pitr_enabled: false,
            wal_archive_dir: None,
        }
    }
}

/// Backup type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BackupType {
    /// Full backup - complete snapshot.
    Full,
    /// Incremental backup - changes since last backup.
    Incremental,
}

impl std::fmt::Display for BackupType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Full => write!(f, "Full"),
            Self::Incremental => write!(f, "Incremental"),
        }
    }
}

/// Backup status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BackupStatus {
    /// Backup in progress.
    InProgress,
    /// Backup completed successfully.
    Completed,
    /// Backup failed.
    Failed,
    /// Backup corrupted.
    Corrupted,
}

impl std::fmt::Display for BackupStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InProgress => write!(f, "InProgress"),
            Self::Completed => write!(f, "Completed"),
            Self::Failed => write!(f, "Failed"),
            Self::Corrupted => write!(f, "Corrupted"),
        }
    }
}

/// Backup metadata.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BackupMetadata {
    /// Unique backup ID.
    pub backup_id: String,
    /// Backup type (full or incremental).
    pub backup_type: BackupType,
    /// Backup status.
    pub status: BackupStatus,
    /// When the backup started.
    pub started_at: String,
    /// When the backup completed.
    pub completed_at: Option<String>,
    /// Parent backup ID (for incremental backups).
    pub parent_backup_id: Option<String>,
    /// Sequence number in backup chain.
    pub sequence_number: u32,
    /// Size in bytes.
    pub size_bytes: u64,
    /// Number of entries backed up.
    pub entry_count: u64,
    /// CRC32 checksum of backup data.
    pub checksum: u32,
    /// Backup file path.
    pub file_path: PathBuf,
    /// Additional metadata.
    pub metadata: HashMap<String, String>,
}

/// Backup entry (a single key-value pair in the backup).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BackupEntry {
    /// Key.
    pub key: Vec<u8>,
    /// Value (None for deletions).
    pub value: Option<Vec<u8>>,
    /// Sequence number.
    pub seq_no: u64,
    /// Timestamp.
    pub timestamp: u64,
}

/// Backup catalog - lists all backups.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BackupCatalog {
    /// All backups indexed by ID.
    pub backups: HashMap<String, BackupMetadata>,
    /// Latest full backup ID.
    pub latest_full: Option<String>,
    /// Latest backup ID.
    pub latest: Option<String>,
}

impl BackupCatalog {
    fn new() -> Self {
        Self {
            backups: HashMap::new(),
            latest_full: None,
            latest: None,
        }
    }

    /// Get backup chain from a full backup to the latest incremental.
    pub fn get_backup_chain(&self, from_full: &str) -> Vec<BackupMetadata> {
        let mut chain = Vec::new();
        let mut current = Some(from_full.to_string());

        while let Some(id) = current {
            if let Some(meta) = self.backups.get(&id) {
                chain.push(meta.clone());
                current = meta.parent_backup_id.clone();
            } else {
                break;
            }
        }

        chain.reverse(); // Full backup first
        chain
    }

    /// Get all backup chains.
    pub fn get_all_chains(&self) -> Vec<Vec<BackupMetadata>> {
        let mut chains = Vec::new();
        let full_backups: Vec<_> = self.backups.values()
            .filter(|b| b.backup_type == BackupType::Full)
            .collect();

        for full in full_backups {
            let chain = self.get_backup_chain(&full.backup_id);
            if !chain.is_empty() {
                chains.push(chain);
            }
        }

        chains
    }
}

/// Backup operation result.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BackupResult {
    /// Backup ID.
    pub backup_id: String,
    /// Backup type.
    pub backup_type: BackupType,
    /// Status.
    pub status: BackupStatus,
    /// Size in bytes.
    pub size_bytes: u64,
    /// Duration in milliseconds.
    pub duration_ms: u64,
    /// Error message if failed.
    pub error: Option<String>,
}

/// Backup manager.
#[derive(Clone)]
pub struct BackupManager {
    config: BackupConfig,
    /// Backup catalog.
    catalog: Arc<RwLock<BackupCatalog>>,
    /// Last full backup time.
    last_full_backup: Arc<RwLock<Option<SystemTime>>>,
    /// Last incremental backup time.
    last_incremental_backup: Arc<RwLock<Option<SystemTime>>>,
}

impl BackupManager {
    /// Create a new backup manager.
    pub fn new(config: BackupConfig) -> Self {
        Self {
            config,
            catalog: Arc::new(RwLock::new(BackupCatalog::new())),
            last_full_backup: Arc::new(RwLock::new(None)),
            last_incremental_backup: Arc::new(RwLock::new(None)),
        }
    }

    /// Initialize backup directory and load catalog.
    pub fn init(&self) -> io::Result<()> {
        if !self.config.enabled {
            return Ok(());
        }

        // Create backup directory
        fs::create_dir_all(&self.config.backup_dir)?;

        // Load existing catalog if available
        let catalog_path = self.config.backup_dir.join("catalog.json");
        if catalog_path.exists() {
            let data = fs::read_to_string(&catalog_path)?;
            if let Ok(catalog) = serde_json::from_str::<BackupCatalog>(&data) {
                *self.catalog.write() = catalog;
                tracing::info!("Loaded backup catalog");
            }
        }

        tracing::info!(
            "Backup manager initialized: dir={}, incremental={}",
            self.config.backup_dir.display(),
            self.config.incremental_enabled
        );

        Ok(())
    }

    /// Generate a unique backup ID.
    fn generate_backup_id() -> String {
        let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
        let random: u32 = rand::random();
        format!("backup_{}_{:08x}", timestamp, random)
    }

    /// Create a full backup.
    ///
    /// This should be called with a snapshot of all current data.
    pub fn create_full_backup(
        &self,
        entries: Vec<BackupEntry>,
    ) -> io::Result<BackupResult> {
        if !self.config.enabled {
            return Err(io::Error::new(io::ErrorKind::Other, "Backup not enabled"));
        }

        let start_time = SystemTime::now();
        let backup_id = Self::generate_backup_id();
        let backup_path = self.config.backup_dir.join(format!("{}.backup", backup_id));

        // Create metadata
        let mut metadata = BackupMetadata {
            backup_id: backup_id.clone(),
            backup_type: BackupType::Full,
            status: BackupStatus::InProgress,
            started_at: chrono::Utc::now().to_rfc3339(),
            completed_at: None,
            parent_backup_id: None,
            sequence_number: 0,
            size_bytes: 0,
            entry_count: entries.len() as u64,
            checksum: 0,
            file_path: backup_path.clone(),
            metadata: HashMap::new(),
        };

        // Write backup data
        let (size, checksum) = self.write_backup_data(&backup_path, &entries, self.config.compress)?;

        metadata.size_bytes = size;
        metadata.checksum = checksum;
        metadata.status = BackupStatus::Completed;
        metadata.completed_at = Some(chrono::Utc::now().to_rfc3339());

        // Update catalog
        {
            let mut catalog = self.catalog.write();
            catalog.backups.insert(backup_id.clone(), metadata.clone());
            catalog.latest_full = Some(backup_id.clone());
            catalog.latest = Some(backup_id.clone());
            self.save_catalog(&catalog)?;
        }

        *self.last_full_backup.write() = Some(start_time);

        let duration = start_time.elapsed().unwrap_or(Duration::ZERO);

        tracing::info!(
            "Full backup completed: id={}, size={}MB, entries={}, duration={}ms",
            backup_id,
            size / 1024 / 1024,
            entries.len(),
            duration.as_millis()
        );

        Ok(BackupResult {
            backup_id,
            backup_type: BackupType::Full,
            status: BackupStatus::Completed,
            size_bytes: size,
            duration_ms: duration.as_millis() as u64,
            error: None,
        })
    }

    /// Create an incremental backup.
    ///
    /// This should be called with only the changes since the last backup.
    pub fn create_incremental_backup(
        &self,
        entries: Vec<BackupEntry>,
    ) -> io::Result<BackupResult> {
        if !self.config.enabled || !self.config.incremental_enabled {
            return Err(io::Error::new(io::ErrorKind::Other, "Incremental backup not enabled"));
        }

        let start_time = SystemTime::now();
        let backup_id = Self::generate_backup_id();
        let backup_path = self.config.backup_dir.join(format!("{}.incbackup", backup_id));

        // Get parent backup ID
        let parent_id = self.catalog.read().latest.clone()
            .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "No full backup exists"))?;

        // Get sequence number
        let sequence = self.catalog.read().backups.get(&parent_id)
            .map(|m| m.sequence_number + 1)
            .unwrap_or(1);

        // Create metadata
        let mut metadata = BackupMetadata {
            backup_id: backup_id.clone(),
            backup_type: BackupType::Incremental,
            status: BackupStatus::InProgress,
            started_at: chrono::Utc::now().to_rfc3339(),
            completed_at: None,
            parent_backup_id: Some(parent_id),
            sequence_number: sequence,
            size_bytes: 0,
            entry_count: entries.len() as u64,
            checksum: 0,
            file_path: backup_path.clone(),
            metadata: HashMap::new(),
        };

        // Write backup data
        let (size, checksum) = self.write_backup_data(&backup_path, &entries, self.config.compress)?;

        metadata.size_bytes = size;
        metadata.checksum = checksum;
        metadata.status = BackupStatus::Completed;
        metadata.completed_at = Some(chrono::Utc::now().to_rfc3339());

        // Update catalog
        {
            let mut catalog = self.catalog.write();
            catalog.backups.insert(backup_id.clone(), metadata.clone());
            catalog.latest = Some(backup_id.clone());
            self.save_catalog(&catalog)?;
        }

        *self.last_incremental_backup.write() = Some(start_time);

        let duration = start_time.elapsed().unwrap_or(Duration::ZERO);

        tracing::info!(
            "Incremental backup completed: id={}, parent={}, size={}KB, entries={}, duration={}ms",
            backup_id,
            metadata.parent_backup_id.as_deref().unwrap_or("none"),
            size / 1024,
            entries.len(),
            duration.as_millis()
        );

        Ok(BackupResult {
            backup_id,
            backup_type: BackupType::Incremental,
            status: BackupStatus::Completed,
            size_bytes: size,
            duration_ms: duration.as_millis() as u64,
            error: None,
        })
    }

    /// Write backup data to file.
    fn write_backup_data(
        &self,
        path: &Path,
        entries: &[BackupEntry],
        compress: bool,
    ) -> io::Result<(u64, u32)> {
        let file = File::create(path)?;
        let mut writer = BufWriter::new(file);

        // Write header
        let header = serde_json::to_vec(&serde_json::json!({
            "version": 1,
            "entries": entries.len(),
            "compressed": compress,
        }))?;
        writer.write_all(&(header.len() as u32).to_le_bytes())?;
        writer.write_all(&header)?;

        // Write entries
        let mut total_size = 0u64;
        let mut hasher = crc32fast::Hasher::new();

        for entry in entries {
            let data = serde_json::to_vec(entry)?;
            let len = data.len() as u32;
            writer.write_all(&len.to_le_bytes())?;
            writer.write_all(&data)?;
            total_size += 4 + len as u64;
            hasher.update(&data);
        }

        writer.flush()?;

        Ok((total_size, hasher.finalize()))
    }

    /// Read backup entries from file.
    pub fn read_backup(&self, backup_id: &str) -> io::Result<Vec<BackupEntry>> {
        let catalog = self.catalog.read();
        let metadata = catalog.backups.get(backup_id)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "Backup not found"))?;

        let file = File::open(&metadata.file_path)?;
        let mut reader = BufReader::new(file);

        // Read header
        let mut header_len = [0u8; 4];
        reader.read_exact(&mut header_len)?;
        let header_len = u32::from_le_bytes(header_len) as usize;
        let mut header = vec![0u8; header_len];
        reader.read_exact(&mut header)?;

        // Read entries
        let mut entries = Vec::new();
        loop {
            let mut len_buf = [0u8; 4];
            match reader.read_exact(&mut len_buf) {
                Ok(()) => {
                    let len = u32::from_le_bytes(len_buf) as usize;
                    let mut data = vec![0u8; len];
                    reader.read_exact(&mut data)?;
                    let entry: BackupEntry = serde_json::from_slice(&data)
                        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                    entries.push(entry);
                }
                Err(ref e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e),
            }
        }

        Ok(entries)
    }

    /// Restore from a full backup and optional incremental backups.
    pub fn restore(
        &self,
        full_backup_id: &str,
        incremental_ids: Option<Vec<&str>>,
    ) -> io::Result<Vec<BackupEntry>> {
        // Read full backup
        let mut all_entries = self.read_backup(full_backup_id)?;

        // Apply incremental backups in order
        if let Some(inc_ids) = incremental_ids {
            for inc_id in inc_ids {
                let inc_entries = self.read_backup(inc_id)?;
                all_entries.extend(inc_entries);
            }
        }

        // Deduplicate by key (keep latest)
        let mut key_map: HashMap<Vec<u8>, BackupEntry> = HashMap::new();
        for entry in all_entries {
            key_map.insert(entry.key.clone(), entry);
        }

        let entries: Vec<BackupEntry> = key_map.into_values().collect();

        tracing::info!("Restored {} entries from backup", entries.len());

        Ok(entries)
    }

    /// Restore to a specific point in time.
    pub fn restore_to_point_in_time(
        &self,
        target_time: &str,
    ) -> io::Result<Vec<BackupEntry>> {
        let target = chrono::DateTime::parse_from_rfc3339(target_time)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;

        let catalog = self.catalog.read();
        let chains = catalog.get_all_chains();

        // Find the best chain for PITR
        let mut best_chain = None;
        for chain in chains {
            if let Some(last) = chain.last() {
                if let Ok(backup_time) = chrono::DateTime::parse_from_rfc3339(&last.completed_at.as_deref().unwrap_or("")) {
                    if backup_time <= target {
                        best_chain = Some(chain);
                    }
                }
            }
        }

        let chain = best_chain.ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "No backup chain available for PITR")
        })?;

        // Restore from the chain
        let full_id = chain.first().unwrap().backup_id.clone();
        let inc_ids: Vec<&str> = chain[1..].iter().map(|m| m.backup_id.as_str()).collect();

        self.restore(&full_id, Some(inc_ids))
    }

    /// Verify backup integrity.
    pub fn verify_backup(&self, backup_id: &str) -> io::Result<BackupVerificationResult> {
        let catalog = self.catalog.read();
        let metadata = catalog.backups.get(backup_id)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "Backup not found"))?;

        // Read and verify checksum
        let entries = self.read_backup(backup_id)?;

        let mut hasher = crc32fast::Hasher::new();
        for entry in &entries {
            let data = serde_json::to_vec(entry)?;
            hasher.update(&data);
        }
        let computed_checksum = hasher.finalize();

        let is_valid = computed_checksum == metadata.checksum;

        Ok(BackupVerificationResult {
            backup_id: backup_id.to_string(),
            is_valid,
            expected_checksum: metadata.checksum,
            actual_checksum: computed_checksum,
            entry_count: entries.len() as u64,
            expected_entry_count: metadata.entry_count,
        })
    }

    /// Save catalog to disk.
    fn save_catalog(&self, catalog: &BackupCatalog) -> io::Result<()> {
        let catalog_path = self.config.backup_dir.join("catalog.json");
        let json = serde_json::to_string_pretty(catalog)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        fs::write(catalog_path, json)?;
        Ok(())
    }

    /// List all backups.
    pub fn list_backups(&self) -> Vec<BackupMetadata> {
        self.catalog.read().backups.values().cloned().collect()
    }

    /// Get backup metadata.
    pub fn get_backup(&self, backup_id: &str) -> Option<BackupMetadata> {
        self.catalog.read().backups.get(backup_id).cloned()
    }

    /// Delete a backup.
    pub fn delete_backup(&self, backup_id: &str) -> io::Result<()> {
        let mut catalog = self.catalog.write();
        if let Some(metadata) = catalog.backups.remove(backup_id) {
            // Delete backup file
            if metadata.file_path.exists() {
                fs::remove_file(&metadata.file_path)?;
            }

            // Update catalog references
            if catalog.latest_full.as_deref() == Some(backup_id) {
                catalog.latest_full = catalog.backups.values()
                    .filter(|b| b.backup_type == BackupType::Full)
                    .max_by_key(|b| b.started_at.clone())
                    .map(|b| b.backup_id.clone());
            }

            if catalog.latest.as_deref() == Some(backup_id) {
                catalog.latest = catalog.backups.values()
                    .max_by_key(|b| b.started_at.clone())
                    .map(|b| b.backup_id.clone());
            }

            self.save_catalog(&catalog)?;

            tracing::info!("Deleted backup: {}", backup_id);
        }

        Ok(())
    }

    /// Get backup status.
    pub fn status(&self) -> BackupStatusInfo {
        let catalog = self.catalog.read();
        BackupStatusInfo {
            enabled: self.config.enabled,
            total_backups: catalog.backups.len(),
            latest_full: catalog.latest_full.clone(),
            latest: catalog.latest.clone(),
            last_full_backup: *self.last_full_backup.read(),
            last_incremental_backup: *self.last_incremental_backup.read(),
        }
    }
}

/// Backup verification result.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BackupVerificationResult {
    pub backup_id: String,
    pub is_valid: bool,
    pub expected_checksum: u32,
    pub actual_checksum: u32,
    pub entry_count: u64,
    pub expected_entry_count: u64,
}

/// Backup status information.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BackupStatusInfo {
    pub enabled: bool,
    pub total_backups: usize,
    pub latest_full: Option<String>,
    pub latest: Option<String>,
    pub last_full_backup: Option<SystemTime>,
    pub last_incremental_backup: Option<SystemTime>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_config(dir: &Path) -> BackupConfig {
        BackupConfig {
            enabled: true,
            backup_dir: dir.to_path_buf(),
            incremental_enabled: true,
            full_backup_interval_hours: 168,
            incremental_interval_hours: 1,
            max_full_backups: 4,
            max_incremental_backups: 168,
            compress: false,
            encrypt: false,
            pitr_enabled: false,
            wal_archive_dir: None,
        }
    }

    fn create_test_entries(count: usize) -> Vec<BackupEntry> {
        (0..count).map(|i| BackupEntry {
            key: format!("key_{}", i).into_bytes(),
            value: Some(format!("value_{}", i).into_bytes()),
            seq_no: i as u64,
            timestamp: 1000000 + i as u64,
        }).collect()
    }

    #[test]
    fn test_full_backup() {
        let tmp_dir = TempDir::new().unwrap();
        let config = test_config(tmp_dir.path());
        let manager = BackupManager::new(config);
        manager.init().unwrap();

        let entries = create_test_entries(100);
        let result = manager.create_full_backup(entries).unwrap();

        assert_eq!(result.status, BackupStatus::Completed);
        assert_eq!(result.backup_type, BackupType::Full);
        assert!(result.size_bytes > 0);
    }

    #[test]
    fn test_incremental_backup() {
        let tmp_dir = TempDir::new().unwrap();
        let config = test_config(tmp_dir.path());
        let manager = BackupManager::new(config);
        manager.init().unwrap();

        // Create full backup first
        let entries = create_test_entries(100);
        manager.create_full_backup(entries).unwrap();

        // Create incremental backup
        let inc_entries = create_test_entries(10);
        let result = manager.create_incremental_backup(inc_entries).unwrap();

        assert_eq!(result.status, BackupStatus::Completed);
        assert_eq!(result.backup_type, BackupType::Incremental);
    }

    #[test]
    fn test_backup_restore() {
        let tmp_dir = TempDir::new().unwrap();
        let config = test_config(tmp_dir.path());
        let manager = BackupManager::new(config);
        manager.init().unwrap();

        // Create backup
        let entries = create_test_entries(50);
        let result = manager.create_full_backup(entries).unwrap();

        // Restore
        let restored = manager.restore(&result.backup_id, None).unwrap();
        assert_eq!(restored.len(), 50);
    }

    #[test]
    fn test_backup_verification() {
        let tmp_dir = TempDir::new().unwrap();
        let config = test_config(tmp_dir.path());
        let manager = BackupManager::new(config);
        manager.init().unwrap();

        let entries = create_test_entries(50);
        let result = manager.create_full_backup(entries).unwrap();

        let verification = manager.verify_backup(&result.backup_id).unwrap();
        assert!(verification.is_valid);
    }

    #[test]
    fn test_backup_catalog() {
        let tmp_dir = TempDir::new().unwrap();
        let config = test_config(tmp_dir.path());
        let manager = BackupManager::new(config);
        manager.init().unwrap();

        // Create multiple backups
        manager.create_full_backup(create_test_entries(10)).unwrap();
        manager.create_incremental_backup(create_test_entries(5)).unwrap();
        manager.create_incremental_backup(create_test_entries(3)).unwrap();

        let backups = manager.list_backups();
        assert_eq!(backups.len(), 3);

        let status = manager.status();
        assert!(status.latest_full.is_some());
        assert!(status.latest.is_some());
    }

    #[test]
    fn test_delete_backup() {
        let tmp_dir = TempDir::new().unwrap();
        let config = test_config(tmp_dir.path());
        let manager = BackupManager::new(config);
        manager.init().unwrap();

        let result = manager.create_full_backup(create_test_entries(10)).unwrap();
        assert_eq!(manager.list_backups().len(), 1);

        manager.delete_backup(&result.backup_id).unwrap();
        assert_eq!(manager.list_backups().len(), 0);
    }
}
