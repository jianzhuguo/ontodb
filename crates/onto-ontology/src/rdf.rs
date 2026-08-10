//! RDF import/export for OntoDB ontologies.
//!
//! Supports:
//! - RDF Turtle parsing (basic subset)
//! - N-Triples export
//! - JSON-LD export

use crate::model::{Class, DataType, Ontology, Property};
use std::collections::HashMap;

/// Errors that can occur during RDF parsing or export.
#[derive(Debug, Clone)]
pub enum RdfError {
    ParseError(String),
    UnsupportedFormat(String),
    InvalidTriple(String),
}

impl std::fmt::Display for RdfError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RdfError::ParseError(msg) => write!(f, "RDF parse error: {}", msg),
            RdfError::UnsupportedFormat(msg) => write!(f, "unsupported format: {}", msg),
            RdfError::InvalidTriple(msg) => write!(f, "invalid triple: {}", msg),
        }
    }
}

impl std::error::Error for RdfError {}

/// An RDF triple (subject, predicate, object).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RdfTriple {
    pub subject: String,
    pub predicate: String,
    pub object: RdfTerm,
}

/// An RDF term (IRI, blank node, or literal).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RdfTerm {
    /// An IRI (Internationalized Resource Identifier).
    Iri(String),
    /// A blank node.
    BlankNode(String),
    /// A literal value with optional language tag or datatype.
    Literal {
        value: String,
        language: Option<String>,
        datatype: Option<String>,
    },
}

/// RDF Turtle parser for basic ontology definitions.
///
/// Supports a subset of Turtle sufficient for OWL ontology declarations:
/// - @prefix declarations
/// - rdf:type triples
/// - rdfs:subClassOf, rdfs:subPropertyOf
/// - owl:equivalentClass, owl:disjointWith
/// - owl:inverseOf, owl:transitiveProperty, owl:symmetricProperty
/// - rdfs:domain, rdfs:range
pub struct TurtleParser {
    prefixes: HashMap<String, String>,
}

impl TurtleParser {
    /// Creates a new Turtle parser.
    pub fn new() -> Self {
        Self {
            prefixes: HashMap::new(),
        }
    }

