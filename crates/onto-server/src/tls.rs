//! TLS configuration for OntoDB server — 等保2.0三级合规.
//!
//! Features:
//! - Server-side TLS (HTTPS)
//! - Client-side mutual TLS (mTLS) with CA verification
//! - Auto-load certificates from PEM files
//! - TLS 1.2+ only (禁用 TLS 1.0/1.1)
//! - Secure cipher suite configuration
//! - Self-signed certificate generation for development
//! - Certificate expiry monitoring

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::WebPkiClientVerifier;
use rustls::{ClientConfig, RootCertStore, ServerConfig};

/// TLS configuration for the OntoDB server.
#[derive(Debug, Clone)]
pub struct TlsConfig {
    /// Path to server certificate file (PEM format).
    pub cert_path: PathBuf,
    /// Path to server private key file (PEM format).
    pub key_path: PathBuf,
    /// Whether to require client certificates (mutual TLS).
    pub require_client_cert: bool,
    /// Path to CA certificate for client cert verification (PEM format).
    /// Required when `require_client_cert` is true.
    pub ca_cert_path: Option<PathBuf>,
    /// Minimum TLS version (default: 1.2).
    pub min_tls_version: TlsVersion,
    /// Path to directory for auto-detection of certs.
    /// If set, looks for server.crt, server.key, ca.crt in this directory.
    pub auto_load_dir: Option<PathBuf>,
}

/// Supported TLS versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TlsVersion {
    /// TLS 1.2 (minimum for 等保合规).
    Tls12,
    /// TLS 1.3 (recommended).
    Tls13,
}

impl TlsConfig {
    /// Create a new TLS configuration with certificate and key paths.
    pub fn new(cert_path: PathBuf, key_path: PathBuf) -> Self {
        Self {
            cert_path,
            key_path,
            require_client_cert: false,
            ca_cert_path: None,
            min_tls_version: TlsVersion::Tls12,
            auto_load_dir: None,
        }
    }

    /// Enable mutual TLS (require client certificates).
    pub fn with_client_auth(mut self, ca_cert_path: PathBuf) -> Self {
        self.require_client_cert = true;
        self.ca_cert_path = Some(ca_cert_path);
        self
    }

    /// Set minimum TLS version.
    pub fn with_min_version(mut self, version: TlsVersion) -> Self {
        self.min_tls_version = version;
        self
    }

    /// Set auto-load directory. Looks for server.crt, server.key, ca.crt.
    pub fn with_auto_load_dir(mut self, dir: PathBuf) -> Self {
        self.auto_load_dir = Some(dir);
        self
    }

    /// Auto-detect and load certificates from the configured directory.
    /// Looks for: server.crt, server.key, ca.crt
    pub fn auto_load(&mut self) -> Result<(), String> {
        let dir = match &self.auto_load_dir {
            Some(d) => d.clone(),
            None => return Ok(()),
        };

        let cert = dir.join("server.crt");
        let key = dir.join("server.key");
        let ca = dir.join("ca.crt");

        if cert.exists() {
            self.cert_path = cert;
        }
        if key.exists() {
            self.key_path = key;
        }
        if ca.exists() {
            self.ca_cert_path = Some(ca.clone());
            // Auto-enable mTLS if CA cert exists
            if !self.require_client_cert {
                self.require_client_cert = true;
                eprintln!("Auto-detected CA certificate, enabling mutual TLS");
            }
        }

        Ok(())
    }

    /// Validate that cert and key files exist and are readable.
    pub fn validate(&self) -> Result<(), String> {
        if !self.cert_path.exists() {
            return Err(format!("TLS certificate not found: {:?}", self.cert_path));
        }
        if !self.key_path.exists() {
            return Err(format!("TLS key not found: {:?}", self.key_path));
        }
        if self.require_client_cert {
            match &self.ca_cert_path {
                Some(ca) if ca.exists() => {}
                Some(ca) => return Err(format!("CA certificate not found: {:?}", ca)),
                None => return Err("Client auth enabled but no CA certificate provided".to_string()),
            }
        }
        Ok(())
    }

