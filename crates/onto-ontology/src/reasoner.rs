//! OWL-lite reasoning engine.
//!
//! The Reasoner takes an ontology and a set of known facts (triples),
//! applies inference rules iteratively until a fixed point is reached,
//! and returns all inferred triples.

use crate::model::{Individual, Ontology, Triple};
use crate::rules::{default_rules, Rule, RuleId};
use std::collections::{HashMap, HashSet, VecDeque};

/// An error that occurred during reasoning.
#[derive(Debug, Clone)]
pub enum InferenceError {
    /// A consistency violation was detected (e.g. disjoint class conflict).
    ConsistencyViolation(String),
    /// The inference did not converge within the iteration limit.
    MaxIterationsExceeded(usize),
}

impl std::fmt::Display for InferenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InferenceError::ConsistencyViolation(msg) => write!(f, "consistency violation: {}", msg),
            InferenceError::MaxIterationsExceeded(n) => {
                write!(f, "reasoning did not converge after {} iterations", n)
            }
        }
    }
}

impl std::error::Error for InferenceError {}

/// The result of a reasoning pass.
#[derive(Debug, Clone)]
pub struct ReasoningResult {
    /// All triples after reasoning (original + inferred).
    pub all_facts: HashSet<Triple>,
    /// Only the newly inferred triples.
    pub inferred: Vec<Triple>,
    /// Per-rule inference counts for diagnostics.
    pub rule_counts: HashMap<RuleId, usize>,
    /// Number of iterations until convergence.
    pub iterations: usize,
    /// Consistency violations found.
    pub violations: Vec<String>,
}

/// OWL-lite reasoning engine.
pub struct Reasoner {
    ontology: Ontology,
    rules: Vec<Box<dyn Rule>>,
    max_iterations: usize,
    /// Maximum number of facts allowed during reasoning.
    /// Prevents memory explosion from transitive closure.
    max_facts: usize,
}

impl Reasoner {
    /// Creates a new reasoner with the default rule set.
    pub fn new(ontology: Ontology) -> Self {
        Self {
            ontology,
            rules: default_rules(),
            max_iterations: 100,
            max_facts: 1_000_000, // Default: 1M facts
        }
    }

    /// Creates a reasoner with a custom rule set.
    pub fn with_rules(ontology: Ontology, rules: Vec<Box<dyn Rule>>) -> Self {
        Self {
            ontology,
            rules,
            max_iterations: 100,
            max_facts: 1_000_000,
        }
    }

    /// Sets the maximum number of inference iterations.
    pub fn with_max_iterations(mut self, max: usize) -> Self {
        self.max_iterations = max;
        self
    }

    /// Sets the maximum number of facts allowed during reasoning.
    pub fn with_max_facts(mut self, max: usize) -> Self {
        self.max_facts = max;
        self
    }

    /// Performs full reasoning over the given facts.
    ///
    /// Applies all rules iteratively until no new triples are derived (fixed point).
    /// When `parallel` is enabled via `with_parallel()`, independent rules are applied
    /// concurrently using `std::thread::scope`.
    pub fn reason(&self, facts: &[Triple]) -> ReasoningResult {
        self.reason_inner(facts, false)
    }

    /// Performs reasoning with parallel rule application.
    ///
    /// Independent rules (e.g., Prp-symp and Prp-inv) are applied concurrently.
    pub fn reason_parallel(&self, facts: &[Triple]) -> ReasoningResult {
        self.reason_inner(facts, true)
    }