    /// Parses a Turtle string into an Ontology.
    pub fn parse(&mut self, input: &str, name: &str) -> Result<Ontology, RdfError> {
        let mut ontology = Ontology::new(name);
        let triples = self.parse_triples(input)?;

        // Process triples to build ontology structure
        let mut class_types: HashMap<String, Vec<String>> = HashMap::new(); // subject -> types
        let mut sub_class_of: HashMap<String, Vec<String>> = HashMap::new();
        let mut sub_prop_of: HashMap<String, Vec<String>> = HashMap::new();
        let mut equivalent_classes: HashMap<String, Vec<String>> = HashMap::new();
        let mut disjoint_with: HashMap<String, Vec<String>> = HashMap::new();
        let mut inverse_of: HashMap<String, String> = HashMap::new();
        let mut transitive_props: Vec<String> = Vec::new();
        let mut symmetric_props: Vec<String> = Vec::new();
        let mut domains: HashMap<String, String> = HashMap::new();
        let mut ranges: HashMap<String, String> = HashMap::new();

        for triple in &triples {
            let subject = self.resolve_iri(&triple.subject);
            let predicate = self.resolve_iri(&triple.predicate);

            // Check for TransitiveProperty and SymmetricProperty type declarations first
            if predicate == "rdf:type" || predicate == "http://www.w3.org/1999/02/22-rdf-syntax-ns#type" {
                if let RdfTerm::Iri(ref obj) = triple.object {
                    let obj_resolved = self.resolve_iri(obj);
                    match obj_resolved.as_str() {
                        "owl:TransitiveProperty" | "http://www.w3.org/2002/07/owl#TransitiveProperty" => {
                            transitive_props.push(subject.clone());
                            continue;
                        }
                        "owl:SymmetricProperty" | "http://www.w3.org/2002/07/owl#SymmetricProperty" => {
                            symmetric_props.push(subject.clone());
                            continue;
                        }
                        _ => {
                            class_types.entry(subject.clone()).or_default().push(obj_resolved);
                        }
                    }
                }
                continue;
            }

            match predicate.as_str() {
                "rdfs:subClassOf" | "http://www.w3.org/2000/01/rdf-schema#subClassOf" => {
                    if let RdfTerm::Iri(ref obj) = triple.object {
                        let obj_resolved = self.resolve_iri(obj);
                        sub_class_of.entry(subject.clone()).or_default().push(obj_resolved);
                    }
                }
                "rdfs:subPropertyOf" | "http://www.w3.org/2000/01/rdf-schema#subPropertyOf" => {
                    if let RdfTerm::Iri(ref obj) = triple.object {
                        let obj_resolved = self.resolve_iri(obj);
                        sub_prop_of.entry(subject.clone()).or_default().push(obj_resolved);
                    }
                }
                "owl:equivalentClass" | "http://www.w3.org/2002/07/owl#equivalentClass" => {
                    if let RdfTerm::Iri(ref obj) = triple.object {
                        let obj_resolved = self.resolve_iri(obj);
                        equivalent_classes.entry(subject.clone()).or_default().push(obj_resolved);
                    }
                }
                "owl:disjointWith" | "http://www.w3.org/2002/07/owl#disjointWith" => {
                    if let RdfTerm::Iri(ref obj) = triple.object {
                        let obj_resolved = self.resolve_iri(obj);
                        disjoint_with.entry(subject.clone()).or_default().push(obj_resolved);
                    }
                }
                "owl:inverseOf" | "http://www.w3.org/2002/07/owl#inverseOf" => {
                    if let RdfTerm::Iri(ref obj) = triple.object {
                        let obj_resolved = self.resolve_iri(obj);
                        inverse_of.insert(subject.clone(), obj_resolved);
                    }
                }
                "rdfs:domain" | "http://www.w3.org/2000/01/rdf-schema#domain" => {
                    if let RdfTerm::Iri(ref obj) = triple.object {
                        let obj_resolved = self.resolve_iri(obj);
                        domains.insert(subject.clone(), obj_resolved);
                    }
                }
                "rdfs:range" | "http://www.w3.org/2000/01/rdf-schema#range" => {
                    if let RdfTerm::Iri(ref obj) = triple.object {
                        let obj_resolved = self.resolve_iri(obj);
                        ranges.insert(subject.clone(), obj_resolved);
                    }
                }
                _ => {} // Ignore unknown predicates
            }
        }

        // Build classes
        for (subject, types) in &class_types {
            let class_name = Self::extract_local_name(subject);
            let is_class = types.iter().any(|t| {
                t.ends_with("#Class") || t.ends_with(":Class")
                    || t == "http://www.w3.org/2002/07/owl#Class"
                    || t == "rdfs:Class"
            });

            if is_class || !types.is_empty() {
                let mut class = Class::new(&class_name);
                class.superclasses = sub_class_of.get(subject).cloned().unwrap_or_default()
                    .iter().map(|s| Self::extract_local_name(s)).collect();
                class.equivalent_classes = equivalent_classes.get(subject).cloned().unwrap_or_default()
                    .iter().map(|s| Self::extract_local_name(s)).collect();
                class.disjoint_with = disjoint_with.get(subject).cloned().unwrap_or_default()
                    .iter().map(|s| Self::extract_local_name(s)).collect();
                ontology.add_class(class);
            }
        }

        // Build properties
        for (subject, types) in &class_types {
            let prop_name = Self::extract_local_name(subject);
            let is_property = types.iter().any(|t| {
                t.ends_with("#Property") || t.ends_with(":Property")
                    || t == "http://www.w3.org/1999/02/22-rdf-syntax-ns#Property"
                    || t == "owl:ObjectProperty" || t == "owl:DatatypeProperty"
            });

            if is_property {
                let domain = domains.get(subject)
                    .map(|d| Self::extract_local_name(d))
                    .unwrap_or_default();
                let range = ranges.get(subject)
                    .map(|r| Self::iri_to_datatype(r))
                    .unwrap_or(DataType::String);

                let mut prop = Property::new(&prop_name, &domain, range);
                prop.subproperty_of = sub_prop_of.get(subject).cloned().unwrap_or_default()
                    .iter().map(|s| Self::extract_local_name(s)).collect();
                prop.inverse_of = inverse_of.get(subject).map(|s| Self::extract_local_name(s));
                prop.is_transitive = transitive_props.contains(subject);
                prop.is_symmetric = symmetric_props.contains(subject);
                ontology.add_property(prop);
            }
        }

        Ok(ontology)
    }

