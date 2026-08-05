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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_type_name() {
        assert_eq!(OntoValue::Null.type_name(), "null");
        assert_eq!(OntoValue::Bool(true).type_name(), "bool");
        assert_eq!(OntoValue::Int64(42).type_name(), "int64");
        assert_eq!(OntoValue::Float64(3.14).type_name(), "float64");
        assert_eq!(OntoValue::String("hello".into()).type_name(), "string");
        assert_eq!(OntoValue::Bytes(vec![1, 2]).type_name(), "bytes");
        assert_eq!(OntoValue::Array(vec![]).type_name(), "array");
        assert_eq!(OntoValue::Object(vec![]).type_name(), "object");
    }

    #[test]
    fn test_from_conversions() {
        assert_eq!(OntoValue::from(true), OntoValue::Bool(true));
        assert_eq!(OntoValue::from(42i64), OntoValue::Int64(42));
        assert_eq!(OntoValue::from(3.14f64), OntoValue::Float64(3.14));
        assert_eq!(OntoValue::from("hello"), OntoValue::String("hello".into()));
        assert_eq!(OntoValue::from(String::from("hello")), OntoValue::String("hello".into()));
    }

    #[test]
    fn test_from_json() {
        assert_eq!(OntoValue::from(serde_json::Value::Null), OntoValue::Null);
        assert_eq!(OntoValue::from(serde_json::json!(true)), OntoValue::Bool(true));
        assert_eq!(OntoValue::from(serde_json::json!(42)), OntoValue::Int64(42));
        assert_eq!(OntoValue::from(serde_json::json!("hello")), OntoValue::String("hello".into()));
    }

    #[test]
    fn test_bytes_roundtrip() {
        let values = vec![
            OntoValue::Null,
            OntoValue::Bool(true),
            OntoValue::Int64(-42),
            OntoValue::Float64(3.14),
            OntoValue::String("hello world".into()),
            OntoValue::Bytes(vec![0, 1, 2, 255]),
            OntoValue::Array(vec![OntoValue::Int64(1), OntoValue::String("two".into())]),
        ];

        for val in values {
            let bytes = val.to_bytes();
            let restored = OntoValue::from_bytes(&bytes).unwrap();
            assert_eq!(val, restored);
        }
    }

    #[test]
    fn test_from_json_array() {
        let json = serde_json::json!([1, "two", true]);
        let val = OntoValue::from(json);
        match val {
            OntoValue::Array(arr) => {
                assert_eq!(arr.len(), 3);
                assert_eq!(arr[0], OntoValue::Int64(1));
                assert_eq!(arr[1], OntoValue::String("two".into()));
                assert_eq!(arr[2], OntoValue::Bool(true));
            }
            _ => panic!("expected Array"),
        }
    }

    #[test]
    fn test_from_json_object() {
        let json = serde_json::json!({"name": "Alice", "age": 30});
        let val = OntoValue::from(json);
        match val {
            OntoValue::Object(obj) => {
                assert_eq!(obj.len(), 2);
                let name = obj.iter().find(|(k, _)| k == "name").unwrap();
                assert_eq!(name.1, OntoValue::String("Alice".into()));
                let age = obj.iter().find(|(k, _)| k == "age").unwrap();
                assert_eq!(age.1, OntoValue::Int64(30));
            }
            _ => panic!("expected Object"),
        }
    }
}