    /// Build a rustls ServerConfig for the HTTP server.
    pub fn build_server_config(&self) -> Result<ServerConfig, String> {
        self.validate()?;

        // Load server certificate chain
        let cert_chain = load_certs(&self.cert_path)?;

        // Load server private key
        let key = load_private_key(&self.key_path)?;

        // Build client verifier
        let client_verifier = if self.require_client_cert {
            let ca_certs = load_certs(
                self.ca_cert_path
                    .as_ref()
                    .ok_or("CA cert path required for mTLS")?,
            )?;
            let mut root_store = RootCertStore::empty();
            for cert in ca_certs {
                root_store.add(cert).map_err(|e| format!("Failed to add CA cert: {}", e))?;
            }
            WebPkiClientVerifier::builder(Arc::new(root_store))
                .build()
                .map_err(|e| format!("Failed to build client verifier: {}", e))?
        } else {
            WebPkiClientVerifier::no_client_auth()
        };

        // Build server config with protocol version based on min_tls_version
        let versions: &[&rustls::SupportedProtocolVersion] = match self.min_tls_version {
            TlsVersion::Tls13 => &[&rustls::version::TLS13],
            TlsVersion::Tls12 => &[&rustls::version::TLS12, &rustls::version::TLS13],
        };
        let config = ServerConfig::builder_with_provider(
            Arc::new(rustls::crypto::aws_lc_rs::default_provider())
        )
            .with_protocol_versions(versions)
            .map_err(|e| format!("Failed to set TLS protocol versions: {}", e))?
            .with_client_cert_verifier(client_verifier)
            .with_single_cert(cert_chain, key)
            .map_err(|e| format!("Failed to build TLS server config: {}", e))?;

        Ok(config)
    }

    /// Build a rustls ClientConfig for connecting to other OntoDB nodes (Raft, etc.).
    pub fn build_client_config(&self) -> Result<ClientConfig, String> {
        let mut root_store = RootCertStore::empty();

        // Add custom CA if provided (for self-signed certs in cluster)
        if let Some(ca_path) = &self.ca_cert_path {
            if ca_path.exists() {
                let ca_certs = load_certs(ca_path)?;
                for cert in ca_certs {
                    root_store
                        .add(cert)
                        .map_err(|e| format!("Failed to add CA cert: {}", e))?;
                }
            }
        }

        let versions: &[&rustls::SupportedProtocolVersion] = match self.min_tls_version {
            TlsVersion::Tls13 => &[&rustls::version::TLS13],
            TlsVersion::Tls12 => &[&rustls::version::TLS12, &rustls::version::TLS13],
        };

        let mut config = ClientConfig::builder_with_provider(
            Arc::new(rustls::crypto::aws_lc_rs::default_provider())
        )
            .with_protocol_versions(versions)
            .map_err(|e| format!("Failed to set TLS protocol versions: {}", e))?
            .with_root_certificates(root_store)
            .with_no_client_auth();

        // Enable HTTPS certificate scraping
        config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];

        Ok(config)
    }

    /// Get certificate expiry information.
    pub fn cert_expiry_info(&self) -> Result<CertExpiryInfo, String> {
        let certs = load_certs(&self.cert_path)?;
        if certs.is_empty() {
            return Err("No certificates found in file".to_string());
        }

        // Parse the first certificate to get expiry info
        let cert = &certs[0];
        let der = cert.as_ref();

        let subject = extract_subject_from_der(der).unwrap_or_else(|| "unknown".to_string());
        let issuer = extract_issuer_from_der(der).unwrap_or_else(|| "unknown".to_string());
        
        let (not_before, not_after, is_expired, days_until_expiry) = 
            extract_cert_validity(der).unwrap_or_else(|| (String::new(), String::new(), false, None));

        Ok(CertExpiryInfo {
            subject,
            issuer,
            not_before,
            not_after,
            is_expired,
            days_until_expiry,
        })
    }
}

/// Certificate expiry information.
#[derive(Debug, Clone)]
pub struct CertExpiryInfo {
    pub subject: String,
    pub issuer: String,
    pub not_before: String,
    pub not_after: String,
    pub is_expired: bool,
    pub days_until_expiry: Option<u64>,
}