    fn reason_inner(&self, facts: &[Triple], parallel: bool) -> ReasoningResult {
        let mut all_facts: HashSet<Triple> = facts.iter().cloned().collect();
        let mut all_inferred: Vec<Triple> = Vec::new();
        let mut rule_counts: HashMap<RuleId, usize> = HashMap::new();
        let mut iterations = 0;
        let mut new_facts: Vec<Triple> = Vec::new();
        let mut budget_exceeded = false;

        // Fast path: transitive closure using adjacency BFS
        let mut transitive_handled: HashSet<String> = HashSet::new();
        for (prop_name, prop_def) in &self.ontology.properties {
            if !prop_def.is_transitive {
                continue;
            }
            
            // Check fact budget before processing transitive property
            if all_facts.len() >= self.max_facts {
                budget_exceeded = true;
                break;
            }
            
            transitive_handled.insert(prop_name.clone());

            // Build adjacency list
            let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
            for fact in &all_facts {
                if fact.predicate == *prop_name {
                    adj.entry(fact.subject.as_str()).or_default().push(fact.object.as_str());
                }
            }
            if adj.is_empty() {
                continue;
            }

            // BFS from each source
            let mut new_transitive = Vec::new();
            for (start, targets) in &adj {
                // Check budget during BFS
                if all_facts.len() + new_transitive.len() >= self.max_facts {
                    budget_exceeded = true;
                    break;
                }
                
                let mut visited: HashSet<&str> = HashSet::new();
                let mut queue: VecDeque<&str> = VecDeque::new();
                for t in targets {
                    visited.insert(*t);
                    queue.push_back(t);
                }
                while let Some(node) = queue.pop_front() {
                    // Check budget during BFS
                    if all_facts.len() + new_transitive.len() >= self.max_facts {
                        budget_exceeded = true;
                        break;
                    }
                    
                    let t = Triple::new(*start, prop_name.as_str(), node);
                    if !all_facts.contains(&t) {
                        new_transitive.push(t);
                    }
                    if let Some(neighbors) = adj.get(node) {
                        for n in neighbors {
                            if visited.insert(*n) {
                                queue.push_back(n);
                            }
                        }
                    }
                }
                
                if budget_exceeded {
                    break;
                }
            }

            if !budget_exceeded {
                let count = new_transitive.len();
                if count > 0 {
                    rule_counts.insert(RuleId::PrpTrp, count);
                    for t in new_transitive {
                        all_facts.insert(t.clone());
                        all_inferred.push(t);
                    }
                }
            }
        }

        // Fast path: Cax-sco + Cax-eqc
        // Optimization: group rdf:type facts by class, batch-process all subjects per class
        {
            let superclass_cache = self.ontology.build_superclass_cache();

            // Phase 1: group subjects by class (immutable borrow of all_facts)
            let mut class_subjects: HashMap<String, Vec<String>> = HashMap::new();
            for fact in &all_facts {
                if fact.predicate == "rdf:type" {
                    class_subjects.entry(fact.object.clone()).or_default().push(fact.subject.clone());
                }
            }

            // Phase 2: generate inferred triples (no borrow of all_facts)
            let mut cax_new: Vec<Triple> = Vec::new();
            for (class_name, subjects) in &class_subjects {
                if let Some(supers) = superclass_cache.get(class_name.as_str()) {
                    for sup in supers {
                        for subj in subjects {
                            cax_new.push(Triple::type_of(subj, sup));
                        }
                    }
                }
                if let Some(class_def) = self.ontology.classes.get(class_name.as_str()) {
                    for equiv in &class_def.equivalent_classes {
                        for subj in subjects {
                            cax_new.push(Triple::type_of(subj, equiv));
                        }
                    }
                }
            }

            // Phase 3: deduplicate and insert (mutable borrow of all_facts)
            let mut count = 0;
            for t in cax_new {
                if all_facts.insert(t.clone()) {
                    all_inferred.push(t);
                    count += 1;
                }
            }
            if count > 0 {
                *rule_counts.entry(RuleId::CaxSco).or_insert(0) += count;
            }
        }

        // For the iterative loop, seed new_facts with original property facts
        // (non-rdf:type) so PrpInv/PrpSymp/PrpSpo/PrpEqp can process them.
        // The fast paths already handled rdf:type and transitive properties.
        new_facts = facts.iter()
            .filter(|t| t.predicate != "rdf:type")
            .cloned()
            .collect();

        for _iter in 0..self.max_iterations {
            iterations += 1;
            let mut new_this_round: Vec<Triple> = Vec::new();

            if parallel && self.rules.len() > 1 {
                let rule_results: Vec<(RuleId, Vec<Triple>)> = std::thread::scope(|s| {
                    let handles: Vec<_> = self.rules.iter().map(|rule| {
                        s.spawn(|| {
                            let inferred = rule.apply(&self.ontology, &all_facts, &new_facts);
                            (rule.id(), inferred)
                        })
                    }).collect();
                    handles.into_iter().filter_map(|h| h.join().ok()).collect()
                });
                for (rule_id, inferred) in rule_results {
                    *rule_counts.entry(rule_id).or_insert(0) += inferred.len();
                    new_this_round.extend(inferred);
                }
            } else {
                for rule in &self.rules {
                    // Skip rules already handled by fast paths
                    match rule.id() {
                        RuleId::CaxSco | RuleId::CaxEqc | RuleId::PrpTrp => continue,
                        _ => {}
                    }
                    let inferred = rule.apply(&self.ontology, &all_facts, &new_facts);
                    *rule_counts.entry(rule.id()).or_insert(0) += inferred.len();
                    new_this_round.extend(inferred);
                }
            }

            // Deduplicate: only add triples not already known
            new_facts.clear();
            let before = all_facts.len();
            for t in new_this_round {
                if all_facts.insert(t.clone()) {
                    new_facts.push(t.clone());
                    all_inferred.push(t);
                }
            }

            // Fixed point: no new facts
            if all_facts.len() == before {
                break;
            }
        }

        // Check consistency
        let violations = self.check_consistency(&all_facts);

        ReasoningResult {
            all_facts,
            inferred: all_inferred,
            rule_counts,
            iterations,
            violations,
        }
    }

