//! Knowledge graph reasoning and rule engine.
//!
//! Provides:
//! - Forward chaining inference (data-driven)
//! - Backward chaining inference (goal-driven)
//! - RDFS/OWL-like reasoning rules
//! - Triple pattern matching
//! - Inference explanation and provenance

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use crate::store::GraphStore;
use crate::model::{Edge, Vertex, PropValue};

/// A triple (subject, predicate, object).
#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct Triple {
    pub subject: String,
    pub predicate: String,
    pub object: String,
}

impl Triple {
    pub fn new(subject: &str, predicate: &str, object: &str) -> Self {
        Self {
            subject: subject.to_string(),
            predicate: predicate.to_string(),
            object: object.to_string(),
        }
    }
}

/// A reasoning rule: IF conditions THEN conclusion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rule {
    /// Rule name/ID.
    pub name: String,
    /// Rule description.
    pub description: String,
    /// Antecedent (conditions): list of triple patterns.
    pub conditions: Vec<TriplePattern>,
    /// Consequent (conclusion): triple to infer.
    pub conclusion: TriplePattern,
    /// Rule priority (higher = more important).
    pub priority: u32,
    /// Whether this rule is enabled.
    pub enabled: bool,
}

/// A triple pattern with optional variables (prefixed with ?).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriplePattern {
    pub subject: PatternTerm,
    pub predicate: PatternTerm,
    pub object: PatternTerm,
}

/// A term in a triple pattern (variable or constant).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PatternTerm {
    /// A variable (e.g., ?x, ?y).
    Variable(String),
    /// A constant value.
    Constant(String),
}

impl PatternTerm {
    pub fn is_variable(&self) -> bool {
        matches!(self, PatternTerm::Variable(_))
    }

    pub fn as_str(&self) -> &str {
        match self {
            PatternTerm::Variable(s) => s,
            PatternTerm::Constant(s) => s,
        }
    }
}

/// Variable binding: variable name -> value.
pub type Bindings = HashMap<String, String>;

/// Inference result with provenance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InferenceResult {
    /// The inferred triple.
    pub triple: Triple,
    /// The rule that produced this inference.
    pub rule_name: String,
    /// The bindings that satisfied the rule.
    pub bindings: Bindings,
    /// Depth of inference (0 = from data, 1 = one rule application, etc.).
    pub depth: u32,
}

/// Knowledge graph reasoner.
pub struct Reasoner {
    /// Built-in RDFS/OWL rules.
    rules: Vec<Rule>,
    /// User-defined custom rules.
    custom_rules: Vec<Rule>,
    /// Inferred triples cache.
    inferred: HashMap<String, HashSet<Triple>>,
    /// Inference history for provenance.
    history: Vec<InferenceResult>,
    /// Maximum inference depth.
    max_depth: u32,
    /// Statistics.
    stats: ReasoningStats,
}

/// Reasoning statistics.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReasoningStats {
    pub rules_applied: u64,
    pub triples_inferred: u64,
    pub inference_time_us: u64,
    pub forward_chain_iterations: u32,
}

impl Reasoner {
    /// Create a new reasoner with default RDFS/OWL rules.
    pub fn new() -> Self {
        let mut reasoner = Self {
            rules: Vec::new(),
            custom_rules: Vec::new(),
            inferred: HashMap::new(),
            history: Vec::new(),
            max_depth: 10,
            stats: ReasoningStats::default(),
        };

        reasoner.add_builtin_rules();
        reasoner
    }