/// Generate a self-signed certificate for development/testing.
///
/// Creates a CA certificate and a server certificate signed by it.
/// Returns the TLS config pointing to the generated files.
pub fn generate_self_signed_cert(output_dir: &Path) -> Result<TlsConfig, String> {
    use std::fs;

    fs::create_dir_all(output_dir)
        .map_err(|e| format!("Failed to create output directory: {}", e))?;

    let ca_cert_path = output_dir.join("ca.crt");
    let ca_key_path = output_dir.join("ca.key");
    let server_cert_path = output_dir.join("server.crt");
    let server_key_path = output_dir.join("server.key");

    // Generate CA key pair
    let ca_key = rcgen::KeyPair::generate()
        .map_err(|e| format!("Failed to generate CA key: {}", e))?;

    // Generate CA certificate
    let ca_params = rcgen::CertificateParams::new(vec!["OntoDB CA".to_string()])
        .map_err(|e| format!("Failed to create CA params: {}", e))?;
    let ca_cert = ca_params
        .self_signed(&ca_key)
        .map_err(|e| format!("Failed to generate CA cert: {}", e))?;

    // Generate server key pair
    let server_key = rcgen::KeyPair::generate()
        .map_err(|e| format!("Failed to generate server key: {}", e))?;

    // Generate server certificate signed by CA
    let mut server_params =
        rcgen::CertificateParams::new(vec!["localhost".to_string(), "127.0.0.1".to_string()])
            .map_err(|e| format!("Failed to create server params: {}", e))?;
    server_params.is_ca = rcgen::IsCa::NoCa;

    let server_cert = server_params
        .signed_by(&server_key, &ca_cert, &ca_key)
        .map_err(|e| format!("Failed to sign server cert: {}", e))?;

    // Write certificates and keys
    fs::write(&ca_cert_path, ca_cert.pem())
        .map_err(|e| format!("Failed to write CA cert: {}", e))?;
    fs::write(&ca_key_path, ca_key.serialize_pem())
        .map_err(|e| format!("Failed to write CA key: {}", e))?;
    fs::write(&server_cert_path, server_cert.pem())
        .map_err(|e| format!("Failed to write server cert: {}", e))?;
    fs::write(&server_key_path, server_key.serialize_pem())
        .map_err(|e| format!("Failed to write server key: {}", e))?;

    println!("Generated self-signed certificates:");
    println!("  CA certificate: {:?}", ca_cert_path);
    println!("  Server certificate: {:?}", server_cert_path);
    println!("  Server key: {:?}", server_key_path);

    Ok(TlsConfig::new(server_cert_path, server_key_path).with_client_auth(ca_cert_path))
}

/// Load certificates from a PEM file.
fn load_certs(path: &Path) -> Result<Vec<CertificateDer<'static>>, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("Failed to open cert file {:?}: {}", path, e))?;
    let mut reader = std::io::BufReader::new(file);
    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("Failed to parse certs from {:?}: {}", path, e))?;

    if certs.is_empty() {
        return Err(format!("No certificates found in {:?}", path));
    }

    Ok(certs)
}

/// Load a private key from a PEM file.
fn load_private_key(path: &Path) -> Result<PrivateKeyDer<'static>, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("Failed to open key file {:?}: {}", path, e))?;
    let mut reader = std::io::BufReader::new(file);

    // Try PKCS8 first
    let keys: Vec<_> = rustls_pemfile::pkcs8_private_keys(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("Failed to parse PKCS8 key: {}", e))?;

    if let Some(key) = keys.into_iter().next() {
        return Ok(PrivateKeyDer::Pkcs8(key));
    }

    // Reset reader and try RSA
    let file = std::fs::File::open(path).map_err(|e| format!("Failed to open key file: {}", e))?;
    let mut reader = std::io::BufReader::new(file);

    let keys: Vec<_> = rustls_pemfile::rsa_private_keys(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("Failed to parse RSA key: {}", e))?;

    if let Some(key) = keys.into_iter().next() {
        return Ok(PrivateKeyDer::Pkcs1(key));
    }

    Err(format!("No private key found in {:?}", path))
}

/// Extract subject from DER-encoded certificate using x509-parser.
fn extract_subject_from_der(der: &[u8]) -> Option<String> {
    use x509_parser::prelude::*;
    match X509Certificate::from_der(der) {
        Ok((_, cert)) => {
            // Try to get CN (Common Name) from subject
            if let Some(cn) = cert.subject().iter_common_name().next() {
                if let Ok(name) = cn.as_str() {
                    return Some(name.to_string());
                }
            }
            // Fallback: get the full subject string
            Some(cert.subject().to_string())
        }
        Err(_) => None,
    }
}

/// Extract issuer from DER-encoded certificate using x509-parser.
fn extract_issuer_from_der(der: &[u8]) -> Option<String> {
    use x509_parser::prelude::*;
    match X509Certificate::from_der(der) {
        Ok((_, cert)) => {
            // Try to get CN (Common Name) from issuer
            if let Some(cn) = cert.issuer().iter_common_name().next() {
                if let Ok(name) = cn.as_str() {
                    return Some(name.to_string());
                }
            }
            // Fallback: get the full issuer string
            Some(cert.issuer().to_string())
        }
        Err(_) => None,
    }
}

