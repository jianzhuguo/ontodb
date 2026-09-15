// Copyright (c) 2024-2026 OntoDB Team
// Licensed under the Business Source License 1.1 (BUSL-1.1).
// See LICENSE for details. Change Date: 2031-09-15.

//! Multi-modal fusion query optimizer.
//!
//! Chooses the optimal execution plan when a query spans multiple data modalities
//! (relational + graph + vector + temporal + spatial + ontology).

use std::collections::HashMap;

/// Query modality types.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Modality {
    Relational,
    Graph,
    Vector,
    Temporal,
    Spatial,
    Ontology,
    FullText,
}

/// Query operation on a specific modality.
#[derive(Debug, Clone)]
pub struct QueryOp {
    pub modality: Modality,
    pub operation: String,
    pub estimated_rows: u64,
    pub estimated_cost_us: u64,
    pub selectivity: f64, // 0.0-1.0, lower = more selective
}

/// Execution plan node.
#[derive(Debug, Clone)]
pub enum PlanNode {
    /// Single modality scan
    Scan(QueryOp),
    /// Filter operation
    Filter { child: Box<PlanNode>, predicate: String, selectivity: f64 },
    /// Join two sub-plans
    Join { left: Box<PlanNode>, right: Box<PlanNode>, join_type: JoinType },
    /// Union of sub-plans
    Union { children: Vec<PlanNode> },
    /// Sort
    Sort { child: Box<PlanNode>, keys: Vec<String> },
    /// Limit
    Limit { child: Box<PlanNode>, count: u64 },
    /// Cross-modal fusion (unique to OntoDB)
    Fusion { children: Vec<PlanNode>, fusion_type: FusionType },
}

#[derive(Debug, Clone)]
pub enum JoinType {
    Inner,
    Left,
    Cross,
}

#[derive(Debug, Clone)]
pub enum FusionType {
    /// Entity ID join across modalities
    EntityJoin,
    /// Vector similarity + relational filter
    VectorRelational,
    /// Spatial filter + temporal range
    SpatialTemporal,
    /// Ontology inference + any modality
    OntologyInfer,
    /// Full-text + vector reranking
    TextVectorRerank,
}

/// Cost model for multi-modal queries.
#[derive(Debug, Clone)]
pub struct CostModel {
    /// Cost per row for each modality (microseconds)
    pub scan_cost: HashMap<Modality, f64>,
    /// Join cost multiplier
    pub join_multiplier: f64,
    /// Fusion cost overhead
    pub fusion_overhead: f64,
}

impl Default for CostModel {
    fn default() -> Self {
        let mut scan_cost = HashMap::new();
        scan_cost.insert(Modality::Relational, 0.1);
        scan_cost.insert(Modality::Graph, 1.0);
        scan_cost.insert(Modality::Vector, 0.5);
        scan_cost.insert(Modality::Temporal, 0.2);
        scan_cost.insert(Modality::Spatial, 0.3);
        scan_cost.insert(Modality::Ontology, 2.0);
        scan_cost.insert(Modality::FullText, 0.3);
        Self { scan_cost, join_multiplier: 1.5, fusion_overhead: 1.2 }
    }
}

/// Multi-modal fusion query optimizer.
pub struct FusionOptimizer {
    cost_model: CostModel,
}

impl FusionOptimizer {
    pub fn new() -> Self {
        Self { cost_model: CostModel::default() }
    }

    pub fn with_cost_model(cost_model: CostModel) -> Self {
        Self { cost_model }
    }

