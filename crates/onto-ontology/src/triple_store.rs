// Copyright (c) 2024-2026 Guo Jianzhu (郭建筑)
// Licensed under the Apache License, Version 2.0.
// See LICENSE for details.
//! Persistent triple store with SPO/POS/OSP indexes.
//!
//! Stores RDF-style triples (subject, predicate, object) in the LSM engine
//! with three auxiliary indexes for efficient lookup by any combination of
//! S, P, O.
//!
//! Key formats:
//! - SPO: `__triple__{S}__{P}__{O}` → primary storage
//! - POS: `__triple_pos__{P}__{O}__{S}` → lookup by predicate+object
//! - OSP: `__triple_osp__{O}__{S}__{P}` → lookup by object+subject
//!
//! Triple keys are routed to the namespace engine that owns the subject's
//! class, so each namespace's triples live in its own `data/{ns}/` directory.

use onto_storage::{EngineSet, LsmEngine};
use std::sync::Arc;

/// Prefix for SPO triple index in LSM.
const PREFIX_SPO: &[u8] = b"__triple__";

/// A persistent triple store backed by the engine set.
///
/// Provides efficient lookup by any combination of subject, predicate, object.
/// Writes are routed to the namespace engine that owns the subject.
pub struct TripleStore {
    engine_set: Arc<EngineSet>,
}

/// A stored triple with its components.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Triple {
    pub subject: String,
    pub predicate: String,
    pub object: String,
}

impl Triple {
    pub fn new(
        subject: impl Into<String>,
        predicate: impl Into<String>,
        object: impl Into<String>,
    ) -> Self {
        Self {
            subject: subject.into(),
            predicate: predicate.into(),
            object: object.into(),
        }
    }
}

impl TripleStore {
    /// Create a new triple store backed by the given engine set.
    pub fn new(engine_set: Arc<EngineSet>) -> Self {
        Self { engine_set }
    }

    /// Resolve the engine that owns `subject`'s namespace.
    ///
    /// Document keys look like `sempet.Drug::carprofen` — extract the class
    /// before `::`, then the namespace before the first `.`. Bare subjects
    /// (no dot in the class region) fall back to the default engine.
    fn engine_for_subject(&self, subject: &str) -> Arc<LsmEngine> {
        let class = subject.split("::").next().unwrap_or(subject);
        self.engine_set.engine_for_class(class)
    }

    /// Persist a triple with all three indexes.
    ///
    /// This is idempotent — storing the same triple twice is a no-op.
    /// All three index keys are written to the subject's namespace engine.
    pub fn add_triple(&self, subject: &str, predicate: &str, object: &str) -> Result<(), String> {
        let engine = self.engine_for_subject(subject);
        // SPO index (primary)
        let spo_key = Self::make_spo_key(subject, predicate, object);
        engine.put(spo_key, vec![]).map_err(|e| e.to_string())?;

        // POS index
        let pos_key = Self::make_pos_key(predicate, object, subject);
        engine.put(pos_key, vec![]).map_err(|e| e.to_string())?;

        // OSP index
        let osp_key = Self::make_osp_key(object, subject, predicate);
        engine.put(osp_key, vec![]).map_err(|e| e.to_string())?;

        Ok(())
    }

    /// Remove a triple from all three indexes.
    pub fn remove_triple(
        &self,
        subject: &str,
        predicate: &str,
        object: &str,
    ) -> Result<(), String> {
        let engine = self.engine_for_subject(subject);
        let spo_key = Self::make_spo_key(subject, predicate, object);
        engine.delete(spo_key).map_err(|e| e.to_string())?;

        let pos_key = Self::make_pos_key(predicate, object, subject);
        engine.delete(pos_key).map_err(|e| e.to_string())?;

        let osp_key = Self::make_osp_key(object, subject, predicate);
        engine.delete(osp_key).map_err(|e| e.to_string())?;

        Ok(())
    }

