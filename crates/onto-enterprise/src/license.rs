//! License validation for OntoDB Enterprise features.
//!
//! This module provides license validation for enterprise features.
//! In development/test mode, a development license is used automatically.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Edition type.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Edition {
    /// Community edition (AGPL-3.0).
    Community,
    /// Enterprise Standard edition.
    Enterprise,
    /// Enterprise Gov/Finance edition.
    EnterpriseGov,
}

impl fmt::Display for Edition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Edition::Community => write!(f, "Community"),
            Edition::Enterprise => write!(f, "Enterprise"),
            Edition::EnterpriseGov => write!(f, "Enterprise Gov/Finance"),
        }
    }
}

/// License information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LicenseInfo {
    /// License edition.
    pub edition: Edition,
    /// License key (optional for community).
    pub license_key: Option<String>,
    /// Expiration timestamp (Unix seconds, None for perpetual).
    pub expires_at: Option<u64>,
    /// Licensed features.
    pub features: Vec<String>,
    /// Maximum number of nodes (0 = unlimited).
    pub max_nodes: u32,
    /// Maximum storage size in bytes (0 = unlimited).
    pub max_storage_bytes: u64,
}

impl LicenseInfo {
    /// Create a community license.
    pub fn community() -> Self {
        Self {
            edition: Edition::Community,
            license_key: None,
            expires_at: None,
            features: vec![
                "storage".to_string(),
                "query".to_string(),
                "vector".to_string(),
                "graph".to_string(),
            ],
            max_nodes: 1,
            max_storage_bytes: 0, // unlimited
        }
    }

    /// Create a development license (all features enabled).
    pub fn development() -> Self {
        Self {
            edition: Edition::EnterpriseGov,
            license_key: Some("dev-license".to_string()),
            expires_at: None,
            features: vec![
                "storage".to_string(),
                "query".to_string(),
                "vector".to_string(),
                "graph".to_string(),
                "cluster".to_string(),
                "sharding".to_string(),
                "encryption".to_string(),
                "audit".to_string(),
                "backup".to_string(),
            ],
            max_nodes: 0,      // unlimited
            max_storage_bytes: 0, // unlimited
        }
    }

    /// Check if a specific feature is enabled.
    pub fn has_feature(&self, feature: &str) -> bool {
        self.features.contains(&feature.to_string())
    }

    /// Check if the license is valid (not expired).
    pub fn is_valid(&self) -> bool {
        if let Some(expires_at) = self.expires_at {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            now < expires_at
        } else {
            true // No expiration = perpetual license
        }
    }

    /// Check if a node can be added (within limits).
    pub fn can_add_node(&self, current_nodes: u32) -> bool {
        self.max_nodes == 0 || current_nodes < self.max_nodes
    }

    /// Check if storage can grow (within limits).
    pub fn can_store_bytes(&self, current_bytes: u64, additional_bytes: u64) -> bool {
        if self.max_storage_bytes == 0 {
            return true; // unlimited
        }
        current_bytes + additional_bytes <= self.max_storage_bytes
    }
}

/// License validator.
pub struct LicenseValidator {
    license: LicenseInfo,
}

impl LicenseValidator {
    /// Create a new license validator with the given license.
    pub fn new(license: LicenseInfo) -> Self {
        Self { license }
    }

    /// Create a validator with community license.
    pub fn community() -> Self {
        Self::new(LicenseInfo::community())
    }

    /// Create a validator with development license.
    pub fn development() -> Self {
        Self::new(LicenseInfo::development())
    }

    /// Get the current license info.
    pub fn license(&self) -> &LicenseInfo {
        &self.license
    }

    /// Get the current edition.
    pub fn edition(&self) -> &Edition {
        &self.license.edition
    }

    /// Check if a feature is available.
    pub fn check_feature(&self, feature: &str) -> Result<(), LicenseError> {
        if !self.license.is_valid() {
            return Err(LicenseError::Expired);
        }
        if !self.license.has_feature(feature) {
            return Err(LicenseError::FeatureNotAvailable {
                feature: feature.to_string(),
                edition: self.license.edition.to_string(),
            });
        }
        Ok(())
    }

    /// Check if a node can be added.
    pub fn check_node_limit(&self, current_nodes: u32) -> Result<(), LicenseError> {
        if !self.license.can_add_node(current_nodes) {
            return Err(LicenseError::NodeLimitExceeded {
                max: self.license.max_nodes,
                current: current_nodes,
            });
        }
        Ok(())
    }

    /// Check if storage can grow.
    pub fn check_storage_limit(&self, current_bytes: u64, additional_bytes: u64) -> Result<(), LicenseError> {
        if !self.license.can_store_bytes(current_bytes, additional_bytes) {
            return Err(LicenseError::StorageLimitExceeded {
                max: self.license.max_storage_bytes,
                current: current_bytes,
                requested: additional_bytes,
            });
        }
        Ok(())
    }
}

/// License validation errors.
#[derive(Debug, thiserror::Error)]
pub enum LicenseError {
    #[error("license has expired")]
    Expired,

    #[error("feature '{feature}' is not available in {edition} edition")]
    FeatureNotAvailable { feature: String, edition: String },

    #[error("node limit exceeded: max {max}, current {current}")]
    NodeLimitExceeded { max: u32, current: u32 },

    #[error("storage limit exceeded: max {max} bytes, current {current} bytes, requested {requested} bytes")]
    StorageLimitExceeded { max: u64, current: u64, requested: u64 },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_community_license() {
        let license = LicenseInfo::community();
        assert_eq!(license.edition, Edition::Community);
        assert!(license.is_valid());
        assert!(license.has_feature("storage"));
        assert!(!license.has_feature("cluster"));
    }

    #[test]
    fn test_development_license() {
        let license = LicenseInfo::development();
        assert_eq!(license.edition, Edition::EnterpriseGov);
        assert!(license.is_valid());
        assert!(license.has_feature("storage"));
        assert!(license.has_feature("cluster"));
        assert!(license.has_feature("encryption"));
    }

    #[test]
    fn test_license_validator() {
        let validator = LicenseValidator::community();
        assert!(validator.check_feature("storage").is_ok());
        assert!(validator.check_feature("cluster").is_err());
    }

    #[test]
    fn test_expired_license() {
        let mut license = LicenseInfo::community();
        license.expires_at = Some(1000000000); // Already expired
        let validator = LicenseValidator::new(license);
        assert!(validator.check_feature("storage").is_err());
    }
}
