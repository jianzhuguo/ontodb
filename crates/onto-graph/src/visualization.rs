//! Graph visualization API.
//!
//! Export graph data in various formats for frontend rendering:
//! - DOT (Graphviz) format
//! - JSON (D3.js compatible)
//! - Cytoscape JSON format
//! - Mermaid diagram format

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Visual configuration for graph elements.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VisualConfig {
    /// Node colors by label.
    pub node_colors: HashMap<String, String>,
    /// Edge colors by label.
    pub edge_colors: HashMap<String, String>,
    /// Node sizes by label.
    pub node_sizes: HashMap<String, f64>,
    /// Whether to show node properties.
    pub show_properties: bool,
    /// Maximum number of nodes to display.
    pub max_nodes: Option<usize>,
    /// Layout algorithm hint.
    pub layout: Option<String>,
}

impl VisualConfig {
    /// Create a biomedical graph visual config.
    pub fn biomedical() -> Self {
        let mut node_colors = HashMap::new();
        node_colors.insert("Drug".to_string(), "#4CAF50".to_string());
        node_colors.insert("Disease".to_string(), "#F44336".to_string());
        node_colors.insert("Protein".to_string(), "#2196F3".to_string());
        node_colors.insert("Variant".to_string(), "#FF9800".to_string());

        let mut edge_colors = HashMap::new();
        edge_colors.insert("treats".to_string(), "#4CAF50".to_string());
        edge_colors.insert("causes".to_string(), "#F44336".to_string());
        edge_colors.insert("targets".to_string(), "#2196F3".to_string());
        edge_colors.insert("associated_with".to_string(), "#9C27B0".to_string());

        let mut node_sizes = HashMap::new();
        node_sizes.insert("Drug".to_string(), 1.5);
        node_sizes.insert("Disease".to_string(), 1.2);
        node_sizes.insert("Protein".to_string(), 1.0);

        Self {
            node_colors,
            edge_colors,
            node_sizes,
            show_properties: true,
            max_nodes: Some(500),
            layout: Some("force".to_string()),
        }
    }
}

/// Export graph to DOT (Graphviz) format.
pub fn to_dot(
    store: &crate::store::GraphStore,
    config: Option<&VisualConfig>,
) -> String {
    let config = config.cloned().unwrap_or_default();
    let mut dot = String::from("digraph G {\n");
    dot.push_str("  rankdir=LR;\n");
    dot.push_str("  node [shape=ellipse, style=filled, fillcolor=lightblue];\n\n");

    // Add vertices
    let vertices = store.get_all_vertices();
    let max_nodes = config.max_nodes.unwrap_or(vertices.len());
    let vertices_to_show = vertices.iter().take(max_nodes);

    for vertex in vertices_to_show {
        let label = vertex.properties
            .get("name")
            .and_then(|v| match v {
                crate::model::PropValue::String(s) => Some(s.clone()),
                _ => None,
            })
            .unwrap_or_else(|| vertex.id.clone());

        let color = vertex.labels.first()
            .and_then(|l| config.node_colors.get(l))
            .cloned()
            .unwrap_or_else(|| "lightblue".to_string());

        let size = vertex.labels.first()
            .and_then(|l| config.node_sizes.get(l))
            .unwrap_or(&1.0);

        dot.push_str(&format!(
            "  \"{}\" [label=\"{}\", fillcolor=\"{}\", width={}];\n",
            vertex.id, label, color, size
        ));
    }

    dot.push('\n');

    // Add edges
    for vertex in &vertices[..vertices.len().min(max_nodes)] {
        for edge in store.get_out_edges(&vertex.id) {
            // Skip edges to vertices not in the display set
            if !vertices[..vertices.len().min(max_nodes)]
                .iter()
                .any(|v| v.id == edge.to)
            {
                continue;
            }

            let color = config.edge_colors.get(&edge.label)
                .cloned()
                .unwrap_or_else(|| "gray".to_string());

            let label = if config.show_properties {
                edge.properties
                    .get("weight")
                    .and_then(|v| match v {
                        crate::model::PropValue::Float(f) => Some(format!("{:.2}", f)),
                        _ => None,
                    })
                    .unwrap_or_else(|| edge.label.clone())
            } else {
                edge.label.clone()
            };

            dot.push_str(&format!(
                "  \"{}\" -> \"{}\" [label=\"{}\", color=\"{}\"];\n",
                edge.from, edge.to, label, color
            ));
        }
    }

    dot.push_str("}\n");
    dot
}

