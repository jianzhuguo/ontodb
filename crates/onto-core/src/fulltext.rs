// Copyright (c) 2024-2026 OntoDB Team
// Licensed under the Business Source License 1.1 (BUSL-1.1).
// See LICENSE for details. Change Date: 2031-09-15.
// On the Change Date, this file will be licensed under Apache License 2.0.

//! Full-text search engine with inverted index and BM25 ranking.
//!
//! Supports Chinese (character-level n-gram) and English (whitespace) tokenization.

use std::collections::{HashMap, BTreeMap};

/// Document stored in the full-text index.
#[derive(Debug, Clone)]
pub struct IndexedDocument {
    pub doc_id: String,
    pub fields: HashMap<String, String>,
    pub token_count: usize,
}

/// Posting entry: document ID + term frequency + field positions.
#[derive(Debug, Clone)]
pub struct Posting {
    pub doc_id: String,
    pub term_freq: u32,
    pub field: String,
}

/// Inverted index entry for a single term.
#[derive(Debug, Clone)]
pub struct PostingList {
    pub term: String,
    pub postings: Vec<Posting>,
    pub doc_freq: u32,
}

/// BM25 parameters.
#[derive(Debug, Clone)]
pub struct Bm25Config {
    pub k1: f64,
    pub b: f64,
}

impl Default for Bm25Config {
    fn default() -> Self {
        Self { k1: 1.2, b: 0.75 }
    }
}

/// Search result with relevance score.
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub doc_id: String,
    pub score: f64,
    pub matched_fields: Vec<String>,
}

/// Full-text search engine.
pub struct FullTextIndex {
    /// Inverted index: term -> posting list.
    inverted_index: HashMap<String, PostingList>,
    /// Forward index: doc_id -> document.
    documents: HashMap<String, IndexedDocument>,
    /// BM25 configuration.
    config: Bm25Config,
    /// Total document count.
    total_docs: u32,
    /// Average document length (in tokens).
    avg_doc_length: f64,
}

impl FullTextIndex {
    pub fn new() -> Self {
        Self {
            inverted_index: HashMap::new(),
            documents: HashMap::new(),
            config: Bm25Config::default(),
            total_docs: 0,
            avg_doc_length: 0.0,
        }
    }

    pub fn with_config(config: Bm25Config) -> Self {
        Self { config, ..Self::new() }
    }

    /// Add a document with named fields.
    pub fn add_document(&mut self, doc_id: &str, fields: HashMap<String, String>) {
        let mut total_tokens = 0usize;

        for (field_name, text) in &fields {
            let tokens = tokenize(text);
            total_tokens += tokens.len();

            // Count term frequencies
            let mut tf_map: HashMap<String, u32> = HashMap::new();
            for token in &tokens {
                *tf_map.entry(token.clone()).or_insert(0) += 1;
            }

            for (term, freq) in &tf_map {
                let posting = Posting {
                    doc_id: doc_id.to_string(),
                    term_freq: *freq,
                    field: field_name.clone(),
                };
                let list = self.inverted_index.entry(term.clone()).or_insert_with(|| PostingList {
                    term: term.clone(),
                    postings: Vec::new(),
                    doc_freq: 0,
                });
                list.postings.push(posting);
                list.doc_freq = list.postings.len() as u32;
            }
        }

        let doc = IndexedDocument {
            doc_id: doc_id.to_string(),
            fields,
            token_count: total_tokens,
        };
        self.documents.insert(doc_id.to_string(), doc);
        self.total_docs += 1;

        // Recalculate average document length
        let total_length: usize = self.documents.values().map(|d| d.token_count).sum();
        self.avg_doc_length = total_length as f64 / self.total_docs as f64;
    }

    /// Search with BM25 ranking.
    pub fn search(&self, query: &str) -> Vec<SearchResult> {
        let query_tokens = tokenize(query);
        if query_tokens.is_empty() || self.total_docs == 0 {
            return Vec::new();
        }

        let mut scores: HashMap<String, (f64, Vec<String>)> = HashMap::new();

        for token in &query_tokens {
            if let Some(list) = self.inverted_index.get(token) {
                let idf = bm25_idf(self.total_docs, list.doc_freq);

                for posting in &list.postings {
                    if let Some(doc) = self.documents.get(&posting.doc_id) {
                        let tf = posting.term_freq as f64;
                        let dl = doc.token_count as f64;
                        let avg_dl = self.avg_doc_length;

                        let score = idf * (tf * (self.config.k1 + 1.0))
                            / (tf + self.config.k1 * (1.0 - self.config.b + self.config.b * dl / avg_dl));

                        let entry = scores.entry(posting.doc_id.clone()).or_insert((0.0, Vec::new()));
                        entry.0 += score;
                        if !entry.1.contains(&posting.field) {
                            entry.1.push(posting.field.clone());
                        }
                    }
                }
            }
        }

        let mut results: Vec<SearchResult> = scores
            .into_iter()
            .map(|(doc_id, (score, fields))| SearchResult {
                doc_id,
                score,
                matched_fields: fields,
            })
            .collect();

        results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        results
    }

    /// Remove a document from the index.
    pub fn remove_document(&mut self, doc_id: &str) {
        if let Some(doc) = self.documents.remove(doc_id) {
            // Rebuild affected posting lists
            for list in self.inverted_index.values_mut() {
                list.postings.retain(|p| p.doc_id != doc_id);
                list.doc_freq = list.postings.len() as u32;
            }
            // Remove empty terms
            self.inverted_index.retain(|_, list| !list.postings.is_empty());

            self.total_docs -= 1;
            if self.total_docs > 0 {
                let total_length: usize = self.documents.values().map(|d| d.token_count).sum();
                self.avg_doc_length = total_length as f64 / self.total_docs as f64;
            } else {
                self.avg_doc_length = 0.0;
            }
        }
    }

