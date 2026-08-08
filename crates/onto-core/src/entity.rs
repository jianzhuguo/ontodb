//! Unified entity identity for OntoDB's multi-modal storage.
//!
//! `EntityId` is the semantic anchor point shared by all three data modalities:
//! - **Relational**: LSM key `{class}::{pk}`
//! - **Graph**: vertex ID `{class}::{pk}`
//! - **Vector**: document key (same as LSM key)
//!
//! All modalities reference the same entity through this unified identifier.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Unified entity identifier — the semantic anchor point for all data modalities.
///
/// An `EntityId` uniquely identifies a logical entity in the database.
/// The same `EntityId` is used as:
/// - LSM storage key (via `to_lsm_key()`)
/// - Graph vertex ID (via `to_vertex_id()`)
/// - Vector index document key (via `to_lsm_key()`)
/// - Triple store subject/object (via `to_vertex_id()`)
///
/// # Examples
///
/// ```
/// use onto_core::EntityId;
///
/// let id = EntityId::new("Product", "000001");
/// assert_eq!(id.to_lsm_key(), b"Product::000001");
/// assert_eq!(id.to_vertex_id(), "Product::000001");
/// assert_eq!(id.class(), "Product");
/// assert_eq!(id.pk(), "000001");
/// ```
#[derive(Debug, Clone, Hash, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct EntityId {
    /// Class name (e.g., "Product", "Employee").
    class: String,
    /// Primary key (e.g., "000001", "uuid-xxx").
    pk: String,
}

impl EntityId {
    /// Creates a new entity identifier.
    pub fn new(class: impl Into<String>, pk: impl Into<String>) -> Self {
        Self {
            class: class.into(),
            pk: pk.into(),
        }
    }

    /// Returns the class name.
    pub fn class(&self) -> &str {
        &self.class
    }

    /// Returns the primary key.
    pub fn pk(&self) -> &str {
        &self.pk
    }

    /// Converts to LSM storage key bytes — shared by relational and vector modalities.
    ///
    /// Format: `{class}::{pk}`
    pub fn to_lsm_key(&self) -> Vec<u8> {
        format!("{}::{}", self.class, self.pk).into_bytes()
    }

    /// Converts to graph vertex ID string — shared by graph and triple modalities.
    ///
    /// Format: `{class}::{pk}`
    pub fn to_vertex_id(&self) -> String {
        format!("{}::{}", self.class, self.pk)
    }

    /// Parses an `EntityId` from LSM key bytes.
    ///
    /// Returns `None` if the key doesn't contain `::` separator.
    pub fn from_lsm_key(key: &[u8]) -> Option<Self> {
        let s = std::str::from_utf8(key).ok()?;
        Self::from_str(s)
    }

    /// Parses an `EntityId` from a string like `Product::000001`.
    ///
    /// Returns `None` if the string doesn't contain `::` separator,
    /// or if either class or pk is empty.
    pub fn from_str(s: &str) -> Option<Self> {
        let (class, pk) = s.split_once("::")?;
        if class.is_empty() || pk.is_empty() {
            return None;
        }
        Some(Self {
            class: class.to_string(),
            pk: pk.to_string(),
        })
    }

    /// Returns true if this entity belongs to the given class or any of its subclasses.
    ///
    /// Note: actual subclass checking requires the ontology; this only checks exact match.
    /// Use the Reasoner for hierarchy-aware checks.
    pub fn is_class(&self, class: &str) -> bool {
        self.class == class
    }

    /// Returns the key prefix for scanning all entities of this class.
    ///
    /// Format: `{class}::`
    pub fn class_prefix(&self) -> Vec<u8> {
        format!("{}::", self.class).into_bytes()
    }
}

impl fmt::Display for EntityId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}::{}", self.class, self.pk)
    }
}

impl AsRef<str> for EntityId {
    fn as_ref(&self) -> &str {
        // We can't return a reference to a temporary string,
        // so this is intentionally not implemented.
        // Use to_string() or to_vertex_id() instead.
        unreachable!("EntityId cannot be borrowed as a single &str due to composite format")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_entity_id_new() {
        let id = EntityId::new("Product", "000001");
        assert_eq!(id.class(), "Product");
        assert_eq!(id.pk(), "000001");
    }

    #[test]
    fn test_entity_id_to_lsm_key() {
        let id = EntityId::new("Product", "000001");
        assert_eq!(id.to_lsm_key(), b"Product::000001");
    }

    #[test]
    fn test_entity_id_to_vertex_id() {
        let id = EntityId::new("Employee", "alice");
        assert_eq!(id.to_vertex_id(), "Employee::alice");
    }

    #[test]
    fn test_entity_id_from_lsm_key() {
        let id = EntityId::from_lsm_key(b"Product::000001").unwrap();
        assert_eq!(id.class(), "Product");
        assert_eq!(id.pk(), "000001");
    }

    #[test]
    fn test_entity_id_from_str() {
        let id = EntityId::from_str("Employee::bob").unwrap();
        assert_eq!(id.class(), "Employee");
        assert_eq!(id.pk(), "bob");
    }

    #[test]
    fn test_entity_id_from_str_no_separator() {
        assert!(EntityId::from_str("no_separator").is_none());
    }

    #[test]
    fn test_entity_id_from_str_empty_class() {
        assert!(EntityId::from_str("::key").is_none());
    }

    #[test]
    fn test_entity_id_from_str_empty_pk() {
        assert!(EntityId::from_str("Class::").is_none());
    }

    #[test]
    fn test_entity_id_display() {
        let id = EntityId::new("Product", "000001");
        assert_eq!(format!("{}", id), "Product::000001");
    }

    #[test]
    fn test_entity_id_roundtrip() {
        let original = EntityId::new("Category", "electronics");
        let key = original.to_lsm_key();
        let restored = EntityId::from_lsm_key(&key).unwrap();
        assert_eq!(original, restored);
    }

    #[test]
    fn test_entity_id_is_class() {
        let id = EntityId::new("Product", "001");
        assert!(id.is_class("Product"));
        assert!(!id.is_class("Employee"));
    }

    #[test]
    fn test_entity_id_class_prefix() {
        let id = EntityId::new("Product", "001");
        assert_eq!(id.class_prefix(), b"Product::");
    }

    #[test]
    fn test_entity_id_equality() {
        let a = EntityId::new("Product", "001");
        let b = EntityId::new("Product", "001");
        let c = EntityId::new("Product", "002");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn test_entity_id_clone() {
        let id = EntityId::new("Product", "001");
        let cloned = id.clone();
        assert_eq!(id, cloned);
    }

    #[test]
    fn test_entity_id_with_long_key() {
        let long_pk = "a".repeat(1000);
        let id = EntityId::new("Product", &long_pk);
        let key = id.to_lsm_key();
        let restored = EntityId::from_lsm_key(&key).unwrap();
        assert_eq!(id, restored);
    }

    #[test]
    fn test_entity_id_with_special_chars() {
        let id = EntityId::new("My-Class", "key/with/slashes");
        assert_eq!(id.to_lsm_key(), b"My-Class::key/with/slashes");
        assert_eq!(id.to_vertex_id(), "My-Class::key/with/slashes");
    }
}