    /// SPO query: given subject and predicate, find all objects.
    pub fn lookup_spo(&self, subject: &str, predicate: &str) -> Result<Vec<String>, String> {
        let engine = self.engine_for_subject(subject);
        let prefix = format!(
            "__triple__{}__{}__",
            Self::escape(subject),
            Self::escape(predicate)
        );
        let results = engine
            .scan_prefix(prefix.as_bytes())
            .map_err(|e| e.to_string())?;

        Ok(results
            .into_iter()
            .filter_map(|(key, _)| {
                let s = std::str::from_utf8(&key).ok()?;
                Self::extract_third_component(s)
            })
            .collect())
    }

    /// SPO query: given subject, find all predicate-object pairs.
    pub fn lookup_s(&self, subject: &str) -> Result<Vec<(String, String)>, String> {
        let engine = self.engine_for_subject(subject);
        let prefix = format!("__triple__{}__", Self::escape(subject));
        let results = engine
            .scan_prefix(prefix.as_bytes())
            .map_err(|e| e.to_string())?;

        Ok(results
            .into_iter()
            .filter_map(|(key, _)| {
                let s = std::str::from_utf8(&key).ok()?;
                Self::extract_last_two_components(s)
            })
            .collect())
    }

    /// POS query: given predicate and object, find all subjects.
    ///
    /// Scans all namespace engines (no subject anchor to route on).
    pub fn lookup_pos(&self, predicate: &str, object: &str) -> Result<Vec<String>, String> {
        let prefix = format!(
            "__triple_pos__{}__{}__",
            Self::escape(predicate),
            Self::escape(object)
        );
        let mut out = Vec::new();
        for engine in self.engine_set.all_engines() {
            let results = engine
                .scan_prefix(prefix.as_bytes())
                .map_err(|e| e.to_string())?;
            for (key, _) in results {
                if let Some(s) = std::str::from_utf8(&key).ok() {
                    if let Some(v) = Self::extract_third_component(s) {
                        out.push(v);
                    }
                }
            }
        }
        Ok(out)
    }

    /// POS query: given predicate, find all subject-object pairs.
    ///
    /// Scans all namespace engines.
    pub fn lookup_p(&self, predicate: &str) -> Result<Vec<(String, String)>, String> {
        let prefix = format!("__triple_pos__{}__", Self::escape(predicate));
        let mut out = Vec::new();
        for engine in self.engine_set.all_engines() {
            let results = engine
                .scan_prefix(prefix.as_bytes())
                .map_err(|e| e.to_string())?;
            for (key, _) in results {
                if let Some(s) = std::str::from_utf8(&key).ok() {
                    if let Some(pair) = Self::extract_last_two_components(s) {
                        out.push(pair);
                    }
                }
            }
        }
        Ok(out)
    }

    /// OSP query: given object and subject, find all predicates.
    pub fn lookup_osp(&self, object: &str, subject: &str) -> Result<Vec<String>, String> {
        let engine = self.engine_for_subject(subject);
        let prefix = format!(
            "__triple_osp__{}__{}__",
            Self::escape(object),
            Self::escape(subject)
        );
        let results = engine
            .scan_prefix(prefix.as_bytes())
            .map_err(|e| e.to_string())?;

        Ok(results
            .into_iter()
            .filter_map(|(key, _)| {
                let s = std::str::from_utf8(&key).ok()?;
                Self::extract_third_component(s)
            })
            .collect())
    }

    /// OSP query: given object, find all subject-predicate pairs.
    ///
    /// Scans all namespace engines.
    pub fn lookup_o(&self, object: &str) -> Result<Vec<(String, String)>, String> {
        let prefix = format!("__triple_osp__{}__", Self::escape(object));
        let mut out = Vec::new();
        for engine in self.engine_set.all_engines() {
            let results = engine
                .scan_prefix(prefix.as_bytes())
                .map_err(|e| e.to_string())?;
            for (key, _) in results {
                if let Some(s) = std::str::from_utf8(&key).ok() {
                    if let Some(pair) = Self::extract_last_two_components(s) {
                        out.push(pair);
                    }
                }
            }
        }
        Ok(out)
    }