    /// Parses Turtle input into a list of triples.
    fn parse_triples(&mut self, input: &str) -> Result<Vec<RdfTriple>, RdfError> {
        let mut triples = Vec::new();

        // Join lines that end with ; or , (continuation)
        let mut joined_lines = Vec::new();
        let mut current_line = String::new();
        for line in input.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                if !current_line.is_empty() {
                    joined_lines.push(current_line.clone());
                    current_line.clear();
                }
                continue;
            }
            if !current_line.is_empty() {
                current_line.push(' ');
            }
            current_line.push_str(trimmed);
            // A line ending with . is a complete statement
            if trimmed.ends_with('.') {
                joined_lines.push(current_line.clone());
                current_line.clear();
            }
        }
        if !current_line.is_empty() {
            joined_lines.push(current_line);
        }

        for line in &joined_lines {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            // Handle prefix declarations
            if line.starts_with("@prefix") {
                self.parse_prefix(line)?;
                continue;
            }

            // Parse subject predicate object . triples
            // Subject is the first token
            let without_dot = line.trim_end_matches('.').trim();
            let first_space = without_dot.find(|c: char| c.is_whitespace()).unwrap_or(without_dot.len());
            let subject = without_dot[..first_space].to_string();
            let rest = without_dot[first_space..].trim();

            // Split by ; for multiple predicate-object pairs
            for po_pair in rest.split(';') {
                let po_pair = po_pair.trim();
                if po_pair.is_empty() {
                    continue;
                }

                // Split by whitespace to get predicate and object(s)
                let first_space = po_pair.find(|c: char| c.is_whitespace()).unwrap_or(po_pair.len());
                let predicate = po_pair[..first_space].trim().to_string();
                let object_str = po_pair[first_space..].trim();

                // Handle multiple objects separated by commas
                for obj in object_str.split(',') {
                    let obj = obj.trim();
                    if obj.is_empty() {
                        continue;
                    }

                    let object = if obj.starts_with('<') && obj.ends_with('>') {
                        RdfTerm::Iri(obj[1..obj.len()-1].to_string())
                    } else if obj.starts_with('"') {
                        // Find the closing quote, handling escaped quotes (\")
                        let mut end_quote = obj.len();
                        let bytes = obj.as_bytes();
                        let mut i = 1;
                        while i < bytes.len() {
                            if bytes[i] == b'\\' && i + 1 < bytes.len() {
                                i += 2; // skip escaped character
                                continue;
                            }
                            if bytes[i] == b'"' {
                                end_quote = i;
                                break;
                            }
                            i += 1;
                        }
                        let raw_value = &obj[1..end_quote];
                        let value = unescape_turtle_string(raw_value);
                        // Safe: end_quote found by loop searching for closing quote
                        let rest = if end_quote + 1 < obj.len() { &obj[end_quote+1..] } else { "" }.trim();
                        let (language, datatype) = if rest.starts_with('@') {
                            (Some(rest[1..].to_string()), None)
                        } else if rest.starts_with("^^") {
                            (None, Some(rest[2..].trim_start_matches('<').trim_end_matches('>').to_string()))
                        } else {
                            (None, None)
                        };
                        RdfTerm::Literal { value, language, datatype }
                    } else if obj.contains(':') {
                        RdfTerm::Iri(self.expand_prefixed_name(obj))
                    } else {
                        RdfTerm::Iri(obj.to_string())
                    };

                    triples.push(RdfTriple {
                        subject: subject.clone(),
                        predicate: predicate.clone(),
                        object,
                    });
                }
            }
        }

        Ok(triples)
    }

    /// Parses a @prefix declaration.
    fn parse_prefix(&mut self, line: &str) -> Result<(), RdfError> {
        // Format: @prefix prefix: <iri> .
        let line = line.trim_end_matches('.').trim();
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 3 {
            let prefix = parts[1].trim_end_matches(':');
            let iri = parts[2].trim_start_matches('<').trim_end_matches('>');
            self.prefixes.insert(prefix.to_string(), iri.to_string());
        }
        Ok(())
    }

    /// Expands a prefixed name to a full IRI.
    fn expand_prefixed_name(&self, name: &str) -> String {
        if let Some(colon_pos) = name.find(':') {
            let prefix = &name[..colon_pos];
            // Safe: colon_pos found by find(':'), so colon_pos + 1 <= name.len()
            let local = &name[colon_pos + 1..];
            if let Some(iri) = self.prefixes.get(prefix) {
                return format!("{}{}", iri, local);
            }
        }
        name.to_string()
    }

    /// Resolves an IRI (expands prefixed names).
    fn resolve_iri(&self, iri: &str) -> String {
        if iri.contains(':') && !iri.starts_with("http://") && !iri.starts_with("https://") {
            self.expand_prefixed_name(iri)
        } else {
            iri.to_string()
        }
    }

    /// Extracts the local name from an IRI.
    fn extract_local_name(iri: &str) -> String {
        if let Some(pos) = iri.rfind('#') {
            // Safe: pos found by rfind('#'), so pos + 1 <= iri.len()
            iri[pos + 1..].to_string()
        } else if let Some(pos) = iri.rfind('/') {
            // Safe: pos found by rfind('/'), so pos + 1 <= iri.len()
            iri[pos + 1..].to_string()
        } else if let Some(pos) = iri.rfind(':') {
            // Safe: pos found by rfind(':'), so pos + 1 <= iri.len()
            iri[pos + 1..].to_string()
        } else {
            iri.to_string()
        }
    }

    /// Converts an IRI to a DataType (for rdfs:range).
    fn iri_to_datatype(iri: &str) -> DataType {
        let local = Self::extract_local_name(iri);
        match local.as_str() {
            "string" | "String" => DataType::String,
            "int" | "integer" | "long" | "Int64" => DataType::Int64,
            "float" | "double" | "decimal" | "Float64" => DataType::Float64,
            "boolean" | "bool" | "Bool" => DataType::Bool,
            _ => DataType::String, // Default to string for unknown types
        }
    }
}

