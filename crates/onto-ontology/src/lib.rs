//! onto-ontology: Ontology engine for OntoDB.
//!
//! Provides ontology model, parser, storage, and inference.

pub mod model;
pub mod parser;
pub mod rdf;
pub mod reasoner;
pub mod rules;
pub mod store;

pub use model::{
    AssertionValue, Class, ClassType, DataType, Individual, Literal, Ontology, Property,
    PropertyAssertion, Restriction, Triple,
};
pub use parser::OntologyParser;
pub use rdf::{RdfError, RdfTerm, RdfTriple, TurtleParser, to_jsonld, to_ntriples};
pub use reasoner::{DerivationStep, InferenceError, Reasoner, ReasoningResult};
pub use rules::{Rule, RuleId};
pub use store::OntologyStore;
