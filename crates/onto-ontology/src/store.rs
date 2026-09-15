//! Ontology store: persistence layer for ontologies.
//!
//! Stores ontology definitions in the storage engine with namespace support.

use crate::model::{Namespace, Ontology};
use onto_core::Result;
use onto_storage::LsmEngine;
use std::sync::Arc;

/// Prefix for ontology keys in the storage engine.
const ONTOLOGY_PREFIX: &[u8] = b"__ontology__";
/// Prefix for namespace keys in the storage engine.
const NAMESPACE_PREFIX: &[u8] = b"__ns__";
/// Default namespace for backward compatibility.
pub const DEFAULT_NAMESPACE: &str = "_default";

/// Stores and retrieves ontologies from the storage engine.
pub struct OntologyStore {
    engine: Arc<LsmEngine>,
}

impl OntologyStore {
    pub fn new(engine: Arc<LsmEngine>) -> Self {
        Self { engine }
    }

    /// Saves an ontology to the storage engine.
    pub fn save(&self, ontology: &Ontology) -> Result<()> {
        let key = Self::make_ontology_key(ontology.namespace.as_deref(), &ontology.name);
        let value = serde_json::to_vec(ontology)
            .map_err(|e| onto_core::CoreError::Serialization(e.to_string()))?;
        self.engine.put(key, value)
    }

    /// Saves an ontology using an already-acquired engine reference.
    pub fn save_with_engine(&self, engine: &LsmEngine, ontology: &Ontology) -> Result<()> {
        let key = Self::make_ontology_key(ontology.namespace.as_deref(), &ontology.name);
        let value = serde_json::to_vec(ontology)
            .map_err(|e| onto_core::CoreError::Serialization(e.to_string()))?;
        engine.put(key, value)
    }

    /// Loads an ontology by name within a namespace.
    pub fn load(&self, namespace: Option<&str>, name: &str) -> Result<Option<Ontology>> {
        let key = Self::make_ontology_key(namespace, name);
        match self.engine.get(&key)? {
            Some(bytes) => {
                let ontology = Ontology::from_json_slice(&bytes)
                    .map_err(|e| onto_core::CoreError::Serialization(e.to_string()))?;
                Ok(Some(ontology))
            }
            None => Ok(None),
        }
    }

    /// Loads an ontology using an already-acquired engine reference.
    pub fn load_with_engine(
        &self,
        engine: &LsmEngine,
        namespace: Option<&str>,
        name: &str,
    ) -> Result<Option<Ontology>> {
        let key = Self::make_ontology_key(namespace, name);
        match engine.get(&key)? {
            Some(bytes) => {
                let ontology = Ontology::from_json_slice(&bytes)
                    .map_err(|e| onto_core::CoreError::Serialization(e.to_string()))?;
                Ok(Some(ontology))
            }
            None => Ok(None),
        }
    }

    /// Deletes an ontology by name within a namespace.
    pub fn delete(&self, namespace: Option<&str>, name: &str) -> Result<()> {
        let key = Self::make_ontology_key(namespace, name);
        self.engine.delete(key)
    }

    /// Saves a namespace to the storage engine.
    pub fn save_namespace(&self, namespace: &Namespace) -> Result<()> {
        let key = Self::make_namespace_key(&namespace.name);
        let value = serde_json::to_vec(namespace)
            .map_err(|e| onto_core::CoreError::Serialization(e.to_string()))?;
        self.engine.put(key, value)
    }

    /// Loads a namespace by name.
    pub fn load_namespace(&self, name: &str) -> Result<Option<Namespace>> {
        let key = Self::make_namespace_key(name);
        match self.engine.get(&key)? {
            Some(bytes) => {
                let namespace = serde_json::from_slice(&bytes)
                    .map_err(|e| onto_core::CoreError::Serialization(e.to_string()))?;
                Ok(Some(namespace))
            }
            None => Ok(None),
        }
    }

    /// Deletes a namespace by name.
    pub fn delete_namespace(&self, name: &str) -> Result<()> {
        let key = Self::make_namespace_key(name);
        self.engine.delete(key)
    }

