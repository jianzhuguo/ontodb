//! Unified entity identity for OntoDB's multi-modal storage.
//!
//! `EntityId` is the semantic anchor point shared by all three data modalities:
//! - **Relational**: LSM key `{namespace}::{class}::{pk}`
//! - **Graph**: vertex ID `{class}::{pk}`
//! - **Vector**: document key (same as LSM key)
//!
//! All modalities reference the same entity through this unified identifier.
//! Namespace support enables multi-tenant data isolation at the entity level.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Default namespace for backward compatibility.
pub const DEFAULT_NAMESPACE: &str = "default";

/// Unified entity identifier — the semantic anchor point for all data modalities.
///
/// An `EntityId` uniquely identifies a logical entity in the database.
/// Format: `{namespace}::{class}::{pk}`
///
/// - `namespace`: Multi-tenant isolation (defaults to "default")
/// - `class`: Entity class/type (e.g., "Product", "Employee")
/// - `pk`: Primary key (e.g., "000001", "uuid-xxx")
///
/// The same `EntityId` is used as:
/// - LSM storage key (via `to_lsm_key()`) — three-part: `{namespace}::{class}::{pk}`
/// - Graph vertex ID (via `to_vertex_id()`) — two-part: `{class}::{pk}`
/// - Vector index document key (via `to_lsm_key()`)
/// - Triple store subject/object (via `to_vertex_id()`)
///
/// # Examples
///
/// ```
/// use onto_core::EntityId;
///
/// // Default namespace
/// let id = EntityId::new("Product", "000001");
/// assert_eq!(id.to_lsm_key(), b"default::Product::000001");
/// assert_eq!(id.namespace(), "default");
///
/// // Custom namespace
/// let id = EntityId::new("Product", "000001").with_namespace("tenant_a");
/// assert_eq!(id.to_lsm_key(), b"tenant_a::Product::000001");
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
    /// Creates a new entity identifier in the default namespace.
    pub fn new(class: impl Into<String>, pk: impl Into<String>) -> Self {
        Self {
            namespace: DEFAULT_NAMESPACE.to_string(),
            class: class.into(),
            pk: pk.into(),
        }
    }

    /// Creates a new entity identifier in a specific namespace.
    pub fn new_namespaced(namespace: impl Into<String>, class: impl Into<String>, pk: impl Into<String>) -> Self {
        Self {
            namespace: namespace.into(),
            class: class.into(),
            pk: pk.into(),
        }
    }

    /// Sets the namespace (builder pattern).
    pub fn with_namespace(mut self, namespace: impl Into<String>) -> Self {
        self.namespace = namespace.into();
        self
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

    /// Converts to graph vertex ID string.
    ///
    /// Format: `{class}::{pk}` (namespace-agnostic for graph traversal)
    pub fn to_vertex_id(&self) -> String {
        format!("{}::{}", self.class, self.pk)
    }

    /// Converts to storage key string (alias for `to_lsm_key` as String).
    ///
    /// Format: `{namespace}::{class}::{pk}`
    pub fn to_storage_key(&self) -> String {
        format!("{}::{}::{}", self.namespace, self.class, self.pk)
    }

    /// Parses an `EntityId` from a storage key string.
    ///
    /// Alias for `from_str`. Supports both three-part and two-part formats.
    pub fn from_storage_key(key: &str) -> Option<Self> {
        Self::from_str(key)
    }

    /// Parses an `EntityId` from LSM key bytes.
    ///
    /// Supports both three-part (`ns::class::pk`) and two-part (`class::pk`) formats.
    /// Two-part keys default to the "default" namespace.
    pub fn from_lsm_key(key: &[u8]) -> Option<Self> {
        let s = std::str::from_utf8(key).ok()?;
        Self::from_str(s)
    }

    /// Parses an `EntityId` from a string.
    ///
    /// Supports:
    /// - Three-part: `namespace::class::pk` → full EntityId
    /// - Two-part: `class::pk` → namespace defaults to "default"
    ///
    /// Returns `None` if the format is invalid.
    pub fn from_str(s: &str) -> Option<Self> {
        let parts: Vec<&str> = s.split("::").collect();
        match parts.len() {
            3 => {
                if parts[0].is_empty() || parts[1].is_empty() || parts[2].is_empty() {
                    return None;
                }
                Some(Self {
                    namespace: parts[0].to_string(),
                    class: parts[1].to_string(),
                    pk: parts[2].to_string(),
                })
            }
            2 => {
                if parts[0].is_empty() || parts[1].is_empty() {
                    return None;
                }
                Some(Self {
                    namespace: DEFAULT_NAMESPACE.to_string(),
                    class: parts[0].to_string(),
                    pk: parts[1].to_string(),
                })
            }
            _ => None,
        }
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
    fn test_entity_id_new_default_namespace() {
        let id = EntityId::new("Product", "000001");
        assert_eq!(id.namespace(), "default");
        assert_eq!(id.class(), "Product");
        assert_eq!(id.pk(), "000001");
    }

    #[test]
    fn test_entity_id_new_namespaced() {
        let id = EntityId::new_namespaced("tenant_a", "Product", "000001");
        assert_eq!(id.namespace(), "tenant_a");
        assert_eq!(id.class(), "Product");
        assert_eq!(id.pk(), "000001");
    }

    #[test]
    fn test_entity_id_with_namespace() {
        let id = EntityId::new("Product", "001").with_namespace("hr");
        assert_eq!(id.namespace(), "hr");
    }

    #[test]
    fn test_entity_id_to_lsm_key() {
        let id = EntityId::new("Product", "000001");
        assert_eq!(id.to_lsm_key(), b"default::Product::000001");

        let id = EntityId::new("Product", "000001").with_namespace("tenant_a");
        assert_eq!(id.to_lsm_key(), b"tenant_a::Product::000001");
    }

    #[test]
    fn test_entity_id_to_vertex_id() {
        let id = EntityId::new("Employee", "alice").with_namespace("hr");
        // vertex_id 不含 namespace（图遍历时命名空间无关）
        assert_eq!(id.to_vertex_id(), "Employee::alice");
    }

    #[test]
    fn test_entity_id_from_lsm_key_three_part() {
        let id = EntityId::from_lsm_key(b"tenant_a::Product::000001").unwrap();
        assert_eq!(id.namespace(), "tenant_a");
        assert_eq!(id.class(), "Product");
        assert_eq!(id.pk(), "000001");
    }

    #[test]
    fn test_entity_id_from_lsm_key_two_part() {
        let id = EntityId::from_lsm_key(b"Product::000001").unwrap();
        assert_eq!(id.namespace(), "default");
        assert_eq!(id.class(), "Product");
        assert_eq!(id.pk(), "000001");
    }

    #[test]
    fn test_entity_id_from_str_three_part() {
        let id = EntityId::from_str("ns::Product::001").unwrap();
        assert_eq!(id.namespace(), "ns");
    }

    #[test]
    fn test_entity_id_from_str_two_part() {
        let id = EntityId::from_str("Product::001").unwrap();
        assert_eq!(id.namespace(), "default");
    }

    #[test]
    fn test_entity_id_from_str_no_separator() {
        assert!(EntityId::from_str("no_separator").is_none());
    }

    #[test]
    fn test_entity_id_from_str_empty_parts() {
        assert!(EntityId::from_str("::key").is_none());
        assert!(EntityId::from_str("Class::").is_none());
        assert!(EntityId::from_str("ns::Class::").is_none());
        assert!(EntityId::from_str("::Class::key").is_none());
    }

    #[test]
    fn test_entity_id_is_class() {
        let id = EntityId::new("Product", "001");
        assert!(id.is_class("Product"));
        assert!(!id.is_class("Employee"));
    }

    #[test]
    fn test_entity_id_class_prefix() {
        let id = EntityId::new("Product", "001").with_namespace("tenant_a");
        assert_eq!(id.class_prefix(), b"tenant_a::Product::");
    }

    #[test]
    fn test_entity_id_namespace_prefix() {
        let id = EntityId::new("Product", "001").with_namespace("tenant_a");
        assert_eq!(id.namespace_prefix(), b"tenant_a::");
    }

    #[test]
    fn test_entity_id_display() {
        let id = EntityId::new("Product", "001").with_namespace("ns");
        assert_eq!(format!("{}", id), "ns::Product::001");
    }

    #[test]
    fn test_entity_id_equality() {
        let a = EntityId::new("Product", "001").with_namespace("ns");
        let b = EntityId::new("Product", "001").with_namespace("ns");
        let c = EntityId::new("Product", "001").with_namespace("other");
        assert_eq!(a, b);
        assert_ne!(a, c); // 不同 namespace 不相等
    }

    #[test]
    fn test_entity_id_lsm_key_roundtrip() {
        let original = EntityId::new("Category", "electronics").with_namespace("shop");
        let key = original.to_lsm_key();
        let restored = EntityId::from_lsm_key(&key).unwrap();
        assert_eq!(original, restored);
    }

    #[test]
    fn test_entity_id_backward_compat_two_part_roundtrip() {
        let id = EntityId::from_lsm_key(b"Product::001").unwrap();
        let key = id.to_lsm_key();
        // 两段式输入，三段式输出（加了 default:: 前缀）
        assert_eq!(key, b"default::Product::001");
    }

    #[test]
    fn test_entity_id_with_special_chars() {
        let id = EntityId::new("My-Class", "key/with/slashes").with_namespace("my-ns");
        assert_eq!(id.to_lsm_key(), b"my-ns::My-Class::key/with/slashes");
        assert_eq!(id.to_vertex_id(), "My-Class::key/with/slashes");
    }

    #[test]
    fn test_entity_id_clone() {
        let id = EntityId::new("Product", "001").with_namespace("ns");
        let cloned = id.clone();
        assert_eq!(id, cloned);
    }
}
