//! Data migration module for OntoDB Enterprise.
//!
//! Provides tools to migrate data from other databases to OntoDB:
//! - PostgreSQL (via SQL dump or direct connection)
//! - MySQL (via SQL dump or direct connection)
//! - Neo4j (via Cypher export or direct connection)
//! - CSV/JSON file import
//!
//! Migration modes:
//! - **Schema migration**: Convert table structures to OntoDB schema
//! - **Data migration**: Transfer data with type conversion
//! - **Incremental migration**: Sync changes from source database

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use parking_lot::RwLock;

/// Migration configuration.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MigrationConfig {
    /// Enable migration functionality.
    pub enabled: bool,
    /// Temporary directory for migration files.
    pub temp_dir: PathBuf,
    /// Batch size for bulk operations.
    pub batch_size: usize,
    /// Enable parallel migration.
    pub parallel: bool,
    /// Number of parallel workers.
    pub parallel_workers: usize,
    /// Continue on error (skip failed rows).
    pub continue_on_error: bool,
    /// Maximum errors before aborting.
    pub max_errors: usize,
}

impl Default for MigrationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            temp_dir: PathBuf::from("./migration_temp"),
            batch_size: 10000,
            parallel: true,
            parallel_workers: 4,
            continue_on_error: true,
            max_errors: 1000,
        }
    }
}

/// Source database type.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum SourceDatabase {
    /// PostgreSQL.
    PostgreSQL,
    /// MySQL.
    MySQL,
    /// Neo4j (graph database).
    Neo4j,
    /// CSV file import.
    CsvFile,
    /// JSON file import.
    JsonFile,
    /// SQL dump file.
    SqlDump,
}

impl std::fmt::Display for SourceDatabase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PostgreSQL => write!(f, "PostgreSQL"),
            Self::MySQL => write!(f, "MySQL"),
            Self::Neo4j => write!(f, "Neo4j"),
            Self::CsvFile => write!(f, "CSV"),
            Self::JsonFile => write!(f, "JSON"),
            Self::SqlDump => write!(f, "SQL Dump"),
        }
    }
}

/// Migration connection configuration.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SourceConfig {
    /// Source database type.
    pub source_type: SourceDatabase,
    /// Connection string or file path.
    pub connection_string: Option<String>,
    /// File path for file-based imports.
    pub file_path: Option<PathBuf>,
    /// Database name.
    pub database: Option<String>,
    /// Schema name (for PostgreSQL).
    pub schema: Option<String>,
    /// Tables/collections to migrate (empty = all).
    pub tables: Vec<String>,
    /// Username.
    pub username: Option<String>,
    /// Password (should be encrypted in production).
    pub password: Option<String>,
    /// Additional options.
    pub options: HashMap<String, String>,
}

/// Migration status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum MigrationStatus {
    /// Not started.
    NotStarted,
    /// In progress.
    InProgress,
    /// Completed successfully.
    Completed,
    /// Completed with errors.
    CompletedWithErrors,
    /// Failed.
    Failed,
    /// Cancelled.
    Cancelled,
}

impl std::fmt::Display for MigrationStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotStarted => write!(f, "Not Started"),
            Self::InProgress => write!(f, "In Progress"),
            Self::Completed => write!(f, "Completed"),
            Self::CompletedWithErrors => write!(f, "Completed with Errors"),
            Self::Failed => write!(f, "Failed"),
            Self::Cancelled => write!(f, "Cancelled"),
        }
    }
}

/// Migration progress.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MigrationProgress {
    /// Current status.
    pub status: MigrationStatus,
    /// Total tables to migrate.
    pub total_tables: usize,
    /// Tables completed.
    pub completed_tables: usize,
    /// Current table being migrated.
    pub current_table: Option<String>,
    /// Total rows in current table.
    pub total_rows: u64,
    /// Rows migrated in current table.
    pub migrated_rows: u64,
    /// Total rows across all tables.
    pub total_rows_all: u64,
    /// Total rows migrated across all tables.
    pub migrated_rows_all: u64,
    /// Errors encountered.
    pub error_count: usize,
    /// Error messages.
    pub errors: Vec<MigrationError>,
    /// Start time.
    pub started_at: Option<String>,
    /// End time.
    pub completed_at: Option<String>,
    /// Duration in milliseconds.
    pub duration_ms: u64,
}