    pub fn doc_count(&self) -> u32 { self.total_docs }
    pub fn term_count(&self) -> usize { self.inverted_index.len() }
    pub fn get_document(&self, doc_id: &str) -> Option<&IndexedDocument> { self.documents.get(doc_id) }
}

/// BM25 IDF calculation.
fn bm25_idf(total_docs: u32, doc_freq: u32) -> f64 {
    let n = total_docs as f64;
    let df = doc_freq as f64;
    ((n - df + 0.5) / (df + 0.5) + 1.0).ln()
}

/// Tokenizer: handles Chinese (character-level) and English (whitespace).
pub fn tokenize(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current_word = String::new();

    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            current_word.push(ch.to_ascii_lowercase());
        } else if is_cjk(ch) {
            // Flush current English word
            if !current_word.is_empty() {
                tokens.push(current_word.clone());
                current_word.clear();
            }
            // Chinese: single character token + bigram
            tokens.push(ch.to_string());
        } else {
            // Whitespace or punctuation: flush
            if !current_word.is_empty() {
                tokens.push(current_word.clone());
                current_word.clear();
            }
        }
    }
    if !current_word.is_empty() {
        tokens.push(current_word);
    }

    // Add bigrams for Chinese
    let chinese_chars: Vec<char> = text.chars().filter(|c| is_cjk(*c)).collect();
    for window in chinese_chars.windows(2) {
        tokens.push(format!("{}{}", window[0], window[1]));
    }

    tokens
}

fn is_cjk(ch: char) -> bool {
    matches!(ch, '\u{4e00}'..='\u{9fff}' | '\u{3400}'..='\u{4dbf}' | '\u{f900}'..='\u{faff}')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tokenize_english() {
        let tokens = tokenize("Hello World Test");
        assert!(tokens.contains(&"hello".to_string()));
        assert!(tokens.contains(&"world".to_string()));
        assert!(tokens.contains(&"test".to_string()));
    }

    #[test]
    fn test_tokenize_chinese() {
        let tokens = tokenize("温度传感器");
        assert!(tokens.contains(&"温".to_string()));
        assert!(tokens.contains(&"度".to_string()));
        assert!(tokens.contains(&"温度".to_string()));
        assert!(tokens.contains(&"传感".to_string()));
        assert!(tokens.contains(&"感器".to_string()));
    }

    #[test]
    fn test_tokenize_mixed() {
        let tokens = tokenize("CPU温度 85°C");
        assert!(tokens.contains(&"cpu".to_string()));
        assert!(tokens.contains(&"温".to_string()));
        assert!(tokens.contains(&"85".to_string()));
    }

    #[test]
    fn test_fulltext_search_basic() {
        let mut idx = FullTextIndex::new();
        let mut fields = HashMap::new();
        fields.insert("content".into(), "The temperature sensor shows 85 degrees".into());
        idx.add_document("doc1", fields);

        let mut fields2 = HashMap::new();
        fields2.insert("content".into(), "The humidity sensor shows 60 percent".into());
        idx.add_document("doc2", fields2);

        let results = idx.search("temperature");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].doc_id, "doc1");
    }

    #[test]
    fn test_fulltext_search_chinese() {
        let mut idx = FullTextIndex::new();
        let mut fields = HashMap::new();
        fields.insert("content".into(), "温度传感器显示85度".into());
        idx.add_document("d1", fields);

        let mut fields2 = HashMap::new();
        fields2.insert("content".into(), "湿度计显示60百分比".into());
        idx.add_document("d2", fields2);

        let results = idx.search("温度传感器");
        assert!(!results.is_empty());
        // d1 should rank highest (contains 温度 + 传感 bigrams)
        assert_eq!(results[0].doc_id, "d1");
    }

    #[test]
    fn test_fulltext_search_bm25_ranking() {
        let mut idx = FullTextIndex::new();

        let mut f1 = HashMap::new();
        f1.insert("content".into(), "sensor temperature temperature temperature".into());
        idx.add_document("d1", f1);

        let mut f2 = HashMap::new();
        f2.insert("content".into(), "sensor temperature".into());
        idx.add_document("d2", f2);

        let results = idx.search("temperature");
        assert_eq!(results.len(), 2);
        // d1 has higher term frequency, should rank higher
        assert_eq!(results[0].doc_id, "d1");
        assert!(results[0].score > results[1].score);
    }

    #[test]
    fn test_fulltext_search_multifield() {
        let mut idx = FullTextIndex::new();
        let mut fields = HashMap::new();
        fields.insert("title".into(), "Temperature Monitor".into());
        fields.insert("content".into(), "A device for monitoring".into());
        idx.add_document("d1", fields);

        let results = idx.search("temperature");
        assert_eq!(results.len(), 1);
        assert!(results[0].matched_fields.contains(&"title".to_string()));
    }

    #[test]
    fn test_fulltext_remove_document() {
        let mut idx = FullTextIndex::new();
        let mut fields = HashMap::new();
        fields.insert("content".into(), "temperature sensor".into());
        idx.add_document("d1", fields);
        assert_eq!(idx.doc_count(), 1);

        idx.remove_document("d1");
        assert_eq!(idx.doc_count(), 0);
        assert_eq!(idx.term_count(), 0);
    }

    #[test]
    fn test_fulltext_empty_query() {
        let mut idx = FullTextIndex::new();
        let mut fields = HashMap::new();
        fields.insert("content".into(), "test".into());
        idx.add_document("d1", fields);

        let results = idx.search("");
        assert_eq!(results.len(), 0);
    }
}