    /// Lists all namespaces.
    pub fn list_namespaces(&self) -> Result<Vec<Namespace>> {
        let entries = self.engine.scan_prefix(NAMESPACE_PREFIX)?;
        let mut namespaces = Vec::new();
        for (_key, val_bytes) in entries {
            if let Ok(namespace) = serde_json::from_slice::<Namespace>(&val_bytes) {
                namespaces.push(namespace);
            }
        }
        Ok(namespaces)
    }

    /// Lists all ontologies in a namespace.
    pub fn list_ontologies_in_namespace(&self, namespace: &str) -> Result<Vec<Ontology>> {
        let prefix = Self::make_ontology_prefix(Some(namespace));
        let entries = self.engine.scan_prefix(&prefix)?;
        let mut ontologies = Vec::new();
        for (_key, val_bytes) in entries {
            if let Ok(ontology) = Ontology::from_json_slice(&val_bytes) {
                ontologies.push(ontology);
            }
        }
        Ok(ontologies)
    }

    /// Finds the ontology that contains the given class name within a namespace.
    pub fn find_ontology_for_class(
        &self,
        engine: &LsmEngine,
        namespace: Option<&str>,
        class_name: &str,
    ) -> Result<Option<Ontology>> {
        let prefix = Self::make_ontology_prefix(namespace);
        let entries = engine.scan_prefix(&prefix)?;
        for (_key, val_bytes) in entries {
            if let Ok(ontology) = Ontology::from_json_slice(&val_bytes) {
                if ontology.classes.contains_key(class_name) {
                    return Ok(Some(ontology));
                }
            }
        }
        Ok(None)
    }

    /// Finds the ontology that contains the given class name across all namespaces.
    pub fn find_ontology_for_class_global(
        &self,
        engine: &LsmEngine,
        class_name: &str,
    ) -> Result<Option<Ontology>> {
        let entries = engine.scan_prefix(ONTOLOGY_PREFIX)?;
        for (_key, val_bytes) in entries {
            if let Ok(ontology) = Ontology::from_json_slice(&val_bytes) {
                if ontology.classes.contains_key(class_name) {
                    return Ok(Some(ontology));
                }
            }
        }
        Ok(None)
    }

    /// Merges all ontologies in a namespace into a single ontology for inheritance resolution.
    pub fn merge_namespace_ontologies(
        &self,
        engine: &LsmEngine,
        namespace: &str,
    ) -> Result<Ontology> {
        let mut merged =
            Ontology::new(format!("__merged_{}__", namespace)).with_namespace(namespace);
        let prefix = Self::make_ontology_prefix(Some(namespace));
        let entries = engine.scan_prefix(&prefix)?;
        for (_key, val_bytes) in entries {
            if let Ok(ontology) = Ontology::from_json_slice(&val_bytes) {
                for (name, class) in &ontology.classes {
                    if !merged.classes.contains_key(name) {
                        merged.classes.insert(name.clone(), class.clone());
                    }
                }
                for (name, prop) in &ontology.properties {
                    if !merged.properties.contains_key(name) {
                        merged.properties.insert(name.clone(), prop.clone());
                    }
                }
            }
        }
        merged.rebuild_indexes();
        Ok(merged)
    }

    /// Makes a key for an ontology, optionally namespaced.
    fn make_ontology_key(namespace: Option<&str>, name: &str) -> Vec<u8> {
        let prefix = Self::make_ontology_prefix(namespace);
        let mut key = Vec::with_capacity(prefix.len() + name.len());
        key.extend_from_slice(&prefix);
        key.extend_from_slice(name.as_bytes());
        key
    }

    /// Makes the prefix for ontology keys in a namespace.
    fn make_ontology_prefix(namespace: Option<&str>) -> Vec<u8> {
        match namespace {
            Some(ns) => {
                let mut prefix = Vec::with_capacity(ONTOLOGY_PREFIX.len() + ns.len() + 2);
                prefix.extend_from_slice(ONTOLOGY_PREFIX);
                prefix.extend_from_slice(ns.as_bytes());
                prefix.extend_from_slice(b"::");
                prefix
            }
            None => ONTOLOGY_PREFIX.to_vec(),
        }
    }

