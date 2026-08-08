//! TLS configuration for OntoDB server.
//!
//! Supports TLS termination via configuration files.
//! For production, place OntoDB behind a reverse proxy (Nginx/Caddy) with TLS.
//! This module provides direct TLS support for development/testing.

use std::path::PathBuf;

/// TLS configuration.
#[derive(Debug, Clone)]
pub struct TlsConfig {
    /// Path to TLS certificate file (PEM format).
    pub cert_path: PathBuf,
    /// Path to TLS private key file (PEM format).
    pub key_path: PathBuf,
    /// Whether to require client certificates (mutual TLS).
    pub require_client_cert: bool,
    /// Path to CA certificate for client cert verification.
    pub ca_cert_path: Option<PathBuf>,
}

impl TlsConfig {
    /// Create a new TLS configuration.
    pub fn new(cert_path: PathBuf, key_path: PathBuf) -> Self {
        Self {
            cert_path,
            key_path,
            require_client_cert: false,
            ca_cert_path: None,
        }
    }

    /// Enable mutual TLS (require client certificates).
    pub fn with_client_auth(mut self, ca_cert_path: PathBuf) -> Self {
        self.require_client_cert = true;
        self.ca_cert_path = Some(ca_cert_path);
        self
    }

    /// Validate that cert and key files exist.
    pub fn validate(&self) -> Result<(), String> {
        if !self.cert_path.exists() {
            return Err(format!("TLS certificate not found: {:?}", self.cert_path));
        }
        if !self.key_path.exists() {
            return Err(format!("TLS key not found: {:?}", self.key_path));
        }
        if self.require_client_cert {
            if let Some(ca) = &self.ca_cert_path {
                if !ca.exists() {
                    return Err(format!("CA certificate not found: {:?}", ca));
                }
            } else {
                return Err("Client auth enabled but no CA certificate provided".to_string());
            }
        }
        Ok(())
    }
}

/// Generate a self-signed certificate for development/testing.
///
/// This is a convenience function for development. For production,
/// use certificates from a trusted CA.
pub fn generate_self_signed_cert(output_dir: &std::path::Path) -> Result<TlsConfig, String> {
    use std::fs;

    fs::create_dir_all(output_dir)
        .map_err(|e| format!("Failed to create output directory: {}", e))?;

    let cert_path = output_dir.join("server.crt");
    let key_path = output_dir.join("server.key");

    // Check if openssl is available
    let output = std::process::Command::new("openssl")
        .args([
            "req", "-x509", "-nodes", "-days", "365",
            "-newkey", "rsa:2048",
            "-keyout", key_path.to_str().unwrap(),
            "-out", cert_path.to_str().unwrap(),
            "-subj", "/CN=localhost/O=OntoDB/C=US",
        ])
        .output()
        .map_err(|e| format!("Failed to run openssl: {}. Install openssl or provide cert/key manually.", e))?;

    if !output.status.success() {
        return Err(format!(
            "openssl failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    Ok(TlsConfig::new(cert_path, key_path))
}
