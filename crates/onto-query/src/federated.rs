// Copyright (c) 2024-2026 OntoDB Team
// Licensed under the Business Source License 1.1 (BUSL-1.1).
// See LICENSE for details. Change Date: 2031-09-15.

//! Federated query — query external data sources without moving data.
//!
//! External sources (MySQL, PostgreSQL, REST API) are mapped as OntoDB namespaces,
//! enabling unified queries across internal and external data.

use std::collections::HashMap;

/// External data source types.
#[derive(Debug, Clone)]
pub enum ExternalSource {
    PostgreSQL {
        host: String,
        port: u16,
        database: String,
    },
    MySQL {
        host: String,
        port: u16,
        database: String,
    },
    RestApi {
        base_url: String,
        auth_header: Option<String>,
    },
    CsvFile {
        path: String,
    },
    JsonFile {
        path: String,
    },
}

/// External table mapping.
#[derive(Debug, Clone)]
pub struct ExternalTable {
    pub namespace: String,
    pub table_name: String,
    pub source: ExternalSource,
    pub columns: Vec<ColumnInfo>,
    pub cache_ttl_ms: u64,
}

#[derive(Debug, Clone)]
pub struct ColumnInfo {
    pub name: String,
    pub col_type: String,
}

/// Federated query engine.
pub struct FederatedEngine {
    tables: HashMap<String, ExternalTable>,
    /// Namespace -> source config
    sources: HashMap<String, ExternalSource>,
}

impl FederatedEngine {
    pub fn new() -> Self {
        Self {
            tables: HashMap::new(),
            sources: HashMap::new(),
        }
    }

    /// Register an external data source as a namespace.
    pub fn register_source(&mut self, namespace: &str, source: ExternalSource) {
        self.sources.insert(namespace.to_string(), source);
    }

    /// Register an external table.
    pub fn register_table(&mut self, table: ExternalTable) {
        let key = format!("{}::{}", table.namespace, table.table_name);
        self.tables.insert(key, table);
    }

    /// Check if a table is external (federated).
    pub fn is_external(&self, namespace: &str, table: &str) -> bool {
        self.tables
            .contains_key(&format!("{}::{}", namespace, table))
    }

    /// Get external table metadata.
    pub fn get_table(&self, namespace: &str, table: &str) -> Option<&ExternalTable> {
        self.tables.get(&format!("{}::{}", namespace, table))
    }

    /// List all registered external tables.
    pub fn list_tables(&self) -> Vec<&ExternalTable> {
        self.tables.values().collect()
    }

    /// List all registered sources.
    pub fn list_sources(&self) -> Vec<(&String, &ExternalSource)> {
        self.sources.iter().collect()
    }

    /// Generate a query plan hint for federated execution.
    pub fn plan_hint(&self, namespace: &str, table: &str) -> Option<FederatedHint> {
        self.get_table(namespace, table).map(|t| FederatedHint {
            source_type: match &t.source {
                ExternalSource::PostgreSQL { .. } => "PostgreSQL".into(),
                ExternalSource::MySQL { .. } => "MySQL".into(),
                ExternalSource::RestApi { .. } => "REST".into(),
                ExternalSource::CsvFile { .. } => "CSV".into(),
                ExternalSource::JsonFile { .. } => "JSON".into(),
            },
            columns: t.columns.iter().map(|c| c.name.clone()).collect(),
            cache_ttl_ms: t.cache_ttl_ms,
            pushdown_supported: matches!(
                t.source,
                ExternalSource::PostgreSQL { .. } | ExternalSource::MySQL { .. }
            ),
        })
    }

    pub fn source_count(&self) -> usize {
        self.sources.len()
    }
    pub fn table_count(&self) -> usize {
        self.tables.len()
    }
}

#[derive(Debug)]
pub struct FederatedHint {
    pub source_type: String,
    pub columns: Vec<String>,
    pub cache_ttl_ms: u64,
    pub pushdown_supported: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_register_source() {
        let mut engine = FederatedEngine::new();
        engine.register_source(
            "mysql_legacy",
            ExternalSource::MySQL {
                host: "10.0.0.1".into(),
                port: 3306,
                database: "legacy".into(),
            },
        );
        assert_eq!(engine.source_count(), 1);
    }

    #[test]
    fn test_register_table() {
        let mut engine = FederatedEngine::new();
        engine.register_source(
            "pg",
            ExternalSource::PostgreSQL {
                host: "localhost".into(),
                port: 5432,
                database: "app".into(),
            },
        );
        engine.register_table(ExternalTable {
            namespace: "pg".into(),
            table_name: "users".into(),
            source: ExternalSource::PostgreSQL {
                host: "localhost".into(),
                port: 5432,
                database: "app".into(),
            },
            columns: vec![
                ColumnInfo {
                    name: "id".into(),
                    col_type: "int".into(),
                },
                ColumnInfo {
                    name: "name".into(),
                    col_type: "text".into(),
                },
            ],
            cache_ttl_ms: 60000,
        });

        assert!(engine.is_external("pg", "users"));
        assert!(!engine.is_external("pg", "orders"));
        assert_eq!(engine.table_count(), 1);
    }

    #[test]
    fn test_plan_hint() {
        let mut engine = FederatedEngine::new();
        engine.register_source(
            "api",
            ExternalSource::RestApi {
                base_url: "https://api.example.com".into(),
                auth_header: Some("Bearer token".into()),
            },
        );
        engine.register_table(ExternalTable {
            namespace: "api".into(),
            table_name: "products".into(),
            source: ExternalSource::RestApi {
                base_url: "https://api.example.com".into(),
                auth_header: None,
            },
            columns: vec![ColumnInfo {
                name: "id".into(),
                col_type: "string".into(),
            }],
            cache_ttl_ms: 300000,
        });

        let hint = engine.plan_hint("api", "products").unwrap();
        assert_eq!(hint.source_type, "REST");
        assert!(!hint.pushdown_supported); // REST doesn't support pushdown
    }

    #[test]
    fn test_pushdown_supported() {
        let mut engine = FederatedEngine::new();
        engine.register_table(ExternalTable {
            namespace: "pg".into(),
            table_name: "orders".into(),
            source: ExternalSource::PostgreSQL {
                host: "localhost".into(),
                port: 5432,
                database: "app".into(),
            },
            columns: vec![],
            cache_ttl_ms: 60000,
        });

        let hint = engine.plan_hint("pg", "orders").unwrap();
        assert!(hint.pushdown_supported); // PostgreSQL supports pushdown
    }

    #[test]
    fn test_list_tables() {
        let mut engine = FederatedEngine::new();
        engine.register_table(ExternalTable {
            namespace: "a".into(),
            table_name: "t1".into(),
            source: ExternalSource::CsvFile {
                path: "/tmp/a.csv".into(),
            },
            columns: vec![],
            cache_ttl_ms: 0,
        });
        engine.register_table(ExternalTable {
            namespace: "b".into(),
            table_name: "t2".into(),
            source: ExternalSource::JsonFile {
                path: "/tmp/b.json".into(),
            },
            columns: vec![],
            cache_ttl_ms: 0,
        });

        assert_eq!(engine.list_tables().len(), 2);
    }
}