/// Graph data in D3.js compatible JSON format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct D3Graph {
    pub nodes: Vec<D3Node>,
    pub links: Vec<D3Link>,
}

/// A node in D3.js format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct D3Node {
    pub id: String,
    pub label: String,
    pub group: String,
    pub properties: HashMap<String, serde_json::Value>,
    pub size: f64,
    pub color: String,
}

/// A link in D3.js format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct D3Link {
    pub source: String,
    pub target: String,
    pub label: String,
    pub color: String,
    pub weight: f64,
}

/// Export graph to D3.js compatible JSON format.
pub fn to_d3_json(
    store: &crate::store::GraphStore,
    config: Option<&VisualConfig>,
) -> D3Graph {
    let config = config.cloned().unwrap_or_default();
    let vertices = store.get_all_vertices();
    let max_nodes = config.max_nodes.unwrap_or(vertices.len());

    let nodes: Vec<D3Node> = vertices
        .iter()
        .take(max_nodes)
        .map(|v| {
            let group = v.labels.first().cloned().unwrap_or_else(|| "default".to_string());
            let color = config.node_colors.get(&group)
                .cloned()
                .unwrap_or_else(|| "#1f77b4".to_string());
            let size = config.node_sizes.get(&group).copied().unwrap_or(1.0);

            let properties = if config.show_properties {
                v.properties.iter().map(|(k, val)| {
                    let json_val = match val {
                        crate::model::PropValue::String(s) => serde_json::json!(s),
                        crate::model::PropValue::Int(i) => serde_json::json!(i),
                        crate::model::PropValue::Float(f) => serde_json::json!(f),
                        crate::model::PropValue::Bool(b) => serde_json::json!(b),
                        _ => serde_json::json!(null),
                    };
                    (k.clone(), json_val)
                }).collect()
            } else {
                HashMap::new()
            };

            D3Node {
                id: v.id.clone(),
                label: v.properties
                    .get("name")
                    .and_then(|val| match val {
                        crate::model::PropValue::String(s) => Some(s.clone()),
                        _ => None,
                    })
                    .unwrap_or_else(|| v.id.clone()),
                group,
                properties,
                size,
                color,
            }
        })
        .collect();

    let node_ids: std::collections::HashSet<String> = nodes.iter().map(|n| n.id.clone()).collect();

    let mut links = Vec::new();
    for vertex in &vertices[..vertices.len().min(max_nodes)] {
        for edge in store.get_out_edges(&vertex.id) {
            if !node_ids.contains(&edge.to) {
                continue;
            }

            let color = config.edge_colors.get(&edge.label)
                .cloned()
                .unwrap_or_else(|| "#999".to_string());

            let weight = edge.properties
                .get("weight")
                .and_then(|v| match v {
                    crate::model::PropValue::Float(f) => Some(*f),
                    crate::model::PropValue::Int(i) => Some(*i as f64),
                    _ => None,
                })
                .unwrap_or(1.0);

            links.push(D3Link {
                source: edge.from.clone(),
                target: edge.to.clone(),
                label: edge.label.clone(),
                color,
                weight,
            });
        }
    }

    D3Graph { nodes, links }
}