    /// Check if a triple exists.
    pub fn contains(&self, subject: &str, predicate: &str, object: &str) -> Result<bool, String> {
        let engine = self.engine_for_subject(subject);
        let key = Self::make_spo_key(subject, predicate, object);
        let result = engine.get(&key).map_err(|e| e.to_string())?;
        Ok(result.is_some())
    }

    /// Get all triples (SPO scan).
    ///
    /// Scans all namespace engines.
    pub fn get_all_triples(&self) -> Result<Vec<Triple>, String> {
        let mut out = Vec::new();
        for engine in self.engine_set.all_engines() {
            let results = engine
                .scan_prefix(PREFIX_SPO)
                .map_err(|e| e.to_string())?;
            for (key, _) in results {
                if let Some(s) = std::str::from_utf8(&key).ok() {
                    if let Some(parts) = Self::parse_spo_key(s) {
                        out.push(Triple::new(parts.0, parts.1, parts.2));
                    }
                }
            }
        }
        Ok(out)
    }

    /// Get all triples for a given subject.
    pub fn get_triples_by_subject(&self, subject: &str) -> Result<Vec<Triple>, String> {
        let pairs = self.lookup_s(subject)?;
        Ok(pairs
            .into_iter()
            .map(|(p, o)| Triple::new(subject, p, o))
            .collect())
    }

    /// Batch add multiple triples.
    ///
    /// Groups entries by subject-namespace engine and writes each group
    /// to its own engine.
    pub fn add_triples(&self, triples: &[(String, String, String)]) -> Result<usize, String> {
        // Group keys by target engine (routed by subject).
        let mut groups: Vec<(Arc<LsmEngine>, Vec<(Vec<u8>, Vec<u8>)>)> = Vec::new();
        for (s, p, o) in triples {
            let engine = self.engine_for_subject(s);
            let entries = [
                (Self::make_spo_key(s, p, o), vec![]),
                (Self::make_pos_key(p, o, s), vec![]),
                (Self::make_osp_key(o, s, p), vec![]),
            ];
            match groups.iter_mut().find(|(e, _)| Arc::ptr_eq(e, &engine)) {
                Some((_, list)) => list.extend(entries),
                None => groups.push((engine, entries.to_vec())),
            }
        }
        let mut count = 0usize;
        for (engine, list) in groups {
            count += engine.put_batch(list).map_err(|e| e.to_string())?;
        }
        Ok(count / 3)
    }

    // ── Key construction ──

    fn make_spo_key(subject: &str, predicate: &str, object: &str) -> Vec<u8> {
        format!(
            "__triple__{}__{}__{}",
            Self::escape(subject),
            Self::escape(predicate),
            Self::escape(object)
        )
        .into_bytes()
    }

    fn make_pos_key(predicate: &str, object: &str, subject: &str) -> Vec<u8> {
        format!(
            "__triple_pos__{}__{}__{}",
            Self::escape(predicate),
            Self::escape(object),
            Self::escape(subject)
        )
        .into_bytes()
    }

    fn make_osp_key(object: &str, subject: &str, predicate: &str) -> Vec<u8> {
        format!(
            "__triple_osp__{}__{}__{}",
            Self::escape(object),
            Self::escape(subject),
            Self::escape(predicate)
        )
        .into_bytes()
    }

    /// Escape special characters in triple components to avoid key collisions.
    fn escape(s: &str) -> String {
        s.replace('\\', "\\\\").replace('_', "\\_")
    }

    /// Unescape triple component.
    fn unescape(s: &str) -> String {
        s.replace("\\\\", "\\").replace("\\_", "_")
    }

    /// Extract the third component from a key like `__triple__A__B__C`.
    fn extract_third_component(key: &str) -> Option<String> {
        // Skip prefix, then find the third component
        let after_prefix = if key.starts_with("__triple_pos__") {
            &key[14..]
        } else if key.starts_with("__triple_osp__") {
            &key[14..]
        } else if key.starts_with("__triple__") {
            &key[10..]
        } else {
            return None;
        };

        // Split by unescaped __
        let parts: Vec<&str> = Self::split_key_parts(after_prefix);
        if parts.len() >= 3 {
            Some(Self::unescape(parts[2]))
        } else {
            None
        }
    }

