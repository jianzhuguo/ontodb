//! Ontology data model.
//!
//! This defines the core types for representing ontologies:
//! - Class: A type/category of entities
//! - Property: An attribute that belongs to a class
//! - Ontology: A named collection of classes and properties

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// A named ontology containing classes and properties.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ontology {
    pub name: String,
    pub classes: HashMap<String, Class>,
    pub properties: HashMap<String, Property>,
    /// Reverse index: parent class → direct children. Rebuilt by `rebuild_indexes()`.
    #[serde(skip)]
    children_of: HashMap<String, Vec<String>>,
    /// Reverse index: class → classes that declare it as equivalent. Rebuilt by `rebuild_indexes()`.
    #[serde(skip)]
    equiv_of: HashMap<String, Vec<String>>,
}

/// OWL-lite class type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[derive(Default)]
pub enum ClassType {
    /// Normal class.
    #[default]
    Normal,
    /// Enumerated class with fixed instances.
    Enum(Vec<String>),
    /// Union of other classes.
    Union(Vec<String>),
    /// Intersection of other classes.
    Intersection(Vec<String>),
}

/// OWL restriction on a property.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Restriction {
    /// owl:someValuesFrom — at least one value must be from the given class.
    SomeValuesFrom { property: String, class: String },
    /// owl:allValuesFrom — all values must be from the given class.
    AllValuesFrom { property: String, class: String },
    /// owl:hasValue — the property must have this specific value.
    HasValue { property: String, value: Literal },
    /// owl:minCardinality — minimum number of values.
    MinCardinality { property: String, min: usize },
    /// owl:maxCardinality — maximum number of values.
    MaxCardinality { property: String, max: usize },
    /// owl:cardinality — exact number of values.
    ExactCardinality { property: String, count: usize },
}

/// A literal value used in restrictions and assertions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Literal {
    String(String),
    Int(i64),
    Float(#[serde(with = "ordered_f64")] f64),
    Bool(bool),
}

impl PartialEq for Literal {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Literal::String(a), Literal::String(b)) => a == b,
            (Literal::Int(a), Literal::Int(b)) => a == b,
            (Literal::Float(a), Literal::Float(b)) => a.to_bits() == b.to_bits(),
            (Literal::Bool(a), Literal::Bool(b)) => a == b,
            _ => false,
        }
    }
}

