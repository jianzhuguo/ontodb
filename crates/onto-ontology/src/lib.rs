//! onto-ontology: Ontology engine for OntoDB.
//!
//! Provides ontology model, parser, storage, and basic inference.

pub mod model;
pub mod parser;
pub mod store;

pub use model::{Class, DataType, Ontology, Property};
pub use parser::OntologyParser;
pub use store::OntologyStore;