/// Unescapes a Turtle string literal, handling \n, \t, \\, \", etc.
fn unescape_turtle_string(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('"') => result.push('"'),
                Some('\\') => result.push('\\'),
                Some('n') => result.push('\n'),
                Some('r') => result.push('\r'),
                Some('t') => result.push('\t'),
                Some('b') => result.push('\u{0008}'),
                Some('f') => result.push('\u{000C}'),
                Some(other) => {
                    result.push('\\');
                    result.push(other);
                }
                None => result.push('\\'),
            }
        } else {
            result.push(c);
        }
    }
    result
}

/// Exports an ontology to N-Triples format.
pub fn to_ntriples(ontology: &Ontology) -> String {
    let mut output = String::new();
    let base = format!("http://ontodb.io/ontology/{}", ontology.name);

    // Export classes
    for (name, class) in &ontology.classes {
        let subject = format!("<{}/class/{}>", base, name);

        // rdf:type rdfs:Class
        output.push_str(&format!(
            "{} <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/2000/01/rdf-schema#Class> .\n",
            subject
        ));

        // rdfs:label
        output.push_str(&format!(
            "{} <http://www.w3.org/2000/01/rdf-schema#label> \"{}\" .\n",
            subject, name
        ));

        // rdfs:subClassOf
        for superclass in &class.superclasses {
            output.push_str(&format!(
                "{} <http://www.w3.org/2000/01/rdf-schema#subClassOf> <{}/class/{}> .\n",
                subject, base, superclass
            ));
        }

        // owl:equivalentClass
        for equiv in &class.equivalent_classes {
            output.push_str(&format!(
                "{} <http://www.w3.org/2002/07/owl#equivalentClass> <{}/class/{}> .\n",
                subject, base, equiv
            ));
        }

        // owl:disjointWith
        for disjoint in &class.disjoint_with {
            output.push_str(&format!(
                "{} <http://www.w3.org/2002/07/owl#disjointWith> <{}/class/{}> .\n",
                subject, base, disjoint
            ));
        }
    }

    // Export properties
    for (name, prop) in &ontology.properties {
        let subject = format!("<{}/property/{}>", base, name);

        // rdf:type rdf:Property
        output.push_str(&format!(
            "{} <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/1999/02/22-rdf-syntax-ns#Property> .\n",
            subject
        ));

        // rdfs:label
        output.push_str(&format!(
            "{} <http://www.w3.org/2000/01/rdf-schema#label> \"{}\" .\n",
            subject, name
        ));

        // rdfs:domain
        output.push_str(&format!(
            "{} <http://www.w3.org/2000/01/rdf-schema#domain> <{}/class/{}> .\n",
            subject, base, prop.domain
        ));

        // rdfs:range
        output.push_str(&format!(
            "{} <http://www.w3.org/2000/01/rdf-schema#range> <http://www.w3.org/2001/XMLSchema#{}> .\n",
            subject, datatype_to_xsd(&prop.range)
        ));

        // rdfs:subPropertyOf
        for parent in &prop.subproperty_of {
            output.push_str(&format!(
                "{} <http://www.w3.org/2000/01/rdf-schema#subPropertyOf> <{}/property/{}> .\n",
                subject, base, parent
            ));
        }

        // owl:inverseOf
        if let Some(ref inverse) = prop.inverse_of {
            output.push_str(&format!(
                "{} <http://www.w3.org/2002/07/owl#inverseOf> <{}/property/{}> .\n",
                subject, base, inverse
            ));
        }

        // Transitive/Symmetric
        if prop.is_transitive {
            output.push_str(&format!(
                "{} <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/2002/07/owl#TransitiveProperty> .\n",
                subject
            ));
        }
        if prop.is_symmetric {
            output.push_str(&format!(
                "{} <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/2002/07/owl#SymmetricProperty> .\n",
                subject
            ));
        }
    }

    output
}

