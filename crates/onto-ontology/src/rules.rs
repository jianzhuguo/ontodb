//! OWL-lite inference rules.
//!
//! Implements the core OWL 2 RL rule subset for lightweight reasoning:
//! - Cax-sco: subclass type propagation
//! - Cax-eqc: equivalent class propagation
//! - prp-spo: subproperty propagation
//! - prp-eqp: equivalent property propagation
//! - prp-inv: inverse property inference
//! - prp-trp: transitive property closure
//! - prp-symp: symmetric property inference

use crate::model::{Ontology, Triple};
use std::collections::{HashMap, HashSet};

/// Identifies which rule produced an inference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RuleId {
    /// Subclass propagation: x type A, A subClassOf B -> x type B
    CaxSco,
    /// Equivalent class: x type A, A equiv B -> x type B
    CaxEqc,
    /// Subproperty: x P y, P subPropOf Q -> x Q y
    PrpSpo,
    /// Equivalent property: x P y, P equivProp Q -> x Q y
    PrpEqp,
    /// Inverse property: x P y, P inverseOf Q -> y Q x
    PrpInv,
    /// Transitive property: x P y, y P z -> x P z
    PrpTrp,
    /// Symmetric property: x P y -> y P x
    PrpSymp,
}

/// An inference rule that operates on an ontology and a set of known triples.
pub trait Rule: Send + Sync {
    fn id(&self) -> RuleId;
    /// Apply the rule. `facts` is the full known set; `new_facts` is the subset
    /// added in the previous iteration (empty on first call).
    /// Rules that can be incremental should extend only from `new_facts`;
    /// others can fall back to scanning `facts`.
    fn apply(
        &self,
        ontology: &Ontology,
        facts: &HashSet<Triple>,
        new_facts: &[Triple],
    ) -> Vec<Triple>;
}

/// Cax-sco: Subclass type propagation.
///
/// If `x rdf:type A` and `A rdfs:subClassOf B`, then `x rdf:type B`.
pub struct CaxSco;

impl Rule for CaxSco {
    fn id(&self) -> RuleId {
        RuleId::CaxSco
    }

    fn apply(
        &self,
        ontology: &Ontology,
        facts: &HashSet<Triple>,
        new_facts: &[Triple],
    ) -> Vec<Triple> {
        let mut inferred = Vec::new();
        // Incremental: only process new rdf:type facts
        let source: Vec<&Triple> = if new_facts.is_empty() {
            facts.iter().collect()
        } else {
            new_facts
                .iter()
                .filter(|t| t.predicate == "rdf:type")
                .collect()
        };
        for fact in source {
            if fact.predicate != "rdf:type" {
                continue;
            }
            let superclasses = ontology.get_all_superclasses(&fact.object);
            for sup in &superclasses {
                let t = Triple::type_of(&fact.subject, sup);
                if !facts.contains(&t) {
                    inferred.push(t);
                }
            }
        }
        inferred
    }
}

/// Cax-eqc: Equivalent class propagation.
///
/// If `x rdf:type A` and `A owl:equivalentClass B`, then `x rdf:type B`.
pub struct CaxEqc;

impl Rule for CaxEqc {
    fn id(&self) -> RuleId {
        RuleId::CaxEqc
    }

    fn apply(
        &self,
        ontology: &Ontology,
        facts: &HashSet<Triple>,
        new_facts: &[Triple],
    ) -> Vec<Triple> {
        let mut inferred = Vec::new();
        let source: Vec<&Triple> = if new_facts.is_empty() {
            facts.iter().collect()
        } else {
            new_facts
                .iter()
                .filter(|t| t.predicate == "rdf:type")
                .collect()
        };
        for fact in source {
            if fact.predicate != "rdf:type" {
                continue;
            }
            if let Some(class) = ontology.classes.get(&fact.object) {
                for equiv in &class.equivalent_classes {
                    let t = Triple::type_of(&fact.subject, equiv);
                    if !facts.contains(&t) {
                        inferred.push(t);
                    }
                }
            }
        }
        inferred
    }
}