/// Migration error.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MigrationError {
    /// Table name.
    pub table: String,
    /// Row number (if applicable).
    pub row: Option<u64>,
    /// Error message.
    pub message: String,
    /// Error code.
    pub code: Option<String>,
    /// Raw data that caused the error.
    pub raw_data: Option<String>,
}

/// Table schema for migration.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TableSchema {
    /// Table name.
    pub name: String,
    /// Columns.
    pub columns: Vec<ColumnDefinition>,
    /// Primary key columns.
    pub primary_key: Vec<String>,
    /// Row count estimate.
    pub row_count_estimate: u64,
}

/// Column definition.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ColumnDefinition {
    /// Column name.
    pub name: String,
    /// Data type in source database.
    pub source_type: String,
    /// Nullable.
    pub nullable: bool,
    /// Default value.
    pub default: Option<String>,
    /// Comment/description.
    pub comment: Option<String>,
}

/// Data type mapping.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TypeMapping {
    /// Source type (e.g., "VARCHAR", "INT", "TEXT").
    pub source_type: String,
    /// OntoDB type (e.g., "string", "integer", "text").
    pub ontodb_type: String,
    /// Conversion function name (if needed).
    pub conversion: Option<String>,
}

/// Migration job.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MigrationJob {
    /// Job ID.
    pub id: String,
    /// Job name.
    pub name: String,
    /// Source configuration.
    pub source: SourceConfig,
    /// Status.
    pub progress: MigrationProgress,
    /// Schema mappings.
    pub type_mappings: Vec<TypeMapping>,
    /// Created at.
    pub created_at: String,
}

/// Data row for migration.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DataRow {
    /// Table name.
    pub table: String,
    /// Column values (key-value pairs).
    pub values: HashMap<String, serde_json::Value>,
    /// Row number in source.
    pub row_number: u64,
}

/// Migration manager.
#[derive(Clone)]
pub struct MigrationManager {
    config: MigrationConfig,
    /// Active migration jobs.
    jobs: Arc<RwLock<HashMap<String, MigrationJob>>>,
    /// Default type mappings for each source database.
    type_mappings: Arc<RwLock<HashMap<SourceDatabase, Vec<TypeMapping>>>>,
}

impl MigrationManager {
    /// Create a new migration manager.
    pub fn new(config: MigrationConfig) -> Self {
        let mut type_mappings = HashMap::new();

        // PostgreSQL type mappings
        type_mappings.insert(SourceDatabase::PostgreSQL, Self::pg_type_mappings());

        // MySQL type mappings
        type_mappings.insert(SourceDatabase::MySQL, Self::mysql_type_mappings());

        // Neo4j type mappings
        type_mappings.insert(SourceDatabase::Neo4j, Self::neo4j_type_mappings());

        Self {
            config,
            jobs: Arc::new(RwLock::new(HashMap::new())),
            type_mappings: Arc::new(RwLock::new(type_mappings)),
        }
    }

