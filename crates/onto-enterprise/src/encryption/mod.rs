//! Encryption module for OntoDB Enterprise Gov/Finance edition.
//!
//! Provides:
//! - TLS transport encryption (via rustls)
//! - AES-256-GCM storage encryption with key derivation
//! - SM4-CBC storage encryption (国密标准)
//! - Key management with master key support

use anyhow::{Context, Result};
use ring::aead::{self, Aad, LessSafeKey, Nonce, UnboundKey};
use ring::rand::{SecureRandom, SystemRandom};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use parking_lot::RwLock;

/// Supported encryption algorithms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum EncryptionAlgorithm {
    /// AES-256-GCM (default, NIST standard)
    Aes256Gcm,
    /// SM4-CBC (国密标准, GB/T 32907-2016)
    Sm4Cbc,
}

impl Default for EncryptionAlgorithm {
    fn default() -> Self {
        Self::Aes256Gcm
    }
}

impl std::fmt::Display for EncryptionAlgorithm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Aes256Gcm => write!(f, "AES-256-GCM"),
            Self::Sm4Cbc => write!(f, "SM4-CBC"),
        }
    }
}

/// Encryption configuration.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EncryptionConfig {
    /// Enable TLS for client connections.
    pub tls_enabled: bool,
    /// Path to TLS certificate file (PEM format).
    pub cert_path: Option<String>,
    /// Path to TLS private key file (PEM format).
    pub key_path: Option<String>,
    /// Path to CA certificate for mutual TLS (optional).
    pub ca_cert_path: Option<String>,
    /// Enable storage-level encryption.
    pub storage_encryption: bool,
    /// Encryption algorithm to use.
    pub algorithm: EncryptionAlgorithm,
    /// Path to master key file or KMS endpoint.
    pub master_key_source: KeySource,
    /// Key rotation interval in days (0 = disabled).
    pub key_rotation_days: u32,
}

/// Source for the master encryption key.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum KeySource {
    /// Local key file (32 bytes binary).
    File(String),
    /// Environment variable containing hex-encoded key.
    Env(String),
    /// KMS endpoint (placeholder for future implementation).
    Kms(String),
}

impl Default for EncryptionConfig {
    fn default() -> Self {
        Self {
            tls_enabled: false,
            cert_path: None,
            key_path: None,
            ca_cert_path: None,
            storage_encryption: false,
            algorithm: EncryptionAlgorithm::Aes256Gcm,
            master_key_source: KeySource::Env("ONTO_MASTER_KEY".to_string()),
            key_rotation_days: 90,
        }
    }
}

/// Encryption key with metadata.
#[derive(Debug, Clone)]
struct EncryptionKey {
    /// The raw key bytes (32 bytes for AES-256).
    key_bytes: Vec<u8>,
    /// Key ID for identification.
    key_id: String,
    /// Creation timestamp.
    created_at: std::time::SystemTime,
}

/// Encryption manager — handles TLS and storage encryption.
#[derive(Clone)]
pub struct EncryptionManager {
    config: EncryptionConfig,
    /// Master key for deriving data encryption keys.
    master_key: Arc<RwLock<Option<EncryptionKey>>>,
    /// Current data encryption key.
    current_dek: Arc<RwLock<Option<EncryptionKey>>>,
    /// Random number generator.
    rng: Arc<SystemRandom>,
}

impl EncryptionManager {
    /// Create a new encryption manager.
    pub fn new(config: EncryptionConfig) -> Self {
        Self {
            config,
            master_key: Arc::new(RwLock::new(None)),
            current_dek: Arc::new(RwLock::new(None)),
            rng: Arc::new(SystemRandom::new()),
        }
    }

    /// Initialize the encryption manager — load master key and derive DEK.
    pub fn init(&self) -> Result<()> {
        if !self.config.storage_encryption && !self.config.tls_enabled {
            return Ok(());
        }

        if self.config.storage_encryption {
            let master_key = self.load_master_key()?;
            *self.master_key.write() = Some(master_key);

            // Derive initial data encryption key
            let dek = self.derive_data_encryption_key()?;
            *self.current_dek.write() = Some(dek);

            tracing::info!("Storage encryption initialized with AES-256-GCM");
        }

        if self.config.tls_enabled {
            self.validate_tls_config()?;
            tracing::info!("TLS configuration validated");
        }

        Ok(())
    }

