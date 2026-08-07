//! Ontology store: persistence layer for ontologies.
//!
//! Stores ontology definitions in the storage engine.

use crate::model::Ontology;
use onto_core::Result;
use onto_storage::LsmEngine;
use std::sync::Arc;

/// Prefix for ontology keys in the storage engine.
const ONTOLOGY_PREFIX: &[u8] = b"__ontology__";

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
        let key = Self::make_key(&ontology.name);
        let value = serde_json::to_vec(ontology)
            .map_err(|e| onto_core::CoreError::Serialization(e.to_string()))?;
        self.engine.put(key, value)
    }

    /// Saves an ontology using an already-acquired engine reference.
    pub fn save_with_engine(&self, engine: &LsmEngine, ontology: &Ontology) -> Result<()> {
        let key = Self::make_key(&ontology.name);
        let value = serde_json::to_vec(ontology)
            .map_err(|e| onto_core::CoreError::Serialization(e.to_string()))?;
        engine.put(key, value)
    }

    /// Loads an ontology by name.
    pub fn load(&self, name: &str) -> Result<Option<Ontology>> {
        let key = Self::make_key(name);
        match self.engine.get(&key)? {
            Some(bytes) => {
                let ontology: Ontology = serde_json::from_slice(&bytes)
                    .map_err(|e| onto_core::CoreError::Serialization(e.to_string()))?;
                Ok(Some(ontology))
            }
            None => Ok(None),
        }
    }

    /// Loads an ontology using an already-acquired engine reference.
    pub fn load_with_engine(&self, engine: &LsmEngine, name: &str) -> Result<Option<Ontology>> {
        let key = Self::make_key(name);
        match engine.get(&key)? {
            Some(bytes) => {
                let ontology: Ontology = serde_json::from_slice(&bytes)
                    .map_err(|e| onto_core::CoreError::Serialization(e.to_string()))?;
                Ok(Some(ontology))
            }
            None => Ok(None),
        }
    }

    /// Deletes an ontology by name.
    pub fn delete(&self, name: &str) -> Result<()> {
        let key = Self::make_key(name);
        self.engine.delete(key)
    }

    /// Finds the ontology that contains the given class name.
    /// Scans all stored ontologies and returns the first one containing the class.
    pub fn find_ontology_for_class(
        &self,
        engine: &LsmEngine,
        class_name: &str,
    ) -> Result<Option<Ontology>> {
        let entries = engine.scan_prefix(ONTOLOGY_PREFIX)?;
        for (_key, val_bytes) in entries {
            if let Ok(ontology) = serde_json::from_slice::<Ontology>(&val_bytes) {
                if ontology.classes.contains_key(class_name) {
                    return Ok(Some(ontology));
                }
            }
        }
        Ok(None)
    }

    fn make_key(name: &str) -> Vec<u8> {
        let mut key = Vec::with_capacity(ONTOLOGY_PREFIX.len() + name.len());
        key.extend_from_slice(ONTOLOGY_PREFIX);
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

        let loaded = store.load("test").unwrap().unwrap();
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

        let result = store.load("nonexistent").unwrap();
        assert!(result.is_none());
    }
}
