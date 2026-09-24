//! Graph pattern matching using subgraph isomorphism.
//!
//! Supports finding all subgraph matches of a pattern graph within a larger graph.
//! Uses backtracking with pruning for efficiency.

use std::collections::{HashMap, HashSet};

/// A pattern graph for subgraph matching.
#[derive(Debug, Clone)]
pub struct PatternGraph {
    /// Vertices in the pattern: vertex_id -> labels
    pub vertices: Vec<PatternVertex>,
    /// Edges in the pattern: (source_idx, target_idx, label)
    pub edges: Vec<PatternEdge>,
}

/// A vertex in the pattern graph.
#[derive(Debug, Clone)]
pub struct PatternVertex {
    /// Variable name (for result binding).
    pub variable: String,
    /// Required labels (must match all).
    pub labels: Vec<String>,
    /// Optional index in the pattern.
    pub index: usize,
}

/// An edge in the pattern graph.
#[derive(Debug, Clone)]
pub struct PatternEdge {
    /// Source vertex index.
    pub source: usize,
    /// Target vertex index.
    pub target: usize,
    /// Required edge label.
    pub label: String,
}

/// A match result: mapping from pattern vertices to graph vertices.
pub type PatternMatch = HashMap<String, String>;

/// Find all subgraph matches of a pattern in the graph.
///
/// Returns a list of matches, where each match maps pattern variable names
/// to graph vertex IDs.
pub fn find_pattern_matches(
    graph: &crate::store::GraphStore,
    pattern: &PatternGraph,
) -> Vec<PatternMatch> {
    if pattern.vertices.is_empty() {
        return Vec::new();
    }

    // Find candidate vertices for each pattern vertex
    let candidates = find_candidates(graph, pattern);

    // Check if any pattern vertex has no candidates
    if candidates.iter().any(|c| c.is_empty()) {
        return Vec::new();
    }

    // Use backtracking to find all matches
    let mut matches = Vec::new();
    let mut current_match = HashMap::new();
    let mut used_vertices = HashSet::new();

    backtrack(
        graph,
        pattern,
        &candidates,
        0,
        &mut current_match,
        &mut used_vertices,
        &mut matches,
    );

    matches
}

/// Find candidate graph vertices for each pattern vertex.
fn find_candidates(
    graph: &crate::store::GraphStore,
    pattern: &PatternGraph,
) -> Vec<Vec<String>> {
    pattern
        .vertices
        .iter()
        .map(|pv| {
            // Get all vertices with matching labels
            let mut candidates = Vec::new();
            for label in &pv.labels {
                let vertices = graph.get_vertices_by_label(label);
                for v in vertices {
                    if !candidates.contains(&v.id) {
                        candidates.push(v.id.clone());
                    }
                }
            }
            candidates
        })
        .collect()
}

/// Backtracking search for subgraph isomorphism.
fn backtrack(
    graph: &crate::store::GraphStore,
    pattern: &PatternGraph,
    candidates: &[Vec<String>],
    pattern_idx: usize,
    current_match: &mut PatternMatch,
    used_vertices: &mut HashSet<String>,
    matches: &mut Vec<PatternMatch>,
) {
    if pattern_idx == pattern.vertices.len() {
        // All pattern vertices matched
        matches.push(current_match.clone());
        return;
    }

    let pv = &pattern.vertices[pattern_idx];

    for candidate in &candidates[pattern_idx] {
        if used_vertices.contains(candidate) {
            continue;
        }

        // Check if this candidate satisfies all edge constraints
        if !check_edge_constraints(graph, pattern, pattern_idx, candidate, current_match) {
            continue;
        }

        // Try this candidate
        current_match.insert(pv.variable.clone(), candidate.clone());
        used_vertices.insert(candidate.clone());

        backtrack(
            graph,
            pattern,
            candidates,
            pattern_idx + 1,
            current_match,
            used_vertices,
            matches,
        );

        // Undo
        current_match.remove(&pv.variable);
        used_vertices.remove(candidate);
    }
}