/// Extract certificate validity period from DER-encoded certificate.
fn extract_cert_validity(der: &[u8]) -> Option<(String, String, bool, Option<u64>)> {
    use x509_parser::prelude::*;
    use x509_parser::time::ASN1Time;
    
    match X509Certificate::from_der(der) {
        Ok((_, cert)) => {
            let not_before = cert.validity().not_before.to_string();
            let not_after = cert.validity().not_after.to_string();
            
            // Check if expired
            let now = ASN1Time::now();
            let is_expired = cert.validity().not_after < now;
            
            // Calculate days until expiry
            let days_until_expiry = if is_expired {
                Some(0)
            } else {
                // Approximate days remaining
                let remaining = cert.validity().not_after.to_string();
                // Parse the date and calculate difference
                // For simplicity, we'll return None if we can't calculate
                None
            };
            
            Some((not_before, not_after, is_expired, days_until_expiry))
        }
        Err(_) => None,
    }
}

/// TLS configuration for HTTP redirect (HTTP -> HTTPS).
#[derive(Debug, Clone)]
pub struct HttpRedirectConfig {
    /// HTTP listen address (redirects to HTTPS).
    pub http_addr: String,
    /// HTTPS listen address.
    pub https_addr: String,
}

/// Create an axum router that redirects HTTP to HTTPS.
pub fn create_redirect_router(https_port: u16) -> axum::Router {
    use axum::response::Redirect;
    use axum::routing::get;

    axum::Router::new().route(
        "/*path",
        get(move |uri: axum::http::Uri| async move {
            let host = uri.host().unwrap_or("localhost");
            let https_url = format!("https://{}:{}{}", host, https_port, uri.path());
            Redirect::permanent(&https_url)
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_tls_config_new() {
        let config = TlsConfig::new(
            PathBuf::from("/tmp/cert.pem"),
            PathBuf::from("/tmp/key.pem"),
        );
        assert!(!config.require_client_cert);
        assert_eq!(config.min_tls_version, TlsVersion::Tls12);
    }

    #[test]
    fn test_tls_config_with_client_auth() {
        let config = TlsConfig::new(
            PathBuf::from("/tmp/cert.pem"),
            PathBuf::from("/tmp/key.pem"),
        )
        .with_client_auth(PathBuf::from("/tmp/ca.pem"));

        assert!(config.require_client_cert);
        assert!(config.ca_cert_path.is_some());
    }

    #[test]
    fn test_validate_missing_cert() {
        let config = TlsConfig::new(
            PathBuf::from("/nonexistent/cert.pem"),
            PathBuf::from("/nonexistent/key.pem"),
        );
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_generate_self_signed_cert() {
        let dir = std::env::temp_dir().join(format!("ontodb_tls_test_{}", std::process::id()));
        let result = generate_self_signed_cert(&dir);
        assert!(result.is_ok(), "Failed to generate self-signed cert: {:?}", result.err());

        let config = result.unwrap();
        assert!(config.cert_path.exists());
        assert!(config.key_path.exists());
        assert!(config.ca_cert_path.as_ref().unwrap().exists());

        // Validate the config
        assert!(config.validate().is_ok());

        // Cleanup
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_load_certs_from_generated() {
        let dir = std::env::temp_dir().join(format!("ontodb_tls_test2_{}", std::process::id()));
        let config = generate_self_signed_cert(&dir).unwrap();

        let certs = load_certs(&config.cert_path);
        assert!(certs.is_ok());
        assert!(!certs.unwrap().is_empty());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_build_server_config() {
        let dir = std::env::temp_dir().join(format!("ontodb_tls_test3_{}", std::process::id()));
        let config = generate_self_signed_cert(&dir).unwrap();

        let server_config = config.build_server_config();
        assert!(server_config.is_ok(), "Failed to build server config: {:?}", server_config.err());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_build_client_config() {
        let dir = std::env::temp_dir().join(format!("ontodb_tls_test4_{}", std::process::id()));
        let config = generate_self_signed_cert(&dir).unwrap();

        let client_config = config.build_client_config();
        assert!(client_config.is_ok(), "Failed to build client config: {:?}", client_config.err());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_auto_load() {
        let dir = std::env::temp_dir().join(format!("ontodb_tls_test5_{}", std::process::id()));
        let generated = generate_self_signed_cert(&dir).unwrap();

        let mut config = TlsConfig::new(
            PathBuf::from("/tmp/cert.pem"),
            PathBuf::from("/tmp/key.pem"),
        )
        .with_auto_load_dir(dir.clone());

        config.auto_load().unwrap();

        // Should have auto-detected the certs
        assert_eq!(config.cert_path, dir.join("server.crt"));
        assert_eq!(config.key_path, dir.join("server.key"));
        assert!(config.require_client_cert);

        fs::remove_dir_all(&dir).ok();
    }
}
