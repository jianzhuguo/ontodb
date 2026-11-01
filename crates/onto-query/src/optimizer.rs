// Copyright (c) 2024-2026 Guo Jianzhu (郭建筑)
// Licensed under the Apache License, Version 2.0.
// See LICENSE for details.
//! Query optimizer: planner and cost model for OntoDB.
//!
//! Converts AST to physical execution plans and selects the lowest-cost plan.

pub mod cost;
pub mod planner;

pub use cost::{CostEstimate, CostModel};
pub use planner::{ExecutionPlan, PlanNode, PlanWindowExpr, QueryPlanner};