    /// Get default PostgreSQL type mappings.
    fn pg_type_mappings() -> Vec<TypeMapping> {
        vec![
            TypeMapping { source_type: "integer".to_string(), ontodb_type: "integer".to_string(), conversion: None },
            TypeMapping { source_type: "bigint".to_string(), ontodb_type: "long".to_string(), conversion: None },
            TypeMapping { source_type: "smallint".to_string(), ontodb_type: "integer".to_string(), conversion: None },
            TypeMapping { source_type: "serial".to_string(), ontodb_type: "integer".to_string(), conversion: None },
            TypeMapping { source_type: "bigserial".to_string(), ontodb_type: "long".to_string(), conversion: None },
            TypeMapping { source_type: "real".to_string(), ontodb_type: "float".to_string(), conversion: None },
            TypeMapping { source_type: "double precision".to_string(), ontodb_type: "double".to_string(), conversion: None },
            TypeMapping { source_type: "numeric".to_string(), ontodb_type: "decimal".to_string(), conversion: None },
            TypeMapping { source_type: "decimal".to_string(), ontodb_type: "decimal".to_string(), conversion: None },
            TypeMapping { source_type: "boolean".to_string(), ontodb_type: "boolean".to_string(), conversion: None },
            TypeMapping { source_type: "varchar".to_string(), ontodb_type: "string".to_string(), conversion: None },
            TypeMapping { source_type: "character varying".to_string(), ontodb_type: "string".to_string(), conversion: None },
            TypeMapping { source_type: "char".to_string(), ontodb_type: "string".to_string(), conversion: None },
            TypeMapping { source_type: "character".to_string(), ontodb_type: "string".to_string(), conversion: None },
            TypeMapping { source_type: "text".to_string(), ontodb_type: "text".to_string(), conversion: None },
            TypeMapping { source_type: "date".to_string(), ontodb_type: "date".to_string(), conversion: None },
            TypeMapping { source_type: "timestamp".to_string(), ontodb_type: "timestamp".to_string(), conversion: None },
            TypeMapping { source_type: "timestamp without time zone".to_string(), ontodb_type: "timestamp".to_string(), conversion: None },
            TypeMapping { source_type: "timestamp with time zone".to_string(), ontodb_type: "timestamp".to_string(), conversion: None },
            TypeMapping { source_type: "time".to_string(), ontodb_type: "time".to_string(), conversion: None },
            TypeMapping { source_type: "uuid".to_string(), ontodb_type: "string".to_string(), conversion: None },
            TypeMapping { source_type: "json".to_string(), ontodb_type: "json".to_string(), conversion: None },
            TypeMapping { source_type: "jsonb".to_string(), ontodb_type: "json".to_string(), conversion: None },
            TypeMapping { source_type: "bytea".to_string(), ontodb_type: "binary".to_string(), conversion: None },
            TypeMapping { source_type: "array".to_string(), ontodb_type: "array".to_string(), conversion: Some("pg_array_to_json".to_string()) },
        ]
    }

    /// Get default MySQL type mappings.
    fn mysql_type_mappings() -> Vec<TypeMapping> {
        vec![
            TypeMapping { source_type: "INT".to_string(), ontodb_type: "integer".to_string(), conversion: None },
            TypeMapping { source_type: "INTEGER".to_string(), ontodb_type: "integer".to_string(), conversion: None },
            TypeMapping { source_type: "TINYINT".to_string(), ontodb_type: "integer".to_string(), conversion: None },
            TypeMapping { source_type: "SMALLINT".to_string(), ontodb_type: "integer".to_string(), conversion: None },
            TypeMapping { source_type: "MEDIUMINT".to_string(), ontodb_type: "integer".to_string(), conversion: None },
            TypeMapping { source_type: "BIGINT".to_string(), ontodb_type: "long".to_string(), conversion: None },
            TypeMapping { source_type: "FLOAT".to_string(), ontodb_type: "float".to_string(), conversion: None },
            TypeMapping { source_type: "DOUBLE".to_string(), ontodb_type: "double".to_string(), conversion: None },
            TypeMapping { source_type: "DECIMAL".to_string(), ontodb_type: "decimal".to_string(), conversion: None },
            TypeMapping { source_type: "NUMERIC".to_string(), ontodb_type: "decimal".to_string(), conversion: None },
            TypeMapping { source_type: "BOOLEAN".to_string(), ontodb_type: "boolean".to_string(), conversion: None },
            TypeMapping { source_type: "TINYINT(1)".to_string(), ontodb_type: "boolean".to_string(), conversion: None },
            TypeMapping { source_type: "VARCHAR".to_string(), ontodb_type: "string".to_string(), conversion: None },
            TypeMapping { source_type: "CHAR".to_string(), ontodb_type: "string".to_string(), conversion: None },
            TypeMapping { source_type: "TEXT".to_string(), ontodb_type: "text".to_string(), conversion: None },
            TypeMapping { source_type: "MEDIUMTEXT".to_string(), ontodb_type: "text".to_string(), conversion: None },
            TypeMapping { source_type: "LONGTEXT".to_string(), ontodb_type: "text".to_string(), conversion: None },
            TypeMapping { source_type: "DATE".to_string(), ontodb_type: "date".to_string(), conversion: None },
            TypeMapping { source_type: "DATETIME".to_string(), ontodb_type: "timestamp".to_string(), conversion: None },
            TypeMapping { source_type: "TIMESTAMP".to_string(), ontodb_type: "timestamp".to_string(), conversion: None },
            TypeMapping { source_type: "TIME".to_string(), ontodb_type: "time".to_string(), conversion: None },
            TypeMapping { source_type: "BLOB".to_string(), ontodb_type: "binary".to_string(), conversion: None },
            TypeMapping { source_type: "JSON".to_string(), ontodb_type: "json".to_string(), conversion: None },
        ]
    }

