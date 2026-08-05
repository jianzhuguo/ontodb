//! Ontology-aware value types for OntoDB.
//!
//! These types represent the semantic data model that sits above raw bytes.

use serde::{Deserialize, Serialize};

/// A typed value in the OntoDB data model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum OntoValue {
    Null,
    Bool(bool),
    Int64(i64),
    Float64(f64),
    String(String),
    Bytes(Vec<u8>),
    Array(Vec<OntoValue>),
    Object(Vec<(String, OntoValue)>),
}

impl OntoValue {
    /// Returns the type name of this value.
    pub fn type_name(&self) -> &'static str {
        match self {
            OntoValue::Null => "null",
            OntoValue::Bool(_) => "bool",
            OntoValue::Int64(_) => "int64",
            OntoValue::Float64(_) => "float64",
            OntoValue::String(_) => "string",
            OntoValue::Bytes(_) => "bytes",
            OntoValue::Array(_) => "array",
            OntoValue::Object(_) => "object",
        }
    }

    /// Serialize to bytes for storage.
    pub fn to_bytes(&self) -> Vec<u8> {
        bincode::serialize(self).expect("serialization should not fail")
    }

    /// Deserialize from bytes.
    pub fn from_bytes(bytes: &[u8]) -> crate::Result<Self> {
        bincode::deserialize(bytes).map_err(|e| crate::CoreError::Serialization(e.to_string()))
    }
}

impl From<bool> for OntoValue {
    fn from(v: bool) -> Self {
        OntoValue::Bool(v)
    }
}

impl From<i64> for OntoValue {
    fn from(v: i64) -> Self {
        OntoValue::Int64(v)
    }
}

impl From<f64> for OntoValue {
    fn from(v: f64) -> Self {
        OntoValue::Float64(v)
    }
}

impl From<String> for OntoValue {
    fn from(v: String) -> Self {
        OntoValue::String(v)
    }
}

impl From<&str> for OntoValue {
    fn from(v: &str) -> Self {
        OntoValue::String(v.to_string())
    }
}

impl From<serde_json::Value> for OntoValue {
    fn from(v: serde_json::Value) -> Self {
        match v {
            serde_json::Value::Null => OntoValue::Null,
            serde_json::Value::Bool(b) => OntoValue::Bool(b),
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    OntoValue::Int64(i)
                } else if let Some(f) = n.as_f64() {
                    OntoValue::Float64(f)
                } else {
                    OntoValue::Null
                }
            }
            serde_json::Value::String(s) => OntoValue::String(s),
            serde_json::Value::Array(arr) => {
                OntoValue::Array(arr.into_iter().map(OntoValue::from).collect())
            }
            serde_json::Value::Object(map) => OntoValue::Object(
                map.into_iter()
                    .map(|(k, v)| (k, OntoValue::from(v)))
                    .collect(),
            ),
        }
    }
}