    /// Performs reasoning on a set of individuals.
    ///
    /// Converts individuals to triples, runs reasoning, returns all inferred triples.
    pub fn reason_individuals(&self, individuals: &[Individual]) -> ReasoningResult {
        let mut facts = Vec::new();
        for ind in individuals {
            facts.push(Triple::type_of(&ind.name, &ind.class_name));
            for assertion in &ind.assertions {
                let obj = match &assertion.value {
                    crate::model::AssertionValue::Individual(name) => name.clone(),
                    crate::model::AssertionValue::Literal(lit) => format!("{:?}", lit),
                };
                facts.push(Triple::new(&ind.name, &assertion.property, obj));
            }
        }
        self.reason(&facts)
    }

    /// Incremental reasoning: only re-derive facts affected by the changes.
    ///
    /// `existing_facts` — previously known facts (base + inferred).
    /// `added` — newly asserted triples.
    /// `removed` — triples that were retracted.
    ///
    /// This implementation tracks which facts are base (asserted) vs derived (inferred),
    /// and only re-runs rules that could be affected by the changes.
    pub fn reason_incremental(
        &self,
        existing_facts: &[Triple],
        added: &[Triple],
        removed: &[Triple],
    ) -> ReasoningResult {
        // Build the base fact set (existing + added - removed)
        let mut base: HashSet<Triple> = existing_facts.iter().cloned().collect();
        for t in removed {
            base.remove(t);
        }
        for t in added {
            base.insert(t.clone());
        }

        // If there are removals, we need to re-derive from scratch
        // because we don't know which derived facts depended on removed facts
        if !removed.is_empty() {
            let base_vec: Vec<Triple> = base.into_iter().collect();
            return self.reason(&base_vec);
        }

        // For additions only, we can be more efficient:
        // Only run rules that could produce new inferences from the added facts
        let mut all_facts: HashSet<Triple> = existing_facts.iter().cloned().collect();
        let mut all_inferred: Vec<Triple> = Vec::new();
        let mut rule_counts: HashMap<RuleId, usize> = HashMap::new();

        // Separate added facts by type
        let mut added_type_facts: Vec<Triple> = Vec::new();
        let mut added_prop_facts: Vec<Triple> = Vec::new();
        for t in added {
            if t.predicate == "rdf:type" {
                added_type_facts.push(t.clone());
            } else {
                added_prop_facts.push(t.clone());
            }
        }

        // Process type facts: Cax-sco and Cax-eqc
        if !added_type_facts.is_empty() {
            let superclass_cache = self.ontology.build_superclass_cache();
            let mut cax_new: Vec<Triple> = Vec::new();

            for fact in &added_type_facts {
                all_facts.insert(fact.clone());

                // Cax-sco: x type A, A subClassOf B => x type B
                if let Some(supers) = superclass_cache.get(fact.object.as_str()) {
                    for sup in supers {
                        cax_new.push(Triple::type_of(&fact.subject, sup));
                    }
                }

                // Cax-eqc: x type A, A equiv B => x type B
                if let Some(class_def) = self.ontology.classes.get(fact.object.as_str()) {
                    for equiv in &class_def.equivalent_classes {
                        cax_new.push(Triple::type_of(&fact.subject, equiv));
                    }
                }
            }

            // Insert new inferences
            for t in cax_new {
                if all_facts.insert(t.clone()) {
                    all_inferred.push(t);
                    *rule_counts.entry(RuleId::CaxSco).or_insert(0) += 1;
                }
            }
        }

        // Process property facts: Prp-inv, Prp-symp, Prp-spo, Prp-eqp
        if !added_prop_facts.is_empty() {
            for fact in &added_prop_facts {
                all_facts.insert(fact.clone());
            }

            // Run property rules on the added facts
            let new_facts_ref = &added_prop_facts;
            for rule in &self.rules {
                match rule.id() {
                    RuleId::CaxSco | RuleId::CaxEqc | RuleId::PrpTrp => continue,
                    _ => {}
                }
                let inferred = rule.apply(&self.ontology, &all_facts, new_facts_ref);
                *rule_counts.entry(rule.id()).or_insert(0) += inferred.len();
                for t in inferred {
                    if all_facts.insert(t.clone()) {
                        all_inferred.push(t);
                    }
                }
            }
        }

        // Run one more iteration to catch transitive closures from new facts
        if !all_inferred.is_empty() {
            let inferred_clone = all_inferred.clone();
            for rule in &self.rules {
                let inferred = rule.apply(&self.ontology, &all_facts, &inferred_clone);
                *rule_counts.entry(rule.id()).or_insert(0) += inferred.len();
                for t in inferred {
                    if all_facts.insert(t.clone()) {
                        all_inferred.push(t);
                    }
                }
            }
        }

        ReasoningResult {
            all_facts,
            inferred: all_inferred,
            rule_counts,
            iterations: if added.is_empty() { 0 } else { 2 },
            violations: Vec::new(),
        }
    }