/// Wrapper to allow `Eq` on `f64` by treating bitwise-equal values as equal.
/// This is safe for ontology literals where NaN is not expected.
mod ordered_f64 {
    use serde::{self, Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S>(val: &f64, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        val.serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<f64, D::Error>
    where
        D: Deserializer<'de>,
    {
        f64::deserialize(deserializer)
    }
}

impl Eq for Literal {}


/// A class in the ontology (like a type or category).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Class {
    pub name: String,
    pub description: Option<String>,
    /// Parent classes (inheritance).
    pub superclasses: Vec<String>,
    /// Properties that belong to this class.
    pub properties: Vec<String>,
    /// OWL-lite: equivalent classes (owl:equivalentClass).
    #[serde(default)]
    pub equivalent_classes: Vec<String>,
    /// OWL-lite: disjoint classes (owl:disjointWith).
    #[serde(default)]
    pub disjoint_with: Vec<String>,
    /// OWL-lite: class type (normal, enum, union).
    #[serde(default)]
    pub class_type: ClassType,
    /// OWL-lite: property restrictions (owl:Restriction).
    #[serde(default)]
    pub restrictions: Vec<Restriction>,
    /// Unique constraints — each entry is a list of column names that must be unique together.
    /// Single-column: `UNIQUE(email)` → `vec![vec!["email"]]`
    /// Composite: `UNIQUE(first_name, last_name)` → `vec![vec!["first_name", "last_name"]]`
    #[serde(default)]
    pub unique_columns: Vec<Vec<String>>,
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
    /// OWL-lite: equivalent properties (owl:equivalentProperty).
    #[serde(default)]
    pub equivalent_properties: Vec<String>,
    /// OWL-lite: inverse property (owl:inverseOf).
    #[serde(default)]
    pub inverse_of: Option<String>,
    /// OWL-lite: transitive property (owl:transitiveProperty).
    #[serde(default)]
    pub is_transitive: bool,
    /// OWL-lite: symmetric property (owl:symmetricProperty).
    #[serde(default)]
    pub is_symmetric: bool,
    /// OWL-lite: functional property (owl:FunctionalProperty).
    #[serde(default)]
    pub is_functional: bool,
    /// OWL-lite: subproperty hierarchy (rdfs:subPropertyOf).
    #[serde(default)]
    pub subproperty_of: Vec<String>,
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
            children_of: HashMap::new(),
            equiv_of: HashMap::new(),
        }
    }

    /// Adds a class to the ontology and updates reverse indexes.
    pub fn add_class(&mut self, class: Class) {
        // Update children_of reverse index
        for parent in &class.superclasses {
            self.children_of
                .entry(parent.clone())
                .or_default()
                .push(class.name.clone());
        }
        // Update equiv_of reverse index (bidirectional)
        for equiv in &class.equivalent_classes {
            self.equiv_of
                .entry(equiv.clone())
                .or_default()
                .push(class.name.clone());
        }
        self.classes.insert(class.name.clone(), class);
    }

    /// Rebuilds reverse indexes after deserialization or bulk class modification.
    pub fn rebuild_indexes(&mut self) {
        self.children_of.clear();
        self.equiv_of.clear();
        for (name, class) in &self.classes {
            for parent in &class.superclasses {
                self.children_of
                    .entry(parent.clone())
                    .or_default()
                    .push(name.clone());
            }
            for equiv in &class.equivalent_classes {
                self.equiv_of
                    .entry(equiv.clone())
                    .or_default()
                    .push(name.clone());
            }
        }
    }

    /// Deserializes an Ontology from JSON bytes and rebuilds reverse indexes.
    pub fn from_json_slice(bytes: &[u8]) -> std::result::Result<Self, serde_json::Error> {
        let mut ontology: Ontology = serde_json::from_slice(bytes)?;
        ontology.rebuild_indexes();
        Ok(ontology)
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
        let mut visited = std::collections::HashSet::new();
        visited.insert(child.to_string());
        self.is_subclass_of_inner(child, parent, &mut visited)
    }

    fn is_subclass_of_inner(
        &self,
        child: &str,
        parent: &str,
        visited: &mut std::collections::HashSet<String>,
    ) -> bool {
        if let Some(class) = self.classes.get(child) {
            for superclass in &class.superclasses {
                if superclass == parent {
                    return true;
                }
                if visited.insert(superclass.clone())
                    && self.is_subclass_of_inner(superclass, parent, visited) {
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

    /// Gets all subclasses of a class (direct and indirect), including equivalent classes.
    pub fn get_all_subclasses(&self, class_name: &str) -> HashSet<String> {
        let mut result = HashSet::new();
        let mut visited = HashSet::new();
        self.collect_subclasses(class_name, &mut result, &mut visited);
        result
    }

    fn collect_subclasses(
        &self,
        class_name: &str,
        result: &mut HashSet<String>,
        visited: &mut HashSet<String>,
    ) {
        if !visited.insert(class_name.to_string()) {
            return;
        }

        // Add direct subclasses via reverse index
        if let Some(children) = self.children_of.get(class_name) {
            for child in children {
                if result.insert(child.clone()) {
                    self.collect_subclasses(child, result, visited);
                }
            }
        }

        // Add equivalent classes and their subclasses (bidirectional)
        if let Some(class) = self.classes.get(class_name) {
            for equiv in &class.equivalent_classes {
                if result.insert(equiv.clone()) {
                    self.collect_subclasses(equiv, result, visited);
                }
            }
        }

        // Add classes that declare class_name as equivalent (via reverse index)
        if let Some(equivs) = self.equiv_of.get(class_name) {
            for equiv in equivs {
                if result.insert(equiv.clone()) {
                    self.collect_subclasses(equiv, result, visited);
                }
            }
        }
    }

    /// Gets all superclasses of a class (direct and indirect), including equivalent classes.
    /// Uses memoized cache for repeated lookups.
    pub fn get_all_superclasses(&self, class_name: &str) -> HashSet<String> {
        let mut result = HashSet::new();
        let mut visited = HashSet::new();
        self.collect_superclasses(class_name, &mut result, &mut visited);
        result
    }

    /// Pre-computes the full superclass map for all classes in one pass.
    /// Call once before batch reasoning to avoid per-fact recursive traversal.
    pub fn build_superclass_cache(&self) -> HashMap<String, HashSet<String>> {
        let mut cache = HashMap::new();
        for class_name in self.classes.keys() {
            let supers = self.get_all_superclasses(class_name);
            cache.insert(class_name.clone(), supers);
        }
        cache
    }

    fn collect_superclasses(
        &self,
        class_name: &str,
        result: &mut HashSet<String>,
        visited: &mut HashSet<String>,
    ) {
        if !visited.insert(class_name.to_string()) {
            return;
        }

        if let Some(class) = self.classes.get(class_name) {
            // Add direct superclasses
            for superclass in &class.superclasses {
                result.insert(superclass.clone());
                self.collect_superclasses(superclass, result, visited);
            }

            // Add equivalent classes and their superclasses
            for equiv in &class.equivalent_classes {
                if result.insert(equiv.clone()) {
                    self.collect_superclasses(equiv, result, visited);
                }
            }
        }
    }

    /// Checks if two classes are equivalent.
    pub fn is_equivalent(&self, class1: &str, class2: &str) -> bool {
        if class1 == class2 {
            return true;
        }

        if let Some(class) = self.classes.get(class1) {
            if class.equivalent_classes.contains(&class2.to_string()) {
                return true;
            }
        }

        if let Some(class) = self.classes.get(class2) {
            if class.equivalent_classes.contains(&class1.to_string()) {
                return true;
            }
        }

        false
    }

    /// Checks if two classes are disjoint.
    pub fn is_disjoint(&self, class1: &str, class2: &str) -> bool {
        if let Some(class) = self.classes.get(class1) {
            if class.disjoint_with.contains(&class2.to_string()) {
                return true;
            }
        }

        if let Some(class) = self.classes.get(class2) {
            if class.disjoint_with.contains(&class1.to_string()) {
                return true;
            }
        }

        false
    }

    /// Validates that no disjoint classes have common subclasses.
    pub fn validate_disjoint_constraints(&self) -> Vec<String> {
        let mut errors = Vec::new();

        for (name, class) in &self.classes {
            for disjoint_class_name in &class.disjoint_with {
                // Check if any subclass of name is also a subclass of disjoint_class_name
                let subclasses1 = self.get_all_subclasses(name);
                let subclasses2 = self.get_all_subclasses(disjoint_class_name);

                for common in subclasses1.intersection(&subclasses2) {
                    errors.push(format!(
                        "Class '{}' is a subclass of both '{}' and '{}' which are disjoint",
                        common, name, disjoint_class_name
                    ));
                }
            }
        }

        errors
    }

    /// Full ontology validation. Returns all errors found.
    ///
    /// Checks:
    /// 1. Cycle inheritance: A extends B extends A
    /// 2. Cycle equivalence: A equiv B equiv A
    /// 3. Disjoint constraint violations
    /// 4. Undefined superclasses
    /// 5. Undefined equivalent classes
    /// 6. Undefined property domains
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();

        // 1. Detect cycle inheritance
        errors.extend(self.validate_no_cycles());

        // 2. Detect cycle equivalence
        errors.extend(self.validate_no_equivalent_cycles());

        // 3. Disjoint constraint violations
        errors.extend(self.validate_disjoint_constraints());

        // 4. Undefined superclasses
        errors.extend(self.validate_superclass_references());

        // 5. Undefined equivalent classes
        errors.extend(self.validate_equivalent_class_references());

        // 6. Undefined property domains
        errors.extend(self.validate_property_domains());

        errors
    }

    /// Detects cycle inheritance: A extends B extends ... extends A.
    ///
    /// Uses DFS with gray/black marking:
    /// - Gray = currently on the recursion stack (ancestor)
    /// - Black = fully explored
    fn validate_no_cycles(&self) -> Vec<String> {
        let mut errors = Vec::new();
        let mut white: HashSet<String> = self.classes.keys().cloned().collect();
        let mut gray: HashSet<String> = HashSet::new();
        let mut black: HashSet<String> = HashSet::new();

        while let Some(class_name) = white.iter().next().cloned() {
            white.remove(&class_name);
            self.dfs_cycle_check(
                &class_name,
                &mut white,
                &mut gray,
                &mut black,
                &mut Vec::new(),
                &mut errors,
            );
        }

        errors
    }

    fn dfs_cycle_check(
        &self,
        class_name: &str,
        white: &mut HashSet<String>,
        gray: &mut HashSet<String>,
        black: &mut HashSet<String>,
        path: &mut Vec<String>,
        errors: &mut Vec<String>,
    ) {
        white.remove(class_name);
        gray.insert(class_name.to_string());
        path.push(class_name.to_string());

        if let Some(class) = self.classes.get(class_name) {
            for superclass in &class.superclasses {
                if gray.contains(superclass.as_str()) {
                    // Found a cycle — report the cycle path
                    let cycle_start = path.iter().position(|c| c == superclass).unwrap_or(0);
                    let cycle_path: Vec<String> = path[cycle_start..].to_vec();
                    errors.push(format!(
                        "Circular inheritance detected: {} -> {}",
                        cycle_path.join(" -> "), superclass
                    ));
                } else if !black.contains(superclass.as_str()) {
                    self.dfs_cycle_check(superclass, white, gray, black, path, errors);
                }
            }
        }

        path.pop();
        gray.remove(class_name);
        black.insert(class_name.to_string());
    }

    /// Detects cycle equivalence: A equiv B equiv ... equiv A.
    fn validate_no_equivalent_cycles(&self) -> Vec<String> {
        let mut errors = Vec::new();
        let mut visited: HashSet<String> = HashSet::new();

        for class_name in self.classes.keys() {
            if visited.contains(class_name) {
                continue;
            }
            let mut chain = Vec::new();
            let mut current = class_name.clone();
            let mut seen_in_chain: HashSet<String> = HashSet::new();

            while !visited.contains(&current) && !seen_in_chain.contains(&current) {
                seen_in_chain.insert(current.clone());
                chain.push(current.clone());

                if let Some(class) = self.classes.get(&current) {
                    if let Some(next) = class.equivalent_classes.first() {
                        current = next.clone();
                        continue;
                    }
                }
                break;
            }

            if seen_in_chain.contains(&current) && chain.len() > 1 {
                // Found a cycle in equivalence chain
                if let Some(start) = chain.iter().position(|c| c == &current) {
                    let cycle: Vec<String> = chain[start..].to_vec();
                    errors.push(format!(
                        "Circular equivalence detected: {} -> {}",
                        cycle.join(" <=> "), current
                    ));
                }
            }

            for c in &chain {
                visited.insert(c.clone());
            }
        }

        errors
    }

    /// Validates that all superclass references point to existing classes.
    fn validate_superclass_references(&self) -> Vec<String> {
        let mut errors = Vec::new();
        for (name, class) in &self.classes {
            for superclass in &class.superclasses {
                if !self.classes.contains_key(superclass.as_str()) {
                    errors.push(format!(
                        "Class '{}' references undefined superclass '{}'",
                        name, superclass
                    ));
                }
            }
        }
        errors
    }

    /// Validates that all equivalent class references point to existing classes.
    fn validate_equivalent_class_references(&self) -> Vec<String> {
        let mut errors = Vec::new();
        for (name, class) in &self.classes {
            for equiv in &class.equivalent_classes {
                if !self.classes.contains_key(equiv.as_str()) {
                    errors.push(format!(
                        "Class '{}' references undefined equivalent class '{}'",
                        name, equiv
                    ));
                }
            }
        }
        errors
    }

    /// Validates that all property domains reference existing classes.
    fn validate_property_domains(&self) -> Vec<String> {
        let mut errors = Vec::new();
        for (name, prop) in &self.properties {
            if !self.classes.contains_key(prop.domain.as_str()) {
                errors.push(format!(
                    "Property '{}' references undefined domain class '{}'",
                    name, prop.domain
                ));
            }
        }
        errors
    }
}

impl Class {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: None,
            superclasses: Vec::new(),
            properties: Vec::new(),
            equivalent_classes: Vec::new(),
            disjoint_with: Vec::new(),
            class_type: ClassType::Normal,
            restrictions: Vec::new(),
            unique_columns: Vec::new(),
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

    pub fn with_equivalent_class(mut self, equiv: impl Into<String>) -> Self {
        self.equivalent_classes.push(equiv.into());
        self
    }

    pub fn with_disjoint(mut self, disjoint: impl Into<String>) -> Self {
        self.disjoint_with.push(disjoint.into());
        self
    }

    pub fn with_class_type(mut self, class_type: ClassType) -> Self {
        self.class_type = class_type;
        self
    }

    pub fn with_restriction(mut self, restriction: Restriction) -> Self {
        self.restrictions.push(restriction);
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
            equivalent_properties: Vec::new(),
            inverse_of: None,
            is_transitive: false,
            is_symmetric: false,
            is_functional: false,
            subproperty_of: Vec::new(),
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

    pub fn with_inverse_of(mut self, prop: impl Into<String>) -> Self {
        self.inverse_of = Some(prop.into());
        self
    }

    pub fn transitive(mut self) -> Self {
        self.is_transitive = true;
        self
    }

    pub fn symmetric(mut self) -> Self {
        self.is_symmetric = true;
        self
    }

    pub fn functional(mut self) -> Self {
        self.is_functional = true;
        self
    }

    pub fn with_subproperty_of(mut self, prop: impl Into<String>) -> Self {
        self.subproperty_of.push(prop.into());
        self
    }

    pub fn with_equivalent_property(mut self, prop: impl Into<String>) -> Self {
        self.equivalent_properties.push(prop.into());
        self
    }
}

/// A property assertion on an individual (subject-predicate-object).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PropertyAssertion {
    pub property: String,
    pub value: AssertionValue,
}

/// The value side of a property assertion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AssertionValue {
    /// Reference to another individual by name.
    Individual(String),
    /// A literal value.
    Literal(Literal),
}

/// An individual (instance) of a class in the ontology.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Individual {
    pub name: String,
    /// The class this individual belongs to (rdf:type).
    pub class_name: String,
    /// Property assertions for this individual.
    pub assertions: Vec<PropertyAssertion>,
}