    /// Add built-in RDFS/OWL reasoning rules.
    fn add_builtin_rules(&mut self) {
        // RDFS: subclass transitivity
        self.rules.push(Rule {
            name: "rdfs:subClassOf_transitive".to_string(),
            description: "If A subClassOf B and B subClassOf C, then A subClassOf C".to_string(),
            conditions: vec![
                TriplePattern {
                    subject: PatternTerm::Variable("a".to_string()),
                    predicate: PatternTerm::Constant("subClassOf".to_string()),
                    object: PatternTerm::Variable("b".to_string()),
                },
                TriplePattern {
                    subject: PatternTerm::Variable("b".to_string()),
                    predicate: PatternTerm::Constant("subClassOf".to_string()),
                    object: PatternTerm::Variable("c".to_string()),
                },
            ],
            conclusion: TriplePattern {
                subject: PatternTerm::Variable("a".to_string()),
                predicate: PatternTerm::Constant("subClassOf".to_string()),
                object: PatternTerm::Variable("c".to_string()),
            },
            priority: 10,
            enabled: true,
        });

        // RDFS: subclass instance propagation
        self.rules.push(Rule {
            name: "rdfs:instanceOf_subclass".to_string(),
            description: "If x type A and A subClassOf B, then x type B".to_string(),
            conditions: vec![
                TriplePattern {
                    subject: PatternTerm::Variable("x".to_string()),
                    predicate: PatternTerm::Constant("type".to_string()),
                    object: PatternTerm::Variable("a".to_string()),
                },
                TriplePattern {
                    subject: PatternTerm::Variable("a".to_string()),
                    predicate: PatternTerm::Constant("subClassOf".to_string()),
                    object: PatternTerm::Variable("b".to_string()),
                },
            ],
            conclusion: TriplePattern {
                subject: PatternTerm::Variable("x".to_string()),
                predicate: PatternTerm::Constant("type".to_string()),
                object: PatternTerm::Variable("b".to_string()),
            },
            priority: 10,
            enabled: true,
        });

        // OWL: symmetric property
        self.rules.push(Rule {
            name: "owl:symmetric".to_string(),
            description: "If P is symmetric and x P y, then y P x".to_string(),
            conditions: vec![
                TriplePattern {
                    subject: PatternTerm::Variable("x".to_string()),
                    predicate: PatternTerm::Variable("p".to_string()),
                    object: PatternTerm::Variable("y".to_string()),
                },
                TriplePattern {
                    subject: PatternTerm::Variable("p".to_string()),
                    predicate: PatternTerm::Constant("type".to_string()),
                    object: PatternTerm::Constant("SymmetricProperty".to_string()),
                },
            ],
            conclusion: TriplePattern {
                subject: PatternTerm::Variable("y".to_string()),
                predicate: PatternTerm::Variable("p".to_string()),
                object: PatternTerm::Variable("x".to_string()),
            },
            priority: 10,
            enabled: true,
        });

        // OWL: transitive property
        self.rules.push(Rule {
            name: "owl:transitive".to_string(),
            description: "If P is transitive and x P y and y P z, then x P z".to_string(),
            conditions: vec![
                TriplePattern {
                    subject: PatternTerm::Variable("x".to_string()),
                    predicate: PatternTerm::Variable("p".to_string()),
                    object: PatternTerm::Variable("y".to_string()),
                },
                TriplePattern {
                    subject: PatternTerm::Variable("y".to_string()),
                    predicate: PatternTerm::Variable("p".to_string()),
                    object: PatternTerm::Variable("z".to_string()),
                },
                TriplePattern {
                    subject: PatternTerm::Variable("p".to_string()),
                    predicate: PatternTerm::Constant("type".to_string()),
                    object: PatternTerm::Constant("TransitiveProperty".to_string()),
                },
            ],
            conclusion: TriplePattern {
                subject: PatternTerm::Variable("x".to_string()),
                predicate: PatternTerm::Variable("p".to_string()),
                object: PatternTerm::Variable("z".to_string()),
            },
            priority: 10,
            enabled: true,
        });

        // OWL: inverse property
        self.rules.push(Rule {
            name: "owl:inverseOf".to_string(),
            description: "If P inverseOf Q and x P y, then y Q x".to_string(),
            conditions: vec![
                TriplePattern {
                    subject: PatternTerm::Variable("x".to_string()),
                    predicate: PatternTerm::Variable("p".to_string()),
                    object: PatternTerm::Variable("y".to_string()),
                },
                TriplePattern {
                    subject: PatternTerm::Variable("p".to_string()),
                    predicate: PatternTerm::Constant("inverseOf".to_string()),
                    object: PatternTerm::Variable("q".to_string()),
                },
            ],
            conclusion: TriplePattern {
                subject: PatternTerm::Variable("y".to_string()),
                predicate: PatternTerm::Variable("q".to_string()),
                object: PatternTerm::Variable("x".to_string()),
            },
            priority: 10,
            enabled: true,
        });

        // Custom domain/range inference
        self.rules.push(Rule {
            name: "rdfs:domain".to_string(),
            description: "If P domain C and x P y, then x type C".to_string(),
            conditions: vec![
                TriplePattern {
                    subject: PatternTerm::Variable("x".to_string()),
                    predicate: PatternTerm::Variable("p".to_string()),
                    object: PatternTerm::Variable("y".to_string()),
                },
                TriplePattern {
                    subject: PatternTerm::Variable("p".to_string()),
                    predicate: PatternTerm::Constant("domain".to_string()),
                    object: PatternTerm::Variable("c".to_string()),
                },
            ],
            conclusion: TriplePattern {
                subject: PatternTerm::Variable("x".to_string()),
                predicate: PatternTerm::Constant("type".to_string()),
                object: PatternTerm::Variable("c".to_string()),
            },
            priority: 5,
            enabled: true,
        });

        self.rules.push(Rule {
            name: "rdfs:range".to_string(),
            description: "If P range C and x P y, then y type C".to_string(),
            conditions: vec![
                TriplePattern {
                    subject: PatternTerm::Variable("x".to_string()),
                    predicate: PatternTerm::Variable("p".to_string()),
                    object: PatternTerm::Variable("y".to_string()),
                },
                TriplePattern {
                    subject: PatternTerm::Variable("p".to_string()),
                    predicate: PatternTerm::Constant("range".to_string()),
                    object: PatternTerm::Variable("c".to_string()),
                },
            ],
            conclusion: TriplePattern {
                subject: PatternTerm::Variable("y".to_string()),
                predicate: PatternTerm::Constant("type".to_string()),
                object: PatternTerm::Variable("c".to_string()),
            },
            priority: 5,
            enabled: true,
        });
    }

