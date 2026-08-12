#![allow(clippy::too_many_arguments)]
#![allow(clippy::type_complexity)]
#![allow(clippy::manual_strip)]
#![allow(clippy::needless_range_loop)]
#![allow(clippy::new_without_default)]
#![allow(clippy::collapsible_match)]
#![allow(clippy::if_same_then_else)]
#![allow(clippy::manual_checked_ops)]
#![allow(clippy::ptr_arg)]
#![allow(clippy::non_canonical_partial_ord_impl)]
#![allow(clippy::should_implement_trait)]
#![allow(clippy::sliced_string_as_bytes)]
#![allow(clippy::len_without_is_empty)]
#![allow(clippy::lines_filter_map_ok)]
#![allow(clippy::vec_init_then_push)]
#![allow(clippy::unnecessary_find_map)]
#![allow(clippy::unnecessary_unwrap)]
#![allow(clippy::result_large_err)]
#![allow(clippy::doc_lazy_continuation)]

//! OntoDB Enterprise features.
//!
//! This crate contains proprietary enterprise functionality organized into
//! three product tiers:
//!
//! - **Open Source**: Core database functionality (no enterprise features)
//! - **Enterprise Standard**: Clustering, sharding, basic backup
//! - **Enterprise Gov/Finance**: All features including encryption, audit retention, CRC validation
//!
//! Feature flags control which modules are compiled:
//! - `cluster` 鈥?Raft consensus, multi-replica, automatic failover
//! - `sharding` 鈥?data sharding, cross-shard queries
//! - `security` 鈥?LDAP/SAML authentication
//! - `encryption` 鈥?TLS transport + AES storage encryption
//! - `backup` 鈥?full backup
//! - `incremental-backup` 鈥?incremental backup
//! - `pitr` 鈥?point-in-time recovery
//! - `observability` 鈥?advanced monitoring, slow query analysis
//! - `audit-retention` 鈥?audit log rotation and retention (绛変繚2.0)
//! - `crc-validation` 鈥?SSTable page-level CRC checksum
//! - `rolling-upgrade` 鈥?cross-version compatibility

// === Cluster features ===
#[cfg(feature = "cluster")]
pub mod cluster;

#[cfg(feature = "cluster")]
pub mod cluster_config;

#[cfg(feature = "cluster")]
pub mod cluster_router;

// === Sharding features ===
#[cfg(feature = "sharding")]
pub mod sharding;

#[cfg(feature = "sharding")]
pub mod cross_shard;

// === Security features (LDAP/SAML) ===
#[cfg(feature = "security")]
pub mod security;

#[cfg(feature = "security")]
pub mod ldap;

// === Encryption (TLS + AES) ===
#[cfg(feature = "encryption")]
pub mod encryption;

// === KMS (Key Management Service) ===
#[cfg(feature = "encryption")]
pub mod kms;

// === Backup features ===
#[cfg(feature = "backup")]
pub mod backup;

// === Observability ===
#[cfg(feature = "observability")]
pub mod observability;

// === Gov/Finance specific modules ===
#[cfg(feature = "audit-retention")]
pub mod audit_retention;

#[cfg(feature = "crc-validation")]
pub mod crc_validation;

#[cfg(feature = "rolling-upgrade")]
pub mod rolling_upgrade;

// === Three-Privilege Separation (涓夋潈鍒嗙珛) RBAC ===
#[cfg(feature = "security")]
pub mod rbac;

// === Data Masking (鏁版嵁鑴辨晱) ===
#[cfg(feature = "security")]
pub mod data_masking;

// === Data Migration (鏁版嵁杩佺Щ) ===
#[cfg(feature = "backup")]
pub mod data_migration;

/// Product tier identification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ProductTier {
    /// Open source edition 鈥?no enterprise features.
    OpenSource,
    /// Enterprise Standard 鈥?clustering, sharding, basic backup.
    EnterpriseStandard,
    /// Enterprise Gov/Finance 鈥?all features including security and compliance.
    EnterpriseGov,
}

/// Returns the current product tier based on compiled features.
pub fn current_tier() -> ProductTier {
    if cfg!(feature = "enterprise-gov") {
        ProductTier::EnterpriseGov
    } else if cfg!(feature = "enterprise-standard") {
        ProductTier::EnterpriseStandard
    } else {
        ProductTier::OpenSource
    }
}

/// Returns a list of enabled enterprise features.
pub fn enabled_features() -> Vec<&'static str> {
    let mut features = Vec::new();

    if cfg!(feature = "cluster") {
        features.push("cluster");
    }
    if cfg!(feature = "sharding") {
        features.push("sharding");
    }
    if cfg!(feature = "security") {
        features.push("security");
    }
    if cfg!(feature = "encryption") {
        features.push("encryption");
    }
    if cfg!(feature = "backup") {
        features.push("backup");
    }
    if cfg!(feature = "incremental-backup") {
        features.push("incremental-backup");
    }
    if cfg!(feature = "pitr") {
        features.push("pitr");
    }
    if cfg!(feature = "observability") {
        features.push("observability");
    }
    if cfg!(feature = "audit-retention") {
        features.push("audit-retention");
    }
    if cfg!(feature = "crc-validation") {
        features.push("crc-validation");
    }
    if cfg!(feature = "rolling-upgrade") {
        features.push("rolling-upgrade");
    }

    features
}

/// Enterprise license verification placeholder.
pub fn is_enterprise_enabled() -> bool {
    current_tier() != ProductTier::OpenSource
}

