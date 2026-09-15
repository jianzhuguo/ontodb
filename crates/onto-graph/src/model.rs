// Copyright (c) 2024-2026 OntoDB Team
// Licensed under the Business Source License 1.1 (BUSL-1.1).
// See LICENSE for details. Change Date: 2031-09-15.
// On the Change Date, this file will be licensed under Apache License 2.0.
//! Graph data model - Property Graph with vertices and edges.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Property value types supported in the graph.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum PropValue {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    List(Vec<PropValue>),
}

impl PartialOrd for PropValue {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        match (self, other) {
            (PropValue::Int(a), PropValue::Int(b)) => a.partial_cmp(b),
            (PropValue::Float(a), PropValue::Float(b)) => a.partial_cmp(b),
            (PropValue::String(a), PropValue::String(b)) => a.partial_cmp(b),
            (PropValue::Bool(a), PropValue::Bool(b)) => a.partial_cmp(b),
            _ => None,
        }
    }
}

impl std::fmt::Display for PropValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PropValue::Null => write!(f, "null"),
            PropValue::Bool(b) => write!(f, "{}", b),
            PropValue::Int(i) => write!(f, "{}", i),
            PropValue::Float(fl) => write!(f, "{}", fl),
            PropValue::String(s) => write!(f, "{}", s),
            PropValue::List(l) => {
                write!(f, "[")?;
                for (i, v) in l.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", v)?;
                }
                write!(f, "]")
            }
        }
    }
}

/// Property map for vertices and edges.
pub type PropertyMap = HashMap<String, PropValue>;

/// A vertex in the graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Vertex {
    /// Unique vertex ID.
    pub id: String,
    /// Vertex labels (e.g., "Person", "Employee").
    pub labels: Vec<String>,
    /// Properties.
    pub properties: PropertyMap,
}

impl Vertex {
    pub fn new(id: impl Into<String>, labels: Vec<String>) -> Self {
        Self {
            id: id.into(),
            labels,
            properties: HashMap::new(),
        }
    }

    pub fn with_property(mut self, key: impl Into<String>, value: PropValue) -> Self {
        self.properties.insert(key.into(), value);
        self
    }
}

/// An edge in the graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    /// Unique edge ID.
    pub id: String,
    /// Source vertex ID.
    pub from: String,
    /// Target vertex ID.
    pub to: String,
    /// Edge label (e.g., "KNOWS", "WORKS_AT").
    pub label: String,
    /// Properties.
    pub properties: PropertyMap,
}

impl Edge {
    pub fn new(
        id: impl Into<String>,
        from: impl Into<String>,
        to: impl Into<String>,
        label: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            from: from.into(),
            to: to.into(),
            label: label.into(),
            properties: HashMap::new(),
        }
    }

    pub fn with_property(mut self, key: impl Into<String>, value: PropValue) -> Self {
        self.properties.insert(key.into(), value);
        self
    }
}

/// A graph element (either vertex or edge).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GraphElement {
    Vertex(Vertex),
    Edge(Edge),
}