    /// Load master key from configured source.
    fn load_master_key(&self) -> Result<EncryptionKey> {
        let key_bytes = match &self.config.master_key_source {
            KeySource::File(path) => {
                let bytes = fs::read(path)
                    .context(format!("Failed to read master key file: {}", path))?;
                if bytes.len() != 32 {
                    anyhow::bail!(
                        "Master key file must be exactly 32 bytes, got {}",
                        bytes.len()
                    );
                }
                bytes
            }
            KeySource::Env(var_name) => {
                let hex_str = std::env::var(var_name)
                    .context(format!("Environment variable {} not set", var_name))?;
                let bytes = hex::decode(&hex_str)
                    .context("Master key must be hex-encoded")?;
                if bytes.len() != 32 {
                    anyhow::bail!(
                        "Master key must be 32 bytes (64 hex chars), got {}",
                        bytes.len()
                    );
                }
                bytes
            }
            KeySource::Kms(endpoint) => {
                // Parse KMS config from endpoint (format: "endpoint|key_name|token")
                let parts: Vec<&str> = endpoint.split('|').collect();
                let (kms_endpoint, key_name, token) = if parts.len() >= 3 {
                    (parts[0], parts[1], parts[2])
                } else if parts.len() == 2 {
                    (parts[0], parts[1], "")
                } else {
                    (endpoint.as_str(), "ontodb-master", "")
                };

                let kms_config = crate::kms::KmsConfig {
                    endpoint: kms_endpoint.to_string(),
                    key_name: key_name.to_string(),
                    token: token.to_string(),
                    timeout_secs: 30,
                    cache_ttl_secs: 300,
                };

                let kms_client = crate::kms::KmsClient::new(kms_config)
                    .context("Failed to create KMS client")?;

                let (bytes, _version) = kms_client.fetch_master_key()
                    .context("Failed to fetch master key from KMS")?;

                tracing::info!("Master key loaded from KMS: {} (version: {})", kms_endpoint, _version);
                bytes
            }
        };

        Ok(EncryptionKey {
            key_bytes,
            key_id: format!("master-{}", chrono::Utc::now().timestamp()),
            created_at: std::time::SystemTime::now(),
        })
    }

    /// Derive a data encryption key from the master key.
    fn derive_data_encryption_key(&self) -> Result<EncryptionKey> {
        let master = self.master_key.read();
        let master = master.as_ref().context("Master key not loaded")?;

        // Generate random salt for key derivation
        let mut salt = vec![0u8; 32];
        self.rng.fill(&mut salt)
            .map_err(|_| anyhow::anyhow!("Failed to generate random salt"))?;

        // Use HKDF for key derivation (simplified — in production, use proper HKDF)
        let mut dek_bytes = vec![0u8; 32];
        for i in 0..32 {
            dek_bytes[i] = master.key_bytes[i] ^ salt[i];
        }

        // Mix in current timestamp for uniqueness
        let timestamp = chrono::Utc::now().timestamp_millis().to_le_bytes();
        for i in 0..8.min(dek_bytes.len()) {
            dek_bytes[i] ^= timestamp[i];
        }

        // Generate unique key ID using timestamp + random suffix
        let mut random_suffix = [0u8; 4];
        self.rng.fill(&mut random_suffix)
            .map_err(|_| anyhow::anyhow!("Failed to generate random suffix"))?;
        let key_id = format!(
            "dek-{}-{:08x}",
            chrono::Utc::now().timestamp_millis(),
            u32::from_le_bytes(random_suffix)
        );

        Ok(EncryptionKey {
            key_bytes: dek_bytes,
            key_id,
            created_at: std::time::SystemTime::now(),
        })
    }

    /// Check if TLS is enabled.
    pub fn is_tls_enabled(&self) -> bool {
        self.config.tls_enabled
    }

    /// Check if storage encryption is enabled.
    pub fn is_storage_encryption_enabled(&self) -> bool {
        self.config.storage_encryption
    }

    /// Validate TLS configuration files exist and are readable.
    fn validate_tls_config(&self) -> Result<()> {
        if let Some(ref cert_path) = self.config.cert_path {
            if !Path::new(cert_path).exists() {
                anyhow::bail!("TLS certificate file not found: {}", cert_path);
            }
        } else {
            anyhow::bail!("TLS enabled but no certificate path configured");
        }

        if let Some(ref key_path) = self.config.key_path {
            if !Path::new(key_path).exists() {
                anyhow::bail!("TLS private key file not found: {}", key_path);
            }
        } else {
            anyhow::bail!("TLS enabled but no private key path configured");
        }

        Ok(())
    }