/// Check if a candidate vertex satisfies all edge constraints with already matched vertices.
fn check_edge_constraints(
    graph: &crate::store::GraphStore,
    pattern: &PatternGraph,
    pattern_idx: usize,
    candidate: &str,
    current_match: &PatternMatch,
) -> bool {
    for edge in &pattern.edges {
        // Check edges where this vertex is source or target
        if edge.source == pattern_idx {
            // This vertex is the source, check if there's an edge to the target
            if let Some(target_var) = pattern.vertices.get(edge.target) {
                if let Some(target_vertex) = current_match.get(&target_var.variable) {
                    // Check if edge exists with correct label
                    let out_edges = graph.get_out_edges(candidate);
                    let has_edge = out_edges
                        .iter()
                        .any(|e| e.to == *target_vertex && e.label == edge.label);
                    if !has_edge {
                        return false;
                    }
                }
            }
        } else if edge.target == pattern_idx {
            // This vertex is the target, check if there's an edge from the source
            if let Some(source_var) = pattern.vertices.get(edge.source) {
                if let Some(source_vertex) = current_match.get(&source_var.variable) {
                    let in_edges = graph.get_in_edges(candidate);
                    let has_edge = in_edges
                        .iter()
                        .any(|e| e.from == *source_vertex && e.label == edge.label);
                    if !has_edge {
                        return false;
                    }
                }
            }
        }
    }
    true
}

/// Convenience function: find matches for a simple pattern.
///
/// Example: find all (Drug)-[treats]->(Disease) patterns
pub fn find_simple_pattern(
    graph: &crate::store::GraphStore,
    source_label: &str,
    edge_label: &str,
    target_label: &str,
) -> Vec<(String, String)> {
    let pattern = PatternGraph {
        vertices: vec![
            PatternVertex {
                variable: "src".to_string(),
                labels: vec![source_label.to_string()],
                index: 0,
            },
            PatternVertex {
                variable: "dst".to_string(),
                labels: vec![target_label.to_string()],
                index: 1,
            },
        ],
        edges: vec![PatternEdge {
            source: 0,
            target: 1,
            label: edge_label.to_string(),
        }],
    };

    find_pattern_matches(graph, &pattern)
        .into_iter()
        .filter_map(|m| {
            let src = m.get("src")?.clone();
            let dst = m.get("dst")?.clone();
            Some((src, dst))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Edge, Vertex};

    fn build_test_graph() -> crate::store::GraphStore {
        let store = crate::store::GraphStore::new();

        // Drug-Disease-Protein graph
        store.add_vertex(Vertex::new("D1", vec!["Drug".to_string()])).unwrap();
        store.add_vertex(Vertex::new("D2", vec!["Drug".to_string()])).unwrap();
        store.add_vertex(Vertex::new("Dis1", vec!["Disease".to_string()])).unwrap();
        store.add_vertex(Vertex::new("Dis2", vec!["Disease".to_string()])).unwrap();
        store.add_vertex(Vertex::new("P1", vec!["Protein".to_string()])).unwrap();

        store.add_edge(Edge::new("e1", "D1", "Dis1", "treats")).unwrap();
        store.add_edge(Edge::new("e2", "D1", "Dis2", "treats")).unwrap();
        store.add_edge(Edge::new("e3", "D2", "Dis1", "treats")).unwrap();
        store.add_edge(Edge::new("e4", "Dis1", "P1", "associated_with")).unwrap();

        store
    }

    #[test]
    fn test_simple_pattern_match() {
        let graph = build_test_graph();
        let matches = find_simple_pattern(&graph, "Drug", "treats", "Disease");

        // Should find 3 matches: D1->Dis1, D1->Dis2, D2->Dis1
        assert_eq!(matches.len(), 3);
    }

    #[test]
    fn test_pattern_match_with_labels() {
        let graph = build_test_graph();

        let pattern = PatternGraph {
            vertices: vec![
                PatternVertex {
                    variable: "drug".to_string(),
                    labels: vec!["Drug".to_string()],
                    index: 0,
                },
                PatternVertex {
                    variable: "disease".to_string(),
                    labels: vec!["Disease".to_string()],
                    index: 1,
                },
                PatternVertex {
                    variable: "protein".to_string(),
                    labels: vec!["Protein".to_string()],
                    index: 2,
                },
            ],
            edges: vec![
                PatternEdge {
                    source: 0,
                    target: 1,
                    label: "treats".to_string(),
                },
                PatternEdge {
                    source: 1,
                    target: 2,
                    label: "associated_with".to_string(),
                },
            ],
        };

        let matches = find_pattern_matches(&graph, &pattern);

        // Should find: D1->Dis1->P1, D1->Dis2->(none), D2->Dis1->P1
        // Only D1->Dis1->P1 and D2->Dis1->P1 have both edges
        assert_eq!(matches.len(), 2);
    }

    #[test]
    fn test_pattern_no_match() {
        let graph = build_test_graph();

        // No "causes" edges
        let matches = find_simple_pattern(&graph, "Drug", "causes", "Disease");
        assert_eq!(matches.len(), 0);
    }

    #[test]
    fn test_pattern_empty_graph() {
        let graph = crate::store::GraphStore::new();
        let matches = find_simple_pattern(&graph, "Drug", "treats", "Disease");
        assert_eq!(matches.len(), 0);
    }
}