    /// Get default Neo4j type mappings.
    fn neo4j_type_mappings() -> Vec<TypeMapping> {
        vec![
            TypeMapping { source_type: "INTEGER".to_string(), ontodb_type: "integer".to_string(), conversion: None },
            TypeMapping { source_type: "FLOAT".to_string(), ontodb_type: "double".to_string(), conversion: None },
            TypeMapping { source_type: "STRING".to_string(), ontodb_type: "string".to_string(), conversion: None },
            TypeMapping { source_type: "BOOLEAN".to_string(), ontodb_type: "boolean".to_string(), conversion: None },
            TypeMapping { source_type: "DATE".to_string(), ontodb_type: "date".to_string(), conversion: None },
            TypeMapping { source_type: "DATE_TIME".to_string(), ontodb_type: "timestamp".to_string(), conversion: None },
            TypeMapping { source_type: "LIST".to_string(), ontodb_type: "array".to_string(), conversion: None },
            TypeMapping { source_type: "MAP".to_string(), ontodb_type: "json".to_string(), conversion: None },
        ]
    }

    /// Create a new migration job.
    pub fn create_job(
        &self,
        name: &str,
        source: SourceConfig,
    ) -> String {
        let job_id = format!("migration_{}", chrono::Utc::now().format("%Y%m%d_%H%M%S"));

        let job = MigrationJob {
            id: job_id.clone(),
            name: name.to_string(),
            source,
            progress: MigrationProgress {
                status: MigrationStatus::NotStarted,
                total_tables: 0,
                completed_tables: 0,
                current_table: None,
                total_rows: 0,
                migrated_rows: 0,
                total_rows_all: 0,
                migrated_rows_all: 0,
                error_count: 0,
                errors: Vec::new(),
                started_at: None,
                completed_at: None,
                duration_ms: 0,
            },
            type_mappings: Vec::new(),
            created_at: chrono::Utc::now().to_rfc3339(),
        };

        self.jobs.write().insert(job_id.clone(), job);
        job_id
    }

    /// Get migration job status.
    pub fn get_job(&self, job_id: &str) -> Option<MigrationJob> {
        self.jobs.read().get(job_id).cloned()
    }

    /// List all migration jobs.
    pub fn list_jobs(&self) -> Vec<MigrationJob> {
        self.jobs.read().values().cloned().collect()
    }

    /// Update job progress.
    pub fn update_progress(&self, job_id: &str, progress: MigrationProgress) {
        if let Some(job) = self.jobs.write().get_mut(job_id) {
            job.progress = progress;
        }
    }