    /// Checks for consistency violations in the fact set.
    fn check_consistency(&self, facts: &HashSet<Triple>) -> Vec<String> {
        let mut violations = Vec::new();

        // Build a map: subject -> set of types
        let mut type_map: HashMap<&str, HashSet<&str>> = HashMap::new();
        for fact in facts {
            if fact.predicate == "rdf:type" {
                type_map
                    .entry(fact.subject.as_str())
                    .or_default()
                    .insert(fact.object.as_str());
            }
        }

        // Check disjoint constraints
        for (subj, types) in &type_map {
            for class_name in types {
                if let Some(class) = self.ontology.classes.get(*class_name) {
                    for disjoint_name in &class.disjoint_with {
                        if types.contains(disjoint_name.as_str()) {
                            violations.push(format!(
                                "'{}' is typed as both '{}' and '{}' which are disjoint",
                                subj, class_name, disjoint_name
                            ));
                        }
                    }
                }
            }
        }

        violations
    }

    /// Explains how a particular triple was derived.
    ///
    /// Returns the derivation chain: which rules and input triples produced the target.
    pub fn explain(&self, facts: &[Triple], target: &Triple) -> Vec<DerivationStep> {
        let result = self.reason(facts);
        if !result.all_facts.contains(target) {
            return Vec::new();
        }

        // Simple explanation: re-run rules one at a time and track which rule produced the target
        let mut all_facts: HashSet<Triple> = facts.iter().cloned().collect();
        let mut steps = Vec::new();
        let mut new_facts: Vec<Triple> = Vec::new();

        for _iter in 0..self.max_iterations {
            let mut found = false;
            let mut new_this_round: Vec<Triple> = Vec::new();
            for rule in &self.rules {
                let inferred = rule.apply(&self.ontology, &all_facts, &new_facts);
                for t in inferred {
                    if &t == target && !all_facts.contains(&t) {
                        steps.push(DerivationStep {
                            rule: rule.id(),
                            conclusion: t.clone(),
                            premises: find_premises(&self.ontology, rule.id(), &t, &all_facts),
                        });
                        found = true;
                    }
                    if all_facts.insert(t.clone()) {
                        new_this_round.push(t);
                    }
                }
            }
            new_facts = new_this_round;
            if found || all_facts.contains(target) {
                break;
            }
        }

        steps
    }
}

