// Copyright (c) 2024-2026 OntoDB Team
// Licensed under the Business Source License 1.1 (BUSL-1.1).
// See LICENSE for details. Change Date: 2031-09-15.
// On the Change Date, this file will be licensed under Apache License 2.0.
//! Query optimizer: planner and cost model for OntoDB.
//!
//! Converts AST to physical execution plans and selects the lowest-cost plan.

pub mod cost;
pub mod planner;

pub use cost::{CostEstimate, CostModel};
pub use planner::{ExecutionPlan, PlanNode, PlanWindowExpr, QueryPlanner};