/// Prp-spo: Subproperty propagation.
///
/// If `x P y` and `P rdfs:subPropertyOf Q`, then `x Q y`.
pub struct PrpSpo;

impl Rule for PrpSpo {
    fn id(&self) -> RuleId {
        RuleId::PrpSpo
    }

    fn apply(
        &self,
        ontology: &Ontology,
        facts: &HashSet<Triple>,
        new_facts: &[Triple],
    ) -> Vec<Triple> {
        let mut inferred = Vec::new();
        let source: Vec<&Triple> = if new_facts.is_empty() {
            facts.iter().collect()
        } else {
            new_facts
                .iter()
                .filter(|t| t.predicate != "rdf:type")
                .collect()
        };
        for fact in source {
            if fact.predicate == "rdf:type" {
                continue;
            }
            collect_superproperties(ontology, &fact.predicate, &mut |super_prop| {
                let t = Triple::new(&fact.subject, super_prop, &fact.object);
                if !facts.contains(&t) {
                    inferred.push(t);
                }
            });
        }
        inferred
    }
}

/// Recursively collects all superproperties of a given property.
fn collect_superproperties(ontology: &Ontology, prop: &str, visitor: &mut dyn FnMut(&str)) {
    let mut visited = HashSet::new();
    collect_superproperties_inner(ontology, prop, &mut visited, visitor);
}

fn collect_superproperties_inner(
    ontology: &Ontology,
    prop: &str,
    visited: &mut HashSet<String>,
    visitor: &mut dyn FnMut(&str),
) {
    if !visited.insert(prop.to_string()) {
        return;
    }
    if let Some(prop_def) = ontology.properties.get(prop) {
        for sup in &prop_def.subproperty_of {
            visitor(sup);
            collect_superproperties_inner(ontology, sup, visited, visitor);
        }
    }
}

/// Prp-eqp: Equivalent property propagation.
///
/// If `x P y` and `P owl:equivalentProperty Q`, then `x Q y`.
pub struct PrpEqp;

impl Rule for PrpEqp {
    fn id(&self) -> RuleId {
        RuleId::PrpEqp
    }

    fn apply(
        &self,
        ontology: &Ontology,
        facts: &HashSet<Triple>,
        new_facts: &[Triple],
    ) -> Vec<Triple> {
        let mut inferred = Vec::new();
        let source: Vec<&Triple> = if new_facts.is_empty() {
            facts.iter().collect()
        } else {
            new_facts
                .iter()
                .filter(|t| t.predicate != "rdf:type")
                .collect()
        };
        for fact in source {
            if fact.predicate == "rdf:type" {
                continue;
            }
            if let Some(prop_def) = ontology.properties.get(&fact.predicate) {
                for equiv in &prop_def.equivalent_properties {
                    let t = Triple::new(&fact.subject, equiv, &fact.object);
                    if !facts.contains(&t) {
                        inferred.push(t);
                    }
                }
            }
        }
        inferred
    }
}

/// Prp-inv: Inverse property inference.
///
/// If `x P y` and `P owl:inverseOf Q`, then `y Q x`.
pub struct PrpInv;

impl Rule for PrpInv {
    fn id(&self) -> RuleId {
        RuleId::PrpInv
    }