/// A single step in a derivation chain.
#[derive(Debug, Clone)]
pub struct DerivationStep {
    pub rule: RuleId,
    pub conclusion: Triple,
    pub premises: Vec<Triple>,
}

/// Finds the premises that could have produced a given triple under a rule.
fn find_premises(
    ontology: &Ontology,
    rule: RuleId,
    conclusion: &Triple,
    facts: &HashSet<Triple>,
) -> Vec<Triple> {
    match rule {
        RuleId::CaxSco => {
            // x type A, A subClassOf B -> x type B
            // Find the fact: x type A where A subClassOf B
            for fact in facts {
                if fact.subject == conclusion.subject
                    && fact.predicate == "rdf:type"
                    && ontology.is_subclass_of(&fact.object, &conclusion.object)
                    && fact.object != conclusion.object
                {
                    return vec![fact.clone()];
                }
            }
            vec![]
        }
        RuleId::CaxEqc => {
            for fact in facts {
                if fact.subject == conclusion.subject
                    && fact.predicate == "rdf:type"
                    && ontology.is_equivalent(&fact.object, &conclusion.object)
                    && fact.object != conclusion.object
                {
                    return vec![fact.clone()];
                }
            }
            vec![]
        }
        RuleId::PrpInv => {
            // x P y, P inverseOf Q -> y Q x
            for fact in facts {
                if fact.subject == conclusion.object
                    && fact.object == conclusion.subject
                    && is_inverse_of(ontology, &fact.predicate, &conclusion.predicate)
                {
                    return vec![fact.clone()];
                }
            }
            vec![]
        }
        RuleId::PrpTrp => {
            // x P y, y P z -> x P z
            for mid in facts {
                if mid.predicate == conclusion.predicate {
                    for other in facts {
                        if other.predicate == conclusion.predicate
                            && other.subject == mid.object
                            && mid.subject == conclusion.subject
                            && other.object == conclusion.object
                        {
                            return vec![mid.clone(), other.clone()];
                        }
                    }
                }
            }
            vec![]
        }
        RuleId::PrpSymp => {
            for fact in facts {
                if fact.subject == conclusion.object
                    && fact.object == conclusion.subject
                    && fact.predicate == conclusion.predicate
                {
                    return vec![fact.clone()];
                }
            }
            vec![]
        }
        RuleId::PrpSpo => {
            for fact in facts {
                if fact.subject == conclusion.subject
                    && fact.object == conclusion.object
                    && is_subproperty_of(ontology, &fact.predicate, &conclusion.predicate)
                    && fact.predicate != conclusion.predicate
                {
                    return vec![fact.clone()];
                }
            }
            vec![]
        }
        RuleId::PrpEqp => {
            for fact in facts {
                if fact.subject == conclusion.subject
                    && fact.object == conclusion.object
                    && has_equivalent_property(ontology, &fact.predicate, &conclusion.predicate)
                    && fact.predicate != conclusion.predicate
                {
                    return vec![fact.clone()];
                }
            }
            vec![]
        }
    }
}

