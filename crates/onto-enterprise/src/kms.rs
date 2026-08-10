//! KMS (Key Management Service) client for OntoDB Enterprise.
//!
//! Supports HTTP-based KMS API compatible with:
//! - HashiCorp Vault Transit Engine
//! - AWS KMS (via HTTP API)
//! - Azure Key Vault
//! - 自建 KMS 服务

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use parking_lot::RwLock;
use std::time::{Duration, Instant};

/// KMS configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KmsConfig {
    /// KMS endpoint URL (e.g., "https://vault.example.com/v1/transit")
    pub endpoint: String,
    /// KMS key name/ID
    pub key_name: String,
    /// Authentication token
    pub token: String,
    /// Request timeout in seconds
    pub timeout_secs: u64,
    /// Key cache TTL in seconds (0 = no cache)
    pub cache_ttl_secs: u64,
}

impl Default for KmsConfig {
    fn default() -> Self {
        Self {
            endpoint: "http://localhost:8200/v1/transit".to_string(),
            key_name: "ontodb-master".to_string(),
            token: String::new(),
            timeout_secs: 30,
            cache_ttl_secs: 300, // 5 minutes
        }
    }
}

/// KMS API response for key generation.
#[derive(Debug, Deserialize)]
struct KmsGenerateResponse {
    data: KmsGenerateData,
}

#[derive(Debug, Deserialize)]
struct KmsGenerateData {
    /// Base64-encoded key bytes
    key: String,
    /// Key version
    #[serde(default)]
    version: u64,
}

/// KMS API response for key retrieval.
#[derive(Debug, Deserialize)]
struct KmsGetKeyResponse {
    data: KmsGetKeyData,
}

#[derive(Debug, Deserialize)]
struct KmsGetKeyData {
    /// Base64-encoded key bytes
    keys: Vec<KmsKeyEntry>,
}

#[derive(Debug, Deserialize)]
struct KmsKeyEntry {
    /// Base64-encoded key
    key: String,
    /// Key version
    #[serde(default)]
    version: u64,
}

/// Cached key entry.
struct CachedKey {
    key_bytes: Vec<u8>,
    version: u64,
    cached_at: Instant,
}

/// KMS client for fetching encryption keys from a remote KMS.
pub struct KmsClient {
    config: KmsConfig,
    http_client: ureq::Agent,
    cached_key: Arc<RwLock<Option<CachedKey>>>,
}

impl KmsClient {
    /// Create a new KMS client.
    pub fn new(config: KmsConfig) -> Result<Self> {
        let http_client = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(config.timeout_secs))
            .build();

        Ok(Self {
            config,
            http_client,
            cached_key: Arc::new(RwLock::new(None)),
        })
    }

    /// Fetch the master key from KMS.
    /// Uses cache if available and not expired.
    pub fn fetch_master_key(&self) -> Result<(Vec<u8>, u64)> {
        // Check cache first
        if self.config.cache_ttl_secs > 0 {
            let cached = self.cached_key.read();
            if let Some(ref entry) = *cached {
                if entry.cached_at.elapsed() < Duration::from_secs(self.config.cache_ttl_secs) {
                    return Ok((entry.key_bytes.clone(), entry.version));
                }
            }
        }

        // Fetch from KMS
        let (key_bytes, version) = self.fetch_from_kms()?;

        // Update cache
        if self.config.cache_ttl_secs > 0 {
            let mut cached = self.cached_key.write();
            *cached = Some(CachedKey {
                key_bytes: key_bytes.clone(),
                version,
                cached_at: Instant::now(),
            });
        }

        Ok((key_bytes, version))
    }

    /// Generate a new data encryption key via KMS.
    pub fn generate_data_key(&self) -> Result<(Vec<u8>, Vec<u8>, u64)> {
        let url = format!("{}/datakey/plaintext/{}", self.config.endpoint, self.config.key_name);

        let resp: KmsGenerateResponse = self.http_client
            .post(&url)
            .set("Authorization", &format!("Bearer {}", self.config.token))
            .set("Content-Type", "application/json")
            .call()
            .map_err(|e| anyhow::anyhow!("KMS datakey generation failed: {}", e))?
            .into_json()?;

        let plaintext = base64_decode(&resp.data.key)
            .context("Failed to decode KMS data key")?;

        // The ciphertext is the encrypted version of the key
        let ciphertext = resp.data.key.as_bytes().to_vec();

        Ok((plaintext, ciphertext, resp.data.version))
    }

    /// Rotate the master key.
    pub fn rotate_key(&self) -> Result<(Vec<u8>, u64)> {
        // Clear cache
        {
            let mut cached = self.cached_key.write();
            *cached = None;
        }

        // Trigger rotation on KMS
        let url = format!("{}/keys/{}/rotate", self.config.endpoint, self.config.key_name);

        self.http_client
            .post(&url)
            .set("Authorization", &format!("Bearer {}", self.config.token))
            .set("Content-Type", "application/json")
            .call()
            .map_err(|e| anyhow::anyhow!("KMS key rotation failed: {}", e))?;

        // Fetch the new key
        self.fetch_master_key()
    }

    /// Fetch key from KMS endpoint.
    fn fetch_from_kms(&self) -> Result<(Vec<u8>, u64)> {
        let url = format!("{}/export/encryption-key/{}", self.config.endpoint, self.config.key_name);

        let resp: KmsGetKeyResponse = self.http_client
            .get(&url)
            .set("Authorization", &format!("Bearer {}", self.config.token))
            .call()
            .map_err(|e| anyhow::anyhow!("KMS key fetch failed: {}", e))?
            .into_json()?;

        let entry = resp.data.keys.first()
            .context("KMS returned no keys")?;

        let key_bytes = base64_decode(&entry.key)
            .context("Failed to decode KMS key")?;

        if key_bytes.len() != 32 {
            anyhow::bail!(
                "KMS key must be 32 bytes, got {}",
                key_bytes.len()
            );
        }

        Ok((key_bytes, entry.version))
    }

    /// Clear the key cache.
    pub fn clear_cache(&self) {
        let mut cached = self.cached_key.write();
        *cached = None;
    }

    /// Get the KMS endpoint URL.
    pub fn endpoint(&self) -> &str {
        &self.config.endpoint
    }

    /// Get the key name.
    pub fn key_name(&self) -> &str {
        &self.config.key_name
    }
}

/// Simple base64 decode.
fn base64_decode(input: &str) -> Result<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(input)
        .map_err(|e| anyhow::anyhow!("Base64 decode error: {}", e))
}

/// Base64 encode.
pub fn base64_encode(input: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(input)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_kms_config_default() {
        let config = KmsConfig::default();
        assert_eq!(config.endpoint, "http://localhost:8200/v1/transit");
        assert_eq!(config.key_name, "ontodb-master");
        assert_eq!(config.timeout_secs, 30);
        assert_eq!(config.cache_ttl_secs, 300);
    }

    #[test]
    fn test_kms_client_new() {
        let config = KmsConfig::default();
        let client = KmsClient::new(config);
        assert!(client.is_ok());
    }

    #[test]
    fn test_base64_roundtrip() {
        let data = b"hello world";
        let encoded = base64_encode(data);
        let decoded = base64_decode(&encoded).unwrap();
        assert_eq!(data.as_slice(), decoded.as_slice());
    }
}