/// Export graph to Cytoscape.js JSON format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CytoscapeGraph {
    pub elements: CytoscapeElements,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CytoscapeElements {
    pub nodes: Vec<CytoscapeNode>,
    pub edges: Vec<CytoscapeEdge>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CytoscapeNode {
    pub data: CytoscapeNodeData,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CytoscapeNodeData {
    pub id: String,
    pub label: String,
    pub group: String,
    #[serde(flatten)]
    pub properties: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CytoscapeEdge {
    pub data: CytoscapeEdgeData,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CytoscapeEdgeData {
    pub id: String,
    pub source: String,
    pub target: String,
    pub label: String,
    pub weight: f64,
}

/// Export graph to Cytoscape.js JSON format.
pub fn to_cytoscape_json(
    store: &crate::store::GraphStore,
    config: Option<&VisualConfig>,
) -> CytoscapeGraph {
    let config = config.cloned().unwrap_or_default();
    let vertices = store.get_all_vertices();
    let max_nodes = config.max_nodes.unwrap_or(vertices.len());

    let nodes: Vec<CytoscapeNode> = vertices
        .iter()
        .take(max_nodes)
        .map(|v| {
            let group = v.labels.first().cloned().unwrap_or_else(|| "default".to_string());

            let mut properties = HashMap::new();
            if config.show_properties {
                for (k, val) in &v.properties {
                    let json_val = match val {
                        crate::model::PropValue::String(s) => serde_json::json!(s),
                        crate::model::PropValue::Int(i) => serde_json::json!(i),
                        crate::model::PropValue::Float(f) => serde_json::json!(f),
                        crate::model::PropValue::Bool(b) => serde_json::json!(b),
                        _ => serde_json::json!(null),
                    };
                    properties.insert(k.clone(), json_val);
                }
            }

            CytoscapeNode {
                data: CytoscapeNodeData {
                    id: v.id.clone(),
                    label: v.properties
                        .get("name")
                        .and_then(|val| match val {
                            crate::model::PropValue::String(s) => Some(s.clone()),
                            _ => None,
                        })
                        .unwrap_or_else(|| v.id.clone()),
                    group,
                    properties,
                },
            }
        })
        .collect();

    let node_ids: std::collections::HashSet<String> = nodes.iter().map(|n| n.data.id.clone()).collect();

    let mut edges = Vec::new();
    let mut edge_id = 0;
    for vertex in &vertices[..vertices.len().min(max_nodes)] {
        for edge in store.get_out_edges(&vertex.id) {
            if !node_ids.contains(&edge.to) {
                continue;
            }

            let weight = edge.properties
                .get("weight")
                .and_then(|v| match v {
                    crate::model::PropValue::Float(f) => Some(*f),
                    crate::model::PropValue::Int(i) => Some(*i as f64),
                    _ => None,
                })
                .unwrap_or(1.0);

            edges.push(CytoscapeEdge {
                data: CytoscapeEdgeData {
                    id: format!("e{}", edge_id),
                    source: edge.from.clone(),
                    target: edge.to.clone(),
                    label: edge.label.clone(),
                    weight,
                },
            });
            edge_id += 1;
        }
    }

    CytoscapeGraph {
        elements: CytoscapeElements { nodes, edges },
    }
}

/// Export graph to Mermaid diagram format.
pub fn to_mermaid(
    store: &crate::store::GraphStore,
    config: Option<&VisualConfig>,
) -> String {
    let config = config.cloned().unwrap_or_default();
    let mut mermaid = String::from("graph LR\n");

    let vertices = store.get_all_vertices();
    let max_nodes = config.max_nodes.unwrap_or(vertices.len());

    // Add nodes
    for vertex in &vertices[..vertices.len().min(max_nodes)] {
        let label = vertex.properties
            .get("name")
            .and_then(|v| match v {
                crate::model::PropValue::String(s) => Some(s.clone()),
                _ => None,
            })
            .unwrap_or_else(|| vertex.id.clone());

        let node_type = vertex.labels.first().cloned().unwrap_or_else(|| "default".to_string());
        let style = match node_type.as_str() {
            "Drug" => "{{",
            "Disease" => "((",
            "Protein" => "[",
            _ => "[",
        };
        let style_end = match node_type.as_str() {
            "Drug" => "}}",
            "Disease" => "))",
            "Protein" => "]",
            _ => "]",
        };

        mermaid.push_str(&format!(
            "    {}{}{}{}\n",
            sanitize_id(&vertex.id), style, label, style_end
        ));
    }

    mermaid.push('\n');

    // Add edges
    let node_ids: std::collections::HashSet<String> = vertices[..vertices.len().min(max_nodes)]
        .iter()
        .map(|v| v.id.clone())
        .collect();

    for vertex in &vertices[..vertices.len().min(max_nodes)] {
        for edge in store.get_out_edges(&vertex.id) {
            if !node_ids.contains(&edge.to) {
                continue;
            }

            mermaid.push_str(&format!(
                "    {} -->|{}| {}\n",
                sanitize_id(&edge.from),
                edge.label,
                sanitize_id(&edge.to)
            ));
        }
    }

    mermaid
}

/// Sanitize an ID for use in Mermaid diagrams.
fn sanitize_id(id: &str) -> String {
    id.replace("::", "_")
        .replace(" ", "_")
        .replace("-", "_")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Edge, Vertex};

    fn build_test_graph() -> crate::store::GraphStore {
        let store = crate::store::GraphStore::new();

        store.add_vertex(Vertex::new("D1", vec!["Drug".to_string()])
            .with_property("name", crate::model::PropValue::String("Aspirin".to_string())))
            .unwrap();
        store.add_vertex(Vertex::new("Dis1", vec!["Disease".to_string()])
            .with_property("name", crate::model::PropValue::String("Headache".to_string())))
            .unwrap();
        store.add_vertex(Vertex::new("P1", vec!["Protein".to_string()])
            .with_property("name", crate::model::PropValue::String("COX-2".to_string())))
            .unwrap();

        store.add_edge(Edge::new("e1", "D1", "Dis1", "treats")).unwrap();
        store.add_edge(Edge::new("e2", "Dis1", "P1", "associated_with")
            .with_property("weight", crate::model::PropValue::Float(0.85)))
            .unwrap();

        store
    }

    #[test]
    fn test_dot_export() {
        let graph = build_test_graph();
        let dot = to_dot(&graph, None);

        assert!(dot.contains("digraph G"));
        assert!(dot.contains("Aspirin"));
        assert!(dot.contains("Headache"));
        assert!(dot.contains("treats"));
    }

    #[test]
    fn test_d3_json_export() {
        let graph = build_test_graph();
        let d3 = to_d3_json(&graph, None);

        assert_eq!(d3.nodes.len(), 3);
        assert_eq!(d3.links.len(), 2);

        let aspirin = d3.nodes.iter().find(|n| n.id == "D1").unwrap();
        assert_eq!(aspirin.label, "Aspirin");
    }

    #[test]
    fn test_cytoscape_export() {
        let graph = build_test_graph();
        let cy = to_cytoscape_json(&graph, None);

        assert_eq!(cy.elements.nodes.len(), 3);
        assert_eq!(cy.elements.edges.len(), 2);
    }

    #[test]
    fn test_mermaid_export() {
        let graph = build_test_graph();
        let mermaid = to_mermaid(&graph, None);

        assert!(mermaid.contains("graph LR"));
        assert!(mermaid.contains("Aspirin"));
        assert!(mermaid.contains("treats"));
    }

    #[test]
    fn test_visual_config_biomedical() {
        let config = VisualConfig::biomedical();
        assert_eq!(config.node_colors.get("Drug"), Some(&"#4CAF50".to_string()));
        assert_eq!(config.edge_colors.get("treats"), Some(&"#4CAF50".to_string()));
    }

    #[test]
    fn test_max_nodes_limit() {
        let graph = build_test_graph();
        let config = VisualConfig {
            max_nodes: Some(2),
            ..Default::default()
        };
        let d3 = to_d3_json(&graph, Some(&config));

        assert_eq!(d3.nodes.len(), 2);
    }
}
