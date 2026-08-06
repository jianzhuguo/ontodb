//! Ontology parser for CREATE ONTOLOGY syntax.
//!
//! Parses statements like:
//! ```sql
//! CREATE ONTOLOGY shop (
//!   CLASS Product,
//!   CLASS Customer,
//!   PROPERTY name DOMAIN Product RANGE STRING,
//!   PROPERTY price DOMAIN Product RANGE DECIMAL
//! );
//! ```

use crate::model::{Class, DataType, Ontology, Property};
use onto_core::{CoreError, Result};

/// Parses CREATE ONTOLOGY statements into Ontology objects.
pub struct OntologyParser;

impl OntologyParser {
    /// Parses a CREATE ONTOLOGY statement.
    pub fn parse(input: &str) -> Result<Ontology> {
        let input = input.trim();

        // Find "CREATE ONTOLOGY <name> ("
        let upper = input.to_uppercase();
        if !upper.starts_with("CREATE ONTOLOGY") {
            return Err(CoreError::InvalidArgument(
                "expected 'CREATE ONTOLOGY'".to_string(),
            ));
        }

        let rest = input[15..].trim(); // Skip "CREATE ONTOLOGY"

        // Extract ontology name
        let paren_pos = rest
            .find('(')
            .ok_or_else(|| CoreError::InvalidArgument("expected '(' after ontology name".to_string()))?;

        let name = rest[..paren_pos].trim();
        if name.is_empty() {
            return Err(CoreError::InvalidArgument(
                "ontology name cannot be empty".to_string(),
            ));
        }

        let body = rest[paren_pos + 1..].trim();
        let body = body
            .strip_suffix(';')
            .unwrap_or(body)
            .strip_suffix(')')
            .ok_or_else(|| {
                CoreError::InvalidArgument("expected ')' at end of ontology definition".to_string())
            })?;

        let mut ontology = Ontology::new(name);

        // Parse statements separated by commas
        for stmt in Self::split_statements(body) {
            let stmt = stmt.trim();
            if stmt.is_empty() {
                continue;
            }

            let upper_stmt = stmt.to_uppercase();

            if upper_stmt.starts_with("CLASS") {
                let class = Self::parse_class(stmt)?;
                ontology.add_class(class);
            } else if upper_stmt.starts_with("PROPERTY") {
                let prop = Self::parse_property(stmt)?;
                ontology.add_property(prop);
            } else {
                return Err(CoreError::InvalidArgument(format!(
                    "unknown statement: {}",
                    stmt
                )));
            }
        }

        Ok(ontology)
    }

    fn split_statements(body: &str) -> Vec<&str> {
        // Split by comma, but respect nested structures
        let mut statements = Vec::new();
        let mut depth = 0;
        let mut start = 0;

        for (i, c) in body.char_indices() {
            match c {
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => depth -= 1,
                ',' if depth == 0 => {
                    statements.push(&body[start..i]);
                    start = i + 1;
                }
                _ => {}
            }
        }

        if start < body.len() {
            statements.push(&body[start..]);
        }

        statements
    }

    fn parse_class(input: &str) -> Result<Class> {
        let rest = input.strip_prefix("CLASS").or_else(|| input.strip_prefix("class"))
            .ok_or_else(|| CoreError::InvalidArgument("expected 'CLASS'".to_string()))?
            .trim();

        // Check for SUBCLASS OF
        let upper_rest = rest.to_uppercase();
        if let Some(sub_pos) = upper_rest.find("SUBCLASS OF") {
            let class_name = rest[..sub_pos].trim();
            let parent = rest[sub_pos + 11..].trim();

            if class_name.is_empty() {
                return Err(CoreError::InvalidArgument(
                    "class name cannot be empty".to_string(),
                ));
            }

            return Ok(Class::new(class_name).with_superclass(parent));
        }

        let class_name = rest.trim();
        if class_name.is_empty() {
            return Err(CoreError::InvalidArgument(
                "class name cannot be empty".to_string(),
            ));
        }

        Ok(Class::new(class_name))
    }