impl Individual {
    pub fn new(name: impl Into<String>, class_name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            class_name: class_name.into(),
            assertions: Vec::new(),
        }
    }

    pub fn with_assertion(mut self, property: impl Into<String>, value: AssertionValue) -> Self {
        self.assertions.push(PropertyAssertion {
            property: property.into(),
            value,
        });
        self
    }
}

/// A triple in SPO (subject-predicate-object) form, used by the reasoner.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
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

    /// Creates a type assertion triple: `subject rdf:type class_name`.
    pub fn type_of(subject: impl Into<String>, class_name: impl Into<String>) -> Self {
        Self::new(subject, "rdf:type", class_name)
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

    #[test]
    fn test_equivalent_classes() {
        let mut onto = Ontology::new("test");

        onto.add_class(Class::new("Student").with_equivalent_class("Learner"));
        onto.add_class(Class::new("Learner").with_equivalent_class("Student"));

        assert!(onto.is_equivalent("Student", "Learner"));
        assert!(onto.is_equivalent("Learner", "Student"));
        assert!(!onto.is_equivalent("Student", "Teacher"));
    }

    #[test]
    fn test_disjoint_classes() {
        let mut onto = Ontology::new("test");

        onto.add_class(Class::new("Manager").with_disjoint("Developer"));
        onto.add_class(Class::new("Developer").with_disjoint("Manager"));

        assert!(onto.is_disjoint("Manager", "Developer"));
        assert!(onto.is_disjoint("Developer", "Manager"));
        assert!(!onto.is_disjoint("Manager", "Employee"));
    }

    #[test]
    fn test_get_all_subclasses() {
        let mut onto = Ontology::new("test");

        onto.add_class(Class::new("Person"));
        onto.add_class(Class::new("Employee").with_superclass("Person"));
        onto.add_class(Class::new("Manager").with_superclass("Employee"));
        onto.add_class(Class::new("Developer").with_superclass("Employee"));

        let subclasses = onto.get_all_subclasses("Person");
        assert!(subclasses.contains("Employee"));
        assert!(subclasses.contains("Manager"));
        assert!(subclasses.contains("Developer"));
        assert_eq!(subclasses.len(), 3);

        let emp_subclasses = onto.get_all_subclasses("Employee");
        assert!(emp_subclasses.contains("Manager"));
        assert!(emp_subclasses.contains("Developer"));
        assert_eq!(emp_subclasses.len(), 2);
    }

    #[test]
    fn test_get_all_subclasses_with_equivalent() {
        let mut onto = Ontology::new("test");

        onto.add_class(Class::new("Person"));
        onto.add_class(
            Class::new("Employee")
                .with_superclass("Person")
                .with_equivalent_class("Worker"),
        );
        onto.add_class(Class::new("Worker").with_equivalent_class("Employee"));

        let subclasses = onto.get_all_subclasses("Person");
        assert!(subclasses.contains("Employee"));
        assert!(subclasses.contains("Worker"));
    }

    #[test]
    fn test_get_all_superclasses() {
        let mut onto = Ontology::new("test");

        onto.add_class(Class::new("Entity"));
        onto.add_class(Class::new("Person").with_superclass("Entity"));
        onto.add_class(Class::new("Employee").with_superclass("Person"));

        let superclasses = onto.get_all_superclasses("Employee");
        assert!(superclasses.contains("Person"));
        assert!(superclasses.contains("Entity"));
        assert_eq!(superclasses.len(), 2);
    }

    #[test]
    fn test_disjoint_constraint_validation() {
        let mut onto = Ontology::new("test");

        onto.add_class(Class::new("Animal"));
        onto.add_class(Class::new("Dog").with_superclass("Animal"));
        onto.add_class(Class::new("Cat").with_superclass("Animal"));

        // No disjoint constraints - should be valid
        assert!(onto.validate_disjoint_constraints().is_empty());

        // Add disjoint constraint
        onto.classes.get_mut("Dog").unwrap().disjoint_with.push("Cat".to_string());
        onto.classes.get_mut("Cat").unwrap().disjoint_with.push("Dog".to_string());

        // Still valid because Dog and Cat don't share subclasses
        assert!(onto.validate_disjoint_constraints().is_empty());
    }

    #[test]
    fn test_transitive_property() {
        let prop = Property::new("ancestor", "Person", DataType::String).transitive();
        assert!(prop.is_transitive);
        assert!(!prop.is_symmetric);
    }

    #[test]
    fn test_symmetric_property() {
        let prop = Property::new("friend", "Person", DataType::String).symmetric();
        assert!(prop.is_symmetric);
        assert!(!prop.is_transitive);
    }

    #[test]
    fn test_functional_property() {
        let prop = Property::new("ssn", "Person", DataType::String).functional();
        assert!(prop.is_functional);
    }

    #[test]
    fn test_inverse_property() {
        let prop = Property::new("worksFor", "Employee", DataType::String)
            .with_inverse_of("employs");
        assert_eq!(prop.inverse_of, Some("employs".to_string()));
    }

    #[test]
    fn test_intersection_class_type() {
        let mut onto = Ontology::new("test");
        onto.add_class(
            Class::new("WorkingStudent")
                .with_class_type(ClassType::Intersection(vec![
                    "Employee".to_string(),
                    "Student".to_string(),
                ])),
        );
        match &onto.classes["WorkingStudent"].class_type {
            ClassType::Intersection(parents) => {
                assert_eq!(parents.len(), 2);
                assert!(parents.contains(&"Employee".to_string()));
                assert!(parents.contains(&"Student".to_string()));
            }
            _ => panic!("expected Intersection"),
        }
    }

    #[test]
    fn test_restriction_some_values_from() {
        let class = Class::new("Parent").with_restriction(Restriction::SomeValuesFrom {
            property: "hasChild".to_string(),
            class: "Person".to_string(),
        });
        assert_eq!(class.restrictions.len(), 1);
        match &class.restrictions[0] {
            Restriction::SomeValuesFrom { property, class } => {
                assert_eq!(property, "hasChild");
                assert_eq!(class, "Person");
            }
            _ => panic!("expected SomeValuesFrom"),
        }
    }

    #[test]
    fn test_restriction_cardinality() {
        let class = Class::new("Couple").with_restriction(Restriction::ExactCardinality {
            property: "hasSpouse".to_string(),
            count: 1,
        });
        match &class.restrictions[0] {
            Restriction::ExactCardinality { property, count } => {
                assert_eq!(property, "hasSpouse");
                assert_eq!(*count, 1);
            }
            _ => panic!("expected ExactCardinality"),
        }
    }

    #[test]
    fn test_individual() {
        let alice = Individual::new("alice", "Person")
            .with_assertion("name", AssertionValue::Literal(Literal::String("Alice".into())))
            .with_assertion("age", AssertionValue::Literal(Literal::Int(30)))
            .with_assertion("friendOf", AssertionValue::Individual("bob".into()));

        assert_eq!(alice.name, "alice");
        assert_eq!(alice.class_name, "Person");
        assert_eq!(alice.assertions.len(), 3);
    }

    #[test]
    fn test_triple() {
        let t = Triple::new("alice", "rdf:type", "Person");
        assert_eq!(t.subject, "alice");
        assert_eq!(t.predicate, "rdf:type");
        assert_eq!(t.object, "Person");

        let t2 = Triple::type_of("bob", "Employee");
        assert_eq!(t2.predicate, "rdf:type");
        assert_eq!(t2.object, "Employee");
    }

    #[test]
    fn test_ontology_with_intersection_and_restrictions() {
        let mut onto = Ontology::new("family");

        onto.add_class(Class::new("Person"));
        onto.add_class(Class::new("Parent").with_restriction(Restriction::MinCardinality {
            property: "hasChild".to_string(),
            min: 1,
        }));
        onto.add_class(Class::new("Child").with_restriction(Restriction::AllValuesFrom {
            property: "hasParent".to_string(),
            class: "Parent".to_string(),
        }));

        // Verify serialize/deserialize roundtrip
        let json = serde_json::to_string(&onto).unwrap();
        let loaded: Ontology = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.classes["Parent"].restrictions.len(), 1);
        assert_eq!(loaded.classes["Child"].restrictions.len(), 1);
    }
}