    /// Add a custom rule.
    pub fn add_rule(&mut self, rule: Rule) {
        self.custom_rules.push(rule);
    }

    /// Get all active rules.
    pub fn active_rules(&self) -> Vec<&Rule> {
        self.rules.iter()
            .chain(self.custom_rules.iter())
            .filter(|r| r.enabled)
            .collect()
    }

    /// Forward chaining inference on a graph store.
    ///
    /// Iteratively applies rules until no new triples can be inferred.
    pub fn forward_chain(&mut self, store: &GraphStore) -> Vec<Triple> {
        let start = std::time::Instant::now();
        let mut all_inferred = Vec::new();
        let mut iteration = 0;

        loop {
            iteration += 1;
            if iteration > self.max_depth {
                break;
            }

            let new_inferred = self.apply_rules(store, &all_inferred);

            if new_inferred.is_empty() {
                break;
            }

            // Add new triples to the store
            for triple in &new_inferred {
                self.add_triple_to_store(store, triple);
            }

            all_inferred.extend(new_inferred);
        }

        self.stats.forward_chain_iterations = iteration;
        self.stats.triples_inferred = all_inferred.len() as u64;
        self.stats.inference_time_us = start.elapsed().as_micros() as u64;

        all_inferred
    }

    /// Apply all active rules once.
    fn apply_rules(&mut self, store: &GraphStore, previously_inferred: &[Triple]) -> Vec<Triple> {
        let mut new_inferred = Vec::new();
        let rules = self.active_rules().into_iter().cloned().collect::<Vec<_>>();

        for rule in &rules {
            let inferred = self.apply_rule(store, rule, previously_inferred);
            for (triple, bindings) in inferred {
                if !self.is_known(store, &triple) && !previously_inferred.contains(&triple) {
                    let result = InferenceResult {
                        triple: triple.clone(),
                        rule_name: rule.name.clone(),
                        bindings,
                        depth: 0,
                    };
                    self.history.push(result);
                    new_inferred.push(triple);
                    self.stats.rules_applied += 1;
                }
            }
        }

        new_inferred
    }