    /// Get type mappings for a source database.
    pub fn get_type_mappings(&self, source: &SourceDatabase) -> Vec<TypeMapping> {
        self.type_mappings.read().get(source).cloned().unwrap_or_default()
    }

    /// Convert a source type to OntoDB type.
    pub fn convert_type(&self, source: &SourceDatabase, source_type: &str) -> Option<String> {
        let mappings = self.type_mappings.read();
        if let Some(mappings) = mappings.get(source) {
            // Try exact match first
            for mapping in mappings {
                if mapping.source_type.eq_ignore_ascii_case(source_type) {
                    return Some(mapping.ontodb_type.clone());
                }
            }
            // Try prefix match (e.g., "VARCHAR(255)" -> "VARCHAR")
            let base_type = source_type.split('(').next().unwrap_or(source_type);
            for mapping in mappings {
                if mapping.source_type.eq_ignore_ascii_case(base_type) {
                    return Some(mapping.ontodb_type.clone());
                }
            }
        }
        None
    }

    /// Parse a CSV file and return rows.
    pub fn parse_csv(&self, path: &Path) -> Result<Vec<DataRow>, MigrationError> {
        let file = std::fs::File::open(path).map_err(|e| MigrationError {
            table: path.to_string_lossy().to_string(),
            row: None,
            message: format!("Failed to open CSV file: {}", e),
            code: None,
            raw_data: None,
        })?;

        let mut reader = csv::ReaderBuilder::new()
            .has_headers(true)
            .from_reader(file);

        let headers: Vec<String> = reader.headers()
            .map_err(|e| MigrationError {
                table: path.to_string_lossy().to_string(),
                row: None,
                message: format!("Failed to read CSV headers: {}", e),
                code: None,
                raw_data: None,
            })?
            .iter()
            .map(|h| h.to_string())
            .collect();

        let mut rows = Vec::new();
        for (row_num, result) in reader.records().enumerate() {
            let record = result.map_err(|e| MigrationError {
                table: path.to_string_lossy().to_string(),
                row: Some(row_num as u64 + 1),
                message: format!("Failed to read CSV row: {}", e),
                code: None,
                raw_data: None,
            })?;

            let mut values = HashMap::new();
            for (i, field) in record.iter().enumerate() {
                if let Some(header) = headers.get(i) {
                    values.insert(header.clone(), serde_json::Value::String(field.to_string()));
                }
            }

            rows.push(DataRow {
                table: path.file_stem().unwrap_or_default().to_string_lossy().to_string(),
                values,
                row_number: row_num as u64 + 1,
            });
        }

        Ok(rows)
    }

    /// Parse a JSON file and return rows.
    pub fn parse_json(&self, path: &Path) -> Result<Vec<DataRow>, MigrationError> {
        let content = std::fs::read_to_string(path).map_err(|e| MigrationError {
            table: path.to_string_lossy().to_string(),
            row: None,
            message: format!("Failed to read JSON file: {}", e),
            code: None,
            raw_data: None,
        })?;

        let value: serde_json::Value = serde_json::from_str(&content).map_err(|e| MigrationError {
            table: path.to_string_lossy().to_string(),
            row: None,
            message: format!("Failed to parse JSON: {}", e),
            code: None,
            raw_data: None,
        })?;

        let mut rows = Vec::new();

        match value {
            serde_json::Value::Array(arr) => {
                for (i, item) in arr.iter().enumerate() {
                    if let serde_json::Value::Object(obj) = item {
                        let values: HashMap<String, serde_json::Value> = obj.iter()
                            .map(|(k, v)| (k.clone(), v.clone()))
                            .collect();
                        rows.push(DataRow {
                            table: path.file_stem().unwrap_or_default().to_string_lossy().to_string(),
                            values,
                            row_number: i as u64 + 1,
                        });
                    }
                }
            }
            serde_json::Value::Object(obj) => {
                let values: HashMap<String, serde_json::Value> = obj.iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                rows.push(DataRow {
                    table: path.file_stem().unwrap_or_default().to_string_lossy().to_string(),
                    values,
                    row_number: 1,
                });
            }
            _ => {
                return Err(MigrationError {
                    table: path.to_string_lossy().to_string(),
                    row: None,
                    message: "JSON file must contain an array or object".to_string(),
                    code: None,
                    raw_data: None,
                });
            }
        }

        Ok(rows)
    }