    /// Encrypt data for storage.
    ///
    /// For AES-256-GCM: Returns [12 bytes nonce][ciphertext][16 bytes tag]
    /// For SM4-CBC: Returns [16 bytes IV][ciphertext (padded)]
    pub fn encrypt_for_storage(&self, plaintext: &[u8]) -> Result<Vec<u8>> {
        if !self.config.storage_encryption {
            return Ok(plaintext.to_vec());
        }

        match self.config.algorithm {
            EncryptionAlgorithm::Aes256Gcm => self.encrypt_aes_gcm(plaintext),
            EncryptionAlgorithm::Sm4Cbc => self.encrypt_sm4_cbc(plaintext),
        }
    }

    /// Decrypt data from storage.
    ///
    /// For AES-256-GCM: Input format [12 bytes nonce][ciphertext_with_tag]
    /// For SM4-CBC: Input format [16 bytes IV][ciphertext (padded)]
    pub fn decrypt_from_storage(&self, encrypted: &[u8]) -> Result<Vec<u8>> {
        if !self.config.storage_encryption {
            return Ok(encrypted.to_vec());
        }

        match self.config.algorithm {
            EncryptionAlgorithm::Aes256Gcm => self.decrypt_aes_gcm(encrypted),
            EncryptionAlgorithm::Sm4Cbc => self.decrypt_sm4_cbc(encrypted),
        }
    }

    /// Encrypt using AES-256-GCM.
    fn encrypt_aes_gcm(&self, plaintext: &[u8]) -> Result<Vec<u8>> {
        let dek = self.current_dek.read();
        let dek = dek.as_ref().context("Data encryption key not initialized")?;

        // Create AES-256-GCM key
        let unbound_key = UnboundKey::new(&aead::AES_256_GCM, &dek.key_bytes)
            .map_err(|_| anyhow::anyhow!("Failed to create encryption key"))?;
        let key = LessSafeKey::new(unbound_key);

        // Generate random nonce (12 bytes)
        let mut nonce_bytes = vec![0u8; 12];
        self.rng.fill(&mut nonce_bytes)
            .map_err(|_| anyhow::anyhow!("Failed to generate nonce"))?;
        let nonce = Nonce::assume_unique_for_key(
            nonce_bytes.clone().try_into()
                .map_err(|_| anyhow::anyhow!("Invalid nonce length"))?
        );

        // Encrypt in-place
        let mut in_out = plaintext.to_vec();
        key.seal_in_place_append_tag(nonce, Aad::empty(), &mut in_out)
            .map_err(|_| anyhow::anyhow!("Encryption failed"))?;

        // Prepend nonce to ciphertext
        let mut result = nonce_bytes;
        result.extend_from_slice(&in_out);

        Ok(result)
    }

    /// Decrypt using AES-256-GCM.
    fn decrypt_aes_gcm(&self, encrypted: &[u8]) -> Result<Vec<u8>> {
        if encrypted.len() < 12 + 16 {
            anyhow::bail!("Encrypted data too short");
        }

        let dek = self.current_dek.read();
        let dek = dek.as_ref().context("Data encryption key not initialized")?;

        // Create AES-256-GCM key
        let unbound_key = UnboundKey::new(&aead::AES_256_GCM, &dek.key_bytes)
            .map_err(|_| anyhow::anyhow!("Failed to create decryption key"))?;
        let key = LessSafeKey::new(unbound_key);

        // Extract nonce
        let nonce_bytes: [u8; 12] = encrypted[..12].try_into()
            .map_err(|_| anyhow::anyhow!("Invalid nonce"))?;
        let nonce = Nonce::assume_unique_for_key(nonce_bytes);

        // Decrypt
        let mut ciphertext_with_tag = encrypted[12..].to_vec();
        let plaintext = key.open_in_place(nonce, Aad::empty(), &mut ciphertext_with_tag)
            .map_err(|_| anyhow::anyhow!("Decryption failed — data may be corrupted or key mismatch"))?;

        Ok(plaintext.to_vec())
    }