    /// Optimize a set of cross-modal query operations into an execution plan.
    pub fn optimize(&self, ops: Vec<QueryOp>) -> PlanNode {
        if ops.is_empty() {
            return PlanNode::Scan(QueryOp {
                modality: Modality::Relational,
                operation: "empty".into(),
                estimated_rows: 0,
                estimated_cost_us: 0,
                selectivity: 1.0,
            });
        }

        if ops.len() == 1 {
            return PlanNode::Scan(ops.into_iter().next().unwrap());
        }

        // Sort by estimated cost (cheapest first — push selective filters down)
        let mut sorted_ops = ops;
        sorted_ops.sort_by(|a, b| {
            let cost_a = self.estimate_cost(a);
            let cost_b = self.estimate_cost(b);
            cost_a.partial_cmp(&cost_b).unwrap_or(std::cmp::Ordering::Equal)
        });

        // Detect fusion patterns
        let modalities: Vec<&Modality> = sorted_ops.iter().map(|o| &o.modality).collect();

        // Vector + Relational → VectorRelational fusion
        if modalities.contains(&&Modality::Vector) && modalities.contains(&&Modality::Relational) {
            return self.build_fusion_plan(&sorted_ops, FusionType::VectorRelational);
        }

        // Spatial + Temporal → SpatialTemporal fusion
        if modalities.contains(&&Modality::Spatial) && modalities.contains(&&Modality::Temporal) {
            return self.build_fusion_plan(&sorted_ops, FusionType::SpatialTemporal);
        }

        // FullText + Vector → TextVectorRerank
        if modalities.contains(&&Modality::FullText) && modalities.contains(&&Modality::Vector) {
            return self.build_fusion_plan(&sorted_ops, FusionType::TextVectorRerank);
        }

        // Ontology + any → OntologyInfer
        if modalities.contains(&&Modality::Ontology) {
            return self.build_fusion_plan(&sorted_ops, FusionType::OntologyInfer);
        }

        // Default: left-deep join tree
        let mut plan = PlanNode::Scan(sorted_ops[0].clone());
        for op in &sorted_ops[1..] {
            plan = PlanNode::Join {
                left: Box::new(plan),
                right: Box::new(PlanNode::Scan(op.clone())),
                join_type: JoinType::Inner,
            };
        }
        plan
    }

    fn build_fusion_plan(&self, ops: &[QueryOp], fusion_type: FusionType) -> PlanNode {
        let children: Vec<PlanNode> = ops.iter().map(|op| PlanNode::Scan(op.clone())).collect();
        PlanNode::Fusion { children, fusion_type }
    }

    fn estimate_cost(&self, op: &QueryOp) -> f64 {
        let base_cost = self.cost_model.scan_cost.get(&op.modality).unwrap_or(&1.0);
        *base_cost * op.estimated_rows as f64 * (1.0 - op.selectivity + 0.01)
    }

    /// Estimate total plan cost.
    pub fn estimate_plan_cost(&self, plan: &PlanNode) -> f64 {
        match plan {
            PlanNode::Scan(op) => self.estimate_cost(op),
            PlanNode::Filter { child, selectivity, .. } => {
                self.estimate_plan_cost(child) * selectivity
            }
            PlanNode::Join { left, right, .. } => {
                self.estimate_plan_cost(left) + self.estimate_plan_cost(right) * self.cost_model.join_multiplier
            }
            PlanNode::Union { children } => {
                children.iter().map(|c| self.estimate_plan_cost(c)).sum()
            }
            PlanNode::Sort { child, .. } => self.estimate_plan_cost(child) * 1.1,
            PlanNode::Limit { child, count } => {
                self.estimate_plan_cost(child).min(*count as f64)
            }
            PlanNode::Fusion { children, .. } => {
                let base: f64 = children.iter().map(|c| self.estimate_plan_cost(c)).sum();
                base * self.cost_model.fusion_overhead
            }
        }
    }
}