    /// Convert a DataRow to OntoDB format.
    pub fn convert_row(
        &self,
        row: &DataRow,
        source: &SourceDatabase,
        type_mappings: &HashMap<String, String>,
    ) -> DataRow {
        let mut converted_values = HashMap::new();

        for (col_name, value) in &row.values {
            // Get the OntoDB type for this column
            let ontodb_type = type_mappings.get(col_name).map(|s| s.as_str()).unwrap_or("string");

            // Convert value based on type
            let converted_value = match ontodb_type {
                "integer" => {
                    match value {
                        serde_json::Value::Number(n) => value.clone(),
                        serde_json::Value::String(s) => {
                            s.parse::<i64>().map(|n| serde_json::json!(n)).unwrap_or(value.clone())
                        }
                        _ => value.clone(),
                    }
                }
                "long" => {
                    match value {
                        serde_json::Value::Number(n) => value.clone(),
                        serde_json::Value::String(s) => {
                            s.parse::<i64>().map(|n| serde_json::json!(n)).unwrap_or(value.clone())
                        }
                        _ => value.clone(),
                    }
                }
                "float" | "double" | "decimal" => {
                    match value {
                        serde_json::Value::Number(n) => value.clone(),
                        serde_json::Value::String(s) => {
                            s.parse::<f64>().map(|n| serde_json::json!(n)).unwrap_or(value.clone())
                        }
                        _ => value.clone(),
                    }
                }
                "boolean" => {
                    match value {
                        serde_json::Value::Bool(_) => value.clone(),
                        serde_json::Value::Number(n) => {
                            serde_json::json!(n.as_i64().unwrap_or(0) != 0)
                        }
                        serde_json::Value::String(s) => {
                            let lower = s.to_lowercase();
                            serde_json::json!(lower == "true" || lower == "1" || lower == "yes")
                        }
                        _ => value.clone(),
                    }
                }
                "json" => {
                    match value {
                        serde_json::Value::String(s) => {
                            serde_json::from_str::<serde_json::Value>(s).unwrap_or(value.clone())
                        }
                        _ => value.clone(),
                    }
                }
                _ => value.clone(), // string, text, date, etc. - no conversion needed
            };

            converted_values.insert(col_name.clone(), converted_value);
        }

        DataRow {
            table: row.table.clone(),
            values: converted_values,
            row_number: row.row_number,
        }
    }

    /// Get migration manager status.
    pub fn status(&self) -> MigrationStatusInfo {
        let jobs = self.jobs.read();
        MigrationStatusInfo {
            enabled: self.config.enabled,
            active_jobs: jobs.values().filter(|j| j.progress.status == MigrationStatus::InProgress).count(),
            total_jobs: jobs.len(),
            completed_jobs: jobs.values().filter(|j| j.progress.status == MigrationStatus::Completed).count(),
            failed_jobs: jobs.values().filter(|j| j.progress.status == MigrationStatus::Failed).count(),
        }
    }
}