    /// Encrypt using SM4-CBC (国密标准 GB/T 32907-2016).
    ///
    /// SM4 is a 128-bit block cipher with 128-bit key.
    /// Uses PKCS7 padding and random IV.
    fn encrypt_sm4_cbc(&self, plaintext: &[u8]) -> Result<Vec<u8>> {
        use gm_sm4::{Sm4CipherMode, CipherMode};

        let dek = self.current_dek.read();
        let dek = dek.as_ref().context("Data encryption key not initialized")?;

        // SM4 uses 128-bit (16 bytes) key
        if dek.key_bytes.len() < 16 {
            anyhow::bail!("SM4 requires at least 16 bytes key");
        }
        let key = &dek.key_bytes[..16];

        // Generate random IV (16 bytes for SM4 block size)
        let mut iv = [0u8; 16];
        self.rng.fill(&mut iv)
            .map_err(|_| anyhow::anyhow!("Failed to generate IV"))?;

        // Create SM4 cipher in CBC mode
        let cipher = Sm4CipherMode::new(key, CipherMode::Cbc)
            .map_err(|e| anyhow::anyhow!("Failed to create SM4 cipher: {}", e))?;

        // Apply PKCS7 padding
        let block_size = 16;
        let padding_len = block_size - (plaintext.len() % block_size);
        let mut padded = plaintext.to_vec();
        padded.extend(vec![padding_len as u8; padding_len]);

        // Encrypt
        let ciphertext = cipher.encrypt(&padded, &iv)
            .map_err(|e| anyhow::anyhow!("SM4 encryption failed: {}", e))?;

        // Prepend IV to ciphertext
        let mut result = Vec::with_capacity(16 + ciphertext.len());
        result.extend_from_slice(&iv);
        result.extend_from_slice(&ciphertext);

        Ok(result)
    }

    /// Decrypt using SM4-CBC.
    fn decrypt_sm4_cbc(&self, encrypted: &[u8]) -> Result<Vec<u8>> {
        use gm_sm4::{Sm4CipherMode, CipherMode};

        if encrypted.len() < 16 {
            anyhow::bail!("SM4 encrypted data too short (need at least IV)");
        }
        if encrypted.len() % 16 != 0 {
            anyhow::bail!("SM4 encrypted data length must be multiple of 16");
        }

        let dek = self.current_dek.read();
        let dek = dek.as_ref().context("Data encryption key not initialized")?;

        // SM4 uses 128-bit (16 bytes) key
        if dek.key_bytes.len() < 16 {
            anyhow::bail!("SM4 requires at least 16 bytes key");
        }
        let key = &dek.key_bytes[..16];

        // Extract IV
        let iv = &encrypted[..16];
        let ciphertext = &encrypted[16..];

        // Create SM4 cipher in CBC mode
        let cipher = Sm4CipherMode::new(key, CipherMode::Cbc)
            .map_err(|e| anyhow::anyhow!("Failed to create SM4 cipher: {}", e))?;

        // Decrypt
        let padded = cipher.decrypt(ciphertext, iv)
            .map_err(|e| anyhow::anyhow!("SM4 decryption failed: {}", e))?;

        // Remove PKCS7 padding
        if padded.is_empty() {
            anyhow::bail!("Decrypted data is empty");
        }
        // Safe: checked padded.is_empty() above
        let padding_len = *padded.last().unwrap() as usize;
        if padding_len == 0 || padding_len > 16 || padding_len > padded.len() {
            anyhow::bail!("Invalid PKCS7 padding");
        }
        // Verify padding bytes
        for i in padded.len() - padding_len..padded.len() {
            if padded[i] != padding_len as u8 {
                anyhow::bail!("Invalid PKCS7 padding");
            }
        }

        let plaintext = padded[..padded.len() - padding_len].to_vec();
        Ok(plaintext)
    }

    /// Get TLS configuration for rustls.
    /// Returns (cert_path, key_path) if TLS is enabled.
    pub fn tls_config(&self) -> Option<TlsConfig> {
        if !self.config.tls_enabled {
            return None;
        }

        Some(TlsConfig {
            cert_path: self.config.cert_path.clone()?,
            key_path: self.config.key_path.clone()?,
            ca_cert_path: self.config.ca_cert_path.clone(),
        })
    }

