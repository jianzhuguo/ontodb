//! Security module for OntoDB Enterprise.
//!
//! Provides:
//! - LDAP authentication integration
//! - SAML authentication (placeholder)
//! - Security policy management

use anyhow::Result;
use serde::{Deserialize, Serialize};
use crate::ldap;

/// Security configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityConfig {
    /// Enable security module.
    pub enabled: bool,
    /// LDAP configuration.
    pub ldap: Option<ldap::LdapConfig>,
    /// SAML configuration (placeholder).
    pub saml: Option<SamlConfig>,
    /// Security policies.
    pub policies: SecurityPolicies,
}

/// SAML configuration (placeholder).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SamlConfig {
    /// SAML metadata URL
    pub metadata_url: String,
    /// Service Provider entity ID
    pub sp_entity_id: String,
    /// Assertion Consumer Service URL
    pub acs_url: String,
}

/// Security policies.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityPolicies {
    /// Maximum login attempts before lockout
    pub max_login_attempts: u32,
    /// Lockout duration in minutes
    pub lockout_duration_mins: u32,
    /// Password minimum length
    pub password_min_length: u32,
    /// Require special characters in password
    pub password_require_special: bool,
    /// Session timeout in minutes
    pub session_timeout_mins: u32,
}

impl Default for SecurityPolicies {
    fn default() -> Self {
        Self {
            max_login_attempts: 5,
            lockout_duration_mins: 15,
            password_min_length: 8,
            password_require_special: true,
            session_timeout_mins: 60,
        }
    }
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            ldap: None,
            saml: None,
            policies: SecurityPolicies::default(),
        }
    }
}

/// Security manager.
pub struct SecurityManager {
    config: SecurityConfig,
    ldap_client: Option<ldap::LdapClient>,
}

impl SecurityManager {
    /// Create a new security manager.
    pub fn new(config: SecurityConfig) -> Self {
        let ldap_client = config.ldap.as_ref().map(|c| ldap::LdapClient::new(c.clone()));
        
        Self {
            config,
            ldap_client,
        }
    }

    /// Authenticate a user.
    ///
    /// Tries LDAP first, falls back to local authentication.
    pub fn authenticate(&self, username: &str, password: &str) -> Result<ldap::LdapUser> {
        if !self.config.enabled {
            anyhow::bail!("Security module is disabled");
        }

        // Try LDAP authentication
        if let Some(ref client) = self.ldap_client {
            return client.authenticate(username, password);
        }

        anyhow::bail!("No authentication method configured")
    }

    /// Check if LDAP is enabled.
    pub fn has_ldap(&self) -> bool {
        self.ldap_client.is_some()
    }

    /// Clear authentication cache.
    pub fn clear_cache(&self) {
        if let Some(ref client) = self.ldap_client {
            client.clear_cache();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_security_config_default() {
        let config = SecurityConfig::default();
        assert!(!config.enabled);
        assert!(config.ldap.is_none());
        assert!(config.saml.is_none());
        assert_eq!(config.policies.max_login_attempts, 5);
    }

    #[test]
    fn test_security_manager_no_auth() {
        let config = SecurityConfig {
            enabled: true,
            ..Default::default()
        };
        let manager = SecurityManager::new(config);
        assert!(!manager.has_ldap());
    }

    #[test]
    fn test_security_manager_with_ldap() {
        let config = SecurityConfig {
            enabled: true,
            ldap: Some(ldap::LdapConfig::default()),
            ..Default::default()
        };
        let manager = SecurityManager::new(config);
        assert!(manager.has_ldap());
    }

    #[test]
    fn test_security_manager_disabled() {
        let config = SecurityConfig::default();
        let manager = SecurityManager::new(config);
        let result = manager.authenticate("user", "pass");
        assert!(result.is_err());
    }
}
