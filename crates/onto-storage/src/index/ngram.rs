// Copyright (c) 2024-2026 Guo Jianzhu (郭建筑)
// Licensed under the Apache License, Version 2.0.
// See LICENSE for details.

//! CJK n-gram substring index.
//!
//! Character-level bigram inverted index: a `contains(needle)` lookup first
//! collects candidate PKs from postings, and the caller performs the exact
//! substring check (n-gram intersection is necessary but not sufficient).
//! Returns `None` for needles shorter than the gram length — callers fall
//! back to a full scan. In-memory only; rebuilt from row data at startup
//! (same lifecycle as the secondary indexes).

use std::collections::HashMap;

use serde_json::Value;

const GRAM_LEN: usize = 2;

/// Single-column n-gram inverted index.
#[derive(Default)]
pub struct NgramIndex {
    /// gram -> primary keys whose value contains the gram
    postings: HashMap<String, Vec<Vec<u8>>>,
}

impl NgramIndex {
    fn tokenize(value: &str) -> Vec<String> {
        let chars: Vec<char> = value.chars().collect();
        if chars.len() < GRAM_LEN {
            return if chars.is_empty() {
                Vec::new()
            } else {
                vec![value.to_string()]
            };
        }
        chars.windows(GRAM_LEN).map(|w| w.iter().collect()).collect()
    }

    pub fn index(&mut self, value: &str, pk: &[u8]) {
        for g in Self::tokenize(value) {
            let list = self.postings.entry(g).or_default();
            if !list.iter().any(|p| p == pk) {
                list.push(pk.to_vec());
            }
        }
    }

    pub fn deindex(&mut self, value: &str, pk: &[u8]) {
        for g in Self::tokenize(value) {
            if let Some(list) = self.postings.get_mut(&g) {
                list.retain(|p| p != pk);
                if list.is_empty() {
                    self.postings.remove(&g);
                }
            }
        }
    }

    /// Candidate PKs for `needle` (intersection pruned from the smallest
    /// posting list). `None` = needle too short, caller should fall back to
    /// a full scan; empty Vec = provably no hit (safe to short-circuit).
    pub fn candidates(&self, needle: &str) -> Option<Vec<Vec<u8>>> {
        // Shorter than a gram: postings can't answer soundly (long values are
        // indexed by bigrams only, so a unigram lookup would wrongly report
        // "provably no hit") — the caller must fall back to a full scan.
        if needle.chars().count() < GRAM_LEN {
            return None;
        }
        let grams = Self::tokenize(needle);
        if grams.is_empty() {
            return None;
        }
        // Start from the smallest posting list, intersect gram by gram.
        let mut base: Option<&Vec<Vec<u8>>> = None;
        for g in &grams {
            let Some(list) = self.postings.get(g) else {
                return Some(Vec::new()); // a gram has no posting → no hit possible
            };
            base = Some(match base {
                Some(b) if b.len() <= list.len() => b,
                _ => list,
            });
        }
        let base = base.unwrap();
        let mut out = Vec::new();
        'pk: for pk in base {
            for g in &grams {
                let Some(list) = self.postings.get(g) else {
                    continue 'pk;
                };
                if !list.iter().any(|p| p == pk) {
                    continue 'pk;
                }
            }
            out.push(pk.clone());
        }
        Some(out)
    }
}

/// One n-gram index per (class, column).
#[derive(Default)]
pub struct NgramManager {
    map: HashMap<(String, String), NgramIndex>,
}

impl NgramManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an n-gram column; returns false if already present (idempotent).
    pub fn create(&mut self, class: &str, column: &str) -> bool {
        self.map
            .entry((class.to_string(), column.to_string()))
            .or_default();
        true
    }

    pub fn has(&self, class: &str, column: &str) -> bool {
        self.map.contains_key(&(class.to_string(), column.to_string()))
    }

    pub fn index_document(&mut self, class: &str, pk: &[u8], doc: &serde_json::Map<String, Value>) {
        for (key, idx) in self.map.iter_mut().filter(|((c, _), _)| c == class) {
            if let Some(v) = doc.get(&key.1).and_then(|v| v.as_str()) {
                idx.index(v, pk);
            }
        }
    }

    pub fn deindex_document(
        &mut self,
        class: &str,
        pk: &[u8],
        doc: &serde_json::Map<String, Value>,
    ) {
        for (key, idx) in self.map.iter_mut().filter(|((c, _), _)| c == class) {
            if let Some(v) = doc.get(&key.1).and_then(|v| v.as_str()) {
                idx.deindex(v, pk);
            }
        }
    }

    /// Candidate PKs; `None` = no index on the column, or needle too short
    /// (caller falls back to a scan).
    pub fn candidates(&self, class: &str, column: &str, needle: &str) -> Option<Vec<Vec<u8>>> {
        self.map
            .get(&(class.to_string(), column.to_string()))
            .and_then(|idx| idx.candidates(needle))
    }

    /// Backfill/incremental: index one value for the given column.
    pub fn index_value(&mut self, class: &str, column: &str, value: &str, pk: &[u8]) {
        if let Some(idx) = self.map.get_mut(&(class.to_string(), column.to_string())) {
            idx.index(value, pk);
        }
    }

    /// Registered n-gram columns (for persisting definitions / startup rebuild).
    pub fn registered(&self) -> Vec<(String, String)> {
        self.map.keys().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ngram_contains_roundtrip() {
        let mut idx = NgramIndex::default();
        idx.index("应急演练计划", b"pk1");
        idx.index("电力调度", b"pk2");
        // substring hit
        let hits = idx.candidates("演练").unwrap();
        assert_eq!(hits, vec![b"pk1".to_vec()]);
        // non-hit
        assert!(idx.candidates("调度台").unwrap().is_empty());
        // after deindex the value is gone
        idx.deindex("应急演练计划", b"pk1");
        assert!(idx.candidates("演练").unwrap().is_empty());
    }

    #[test]
    fn ngram_short_needle_falls_back() {
        let idx = NgramIndex::default();
        assert!(idx.candidates("应").is_none(), "single-char needle falls back to scan");
        assert!(idx.candidates("").is_none());
    }

    #[test]
    fn ngram_manager_column_scoped() {
        let mut mgr = NgramManager::new();
        assert!(mgr.create("T", "name_zh"));
        let doc = serde_json::json!({"name_zh": "应急演练计划"});
        mgr.index_document("T", b"pk1", doc.as_object().unwrap());
        assert!(mgr.candidates("T", "name_zh", "演练").unwrap().len() == 1);
        assert!(mgr.candidates("T", "other", "演练").is_none(), "unindexed column -> None (scan fallback)");
    }
}