    /// Generate a new master key file at the specified path.
    /// Returns the hex-encoded key for storage in environment variables.
    pub fn generate_master_key(path: &Path) -> Result<String> {
        let rng = SystemRandom::new();
        let mut key_bytes = vec![0u8; 32];
        rng.fill(&mut key_bytes)
            .map_err(|_| anyhow::anyhow!("Failed to generate random key"))?;

        // Write binary key to file
        fs::write(path, &key_bytes)
            .context("Failed to write master key file")?;

        // Set restrictive permissions on Unix
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))
                .context("Failed to set key file permissions")?;
        }

        // Return hex-encoded key
        Ok(hex::encode(&key_bytes))
    }

    /// Rotate the data encryption key.
    /// Call this periodically based on key_rotation_days config.
    pub fn rotate_data_key(&self) -> Result<()> {
        if !self.config.storage_encryption {
            return Ok(());
        }

        let new_dek = self.derive_data_encryption_key()?;
        let old_key_id = self.current_dek.read().as_ref().map(|k| k.key_id.clone());

        *self.current_dek.write() = Some(new_dek.clone());

        tracing::info!(
            "Data encryption key rotated: {:?} -> {}",
            old_key_id,
            new_dek.key_id
        );

        Ok(())
    }

    /// Get current encryption status.
    pub fn status(&self) -> EncryptionStatus {
        EncryptionStatus {
            tls_enabled: self.config.tls_enabled,
            storage_encryption: self.config.storage_encryption,
            algorithm: self.config.algorithm,
            master_key_loaded: self.master_key.read().is_some(),
            current_key_id: self.current_dek.read().as_ref().map(|k| k.key_id.clone()),
            key_rotation_days: self.config.key_rotation_days,
        }
    }
}

/// TLS configuration.
#[derive(Debug, Clone)]
pub struct TlsConfig {
    pub cert_path: String,
    pub key_path: String,
    pub ca_cert_path: Option<String>,
}