/// Exports an ontology to JSON-LD format.
pub fn to_jsonld(ontology: &Ontology) -> serde_json::Value {
    let base = format!("http://ontodb.io/ontology/{}", ontology.name);
    let mut context = serde_json::Map::new();
    context.insert("@base".to_string(), serde_json::json!(base));
    context.insert("rdf".to_string(), serde_json::json!("http://www.w3.org/1999/02/22-rdf-syntax-ns#"));
    context.insert("rdfs".to_string(), serde_json::json!("http://www.w3.org/2000/01/rdf-schema#"));
    context.insert("owl".to_string(), serde_json::json!("http://www.w3.org/2002/07/owl#"));

    let mut graph = Vec::new();

    // Export classes
    for (name, class) in &ontology.classes {
        let mut class_json = serde_json::Map::new();
        class_json.insert("@id".to_string(), serde_json::json!(format!("class/{}", name)));
        class_json.insert("@type".to_string(), serde_json::json!(["rdfs:Class"]));
        class_json.insert("rdfs:label".to_string(), serde_json::json!(name));

        if !class.superclasses.is_empty() {
            class_json.insert("rdfs:subClassOf".to_string(),
                serde_json::json!(class.superclasses.iter().map(|s| format!("class/{}", s)).collect::<Vec<_>>()));
        }
        if !class.equivalent_classes.is_empty() {
            class_json.insert("owl:equivalentClass".to_string(),
                serde_json::json!(class.equivalent_classes.iter().map(|s| format!("class/{}", s)).collect::<Vec<_>>()));
        }
        if !class.disjoint_with.is_empty() {
            class_json.insert("owl:disjointWith".to_string(),
                serde_json::json!(class.disjoint_with.iter().map(|s| format!("class/{}", s)).collect::<Vec<_>>()));
        }

        graph.push(serde_json::Value::Object(class_json));
    }

    // Export properties
    for (name, prop) in &ontology.properties {
        let mut prop_json = serde_json::Map::new();
        prop_json.insert("@id".to_string(), serde_json::json!(format!("property/{}", name)));
        prop_json.insert("@type".to_string(), serde_json::json!(["rdf:Property"]));
        prop_json.insert("rdfs:label".to_string(), serde_json::json!(name));
        prop_json.insert("rdfs:domain".to_string(), serde_json::json!({"@id": format!("class/{}", prop.domain)}));
        prop_json.insert("rdfs:range".to_string(), serde_json::json!({"@id": format!("http://www.w3.org/2001/XMLSchema#{}", datatype_to_xsd(&prop.range))}));

        if !prop.subproperty_of.is_empty() {
            prop_json.insert("rdfs:subPropertyOf".to_string(),
                serde_json::json!(prop.subproperty_of.iter().map(|s| format!("property/{}", s)).collect::<Vec<_>>()));
        }
        if let Some(ref inverse) = prop.inverse_of {
            prop_json.insert("owl:inverseOf".to_string(), serde_json::json!({"@id": format!("property/{}", inverse)}));
        }
        if prop.is_transitive {
            prop_json.insert("@type".to_string(),
                serde_json::json!(["rdf:Property", "owl:TransitiveProperty"]));
        }
        if prop.is_symmetric {
            prop_json.insert("@type".to_string(),
                serde_json::json!(["rdf:Property", "owl:SymmetricProperty"]));
        }

        graph.push(serde_json::Value::Object(prop_json));
    }

    serde_json::json!({
        "@context": context,
        "@graph": graph
    })
}

