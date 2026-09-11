//! Unified entity identity for OntoDB's multi-modal storage.
//!
//! `EntityId` is the semantic anchor point shared by all three data modalities:
//! - **Relational**: LSM key `{namespace}::{class}::{pk}`
//! - **Graph**: vertex ID `{class}::{pk}`
//! - **Vector**: document key (same as LSM key)
//!
//! All modalities reference the same entity through this unified identifier.
//! **Namespace is required** — no silent defaults, no data pollution.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Unified entity identifier — the semantic anchor point for all data modalities.
///
/// Format: `{namespace}::{class}::{pk}`
///
/// All three fields are **required**. There is no default namespace —
/// callers must explicitly provide the namespace to prevent data pollution
/// in multi-tenant environments.
///
/// # Examples
///
/// ```
/// use onto_core::EntityId;
///
/// let id = EntityId::new("tenant_a", "Product", "000001");
/// assert_eq!(id.to_lsm_key(), b"tenant_a::Product::000001");
/// assert_eq!(id.namespace(), "tenant_a");
/// assert_eq!(id.class(), "Product");
/// assert_eq!(id.pk(), "000001");
/// ```
#[derive(Debug, Clone, Hash, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct EntityId {
    /// Namespace for multi-tenant isolation (e.g., "default", "tenant_a").
    namespace: String,
    /// Class name (e.g., "Product", "Employee").
    class: String,
    /// Primary key (e.g., "000001", "uuid-xxx").
    pk: String,
}

impl EntityId {
    /// Creates a new entity identifier. All three fields are required.
    pub fn new(
        namespace: impl Into<String>,
        class: impl Into<String>,
        pk: impl Into<String>,
    ) -> Self {
        Self {
            namespace: namespace.into(),
            class: class.into(),
            pk: pk.into(),
        }
    }

    /// Returns the namespace.
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    /// Returns the class name.
    pub fn class(&self) -> &str {
        &self.class
    }

    /// Returns the primary key.
    pub fn pk(&self) -> &str {
        &self.pk
    }

    /// Converts to LSM storage key bytes.
    ///
    /// Format: `{namespace}::{class}::{pk}`
    pub fn to_lsm_key(&self) -> Vec<u8> {
        format!("{}::{}::{}", self.namespace, self.class, self.pk).into_bytes()
    }

    /// Converts to storage key string.
    ///
    /// Format: `{namespace}::{class}::{pk}`
    pub fn to_storage_key(&self) -> String {
        format!("{}::{}::{}", self.namespace, self.class, self.pk)
    }

    /// Converts to graph vertex ID string.
    ///
    /// Format: `{class}::{pk}` (namespace-agnostic for graph traversal)
    pub fn to_vertex_id(&self) -> String {
        format!("{}::{}", self.class, self.pk)
    }

    /// Parses an `EntityId` from LSM key bytes.
    ///
    /// Format: `{namespace}::{class}::{pk}` (three-part only)
    pub fn from_lsm_key(key: &[u8]) -> Option<Self> {
        let s = std::str::from_utf8(key).ok()?;
        Self::from_str(s)
    }

    /// Parses an `EntityId` from a storage key string.
    ///
    /// Alias for `from_str`.
    pub fn from_storage_key(key: &str) -> Option<Self> {
        Self::from_str(key)
    }

    /// Parses an `EntityId` from a string.
    ///
    /// Format: `{namespace}::{class}::{pk}` (three-part only)
    ///
    /// Returns `None` if the format is invalid (must have exactly 3 non-empty parts).
    pub fn from_str(s: &str) -> Option<Self> {
        let parts: Vec<&str> = s.split("::").collect();
        if parts.len() != 3 {
            return None;
        }
        if parts[0].is_empty() || parts[1].is_empty() || parts[2].is_empty() {
            return None;
        }
        Some(Self {
            namespace: parts[0].to_string(),
            class: parts[1].to_string(),
            pk: parts[2].to_string(),
        })
    }

    /// Returns true if this entity belongs to the given class.
    pub fn is_class(&self, class: &str) -> bool {
        self.class == class
    }

    /// Returns the key prefix for scanning all entities of this class in this namespace.
    ///
    /// Format: `{namespace}::{class}::`
    pub fn class_prefix(&self) -> Vec<u8> {
        format!("{}::{}::", self.namespace, self.class).into_bytes()
    }

    /// Returns the key prefix for scanning all entities in this namespace.
    ///
    /// Format: `{namespace}::`
    pub fn namespace_prefix(&self) -> Vec<u8> {
        format!("{}::", self.namespace).into_bytes()
    }
}