    /// Apply a single rule and return inferred triples with bindings.
    fn apply_rule(
        &self,
        store: &GraphStore,
        rule: &Rule,
        _previously_inferred: &[Triple],
    ) -> Vec<(Triple, Bindings)> {
        if rule.conditions.is_empty() {
            return Vec::new();
        }

        // Find all bindings that satisfy the first condition
        let first_bindings = self.match_pattern(store, &rule.conditions[0]);

        // Filter bindings through remaining conditions
        let mut valid_bindings = first_bindings;

        for condition in &rule.conditions[1..] {
            let mut new_bindings = Vec::new();
            for binding in &valid_bindings {
                let expanded = self.expand_pattern(condition, binding);
                let matches = self.match_pattern(store, &expanded);
                for m in matches {
                    let mut merged = binding.clone();
                    merged.extend(m);
                    new_bindings.push(merged);
                }
            }
            valid_bindings = new_bindings;
        }

        // Generate conclusions
        valid_bindings.into_iter()
            .filter_map(|binding| {
                let conclusion = self.expand_pattern(&rule.conclusion, &binding);
                let triple = self.pattern_to_triple(&conclusion)?;
                Some((triple, binding))
            })
            .collect()
    }

    /// Match a triple pattern against the graph store.
    fn match_pattern(&self, store: &GraphStore, pattern: &TriplePattern) -> Vec<Bindings> {
        let mut results = Vec::new();

        // Get all vertices as starting points
        let vertices = store.get_all_vertices();

        for vertex in &vertices {
            let mut binding = Bindings::new();

            // Match subject
            match &pattern.subject {
                PatternTerm::Variable(var) => {
                    binding.insert(var.clone(), vertex.id.clone());
                }
                PatternTerm::Constant(constant) => {
                    if vertex.id != *constant {
                        continue;
                    }
                }
            }

            // Get outgoing edges
            let edges = store.get_out_edges(&vertex.id);

            for edge in &edges {
                let mut edge_binding = binding.clone();

                // Match predicate
                match &pattern.predicate {
                    PatternTerm::Variable(var) => {
                        edge_binding.insert(var.clone(), edge.label.clone());
                    }
                    PatternTerm::Constant(constant) => {
                        if edge.label != *constant {
                            continue;
                        }
                    }
                }

                // Match object
                match &pattern.object {
                    PatternTerm::Variable(var) => {
                        edge_binding.insert(var.clone(), edge.to.clone());
                    }
                    PatternTerm::Constant(constant) => {
                        if edge.to != *constant {
                            continue;
                        }
                    }
                }

                results.push(edge_binding);
            }
        }

        results
    }

    /// Expand a pattern by replacing variables with their bindings.
    fn expand_pattern(&self, pattern: &TriplePattern, bindings: &Bindings) -> TriplePattern {
        TriplePattern {
            subject: self.expand_term(&pattern.subject, bindings),
            predicate: self.expand_term(&pattern.predicate, bindings),
            object: self.expand_term(&pattern.object, bindings),
        }
    }

    /// Expand a term by replacing variables with their bindings.
    fn expand_term(&self, term: &PatternTerm, bindings: &Bindings) -> PatternTerm {
        match term {
            PatternTerm::Variable(var) => {
                if let Some(value) = bindings.get(var) {
                    PatternTerm::Constant(value.clone())
                } else {
                    term.clone()
                }
            }
            PatternTerm::Constant(_) => term.clone(),
        }
    }

    /// Convert a pattern to a triple (all terms must be constants).
    fn pattern_to_triple(&self, pattern: &TriplePattern) -> Option<Triple> {
        let subject = match &pattern.subject {
            PatternTerm::Constant(s) => s.clone(),
            PatternTerm::Variable(_) => return None,
        };
        let predicate = match &pattern.predicate {
            PatternTerm::Constant(s) => s.clone(),
            PatternTerm::Variable(_) => return None,
        };
        let object = match &pattern.object {
            PatternTerm::Constant(s) => s.clone(),
            PatternTerm::Variable(_) => return None,
        };
        Some(Triple::new(&subject, &predicate, &object))
    }

