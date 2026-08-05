//! Ontology data model.
//!
//! This defines the core types for representing ontologies:
//! - Class: A type/category of entities
//! - Property: An attribute that belongs to a class
//! - Ontology: A named collection of classes and properties

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A named ontology containing classes and properties.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ontology {
    pub name: String,
    pub classes: HashMap<String, Class>,
    pub properties: HashMap<String, Property>,
}

/// A class in the ontology (like a type or category).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Class {
    pub name: String,
    pub description: Option<String>,
    /// Parent classes (inheritance).
    pub superclasses: Vec<String>,
    /// Properties that belong to this class.
    pub properties: Vec<String>,
}

/// A property (attribute) in the ontology.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Property {
    pub name: String,
    pub description: Option<String>,
    /// The class this property belongs to.
    pub domain: String,
    /// The data type of this property's values.
    pub range: DataType,
    /// Whether this property is required.
    pub required: bool,
    /// Whether this property can have multiple values.
    pub multi_valued: bool,
}

/// Supported data types for property values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DataType {
    String,
    Int64,
    Float64,
    Bool,
    Bytes,
    Array,
    Object,
}

impl DataType {
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_uppercase().as_str() {
            "STRING" | "STR" | "VARCHAR" | "TEXT" => Some(DataType::String),
            "INT64" | "INT" | "INTEGER" | "BIGINT" => Some(DataType::Int64),
            "FLOAT64" | "FLOAT" | "DOUBLE" | "DECIMAL" | "NUMERIC" => Some(DataType::Float64),
            "BOOL" | "BOOLEAN" => Some(DataType::Bool),
            "BYTES" | "BINARY" | "BLOB" => Some(DataType::Bytes),
            "ARRAY" | "LIST" => Some(DataType::Array),
            "OBJECT" | "MAP" | "JSON" => Some(DataType::Object),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            DataType::String => "STRING",
            DataType::Int64 => "INT64",
            DataType::Float64 => "FLOAT64",
            DataType::Bool => "BOOL",
            DataType::Bytes => "BYTES",
            DataType::Array => "ARRAY",
            DataType::Object => "OBJECT",
        }
    }
}

impl Ontology {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            classes: HashMap::new(),
            properties: HashMap::new(),
        }
    }

    /// Adds a class to the ontology.
    pub fn add_class(&mut self, class: Class) {
        self.classes.insert(class.name.clone(), class);
    }

    /// Adds a property to the ontology.
    pub fn add_property(&mut self, property: Property) {
        // Auto-register the property in its domain class
        if let Some(class) = self.classes.get_mut(&property.domain) {
            if !class.properties.contains(&property.name) {
                class.properties.push(property.name.clone());
            }
        }
        self.properties.insert(property.name.clone(), property);
    }

    /// Gets a class by name, including inherited properties.
    pub fn get_class(&self, name: &str) -> Option<&Class> {
        self.classes.get(name)
    }

    /// Gets all properties for a class, including inherited ones.
    pub fn get_class_properties(&self, class_name: &str) -> Vec<&Property> {
        let mut props = Vec::new();
        let mut visited = std::collections::HashSet::new();
        self.collect_properties(class_name, &mut props, &mut visited);
        props
    }

    /// Checks if a class is a subclass of another (directly or indirectly).
    pub fn is_subclass_of(&self, child: &str, parent: &str) -> bool {
        if child == parent {
            return true;
        }

        if let Some(class) = self.classes.get(child) {
            for superclass in &class.superclasses {
                if self.is_subclass_of(superclass, parent) {
                    return true;
                }
            }
        }

        false
    }

    fn collect_properties<'a>(
        &'a self,
        class_name: &str,
        props: &mut Vec<&'a Property>,
        visited: &mut std::collections::HashSet<String>,
    ) {
        if !visited.insert(class_name.to_string()) {
            return; // Avoid cycles
        }

        if let Some(class) = self.classes.get(class_name) {
            // Collect own properties
            for prop_name in &class.properties {
                if let Some(prop) = self.properties.get(prop_name) {
                    props.push(prop);
                }
            }

            // Recurse into superclasses
            for superclass in &class.superclasses {
                self.collect_properties(superclass, props, visited);
            }
        }
    }
}

impl Class {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: None,
            superclasses: Vec::new(),
            properties: Vec::new(),
        }
    }

    pub fn with_superclass(mut self, superclass: impl Into<String>) -> Self {
        self.superclasses.push(superclass.into());
        self
    }

    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = Some(desc.into());
        self
    }
}

impl Property {
    pub fn new(
        name: impl Into<String>,
        domain: impl Into<String>,
        range: DataType,
    ) -> Self {
        Self {
            name: name.into(),
            description: None,
            domain: domain.into(),
            range,
            required: false,
            multi_valued: false,
        }
    }

    pub fn required(mut self) -> Self {
        self.required = true;
        self
    }

    pub fn multi_valued(mut self) -> Self {
        self.multi_valued = true;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ontology_basic() {
        let mut onto = Ontology::new("shop");

        onto.add_class(Class::new("Product"));
        onto.add_class(Class::new("Customer"));

        onto.add_property(Property::new("name", "Product", DataType::String).required());
        onto.add_property(Property::new("price", "Product", DataType::Float64));
        onto.add_property(Property::new("email", "Customer", DataType::String));

        assert!(onto.get_class("Product").is_some());
        assert!(onto.get_class("Missing").is_none());

        let props = onto.get_class_properties("Product");
        assert_eq!(props.len(), 2);
    }

    #[test]
    fn test_ontology_inheritance() {
        let mut onto = Ontology::new("test");

        onto.add_class(Class::new("Person"));
        onto.add_class(Class::new("Employee").with_superclass("Person"));

        onto.add_property(Property::new("name", "Person", DataType::String));
        onto.add_property(Property::new("salary", "Employee", DataType::Float64));

        assert!(onto.is_subclass_of("Employee", "Person"));
        assert!(!onto.is_subclass_of("Person", "Employee"));

        // Employee should have both name and salary
        let props = onto.get_class_properties("Employee");
        assert_eq!(props.len(), 2);
    }
}