    fn apply(
        &self,
        ontology: &Ontology,
        facts: &HashSet<Triple>,
        new_facts: &[Triple],
    ) -> Vec<Triple> {
        // Pre-compute reverse index: inverse_of target -> list of property names
        // This avoids O(facts × properties) full scan
        let mut reverse_index: HashMap<&str, Vec<&str>> = HashMap::new();
        for (name, prop_def) in &ontology.properties {
            if let Some(ref inverse) = prop_def.inverse_of {
                reverse_index
                    .entry(inverse.as_str())
                    .or_default()
                    .push(name);
            }
        }

        let mut inferred = Vec::new();
        let source: Vec<&Triple> = if new_facts.is_empty() {
            facts.iter().collect()
        } else {
            new_facts
                .iter()
                .filter(|t| t.predicate != "rdf:type")
                .collect()
        };
        for fact in source {
            if fact.predicate == "rdf:type" {
                continue;
            }
            // Check if this property has a direct inverse
            if let Some(prop_def) = ontology.properties.get(&fact.predicate) {
                if let Some(ref inverse) = prop_def.inverse_of {
                    let t = Triple::new(&fact.object, inverse, &fact.subject);
                    if !facts.contains(&t) {
                        inferred.push(t);
                    }
                }
            }
            // Check reverse index for properties that have this predicate as their inverse
            if let Some(inverse_props) = reverse_index.get(fact.predicate.as_str()) {
                for &prop_name in inverse_props {
                    let t = Triple::new(&fact.object, prop_name, &fact.subject);
                    if !facts.contains(&t) {
                        inferred.push(t);
                    }
                }
            }
        }
        inferred
    }
}

/// Prp-trp: Transitive property closure.
///
/// If `x P y` and `y P z` and `P` is transitive, then `x P z`.
///
/// Incremental optimization: on iterations after the first, only extend
/// from new facts. For each new (a, b), look up all (b, c) in the full set
/// to infer (a, c), and all (z, a) to infer (z, b).
pub struct PrpTrp;

impl Rule for PrpTrp {
    fn id(&self) -> RuleId {
        RuleId::PrpTrp
    }

