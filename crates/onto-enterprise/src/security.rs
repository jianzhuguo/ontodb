//! Security module (placeholder).
//!
//! TODO: Implement LDAP/SAML authentication integration.

/// Security configuration placeholder.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SecurityConfig {
    pub enabled: bool,
    pub ldap_enabled: bool,
    pub ldap_url: Option<String>,
    pub saml_enabled: bool,
    pub saml_metadata_url: Option<String>,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            ldap_enabled: false,
            ldap_url: None,
            saml_enabled: false,
            saml_metadata_url: None,
        }
    }
}