    /// Check if a triple is already known (in the store).
    fn is_known(&self, store: &GraphStore, triple: &Triple) -> bool {
        // Check edges
        let edges = store.get_out_edges(&triple.subject);
        edges.iter().any(|e| e.label == triple.predicate && e.to == triple.object)
    }

    /// Add a triple to the graph store.
    fn add_triple_to_store(&self, store: &GraphStore, triple: &Triple) {
        // Ensure vertices exist
        if store.get_vertex(&triple.subject).is_none() {
            let _ = store.add_vertex(Vertex::new(&triple.subject, vec![]));
        }
        if store.get_vertex(&triple.object).is_none() {
            let _ = store.add_vertex(Vertex::new(&triple.object, vec![]));
        }

        // Add edge
        let edge_id = format!("{}->{}::{}", triple.subject, triple.object, triple.predicate);
        let _ = store.add_edge(Edge::new(&edge_id, &triple.subject, &triple.object, &triple.predicate));
    }

    /// Backward chaining: find all triples matching a goal pattern.
    pub fn backward_chain(&self, store: &GraphStore, goal: &TriplePattern) -> Vec<Bindings> {
        // First, try direct matching
        let direct_matches = self.match_pattern(store, goal);

        if !direct_matches.is_empty() {
            return direct_matches;
        }

        // Try to prove the goal using rules
        let mut results = Vec::new();

        for rule in self.active_rules() {
            // Check if rule conclusion matches the goal
            if !self.patterns_compatible(&rule.conclusion, goal) {
                continue;
            }

            // Try to prove the rule's conditions
            let condition_bindings = self.prove_conditions(store, &rule.conditions);

            for bindings in condition_bindings {
                // Apply bindings to conclusion
                let expanded_conclusion = self.expand_pattern(&rule.conclusion, &bindings);

                // Check if expanded conclusion matches goal
                if let Some(goal_bindings) = self.match_patterns(&expanded_conclusion, goal) {
                    let mut merged = bindings;
                    merged.extend(goal_bindings);
                    results.push(merged);
                }
            }
        }

        results
    }

    /// Check if two patterns are compatible (can potentially match).
    fn patterns_compatible(&self, a: &TriplePattern, b: &TriplePattern) -> bool {
        // Constants must match
        if let (PatternTerm::Constant(a), PatternTerm::Constant(b)) = (&a.subject, &b.subject) {
            if a != b { return false; }
        }
        if let (PatternTerm::Constant(a), PatternTerm::Constant(b)) = (&a.predicate, &b.predicate) {
            if a != b { return false; }
        }
        if let (PatternTerm::Constant(a), PatternTerm::Constant(b)) = (&a.object, &b.object) {
            if a != b { return false; }
        }
        true
    }

    /// Match two patterns and return bindings.
    fn match_patterns(&self, pattern: &TriplePattern, goal: &TriplePattern) -> Option<Bindings> {
        let mut bindings = Bindings::new();

        // Match subject
        match (&pattern.subject, &goal.subject) {
            (PatternTerm::Constant(a), PatternTerm::Constant(b)) => {
                if a != b { return None; }
            }
            (PatternTerm::Constant(val), PatternTerm::Variable(var)) => {
                bindings.insert(var.clone(), val.clone());
            }
            (PatternTerm::Variable(var), PatternTerm::Constant(val)) => {
                bindings.insert(var.clone(), val.clone());
            }
            _ => {}
        }

        // Similar for predicate and object...
        match (&pattern.predicate, &goal.predicate) {
            (PatternTerm::Constant(a), PatternTerm::Constant(b)) => {
                if a != b { return None; }
            }
            (PatternTerm::Constant(val), PatternTerm::Variable(var)) => {
                bindings.insert(var.clone(), val.clone());
            }
            (PatternTerm::Variable(var), PatternTerm::Constant(val)) => {
                bindings.insert(var.clone(), val.clone());
            }
            _ => {}
        }

        match (&pattern.object, &goal.object) {
            (PatternTerm::Constant(a), PatternTerm::Constant(b)) => {
                if a != b { return None; }
            }
            (PatternTerm::Constant(val), PatternTerm::Variable(var)) => {
                bindings.insert(var.clone(), val.clone());
            }
            (PatternTerm::Variable(var), PatternTerm::Constant(val)) => {
                bindings.insert(var.clone(), val.clone());
            }
            _ => {}
        }

        Some(bindings)
    }