    fn apply(
        &self,
        ontology: &Ontology,
        facts: &HashSet<Triple>,
        new_facts: &[Triple],
    ) -> Vec<Triple> {
        let mut inferred = Vec::new();

        // Identify transitive predicates once
        let transitive_preds: Vec<&str> = ontology
            .properties
            .iter()
            .filter(|(_, p)| p.is_transitive)
            .map(|(name, _)| name.as_str())
            .collect();

        if transitive_preds.is_empty() {
            return inferred;
        }

        if new_facts.is_empty() {
            // First iteration: build obj_map from all facts, then do one-hop extension
            let mut obj_map: std::collections::HashMap<&str, Vec<&str>> =
                std::collections::HashMap::new();
            for fact in facts {
                if transitive_preds.contains(&fact.predicate.as_str()) {
                    obj_map
                        .entry(fact.subject.as_str())
                        .or_default()
                        .push(fact.object.as_str());
                }
            }
            // Iterate facts directly for first iteration
            for fact in facts {
                if !transitive_preds.contains(&fact.predicate.as_str()) {
                    continue;
                }
                let a = &fact.subject;
                let pred = &fact.predicate;
                if let Some(b_list) = obj_map.get(a.as_str()) {
                    for b in b_list {
                        if let Some(c_list) = obj_map.get(*b) {
                            for c in c_list {
                                if a.as_str() != *c {
                                    let t = Triple::new(a.as_str(), pred.as_str(), *c);
                                    if !facts.contains(&t) {
                                        inferred.push(t);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        } else {
            // Incremental: only extend from new facts
            // Build lookup indexes from full facts (only transitive predicates)
            let mut obj_map: std::collections::HashMap<&str, Vec<&str>> =
                std::collections::HashMap::new();
            let mut subj_map: std::collections::HashMap<&str, Vec<&str>> =
                std::collections::HashMap::new();
            for fact in facts.iter() {
                if transitive_preds.contains(&fact.predicate.as_str()) {
                    obj_map
                        .entry(fact.subject.as_str())
                        .or_default()
                        .push(fact.object.as_str());
                    subj_map
                        .entry(fact.object.as_str())
                        .or_default()
                        .push(fact.subject.as_str());
                }
            }

            for fact in new_facts {
                if !transitive_preds.contains(&fact.predicate.as_str()) {
                    continue;
                }
                let pred = &fact.predicate;
                let a = &fact.subject;
                let b = &fact.object;

                // Forward: (a, b) + (b, c) -> (a, c)
                if let Some(c_list) = obj_map.get(b.as_str()) {
                    for c in c_list {
                        if a.as_str() != *c {
                            let t = Triple::new(a.as_str(), pred.as_str(), *c);
                            if !facts.contains(&t) {
                                inferred.push(t);
                            }
                        }
                    }
                }
                // Backward: (z, a) + (a, b) -> (z, b)
                if let Some(z_list) = subj_map.get(a.as_str()) {
                    for z in z_list {
                        if *z != b.as_str() {
                            let t = Triple::new(*z, pred.as_str(), b.as_str());
                            if !facts.contains(&t) {
                                inferred.push(t);
                            }
                        }
                    }
                }
            }
        }
        inferred
    }
}

/// Prp-symp: Symmetric property inference.
///
/// If `x P y` and `P` is symmetric, then `y P x`.
pub struct PrpSymp;

impl Rule for PrpSymp {
    fn id(&self) -> RuleId {
        RuleId::PrpSymp
    }

    fn apply(
        &self,
        ontology: &Ontology,
        facts: &HashSet<Triple>,
        new_facts: &[Triple],
    ) -> Vec<Triple> {
        let mut inferred = Vec::new();
        let source: Vec<&Triple> = if new_facts.is_empty() {
            facts.iter().collect()
        } else {
            new_facts
                .iter()
                .filter(|t| t.predicate != "rdf:type")
                .collect()
        };
        for fact in source {
            if fact.predicate == "rdf:type" {
                continue;
            }
            if let Some(prop_def) = ontology.properties.get(&fact.predicate) {
                if prop_def.is_symmetric {
                    let t = Triple::new(&fact.object, &fact.predicate, &fact.subject);
                    if !facts.contains(&t) {
                        inferred.push(t);
                    }
                }
            }
        }
        inferred
    }
}

/// Returns the default set of OWL-lite inference rules.
pub fn default_rules() -> Vec<Box<dyn Rule>> {
    vec![
        Box::new(CaxSco),
        Box::new(CaxEqc),
        Box::new(PrpSpo),
        Box::new(PrpEqp),
        Box::new(PrpInv),
        Box::new(PrpTrp),
        Box::new(PrpSymp),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Class, DataType, Ontology, Property};

    fn make_ontology() -> Ontology {
        let mut onto = Ontology::new("test");

        onto.add_class(Class::new("Thing"));
        onto.add_class(Class::new("Person").with_superclass("Thing"));
        onto.add_class(Class::new("Employee").with_superclass("Person"));
        onto.add_class(Class::new("Manager").with_superclass("Employee"));
        onto.add_class(Class::new("Worker").with_equivalent_class("Employee"));
        onto.classes
            .get_mut("Employee")
            .unwrap()
            .equivalent_classes
            .push("Worker".to_string());

        onto.add_property(Property::new("name", "Person", DataType::String));
        onto.add_property(
            Property::new("reportsTo", "Employee", DataType::String).with_inverse_of("manages"),
        );
        onto.add_property(
            Property::new("manages", "Manager", DataType::String).with_inverse_of("reportsTo"),
        );
        onto.add_property(Property::new("ancestor", "Person", DataType::String).transitive());
        onto.add_property(Property::new("friendOf", "Person", DataType::String).symmetric());
        onto.add_property(
            Property::new("worksUnder", "Employee", DataType::String)
                .with_subproperty_of("reportsTo"),
        );

        onto
    }

    #[test]
    fn test_cax_sco_basic() {
        let onto = make_ontology();
        let facts: HashSet<Triple> = vec![Triple::type_of("alice", "Employee")]
            .into_iter()
            .collect();

        let rule = CaxSco;
        let inferred = rule.apply(&onto, &facts, &[]);

        assert!(inferred.contains(&Triple::type_of("alice", "Person")));
        assert!(inferred.contains(&Triple::type_of("alice", "Thing")));
    }

    #[test]
    fn test_cax_sco_manager() {
        let onto = make_ontology();
        let facts: HashSet<Triple> = vec![Triple::type_of("bob", "Manager")]
            .into_iter()
            .collect();

        let rule = CaxSco;
        let inferred = rule.apply(&onto, &facts, &[]);

        assert!(inferred.contains(&Triple::type_of("bob", "Employee")));
        assert!(inferred.contains(&Triple::type_of("bob", "Person")));
        assert!(inferred.contains(&Triple::type_of("bob", "Thing")));
    }

    #[test]
    fn test_cax_eqc() {
        let onto = make_ontology();
        let facts: HashSet<Triple> = vec![Triple::type_of("alice", "Employee")]
            .into_iter()
            .collect();

        let rule = CaxEqc;
        let inferred = rule.apply(&onto, &facts, &[]);

        assert!(inferred.contains(&Triple::type_of("alice", "Worker")));
    }

    #[test]
    fn test_prp_inv() {
        let onto = make_ontology();
        let facts: HashSet<Triple> = vec![Triple::new("alice", "reportsTo", "bob")]
            .into_iter()
            .collect();

        let rule = PrpInv;
        let inferred = rule.apply(&onto, &facts, &[]);

        assert!(inferred.contains(&Triple::new("bob", "manages", "alice")));
    }

    #[test]
    fn test_prp_trp() {
        let onto = make_ontology();
        let facts: HashSet<Triple> = vec![
            Triple::new("alice", "ancestor", "bob"),
            Triple::new("bob", "ancestor", "charlie"),
        ]
        .into_iter()
        .collect();

        let rule = PrpTrp;
        let inferred = rule.apply(&onto, &facts, &[]);

        assert!(inferred.contains(&Triple::new("alice", "ancestor", "charlie")));
    }

    #[test]
    fn test_prp_trp_no_short_self_loop() {
        let onto = make_ontology();
        let facts: HashSet<Triple> = vec![Triple::new("alice", "ancestor", "bob")]
            .into_iter()
            .collect();

        let rule = PrpTrp;
        let inferred = rule.apply(&onto, &facts, &[]);

        assert!(!inferred.contains(&Triple::new("alice", "ancestor", "alice")));
    }

    #[test]
    fn test_prp_symp() {
        let onto = make_ontology();
        let facts: HashSet<Triple> = vec![Triple::new("alice", "friendOf", "bob")]
            .into_iter()
            .collect();

        let rule = PrpSymp;
        let inferred = rule.apply(&onto, &facts, &[]);

        assert!(inferred.contains(&Triple::new("bob", "friendOf", "alice")));
    }

    #[test]
    fn test_prp_spo() {
        let onto = make_ontology();
        let facts: HashSet<Triple> = vec![Triple::new("alice", "worksUnder", "bob")]
            .into_iter()
            .collect();

        let rule = PrpSpo;
        let inferred = rule.apply(&onto, &facts, &[]);

        assert!(inferred.contains(&Triple::new("alice", "reportsTo", "bob")));
    }

    #[test]
    fn test_prp_eqp() {
        let mut onto = make_ontology();
        onto.add_property(
            Property::new("email", "Person", DataType::String)
                .with_equivalent_property("emailAddress"),
        );

        let facts: HashSet<Triple> = vec![Triple::new("alice", "email", "a@b.com")]
            .into_iter()
            .collect();

        let rule = PrpEqp;
        let inferred = rule.apply(&onto, &facts, &[]);

        assert!(inferred.contains(&Triple::new("alice", "emailAddress", "a@b.com")));
    }

    #[test]
    fn test_default_rules_returns_all() {
        let rules = default_rules();
        assert_eq!(rules.len(), 7);
        let ids: HashSet<RuleId> = rules.iter().map(|r| r.id()).collect();
        assert!(ids.contains(&RuleId::CaxSco));
        assert!(ids.contains(&RuleId::CaxEqc));
        assert!(ids.contains(&RuleId::PrpSpo));
        assert!(ids.contains(&RuleId::PrpEqp));
        assert!(ids.contains(&RuleId::PrpInv));
        assert!(ids.contains(&RuleId::PrpTrp));
        assert!(ids.contains(&RuleId::PrpSymp));
    }
}