    /// Makes a key for a namespace.
    fn make_namespace_key(name: &str) -> Vec<u8> {
        let mut key = Vec::with_capacity(NAMESPACE_PREFIX.len() + name.len());
        key.extend_from_slice(NAMESPACE_PREFIX);
        key.extend_from_slice(name.as_bytes());
        key
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Class, DataType, Ontology, Property};
    use onto_storage::StorageOptions;
    use tempfile::tempdir;

    #[test]
    fn test_ontology_store_save_and_load() {
        let dir = tempdir().unwrap();
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            ..Default::default()
        };
        let engine = Arc::new(LsmEngine::open(options).unwrap());
        let store = OntologyStore::new(engine);

        let mut onto = Ontology::new("test");
        onto.add_class(Class::new("Person"));
        onto.add_property(Property::new("name", "Person", DataType::String));

        store.save(&onto).unwrap();

        let loaded = store.load(None, "test").unwrap().unwrap();
        assert_eq!(loaded.name, "test");
        assert!(loaded.get_class("Person").is_some());
        assert_eq!(loaded.properties["name"].range, DataType::String);
    }

    #[test]
    fn test_ontology_store_not_found() {
        let dir = tempdir().unwrap();
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            ..Default::default()
        };
        let engine = Arc::new(LsmEngine::open(options).unwrap());
        let store = OntologyStore::new(engine);

        let result = store.load(None, "nonexistent").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_namespace_operations() {
        let dir = tempdir().unwrap();
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            ..Default::default()
        };
        let engine = Arc::new(LsmEngine::open(options).unwrap());
        let store = OntologyStore::new(engine);

        // Create namespace
        let ns = Namespace::new("hr");
        store.save_namespace(&ns).unwrap();

        // Load namespace
        let loaded = store.load_namespace("hr").unwrap().unwrap();
        assert_eq!(loaded.name, "hr");

        // List namespaces
        let namespaces = store.list_namespaces().unwrap();
        assert_eq!(namespaces.len(), 1);
        assert_eq!(namespaces[0].name, "hr");

        // Delete namespace
        store.delete_namespace("hr").unwrap();
        let result = store.load_namespace("hr").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_namespaced_ontology_operations() {
        let dir = tempdir().unwrap();
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            ..Default::default()
        };
        let engine = Arc::new(LsmEngine::open(options).unwrap());
        let store = OntologyStore::new(engine);

        // Create namespaced ontology
        let mut onto = Ontology::new("shop").with_namespace("hr");
        onto.add_class(Class::new("Employee"));
        onto.add_property(Property::new("name", "Employee", DataType::String));

        store.save(&onto).unwrap();

        // Load with namespace
        let loaded = store.load(Some("hr"), "shop").unwrap().unwrap();
        assert_eq!(loaded.name, "shop");
        assert_eq!(loaded.namespace, Some("hr".to_string()));
        assert!(loaded.get_class("Employee").is_some());

        // Should not be found without namespace
        let result = store.load(None, "shop").unwrap();
        assert!(result.is_none());

        // List ontologies in namespace
        let ontologies = store.list_ontologies_in_namespace("hr").unwrap();
        assert_eq!(ontologies.len(), 1);
        assert_eq!(ontologies[0].name, "shop");
    }

    #[test]
    fn test_merge_namespace_ontologies() {
        let dir = tempdir().unwrap();
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            ..Default::default()
        };
        let engine = Arc::new(LsmEngine::open(options).unwrap());
        let store = OntologyStore::new(engine.clone());

        // Create multiple ontologies in same namespace
        let mut onto1 = Ontology::new("base").with_namespace("hr");
        onto1.add_class(Class::new("Person"));
        onto1.add_property(Property::new("name", "Person", DataType::String));
        store.save(&onto1).unwrap();

        let mut onto2 = Ontology::new("extended").with_namespace("hr");
        onto2.add_class(Class::new("Employee").with_superclass("Person"));
        onto2.add_property(Property::new("salary", "Employee", DataType::Float64));
        store.save(&onto2).unwrap();

        // Merge should combine both
        let merged = store.merge_namespace_ontologies(&engine, "hr").unwrap();
        assert!(merged.get_class("Person").is_some());
        assert!(merged.get_class("Employee").is_some());
        assert!(merged.is_subclass_of("Employee", "Person"));
    }
}