impl fmt::Display for EntityId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}::{}::{}", self.namespace, self.class, self.pk)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_entity_id_new() {
        let id = EntityId::new("default", "Product", "000001");
        assert_eq!(id.namespace(), "default");
        assert_eq!(id.class(), "Product");
        assert_eq!(id.pk(), "000001");
    }

    #[test]
    fn test_entity_id_custom_namespace() {
        let id = EntityId::new("tenant_a", "Product", "000001");
        assert_eq!(id.namespace(), "tenant_a");
    }

    #[test]
    fn test_entity_id_to_lsm_key() {
        let id = EntityId::new("tenant_a", "Product", "000001");
        assert_eq!(id.to_lsm_key(), b"tenant_a::Product::000001");
    }

    #[test]
    fn test_entity_id_to_storage_key() {
        let id = EntityId::new("ns", "Product", "001");
        assert_eq!(id.to_storage_key(), "ns::Product::001");
    }

    #[test]
    fn test_entity_id_to_vertex_id() {
        let id = EntityId::new("hr", "Employee", "alice");
        assert_eq!(id.to_vertex_id(), "Employee::alice");
    }

    #[test]
    fn test_entity_id_from_lsm_key() {
        let id = EntityId::from_lsm_key(b"tenant_a::Product::000001").unwrap();
        assert_eq!(id.namespace(), "tenant_a");
        assert_eq!(id.class(), "Product");
        assert_eq!(id.pk(), "000001");
    }

    #[test]
    fn test_entity_id_from_storage_key() {
        let id = EntityId::from_storage_key("ns::Product::001").unwrap();
        assert_eq!(id.namespace(), "ns");
    }

    #[test]
    fn test_entity_id_from_str_three_part() {
        let id = EntityId::from_str("ns::Product::001").unwrap();
        assert_eq!(id.namespace(), "ns");
    }

    #[test]
    fn test_entity_id_from_str_two_part_fails() {
        // Two-part format is no longer valid — namespace is required
        assert!(EntityId::from_str("Product::001").is_none());
    }

    #[test]
    fn test_entity_id_from_str_no_separator() {
        assert!(EntityId::from_str("no_separator").is_none());
    }

    #[test]
    fn test_entity_id_from_str_empty_parts() {
        assert!(EntityId::from_str("::Class::key").is_none());
        assert!(EntityId::from_str("ns::Class::").is_none());
        assert!(EntityId::from_str("ns::::key").is_none());
        assert!(EntityId::from_str("").is_none());
    }

    #[test]
    fn test_entity_id_from_str_four_parts() {
        // Too many parts
        assert!(EntityId::from_str("a::b::c::d").is_none());
    }

    #[test]
    fn test_entity_id_is_class() {
        let id = EntityId::new("ns", "Product", "001");
        assert!(id.is_class("Product"));
        assert!(!id.is_class("Employee"));
    }

    #[test]
    fn test_entity_id_class_prefix() {
        let id = EntityId::new("tenant_a", "Product", "001");
        assert_eq!(id.class_prefix(), b"tenant_a::Product::");
    }

    #[test]
    fn test_entity_id_namespace_prefix() {
        let id = EntityId::new("tenant_a", "Product", "001");
        assert_eq!(id.namespace_prefix(), b"tenant_a::");
    }

    #[test]
    fn test_entity_id_display() {
        let id = EntityId::new("ns", "Product", "001");
        assert_eq!(format!("{}", id), "ns::Product::001");
    }

    #[test]
    fn test_entity_id_equality() {
        let a = EntityId::new("ns", "Product", "001");
        let b = EntityId::new("ns", "Product", "001");
        let c = EntityId::new("other", "Product", "001");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn test_entity_id_namespace_isolation() {
        let a = EntityId::new("tenant_a", "Device", "001");
        let b = EntityId::new("tenant_b", "Device", "001");
        assert_ne!(a, b);
        assert_ne!(a.to_lsm_key(), b.to_lsm_key());
    }

    #[test]
    fn test_entity_id_lsm_key_roundtrip() {
        let original = EntityId::new("shop", "Category", "electronics");
        let key = original.to_lsm_key();
        let restored = EntityId::from_lsm_key(&key).unwrap();
        assert_eq!(original, restored);
    }

    #[test]
    fn test_entity_id_with_special_chars() {
        let id = EntityId::new("my-ns", "My-Class", "key/with/slashes");
        assert_eq!(id.to_lsm_key(), b"my-ns::My-Class::key/with/slashes");
        assert_eq!(id.to_vertex_id(), "My-Class::key/with/slashes");
    }

    #[test]
    fn test_entity_id_clone() {
        let id = EntityId::new("ns", "Product", "001");
        let cloned = id.clone();
        assert_eq!(id, cloned);
    }
}