/// Print execution plan as tree.
pub fn explain_plan(plan: &PlanNode, depth: usize) -> String {
    let indent = "  ".repeat(depth);
    match plan {
        PlanNode::Scan(op) => format!("{}Scan({:?}) rows={} cost={}µs", indent, op.modality, op.estimated_rows, op.estimated_cost_us),
        PlanNode::Filter { child, predicate, selectivity } => {
            let mut s = format!("{}Filter({}, sel={:.2})\n", indent, predicate, selectivity);
            s += &explain_plan(child, depth + 1);
            s
        }
        PlanNode::Join { left, right, join_type } => {
            let mut s = format!("{:?}Join({:?})\n", indent, join_type);
            s += &explain_plan(left, depth + 1);
            s += "\n";
            s += &explain_plan(right, depth + 1);
            s
        }
        PlanNode::Fusion { children, fusion_type } => {
            let mut s = format!("{}Fusion({:?})\n", indent, fusion_type);
            for child in children {
                s += &explain_plan(child, depth + 1);
                s += "\n";
            }
            s
        }
        PlanNode::Union { children } => {
            let mut s = format!("{}Union\n", indent);
            for child in children {
                s += &explain_plan(child, depth + 1);
                s += "\n";
            }
            s
        }
        PlanNode::Sort { child, keys } => {
            let mut s = format!("{}Sort(keys={:?})\n", indent, keys);
            s += &explain_plan(child, depth + 1);
            s
        }
        PlanNode::Limit { child, count } => {
            let mut s = format!("{}Limit({})\n", indent, count);
            s += &explain_plan(child, depth + 1);
            s
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_op(modality: Modality, rows: u64, selectivity: f64) -> QueryOp {
        QueryOp {
            modality,
            operation: "scan".into(),
            estimated_rows: rows,
            estimated_cost_us: rows * 10,
            selectivity,
        }
    }

    #[test]
    fn test_single_modality() {
        let optimizer = FusionOptimizer::new();
        let plan = optimizer.optimize(vec![make_op(Modality::Relational, 100, 0.5)]);
        match plan {
            PlanNode::Scan(_) => {}
            _ => panic!("Expected Scan"),
        }
    }

    #[test]
    fn test_vector_relational_fusion() {
        let optimizer = FusionOptimizer::new();
        let ops = vec![
            make_op(Modality::Vector, 1000, 0.1),
            make_op(Modality::Relational, 500, 0.5),
        ];
        let plan = optimizer.optimize(ops);
        match plan {
            PlanNode::Fusion { fusion_type: FusionType::VectorRelational, .. } => {}
            _ => panic!("Expected VectorRelational Fusion, got {:?}", plan),
        }
    }

    #[test]
    fn test_spatial_temporal_fusion() {
        let optimizer = FusionOptimizer::new();
        let ops = vec![
            make_op(Modality::Spatial, 200, 0.3),
            make_op(Modality::Temporal, 1000, 0.2),
        ];
        let plan = optimizer.optimize(ops);
        match plan {
            PlanNode::Fusion { fusion_type: FusionType::SpatialTemporal, .. } => {}
            _ => panic!("Expected SpatialTemporal Fusion"),
        }
    }

    #[test]
    fn test_ontology_infer_fusion() {
        let optimizer = FusionOptimizer::new();
        let ops = vec![
            make_op(Modality::Ontology, 50, 0.8),
            make_op(Modality::Relational, 200, 0.3),
        ];
        let plan = optimizer.optimize(ops);
        match plan {
            PlanNode::Fusion { fusion_type: FusionType::OntologyInfer, .. } => {}
            _ => panic!("Expected OntologyInfer Fusion"),
        }
    }

    #[test]
    fn test_text_vector_rerank() {
        let optimizer = FusionOptimizer::new();
        let ops = vec![
            make_op(Modality::FullText, 500, 0.2),
            make_op(Modality::Vector, 1000, 0.1),
        ];
        let plan = optimizer.optimize(ops);
        match plan {
            PlanNode::Fusion { fusion_type: FusionType::TextVectorRerank, .. } => {}
            _ => panic!("Expected TextVectorRerank Fusion"),
        }
    }

    #[test]
    fn test_plan_cost_estimation() {
        let optimizer = FusionOptimizer::new();
        let ops = vec![
            make_op(Modality::Relational, 100, 0.5),
            make_op(Modality::Graph, 50, 0.3),
        ];
        let plan = optimizer.optimize(ops);
        let cost = optimizer.estimate_plan_cost(&plan);
        assert!(cost > 0.0);
    }

    #[test]
    fn test_explain_plan() {
        let optimizer = FusionOptimizer::new();
        let ops = vec![
            make_op(Modality::Vector, 1000, 0.1),
            make_op(Modality::Relational, 500, 0.5),
        ];
        let plan = optimizer.optimize(ops);
        let explanation = explain_plan(&plan, 0);
        assert!(explanation.contains("Fusion"));
        assert!(explanation.contains("Scan"));
    }

    #[test]
    fn test_empty_query() {
        let optimizer = FusionOptimizer::new();
        let plan = optimizer.optimize(vec![]);
        match plan {
            PlanNode::Scan(op) => assert_eq!(op.estimated_rows, 0),
            _ => panic!("Expected empty Scan"),
        }
    }
}