    /// Prove a set of conditions.
    fn prove_conditions(&self, store: &GraphStore, conditions: &[TriplePattern]) -> Vec<Bindings> {
        if conditions.is_empty() {
            return vec![Bindings::new()];
        }

        let first_bindings = self.match_pattern(store, &conditions[0]);
        let mut results = Vec::new();

        for binding in first_bindings {
            let remaining = &conditions[1..];
            if remaining.is_empty() {
                results.push(binding);
            } else {
                // Expand remaining conditions with current bindings
                let expanded: Vec<TriplePattern> = remaining.iter()
                    .map(|c| self.expand_pattern(c, &binding))
                    .collect();

                let sub_results = self.prove_conditions(store, &expanded);
                for sub in sub_results {
                    let mut merged = binding.clone();
                    merged.extend(sub);
                    results.push(merged);
                }
            }
        }

        results
    }

    /// Get inference history (provenance).
    pub fn history(&self) -> &[InferenceResult] {
        &self.history
    }

    /// Get reasoning statistics.
    pub fn stats(&self) -> &ReasoningStats {
        &self.stats
    }

    /// Clear inference cache.
    pub fn clear(&mut self) {
        self.inferred.clear();
        self.history.clear();
        self.stats = ReasoningStats::default();
    }
}

impl Default for Reasoner {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Edge, Vertex};

    fn build_ontology_graph() -> GraphStore {
        let store = GraphStore::new();

        // Classes
        store.add_vertex(Vertex::new("Animal", vec!["Class".to_string()])).unwrap();
        store.add_vertex(Vertex::new("Mammal", vec!["Class".to_string()])).unwrap();
        store.add_vertex(Vertex::new("Dog", vec!["Class".to_string()])).unwrap();
        store.add_vertex(Vertex::new("Cat", vec!["Class".to_string()])).unwrap();

        // Subclass relationships
        store.add_edge(Edge::new("e1", "Mammal", "Animal", "subClassOf")).unwrap();
        store.add_edge(Edge::new("e2", "Dog", "Mammal", "subClassOf")).unwrap();
        store.add_edge(Edge::new("e3", "Cat", "Mammal", "subClassOf")).unwrap();

        // Instances
        store.add_vertex(Vertex::new("Rex", vec!["Instance".to_string()])).unwrap();
        store.add_edge(Edge::new("e4", "Rex", "Dog", "type")).unwrap();

        // Properties
        store.add_vertex(Vertex::new("knows", vec!["Property".to_string()])).unwrap();
        store.add_vertex(Vertex::new("SymmetricProperty", vec!["Class".to_string()])).unwrap();
        store.add_edge(Edge::new("e5", "knows", "SymmetricProperty", "type")).unwrap();

        store.add_vertex(Vertex::new("Alice", vec!["Instance".to_string()])).unwrap();
        store.add_vertex(Vertex::new("Bob", vec!["Instance".to_string()])).unwrap();
        store.add_edge(Edge::new("e6", "Alice", "Bob", "knows")).unwrap();

        store
    }

    #[test]
    fn test_subclass_transitivity() {
        let store = build_ontology_graph();
        let mut reasoner = Reasoner::new();

        let inferred = reasoner.forward_chain(&store);

        // Should infer: Dog subClassOf Animal
        let has_dog_animal = inferred.iter().any(|t|
            t.subject == "Dog" && t.predicate == "subClassOf" && t.object == "Animal"
        );
        assert!(has_dog_animal, "Should infer Dog subClassOf Animal");
    }

    #[test]
    fn test_instance_propagation() {
        let store = build_ontology_graph();
        let mut reasoner = Reasoner::new();

        let inferred = reasoner.forward_chain(&store);

        // Should infer: Rex type Mammal, Rex type Animal
        let has_rex_mammal = inferred.iter().any(|t|
            t.subject == "Rex" && t.predicate == "type" && t.object == "Mammal"
        );
        let has_rex_animal = inferred.iter().any(|t|
            t.subject == "Rex" && t.predicate == "type" && t.object == "Animal"
        );
        assert!(has_rex_mammal, "Should infer Rex type Mammal");
        assert!(has_rex_animal, "Should infer Rex type Animal");
    }

    #[test]
    fn test_symmetric_property() {
        let store = build_ontology_graph();
        let mut reasoner = Reasoner::new();

        let inferred = reasoner.forward_chain(&store);

        // Should infer: Bob knows Alice
        let has_bob_alice = inferred.iter().any(|t|
            t.subject == "Bob" && t.predicate == "knows" && t.object == "Alice"
        );
        assert!(has_bob_alice, "Should infer Bob knows Alice");
    }

    #[test]
    fn test_backward_chaining() {
        let store = build_ontology_graph();
        let reasoner = Reasoner::new();

        let goal = TriplePattern {
            subject: PatternTerm::Constant("Rex".to_string()),
            predicate: PatternTerm::Constant("type".to_string()),
            object: PatternTerm::Variable("class".to_string()),
        };

        let results = reasoner.backward_chain(&store, &goal);
        assert!(!results.is_empty(), "Should find at least one type for Rex");

        // Direct type should be Dog
        let has_dog = results.iter().any(|b| b.get("class") == Some(&"Dog".to_string()));
        assert!(has_dog, "Should find Rex type Dog");
    }

    #[test]
    fn test_custom_rule() {
        let store = build_ontology_graph();
        let mut reasoner = Reasoner::new();

        // Add custom rule: if x knows y, then y knows x (friendOf symmetry)
        reasoner.add_rule(Rule {
            name: "custom:friendOf".to_string(),
            description: "Friend of friend".to_string(),
            conditions: vec![
                TriplePattern {
                    subject: PatternTerm::Variable("x".to_string()),
                    predicate: PatternTerm::Constant("knows".to_string()),
                    object: PatternTerm::Variable("y".to_string()),
                },
            ],
            conclusion: TriplePattern {
                subject: PatternTerm::Variable("y".to_string()),
                predicate: PatternTerm::Constant("friendOf".to_string()),
                object: PatternTerm::Variable("x".to_string()),
            },
            priority: 15,
            enabled: true,
        });

        let inferred = reasoner.forward_chain(&store);

        // Should infer: Bob friendOf Alice
        let has_friend = inferred.iter().any(|t|
            t.subject == "Bob" && t.predicate == "friendOf" && t.object == "Alice"
        );
        assert!(has_friend, "Should infer Bob friendOf Alice");
    }

    #[test]
    fn test_reasoning_stats() {
        let store = build_ontology_graph();
        let mut reasoner = Reasoner::new();

        reasoner.forward_chain(&store);

        let stats = reasoner.stats();
        assert!(stats.rules_applied > 0);
        assert!(stats.triples_inferred > 0);
        assert!(stats.forward_chain_iterations > 0);
    }

    #[test]
    fn test_pattern_matching() {
        let store = build_ontology_graph();
        let reasoner = Reasoner::new();

        let pattern = TriplePattern {
            subject: PatternTerm::Variable("s".to_string()),
            predicate: PatternTerm::Constant("type".to_string()),
            object: PatternTerm::Constant("Dog".to_string()),
        };

        let bindings = reasoner.match_pattern(&store, &pattern);
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].get("s"), Some(&"Rex".to_string()));
    }
}