/// Migration status information.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MigrationStatusInfo {
    pub enabled: bool,
    pub active_jobs: usize,
    pub total_jobs: usize,
    pub completed_jobs: usize,
    pub failed_jobs: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pg_type_mapping() {
        let manager = MigrationManager::new(MigrationConfig::default());

        assert_eq!(manager.convert_type(&SourceDatabase::PostgreSQL, "integer"), Some("integer".to_string()));
        assert_eq!(manager.convert_type(&SourceDatabase::PostgreSQL, "VARCHAR"), Some("string".to_string()));
        assert_eq!(manager.convert_type(&SourceDatabase::PostgreSQL, "timestamp"), Some("timestamp".to_string()));
        assert_eq!(manager.convert_type(&SourceDatabase::PostgreSQL, "boolean"), Some("boolean".to_string()));
    }

    #[test]
    fn test_mysql_type_mapping() {
        let manager = MigrationManager::new(MigrationConfig::default());

        assert_eq!(manager.convert_type(&SourceDatabase::MySQL, "INT"), Some("integer".to_string()));
        assert_eq!(manager.convert_type(&SourceDatabase::MySQL, "VARCHAR(255)"), Some("string".to_string()));
        assert_eq!(manager.convert_type(&SourceDatabase::MySQL, "DATETIME"), Some("timestamp".to_string()));
    }

    #[test]
    fn test_csv_parsing() {
        let tmp_dir = tempfile::TempDir::new().unwrap();
        let csv_path = tmp_dir.path().join("test.csv");
        std::fs::write(&csv_path, "name,age,email\nAlice,30,alice@example.com\nBob,25,bob@example.com").unwrap();

        let manager = MigrationManager::new(MigrationConfig::default());
        let rows = manager.parse_csv(&csv_path).unwrap();

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].values.get("name").unwrap(), &serde_json::json!("Alice"));
        assert_eq!(rows[1].values.get("age").unwrap(), &serde_json::json!("25"));
    }

    #[test]
    fn test_json_parsing() {
        let tmp_dir = tempfile::TempDir::new().unwrap();
        let json_path = tmp_dir.path().join("test.json");
        std::fs::write(&json_path, r#"[{"name":"Alice","age":30},{"name":"Bob","age":25}]"#).unwrap();

        let manager = MigrationManager::new(MigrationConfig::default());
        let rows = manager.parse_json(&json_path).unwrap();

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].values.get("name").unwrap(), &serde_json::json!("Alice"));
    }

    #[test]
    fn test_row_conversion() {
        let manager = MigrationManager::new(MigrationConfig::default());

        let mut type_mappings = HashMap::new();
        type_mappings.insert("age".to_string(), "integer".to_string());
        type_mappings.insert("name".to_string(), "string".to_string());

        let row = DataRow {
            table: "users".to_string(),
            values: {
                let mut m = HashMap::new();
                m.insert("name".to_string(), serde_json::json!("Alice"));
                m.insert("age".to_string(), serde_json::json!("30")); // String "30"
                m
            },
            row_number: 1,
        };

        let converted = manager.convert_row(&row, &SourceDatabase::PostgreSQL, &type_mappings);

        // age should be converted from string to integer
        assert_eq!(converted.values.get("age").unwrap(), &serde_json::json!(30));
        // name should remain as string
        assert_eq!(converted.values.get("name").unwrap(), &serde_json::json!("Alice"));
    }

    #[test]
    fn test_job_management() {
        let config = MigrationConfig {
            enabled: true,
            ..Default::default()
        };
        let manager = MigrationManager::new(config);

        let source = SourceConfig {
            source_type: SourceDatabase::PostgreSQL,
            connection_string: Some("postgresql://localhost/test".to_string()),
            file_path: None,
            database: Some("testdb".to_string()),
            schema: Some("public".to_string()),
            tables: vec!["users".to_string()],
            username: Some("postgres".to_string()),
            password: None,
            options: HashMap::new(),
        };

        let job_id = manager.create_job("Test Migration", source);
        assert!(!job_id.is_empty());

        let job = manager.get_job(&job_id).unwrap();
        assert_eq!(job.name, "Test Migration");
        assert_eq!(job.progress.status, MigrationStatus::NotStarted);

        let jobs = manager.list_jobs();
        assert_eq!(jobs.len(), 1);
    }

    #[test]
    fn test_status() {
        let config = MigrationConfig {
            enabled: true,
            ..Default::default()
        };
        let manager = MigrationManager::new(config);

        let status = manager.status();
        assert!(status.enabled);
        assert_eq!(status.total_jobs, 0);
    }
}