    /// Extract the last two components from a key.
    fn extract_last_two_components(key: &str) -> Option<(String, String)> {
        let after_prefix = if key.starts_with("__triple_pos__") {
            &key[14..]
        } else if key.starts_with("__triple_osp__") {
            &key[14..]
        } else if key.starts_with("__triple__") {
            &key[10..]
        } else {
            return None;
        };

        let parts: Vec<&str> = Self::split_key_parts(after_prefix);
        if parts.len() >= 3 {
            Some((Self::unescape(parts[1]), Self::unescape(parts[2])))
        } else {
            None
        }
    }

    /// Parse an SPO key into (subject, predicate, object).
    fn parse_spo_key(key: &str) -> Option<(&str, &str, &str)> {
        let after_prefix = key.strip_prefix("__triple__")?;
        let parts: Vec<&str> = Self::split_key_parts(after_prefix);
        if parts.len() >= 3 {
            Some((parts[0], parts[1], parts[2]))
        } else {
            None
        }
    }

    /// Split key parts by unescaped `__` separators.
    fn split_key_parts(s: &str) -> Vec<&str> {
        let mut parts = Vec::new();
        let mut start = 0;
        let bytes = s.as_bytes();
        let mut i = 0;

        while i < bytes.len() {
            if i + 1 < bytes.len() && bytes[i] == b'_' && bytes[i + 1] == b'_' {
                // Check if escaped
                if i > 0 && bytes[i - 1] == b'\\' {
                    i += 2;
                    continue;
                }
                parts.push(&s[start..i]);
                start = i + 2;
                i += 2;
            } else {
                i += 1;
            }
        }
        // Last part
        if start < s.len() {
            parts.push(&s[start..]);
        }

        parts
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use onto_storage::{EngineSet, StorageOptions};
    use tempfile::TempDir;

    fn create_test_store() -> (TripleStore, TempDir) {
        let dir = TempDir::new().unwrap();
        let options = StorageOptions {
            data_dir: dir.path().to_path_buf(),
            memtable_size_limit: 4 * 1024 * 1024,
            ..Default::default()
        };
        let engine_set = Arc::new(EngineSet::open_with_discovery(options).unwrap());
        let store = TripleStore::new(engine_set);
        (store, dir)
    }

    #[test]
    fn test_add_and_lookup_spo() {
        let (store, _dir) = create_test_store();

        store.add_triple("Alice", "knows", "Bob").unwrap();
        store.add_triple("Alice", "knows", "Charlie").unwrap();
        store.add_triple("Alice", "age", "30").unwrap();

        let friends = store.lookup_spo("Alice", "knows").unwrap();
        assert_eq!(friends.len(), 2);
        assert!(friends.contains(&"Bob".to_string()));
        assert!(friends.contains(&"Charlie".to_string()));

        let ages = store.lookup_spo("Alice", "age").unwrap();
        assert_eq!(ages, vec!["30"]);
    }

    #[test]
    fn test_lookup_pos() {
        let (store, _dir) = create_test_store();

        store.add_triple("Alice", "knows", "Bob").unwrap();
        store.add_triple("Charlie", "knows", "Bob").unwrap();

        let knowers = store.lookup_pos("knows", "Bob").unwrap();
        assert_eq!(knowers.len(), 2);
        assert!(knowers.contains(&"Alice".to_string()));
        assert!(knowers.contains(&"Charlie".to_string()));
    }

    #[test]
    fn test_lookup_osp() {
        let (store, _dir) = create_test_store();

        store.add_triple("Alice", "knows", "Bob").unwrap();
        store.add_triple("Alice", "age", "30").unwrap();

        let predicates = store.lookup_osp("Bob", "Alice").unwrap();
        assert_eq!(predicates, vec!["knows"]);
    }

    #[test]
    fn test_lookup_s() {
        let (store, _dir) = create_test_store();

        store.add_triple("Alice", "knows", "Bob").unwrap();
        store.add_triple("Alice", "age", "30").unwrap();

        let pairs = store.lookup_s("Alice").unwrap();
        assert_eq!(pairs.len(), 2);
    }

    #[test]
    fn test_lookup_p() {
        let (store, _dir) = create_test_store();

        store.add_triple("Alice", "knows", "Bob").unwrap();
        store.add_triple("Charlie", "knows", "Dave").unwrap();

        let pairs = store.lookup_p("knows").unwrap();
        assert_eq!(pairs.len(), 2);
    }

    #[test]
    fn test_lookup_o() {
        let (store, _dir) = create_test_store();

        store.add_triple("Alice", "knows", "Bob").unwrap();
        store.add_triple("Charlie", "likes", "Bob").unwrap();

        let pairs = store.lookup_o("Bob").unwrap();
        assert_eq!(pairs.len(), 2);
    }

    #[test]
    fn test_contains() {
        let (store, _dir) = create_test_store();

        store.add_triple("Alice", "knows", "Bob").unwrap();

        assert!(store.contains("Alice", "knows", "Bob").unwrap());
        assert!(!store.contains("Alice", "knows", "Charlie").unwrap());
        assert!(!store.contains("Bob", "knows", "Alice").unwrap());
    }

    #[test]
    fn test_remove_triple() {
        let (store, _dir) = create_test_store();

        store.add_triple("Alice", "knows", "Bob").unwrap();
        assert!(store.contains("Alice", "knows", "Bob").unwrap());

        store.remove_triple("Alice", "knows", "Bob").unwrap();
        assert!(!store.contains("Alice", "knows", "Bob").unwrap());
    }

    #[test]
    fn test_get_all_triples() {
        let (store, _dir) = create_test_store();

        store.add_triple("Alice", "knows", "Bob").unwrap();
        store.add_triple("Alice", "age", "30").unwrap();
        store.add_triple("Bob", "age", "25").unwrap();

        let all = store.get_all_triples().unwrap();
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn test_get_triples_by_subject() {
        let (store, _dir) = create_test_store();

        store.add_triple("Alice", "knows", "Bob").unwrap();
        store.add_triple("Alice", "age", "30").unwrap();
        store.add_triple("Bob", "age", "25").unwrap();

        let alice_triples = store.get_triples_by_subject("Alice").unwrap();
        assert_eq!(alice_triples.len(), 2);

        let bob_triples = store.get_triples_by_subject("Bob").unwrap();
        assert_eq!(bob_triples.len(), 1);
    }

    #[test]
    fn test_batch_add() {
        let (store, _dir) = create_test_store();

        let triples = vec![
            ("Alice".to_string(), "knows".to_string(), "Bob".to_string()),
            ("Alice".to_string(), "age".to_string(), "30".to_string()),
            ("Bob".to_string(), "age".to_string(), "25".to_string()),
        ];

        let count = store.add_triples(&triples).unwrap();
        assert_eq!(count, 3);

        let all = store.get_all_triples().unwrap();
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn test_idempotent_add() {
        let (store, _dir) = create_test_store();

        store.add_triple("Alice", "knows", "Bob").unwrap();
        store.add_triple("Alice", "knows", "Bob").unwrap(); // duplicate

        let friends = store.lookup_spo("Alice", "knows").unwrap();
        assert_eq!(friends.len(), 1); // should not duplicate
    }

    #[test]
    fn test_special_characters() {
        let (store, _dir) = create_test_store();

        store
            .add_triple("Product::001", "rdf:type", "Product")
            .unwrap();
        store
            .add_triple("http://example.org/Person", "name", "Alice")
            .unwrap();

        let types = store.lookup_spo("Product::001", "rdf:type").unwrap();
        assert_eq!(types, vec!["Product"]);

        let names = store
            .lookup_spo("http://example.org/Person", "name")
            .unwrap();
        assert_eq!(names, vec!["Alice"]);
    }
}