/// Encryption status for monitoring.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EncryptionStatus {
    pub tls_enabled: bool,
    pub storage_encryption: bool,
    pub algorithm: EncryptionAlgorithm,
    pub master_key_loaded: bool,
    pub current_key_id: Option<String>,
    pub key_rotation_days: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[test]
    fn test_encryption_disabled_by_default() {
        let config = EncryptionConfig::default();
        let manager = EncryptionManager::new(config);
        assert!(!manager.is_tls_enabled());
        assert!(!manager.is_storage_encryption_enabled());
    }

    #[test]
    fn test_encrypt_passthrough_when_disabled() {
        let config = EncryptionConfig::default();
        let manager = EncryptionManager::new(config);
        let data = b"hello world";
        let encrypted = manager.encrypt_for_storage(data).unwrap();
        assert_eq!(encrypted, data);
    }

    #[test]
    fn test_decrypt_passthrough_when_disabled() {
        let config = EncryptionConfig::default();
        let manager = EncryptionManager::new(config);
        let data = b"hello world";
        let decrypted = manager.decrypt_from_storage(data).unwrap();
        assert_eq!(decrypted, data);
    }

    #[test]
    fn test_generate_master_key() {
        let tmpfile = NamedTempFile::new().unwrap();
        let path = tmpfile.path();

        let hex_key = EncryptionManager::generate_master_key(path).unwrap();
        assert_eq!(hex_key.len(), 64); // 32 bytes = 64 hex chars

        // Verify file was written
        let file_bytes = std::fs::read(path).unwrap();
        assert_eq!(file_bytes.len(), 32);
    }

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        // Create a temporary key file
        let tmpfile = NamedTempFile::new().unwrap();
        let key_path = tmpfile.path();
        EncryptionManager::generate_master_key(key_path).unwrap();

        let config = EncryptionConfig {
            storage_encryption: true,
            master_key_source: KeySource::File(key_path.to_string_lossy().to_string()),
            ..Default::default()
        };

        let manager = EncryptionManager::new(config);
        manager.init().unwrap();

        let plaintext = b"Hello, OntoDB encryption!";
        let encrypted = manager.encrypt_for_storage(plaintext).unwrap();

        // Encrypted should be different from plaintext
        assert_ne!(encrypted, plaintext);

        // Encrypted should be longer (nonce + tag overhead)
        assert!(encrypted.len() > plaintext.len());

        // Decrypt should recover original
        let decrypted = manager.decrypt_from_storage(&encrypted).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn test_encrypt_different_nonces() {
        let tmpfile = NamedTempFile::new().unwrap();
        let key_path = tmpfile.path();
        EncryptionManager::generate_master_key(key_path).unwrap();

        let config = EncryptionConfig {
            storage_encryption: true,
            master_key_source: KeySource::File(key_path.to_string_lossy().to_string()),
            ..Default::default()
        };

        let manager = EncryptionManager::new(config);
        manager.init().unwrap();

        let plaintext = b"same data";
        let enc1 = manager.encrypt_for_storage(plaintext).unwrap();
        let enc2 = manager.encrypt_for_storage(plaintext).unwrap();

        // Same plaintext should produce different ciphertext (due to random nonces)
        assert_ne!(enc1, enc2);

        // But both should decrypt to the same plaintext
        assert_eq!(manager.decrypt_from_storage(&enc1).unwrap(), plaintext);
        assert_eq!(manager.decrypt_from_storage(&enc2).unwrap(), plaintext);
    }

    #[test]
    fn test_key_rotation() {
        let tmpfile = NamedTempFile::new().unwrap();
        let key_path = tmpfile.path();
        EncryptionManager::generate_master_key(key_path).unwrap();

        let config = EncryptionConfig {
            storage_encryption: true,
            master_key_source: KeySource::File(key_path.to_string_lossy().to_string()),
            ..Default::default()
        };

        let manager = EncryptionManager::new(config);
        manager.init().unwrap();

        let old_key_id = manager.status().current_key_id.clone();

        // Rotate key
        manager.rotate_data_key().unwrap();

        let new_key_id = manager.status().current_key_id.clone();
        assert_ne!(old_key_id, new_key_id);
    }

    #[test]
    fn test_sm4_encrypt_decrypt_roundtrip() {
        let tmpfile = NamedTempFile::new().unwrap();
        let key_path = tmpfile.path();
        EncryptionManager::generate_master_key(key_path).unwrap();

        let config = EncryptionConfig {
            storage_encryption: true,
            algorithm: EncryptionAlgorithm::Sm4Cbc,
            master_key_source: KeySource::File(key_path.to_string_lossy().to_string()),
            ..Default::default()
        };

        let manager = EncryptionManager::new(config);
        manager.init().unwrap();

        let plaintext = b"Hello, OntoDB SM4 encryption!";
        let encrypted = manager.encrypt_for_storage(plaintext).unwrap();

        // Encrypted should be different from plaintext
        assert_ne!(encrypted, plaintext);

        // Encrypted should be longer (IV + padding)
        assert!(encrypted.len() > plaintext.len());

        // Decrypt should recover original
        let decrypted = manager.decrypt_from_storage(&encrypted).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn test_sm4_encrypt_different_ivs() {
        let tmpfile = NamedTempFile::new().unwrap();
        let key_path = tmpfile.path();
        EncryptionManager::generate_master_key(key_path).unwrap();

        let config = EncryptionConfig {
            storage_encryption: true,
            algorithm: EncryptionAlgorithm::Sm4Cbc,
            master_key_source: KeySource::File(key_path.to_string_lossy().to_string()),
            ..Default::default()
        };

        let manager = EncryptionManager::new(config);
        manager.init().unwrap();

        let plaintext = b"same data for SM4";
        let enc1 = manager.encrypt_for_storage(plaintext).unwrap();
        let enc2 = manager.encrypt_for_storage(plaintext).unwrap();

        // Same plaintext should produce different ciphertext (due to random IVs)
        assert_ne!(enc1, enc2);

        // But both should decrypt to the same plaintext
        assert_eq!(manager.decrypt_from_storage(&enc1).unwrap(), plaintext);
        assert_eq!(manager.decrypt_from_storage(&enc2).unwrap(), plaintext);
    }

    #[test]
    fn test_sm4_algorithm_display() {
        assert_eq!(format!("{}", EncryptionAlgorithm::Aes256Gcm), "AES-256-GCM");
        assert_eq!(format!("{}", EncryptionAlgorithm::Sm4Cbc), "SM4-CBC");
    }

    #[test]
    fn test_encryption_status_includes_algorithm() {
        let tmpfile = NamedTempFile::new().unwrap();
        let key_path = tmpfile.path();
        EncryptionManager::generate_master_key(key_path).unwrap();

        // Test with AES
        let config_aes = EncryptionConfig {
            storage_encryption: true,
            algorithm: EncryptionAlgorithm::Aes256Gcm,
            master_key_source: KeySource::File(key_path.to_string_lossy().to_string()),
            ..Default::default()
        };
        let manager_aes = EncryptionManager::new(config_aes);
        manager_aes.init().unwrap();
        assert_eq!(manager_aes.status().algorithm, EncryptionAlgorithm::Aes256Gcm);

        // Test with SM4
        let config_sm4 = EncryptionConfig {
            storage_encryption: true,
            algorithm: EncryptionAlgorithm::Sm4Cbc,
            master_key_source: KeySource::File(key_path.to_string_lossy().to_string()),
            ..Default::default()
        };
        let manager_sm4 = EncryptionManager::new(config_sm4);
        manager_sm4.init().unwrap();
        assert_eq!(manager_sm4.status().algorithm, EncryptionAlgorithm::Sm4Cbc);
    }
}
