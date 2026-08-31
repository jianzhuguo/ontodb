#![allow(clippy::too_many_arguments)]
#![allow(clippy::type_complexity)]
#![allow(clippy::manual_strip)]
#![allow(clippy::needless_range_loop)]
#![allow(clippy::new_without_default)]
#![allow(clippy::collapsible_match)]
#![allow(clippy::if_same_then_else)]
#![allow(clippy::manual_checked_ops)]
#![allow(clippy::ptr_arg)]
#![allow(clippy::non_canonical_partial_ord_impl)]
#![allow(clippy::should_implement_trait)]
#![allow(clippy::sliced_string_as_bytes)]
#![allow(clippy::len_without_is_empty)]
#![allow(clippy::lines_filter_map_ok)]
#![allow(clippy::vec_init_then_push)]
#![allow(clippy::unnecessary_find_map)]
#![allow(clippy::unnecessary_unwrap)]
#![allow(clippy::result_large_err)]
#![allow(clippy::doc_lazy_continuation)]

//! onto-ontology: Ontology engine for OntoDB.
//!
//! Provides ontology model, parser, storage, and inference.

pub mod model;
pub mod parser;
pub mod rdf;
pub mod reasoner;
pub mod rules;
pub mod store;
pub mod triple_store;

pub use model::{
    AssertionValue, Class, ClassType, DataType, Individual, Literal, Namespace, Ontology, Property,
    PropertyAssertion, Restriction, Triple,
};
pub use parser::OntologyParser;
pub use rdf::{RdfError, RdfTerm, RdfTriple, TurtleParser, to_jsonld, to_ntriples};
pub use reasoner::{DerivationStep, InferenceError, Reasoner, ReasoningResult};
pub use rules::{Rule, RuleId};
pub use store::{OntologyStore, DEFAULT_NAMESPACE};
pub use triple_store::TripleStore;