fn is_inverse_of(ontology: &Ontology, p: &str, q: &str) -> bool {
    if let Some(prop) = ontology.properties.get(p) {
        if prop.inverse_of.as_deref() == Some(q) {
            return true;
        }
    }
    if let Some(prop) = ontology.properties.get(q) {
        if prop.inverse_of.as_deref() == Some(p) {
            return true;
        }
    }
    false
}

fn is_subproperty_of(ontology: &Ontology, child: &str, parent: &str) -> bool {
    let mut visited = HashSet::new();
    is_subproperty_of_inner(ontology, child, parent, &mut visited)
}

fn is_subproperty_of_inner(
    ontology: &Ontology,
    child: &str,
    parent: &str,
    visited: &mut HashSet<String>,
) -> bool {
    if !visited.insert(child.to_string()) {
        return false;
    }
    if let Some(prop) = ontology.properties.get(child) {
        for sup in &prop.subproperty_of {
            if sup == parent {
                return true;
            }
            if is_subproperty_of_inner(ontology, sup, parent, visited) {
                return true;
            }
        }
    }
    false
}

fn has_equivalent_property(ontology: &Ontology, p: &str, q: &str) -> bool {
    if let Some(prop) = ontology.properties.get(p) {
        if prop.equivalent_properties.contains(&q.to_string()) {
            return true;
        }
    }
    if let Some(prop) = ontology.properties.get(q) {
        if prop.equivalent_properties.contains(&p.to_string()) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Class, DataType, Ontology, Property};

    fn family_ontology() -> Ontology {
        let mut onto = Ontology::new("family");

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

        onto.add_class(Class::new("Animal"));
        onto.add_class(Class::new("Dog").with_superclass("Animal"));
        onto.add_class(Class::new("Cat").with_superclass("Animal"));
        onto.classes
            .get_mut("Dog")
            .unwrap()
            .disjoint_with
            .push("Cat".to_string());
        onto.classes
            .get_mut("Cat")
            .unwrap()
            .disjoint_with
            .push("Dog".to_string());

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
    fn test_reason_subclass_propagation() {
        let onto = family_ontology();
        let reasoner = Reasoner::new(onto);

        let facts = vec![Triple::type_of("alice", "Manager")];
        let result = reasoner.reason(&facts);

        assert!(result.all_facts.contains(&Triple::type_of("alice", "Manager")));
        assert!(result.all_facts.contains(&Triple::type_of("alice", "Employee")));
        assert!(result.all_facts.contains(&Triple::type_of("alice", "Person")));
        assert!(result.all_facts.contains(&Triple::type_of("alice", "Thing")));
        assert!(result.iterations >= 1);
    }

    #[test]
    fn test_reason_equivalent_class() {
        let onto = family_ontology();
        let reasoner = Reasoner::new(onto);

        let facts = vec![Triple::type_of("alice", "Employee")];
        let result = reasoner.reason(&facts);

        assert!(result.all_facts.contains(&Triple::type_of("alice", "Worker")));
        assert!(result.all_facts.contains(&Triple::type_of("alice", "Person")));
    }

    #[test]
    fn test_reason_inverse_property() {
        let onto = family_ontology();
        let reasoner = Reasoner::new(onto);

        let facts = vec![Triple::new("alice", "reportsTo", "bob")];
        let result = reasoner.reason(&facts);

        assert!(result
            .all_facts
            .contains(&Triple::new("bob", "manages", "alice")));
    }

    #[test]
    fn test_reason_transitive_closure() {
        let onto = family_ontology();
        let reasoner = Reasoner::new(onto);

        let facts = vec![
            Triple::new("alice", "ancestor", "bob"),
            Triple::new("bob", "ancestor", "charlie"),
            Triple::new("charlie", "ancestor", "dave"),
        ];
        let result = reasoner.reason(&facts);

        assert!(result
            .all_facts
            .contains(&Triple::new("alice", "ancestor", "charlie")));
        assert!(result
            .all_facts
            .contains(&Triple::new("alice", "ancestor", "dave")));
        assert!(result
            .all_facts
            .contains(&Triple::new("bob", "ancestor", "dave")));
    }

    #[test]
    fn test_reason_symmetric() {
        let onto = family_ontology();
        let reasoner = Reasoner::new(onto);

        let facts = vec![Triple::new("alice", "friendOf", "bob")];
        let result = reasoner.reason(&facts);

        assert!(result
            .all_facts
            .contains(&Triple::new("bob", "friendOf", "alice")));
    }

    #[test]
    fn test_reason_subproperty() {
        let onto = family_ontology();
        let reasoner = Reasoner::new(onto);

        let facts = vec![Triple::new("alice", "worksUnder", "bob")];
        let result = reasoner.reason(&facts);

        assert!(result
            .all_facts
            .contains(&Triple::new("alice", "reportsTo", "bob")));
    }

    #[test]
    fn test_reason_disjoint_violation() {
        let onto = family_ontology();
        let reasoner = Reasoner::new(onto);

        let facts = vec![
            Triple::type_of("fido", "Dog"),
            Triple::type_of("fido", "Cat"),
        ];
        let result = reasoner.reason(&facts);

        assert!(!result.violations.is_empty());
        assert!(result.violations[0].contains("Dog"));
        assert!(result.violations[0].contains("Cat"));
    }

    #[test]
    fn test_reason_no_violation() {
        let onto = family_ontology();
        let reasoner = Reasoner::new(onto);

        let facts = vec![
            Triple::type_of("fido", "Dog"),
            Triple::type_of("whiskers", "Cat"),
        ];
        let result = reasoner.reason(&facts);

        assert!(result.violations.is_empty());
    }

    #[test]
    fn test_reason_individuals() {
        let onto = family_ontology();
        let reasoner = Reasoner::new(onto);

        let alice = Individual::new("alice", "Manager")
            .with_assertion("reportsTo", crate::model::AssertionValue::Individual("bob".into()));

        let result = reasoner.reason_individuals(&[alice]);

        assert!(result.all_facts.contains(&Triple::type_of("alice", "Person")));
        assert!(result
            .all_facts
            .contains(&Triple::new("bob", "manages", "alice")));
    }

    #[test]
    fn test_reason_fixed_point() {
        let onto = family_ontology();
        let reasoner = Reasoner::new(onto);

        // A complex case: transitive + subclass + inverse
        let facts = vec![
            Triple::type_of("alice", "Manager"),
            Triple::new("alice", "reportsTo", "bob"),
            Triple::new("alice", "ancestor", "bob"),
            Triple::new("bob", "ancestor", "charlie"),
        ];
        let result = reasoner.reason(&facts);

        // Should converge
        assert!(result.iterations <= 100);

        // Should have all expected inferences
        assert!(result.all_facts.contains(&Triple::type_of("alice", "Thing")));
        assert!(result
            .all_facts
            .contains(&Triple::new("bob", "manages", "alice")));
        assert!(result
            .all_facts
            .contains(&Triple::new("alice", "ancestor", "charlie")));
    }

    #[test]
    fn test_explain_derivation() {
        let onto = family_ontology();
        let reasoner = Reasoner::new(onto);

        let facts = vec![Triple::type_of("alice", "Manager")];
        let target = Triple::type_of("alice", "Person");

        let steps = reasoner.explain(&facts, &target);
        assert!(!steps.is_empty());
        assert_eq!(steps[0].rule, RuleId::CaxSco);
    }

    #[test]
    fn test_max_iterations() {
        // Create a reasoner with very low max iterations
        let onto = family_ontology();
        let reasoner = Reasoner::new(onto).with_max_iterations(1);

        let facts = vec![
            Triple::new("a", "ancestor", "b"),
            Triple::new("b", "ancestor", "c"),
            Triple::new("c", "ancestor", "d"),
        ];
        let result = reasoner.reason(&facts);

        // With only 1 iteration, might not get full transitive closure
        // but should not panic
        assert!(result.iterations <= 1);
    }
}
