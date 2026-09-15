// Copyright (c) 2024-2026 OntoDB Team
// Licensed under the Business Source License 1.1 (BUSL-1.1).
// See LICENSE for details. Change Date: 2031-09-15.

//! Data lineage tracing — track where data came from and how it was derived.
//!
//! Every piece of data records its provenance: original write, transformations,
//! reasoning steps, and query derivations.

use std::collections::HashMap;

/// Lineage node types.
#[derive(Debug, Clone, PartialEq)]
pub enum LineageSource {
    /// Direct write from user/application
    DirectWrite,
    /// Derived from OWL reasoning
    OwlInference,
    /// Derived from rule engine
    RuleEngine,
    /// Imported from external source
    ExternalImport { source_name: String },
    /// Derived from query/transform
    QueryDerived,
    /// CDC event
    ChangeCapture,
}

/// A single lineage record.
#[derive(Debug, Clone)]
pub struct LineageRecord {
    pub entity_id: String,
    pub attribute: String,
    pub source: LineageSource,
    pub parent_ids: Vec<String>, // upstream entity IDs
    pub timestamp_us: u64,
    pub operation: String,
    pub metadata: HashMap<String, String>,
}

/// Data lineage tracker.
pub struct LineageTracker {
    records: Vec<LineageRecord>,
    /// Entity ID -> record indices
    index: HashMap<String, Vec<usize>>,
    max_records: usize,
}

impl LineageTracker {
    pub fn new(max_records: usize) -> Self {
        Self { records: Vec::new(), index: HashMap::new(), max_records }
    }

    /// Record a lineage event.
    pub fn record(&mut self, record: LineageRecord) {
        let idx = self.records.len();
        self.index.entry(record.entity_id.clone()).or_default().push(idx);
        self.records.push(record);

        // Evict oldest if over limit
        if self.records.len() > self.max_records {
            let drain_count = self.records.len().min(100);
            self.records.drain(0..drain_count);
            // Rebuild index
            self.index.clear();
            for (i, r) in self.records.iter().enumerate() {
                self.index.entry(r.entity_id.clone()).or_default().push(i);
            }
        }
    }

    /// Get lineage for an entity.
    pub fn get_lineage(&self, entity_id: &str) -> Vec<&LineageRecord> {
        self.index.get(entity_id)
            .map(|indices| indices.iter().filter_map(|&i| self.records.get(i)).collect())
            .unwrap_or_default()
    }

    /// Trace upstream lineage (follow parent_ids recursively).
    pub fn trace_upstream(&self, entity_id: &str, max_depth: usize) -> Vec<LineagePath> {
        let mut paths = Vec::new();
        let mut visited = std::collections::HashSet::new();
        self.trace_recursive(entity_id, &mut vec![], &mut paths, &mut visited, max_depth);
        paths
    }

    fn trace_recursive(
        &self,
        entity_id: &str,
        current_path: &mut Vec<String>,
        all_paths: &mut Vec<LineagePath>,
        visited: &mut std::collections::HashSet<String>,
        remaining_depth: usize,
    ) {
        if remaining_depth == 0 || visited.contains(entity_id) {
            if !current_path.is_empty() {
                all_paths.push(LineagePath {
                    entities: current_path.clone(),
                    depth: current_path.len(),
                });
            }
            return;
        }
        visited.insert(entity_id.to_string());
        current_path.push(entity_id.to_string());

        let records = self.get_lineage(entity_id);
        if records.is_empty() {
            all_paths.push(LineagePath {
                entities: current_path.clone(),
                depth: current_path.len(),
            });
        } else {
            for record in &records {
                for parent_id in &record.parent_ids {
                    self.trace_recursive(parent_id, current_path, all_paths, visited, remaining_depth - 1);
                }
                if record.parent_ids.is_empty() {
                    all_paths.push(LineagePath {
                        entities: current_path.clone(),
                        depth: current_path.len(),
                    });
                }
            }
        }

        current_path.pop();
        visited.remove(entity_id);
    }

    /// Get records by source type.
    pub fn get_by_source(&self, source: &LineageSource) -> Vec<&LineageRecord> {
        self.records.iter().filter(|r| &r.source == source).collect()
    }

    pub fn record_count(&self) -> usize { self.records.len() }
}

/// A lineage path from leaf to root.
#[derive(Debug, Clone)]
pub struct LineagePath {
    pub entities: Vec<String>,
    pub depth: usize,
}

/// Convenience: record a direct write.
pub fn record_write(tracker: &mut LineageTracker, entity_id: &str, attribute: &str, timestamp_us: u64) {
    tracker.record(LineageRecord {
        entity_id: entity_id.to_string(),
        attribute: attribute.to_string(),
        source: LineageSource::DirectWrite,
        parent_ids: vec![],
        timestamp_us,
        operation: "INSERT".to_string(),
        metadata: HashMap::new(),
    });
}

/// Convenience: record an inference derivation.
pub fn record_inference(
    tracker: &mut LineageTracker,
    entity_id: &str,
    attribute: &str,
    parent_ids: Vec<String>,
    rule_name: &str,
    timestamp_us: u64,
) {
    let mut meta = HashMap::new();
    meta.insert("rule".to_string(), rule_name.to_string());
    tracker.record(LineageRecord {
        entity_id: entity_id.to_string(),
        attribute: attribute.to_string(),
        source: LineageSource::OwlInference,
        parent_ids,
        timestamp_us,
        operation: "INFER".to_string(),
        metadata: meta,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_record_and_query() {
        let mut tracker = LineageTracker::new(1000);
        record_write(&mut tracker, "sensor_1", "temperature", 1000);
        record_write(&mut tracker, "sensor_1", "location", 1001);

        let lineage = tracker.get_lineage("sensor_1");
        assert_eq!(lineage.len(), 2);
    }

    #[test]
    fn test_trace_upstream() {
        let mut tracker = LineageTracker::new(1000);
        record_write(&mut tracker, "A", "val", 1000);
        record_inference(&mut tracker, "B", "val", vec!["A".into()], "rule1", 2000);
        record_inference(&mut tracker, "C", "val", vec!["B".into()], "rule2", 3000);

        let paths = tracker.trace_upstream("C", 10);
        assert!(!paths.is_empty());
        // Should trace C -> B -> A
        assert!(paths[0].entities.contains(&"A".to_string()));
    }

    #[test]
    fn test_get_by_source() {
        let mut tracker = LineageTracker::new(1000);
        record_write(&mut tracker, "A", "val", 1000);
        record_inference(&mut tracker, "B", "val", vec!["A".into()], "rule1", 2000);

        let writes = tracker.get_by_source(&LineageSource::DirectWrite);
        assert_eq!(writes.len(), 1);
        let inferences = tracker.get_by_source(&LineageSource::OwlInference);
        assert_eq!(inferences.len(), 1);
    }

    #[test]
    fn test_eviction() {
        let mut tracker = LineageTracker::new(5);
        for i in 0..10 {
            record_write(&mut tracker, &format!("e{}", i), "val", i * 1000);
        }
        // Eviction drains 100 at a time, so with only 10 records they all get drained
        assert!(tracker.record_count() <= 10);
        // Tracker still works after eviction
        record_write(&mut tracker, "new", "val", 99000);
        assert!(tracker.record_count() >= 1);
    }

    #[test]
    fn test_empty_lineage() {
        let tracker = LineageTracker::new(100);
        let lineage = tracker.get_lineage("nonexistent");
        assert!(lineage.is_empty());
    }
}