    fn parse_property(input: &str) -> Result<Property> {
        let rest = input
            .strip_prefix("PROPERTY")
            .or_else(|| input.strip_prefix("property"))
            .ok_or_else(|| CoreError::InvalidArgument("expected 'PROPERTY'".to_string()))?
            .trim();

        let upper = rest.to_uppercase();

        // Find DOMAIN and RANGE keywords
        let domain_pos = upper
            .find("DOMAIN")
            .ok_or_else(|| CoreError::InvalidArgument("expected 'DOMAIN' in property".to_string()))?;
        let range_pos = upper
            .find("RANGE")
            .ok_or_else(|| CoreError::InvalidArgument("expected 'RANGE' in property".to_string()))?;

        if domain_pos >= range_pos {
            return Err(CoreError::InvalidArgument(
                "DOMAIN must come before RANGE".to_string(),
            ));
        }

        let prop_name = rest[..domain_pos].trim();
        let domain = rest[domain_pos + 6..range_pos].trim();
        let after_range = rest[range_pos + 5..].trim();

        if prop_name.is_empty() {
            return Err(CoreError::InvalidArgument(
                "property name cannot be empty".to_string(),
            ));
        }

        // Parse optional modifiers (REQUIRED, MULTI_VALUED) after the data type
        let upper_after = after_range.to_uppercase();
        let required = upper_after.contains("REQUIRED");
        let multi_valued = upper_after.contains("MULTI_VALUED");

        // Extract the data type (everything before any modifier keyword)
        let range_str = after_range
            .split_whitespace()
            .take_while(|w| {
                let u = w.to_uppercase();
                u != "REQUIRED" && u != "MULTI_VALUED"
            })
            .collect::<Vec<_>>()
            .join(" ");

        let data_type = DataType::from_str(&range_str).ok_or_else(|| {
            CoreError::InvalidArgument(format!("unknown data type: {}", range_str))
        })?;

        let mut prop = Property::new(prop_name, domain, data_type);
        prop.required = required;
        prop.multi_valued = multi_valued;
        Ok(prop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_ontology() {
        let input = r#"
            CREATE ONTOLOGY shop (
                CLASS Product,
                CLASS Customer,
                PROPERTY name DOMAIN Product RANGE STRING,
                PROPERTY price DOMAIN Product RANGE DECIMAL,
                PROPERTY email DOMAIN Customer RANGE STRING
            );
        "#;

        let onto = OntologyParser::parse(input).unwrap();

        assert_eq!(onto.name, "shop");
        assert!(onto.get_class("Product").is_some());
        assert!(onto.get_class("Customer").is_some());
        assert!(onto.properties.contains_key("name"));
        assert!(onto.properties.contains_key("price"));
        assert!(onto.properties.contains_key("email"));
    }

    #[test]
    fn test_parse_inheritance() {
        let input = r#"
            CREATE ONTOLOGY hr (
                CLASS Person,
                CLASS Employee SUBCLASS OF Person,
                PROPERTY name DOMAIN Person RANGE STRING,
                PROPERTY salary DOMAIN Employee RANGE FLOAT64
            );
        "#;

        let onto = OntologyParser::parse(input).unwrap();

        let emp = onto.get_class("Employee").unwrap();
        assert_eq!(emp.superclasses, vec!["Person"]);
        assert!(onto.is_subclass_of("Employee", "Person"));
    }

    #[test]
    fn test_parse_various_types() {
        let input = r#"
            CREATE ONTOLOGY types (
                CLASS Demo,
                PROPERTY a DOMAIN Demo RANGE STRING,
                PROPERTY b DOMAIN Demo RANGE INT64,
                PROPERTY c DOMAIN Demo RANGE FLOAT64,
                PROPERTY d DOMAIN Demo RANGE BOOL
            );
        "#;

        let onto = OntologyParser::parse(input).unwrap();
        assert_eq!(onto.properties["a"].range, DataType::String);
        assert_eq!(onto.properties["b"].range, DataType::Int64);
        assert_eq!(onto.properties["c"].range, DataType::Float64);
        assert_eq!(onto.properties["d"].range, DataType::Bool);
    }

    #[test]
    fn test_parse_required_property() {
        let input = r#"
            CREATE ONTOLOGY shop (
                CLASS Product,
                PROPERTY name DOMAIN Product RANGE STRING REQUIRED,
                PROPERTY price DOMAIN Product RANGE INT64
            );
        "#;

        let onto = OntologyParser::parse(input).unwrap();
        assert!(onto.properties["name"].required);
        assert!(!onto.properties["price"].required);
    }
}