/// Converts a DataType to XSD type name.
fn datatype_to_xsd(dt: &DataType) -> &'static str {
    match dt {
        DataType::String => "string",
        DataType::Int64 => "integer",
        DataType::Float64 => "double",
        DataType::Bool => "boolean",
        DataType::Bytes => "base64Binary",
        DataType::Array => "string",
        DataType::Object => "string",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_basic_turtle() {
        let mut parser = TurtleParser::new();
        let input = r#"
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix ex: <http://example.org/> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .

ex:Person rdf:type owl:Class .
ex:Employee rdf:type owl:Class ;
    rdfs:subClassOf ex:Person .
ex:name rdf:type rdf:Property ;
    rdfs:domain ex:Person ;
    rdfs:range xsd:string .
"#;

        let ontology = parser.parse(input, "test").unwrap();
        assert!(ontology.classes.contains_key("Person"), "Should have Person class");
        assert!(ontology.classes.contains_key("Employee"), "Should have Employee class");
        assert!(ontology.properties.contains_key("name"), "Should have name property");
    }

    #[test]
    fn test_ntriples_export() {
        let mut ontology = Ontology::new("test");
        ontology.add_class(Class::new("Person"));
        ontology.add_class(Class::new("Employee").with_superclass("Person"));
        ontology.add_property(Property::new("name", "Person", DataType::String));

        let ntriples = to_ntriples(&ontology);
        assert!(ntriples.contains("Person"));
        assert!(ntriples.contains("Employee"));
        assert!(ntriples.contains("subClassOf"));
    }

    #[test]
    fn test_jsonld_export() {
        let mut ontology = Ontology::new("test");
        ontology.add_class(Class::new("Person"));
        ontology.add_property(Property::new("name", "Person", DataType::String));

        let jsonld = to_jsonld(&ontology);
        assert!(jsonld.get("@context").is_some());
        assert!(jsonld.get("@graph").is_some());
    }
}