/// Enterprise features initialization configuration.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct EnterpriseConfig {
    /// Encryption configuration (Gov/Finance edition).
    #[cfg(feature = "encryption")]
    pub encryption: encryption::EncryptionConfig,
    
    /// Audit retention configuration (Gov/Finance edition).
    #[cfg(feature = "audit-retention")]
    pub audit_retention: audit_retention::AuditRetentionConfig,
    
    /// CRC validation configuration (Gov/Finance edition).
    #[cfg(feature = "crc-validation")]
    pub crc_validation: crc_validation::CrcValidationConfig,
}

/// Initialized enterprise features.
pub struct EnterpriseFeatures {
    /// Encryption manager (Gov/Finance edition).
    #[cfg(feature = "encryption")]
    pub encryption: Option<encryption::EncryptionManager>,
    
    /// Audit retention manager (Gov/Finance edition).
    #[cfg(feature = "audit-retention")]
    pub audit_retention: Option<audit_retention::AuditRetentionManager>,
    
    /// CRC validation manager (Gov/Finance edition).
    #[cfg(feature = "crc-validation")]
    pub crc_validation: Option<crc_validation::CrcValidationManager>,
}

impl EnterpriseFeatures {
    /// Initialize enterprise features based on configuration.
    /// For Open Source edition, this returns empty features.
    /// For Gov/Finance edition, this initializes encryption and audit.
    pub fn init(_config: &EnterpriseConfig) -> anyhow::Result<Self> {
        let _tier = current_tier();
        
        #[cfg(feature = "encryption")]
        let encryption = if tier == ProductTier::EnterpriseGov && config.encryption.storage_encryption {
            let manager = encryption::EncryptionManager::new(config.encryption.clone());
            manager.init()?;
            tracing::info!("Encryption initialized (AES-256-GCM)");
            Some(manager)
        } else {
            None
        };
        
        #[cfg(feature = "audit-retention")]
        let audit_retention = if tier == ProductTier::EnterpriseGov && config.audit_retention.enabled {
            let manager = audit_retention::AuditRetentionManager::new(config.audit_retention.clone());
            manager.init()?;
            tracing::info!("Audit retention initialized ({} days retention)", config.audit_retention.retention_days);
            Some(manager)
        } else {
            None
        };
        
        #[cfg(feature = "crc-validation")]
        let crc_validation = if tier == ProductTier::EnterpriseGov && config.crc_validation.enabled {
            let manager = crc_validation::CrcValidationManager::new(config.crc_validation.clone());
            tracing::info!("CRC validation initialized");
            Some(manager)
        } else {
            None
        };
        
        Ok(Self {
            #[cfg(feature = "encryption")]
            encryption,
            #[cfg(feature = "audit-retention")]
            audit_retention,
            #[cfg(feature = "crc-validation")]
            crc_validation,
        })
    }
    
    /// Shutdown all enterprise features gracefully.
    pub fn shutdown(&self) -> anyhow::Result<()> {
        #[cfg(feature = "audit-retention")]
        if let Some(ref manager) = self.audit_retention {
            manager.shutdown()?;
            tracing::info!("Audit retention shut down");
        }
        
        Ok(())
    }
}

/// Create default enterprise config for Gov/Finance edition.
pub fn default_gov_config() -> EnterpriseConfig {
    EnterpriseConfig {
        #[cfg(feature = "encryption")]
        encryption: encryption::EncryptionConfig {
            storage_encryption: true,
            algorithm: encryption::EncryptionAlgorithm::Sm4Cbc, // 鏀夸紒鐗堥粯璁や娇鐢ㄥ浗瀵哠M4
            master_key_source: encryption::KeySource::Env("ONTO_MASTER_KEY".to_string()),
            ..Default::default()
        },
        #[cfg(feature = "audit-retention")]
        audit_retention: audit_retention::AuditRetentionConfig {
            enabled: true,
            retention_days: 180, // 绛変繚2.0瑕佹眰
            compress_rotated: true,
            ..Default::default()
        },
        #[cfg(feature = "crc-validation")]
        crc_validation: crc_validation::CrcValidationConfig {
            enabled: true,
            ..Default::default()
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_product_tier() {
        let tier = current_tier();
        // Tier depends on which features are enabled at compile time
        #[cfg(feature = "enterprise-gov")]
        assert_eq!(tier, ProductTier::EnterpriseGov);

        #[cfg(all(feature = "enterprise-standard", not(feature = "enterprise-gov")))]
        assert_eq!(tier, ProductTier::EnterpriseStandard);

        #[cfg(not(any(feature = "enterprise-gov", feature = "enterprise-standard")))]
        assert_eq!(tier, ProductTier::OpenSource);
    }

    #[test]
    fn test_enabled_features() {
        let features = enabled_features();
        // Features depend on compile-time configuration
        // When running with any feature flag, features should not be empty
        #[cfg(any(feature = "enterprise-gov", feature = "enterprise-standard", feature = "security", feature = "encryption", feature = "sharding", feature = "cluster", feature = "backup", feature = "observability"))]
        assert!(!features.is_empty());

        #[cfg(not(any(feature = "enterprise-gov", feature = "enterprise-standard", feature = "security", feature = "encryption", feature = "sharding", feature = "cluster", feature = "backup", feature = "observability")))]
        assert!(features.is_empty());
    }

    #[test]
    fn test_product_tier_display() {
        assert_eq!(format!("{:?}", ProductTier::OpenSource), "OpenSource");
        assert_eq!(format!("{:?}", ProductTier::EnterpriseStandard), "EnterpriseStandard");
        assert_eq!(format!("{:?}", ProductTier::EnterpriseGov), "EnterpriseGov");
    }
}
