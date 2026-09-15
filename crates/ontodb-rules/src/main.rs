// Copyright (c) 2024-2026 OntoDB Team
// Licensed under the Business Source License 1.1 (BUSL-1.1).
// See LICENSE for details. Change Date: 2031-09-15.
// On the Change Date, this file will be licensed under Apache License 2.0.
//! SemEBL Rules API Server
//!
//! 独立的规则引擎REST API服务器，端口7912。
//! 支持规则增删改查、热更新、DSL解析、实时测试。

use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

// ============================================================
// 数据结构
// ============================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleDto {
    pub rule_id: String,
    pub name: String,
    pub dsl: String,
    pub priority: String,
    pub enabled: bool,
    pub conditions: Vec<ConditionDto>,
    pub actions: Vec<ActionDto>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConditionDto {
    pub entity: String,
    pub attribute: String,
    pub operator: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionDto {
    pub entity: String,
    pub attribute: String,
    pub value: String,
}

pub struct RuleEngineState {
    pub rules: Mutex<HashMap<String, RuleDto>>,
    pub rules_dir: String,
}

impl RuleEngineState {
    pub fn new(rules_dir: String) -> Self {
        Self {
            rules: Mutex::new(HashMap::new()),
            rules_dir,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct CreateRuleRequest {
    pub name: String,
    pub dsl: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateRuleRequest {
    pub name: Option<String>,
    pub dsl: Option<String>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct TestRuleRequest {
    pub facts: HashMap<String, HashMap<String, String>>,
}

#[derive(Debug, Serialize)]
pub struct RuleResponse {
    pub success: bool,
    pub rule: Option<RuleDto>,
    pub message: String,
}

#[derive(Debug, Serialize)]
pub struct RulesListResponse {
    pub rules: Vec<RuleDto>,
    pub total: usize,
}

// ============================================================
// API Handlers
// ============================================================

async fn list_rules(State(state): State<Arc<RuleEngineState>>) -> impl IntoResponse {
    let rules = state.rules.lock().unwrap();
    let rules_vec: Vec<RuleDto> = rules.values().cloned().collect();
    let total = rules_vec.len();
    Json(RulesListResponse {
        rules: rules_vec,
        total,
    })
}

async fn create_rule(
    State(state): State<Arc<RuleEngineState>>,
    Json(req): Json<CreateRuleRequest>,
) -> impl IntoResponse {
    let rule_id = format!("rule_{}", chrono::Utc::now().timestamp_millis());
    let (conditions, actions) = parse_dsl(&req.dsl);
    let priority = extract_priority(&req.dsl);

    let rule = RuleDto {
        rule_id: rule_id.clone(),
        name: req.name,
        dsl: req.dsl,
        priority,
        enabled: true,
        conditions,
        actions,
        created_at: chrono::Utc::now().to_rfc3339(),
        updated_at: chrono::Utc::now().to_rfc3339(),
    };

    state.rules.lock().unwrap().insert(rule_id, rule.clone());
    let _ = save_rule_file(&state.rules_dir, &rule);

    (
        StatusCode::CREATED,
        Json(RuleResponse {
            success: true,
            rule: Some(rule),
            message: "Rule created".to_string(),
        }),
    )
}

async fn update_rule(
    axum::extract::Path(rule_id): axum::extract::Path<String>,
    State(state): State<Arc<RuleEngineState>>,
    Json(req): Json<UpdateRuleRequest>,
) -> impl IntoResponse {
    let mut rules = state.rules.lock().unwrap();
    match rules.get_mut(&rule_id) {
        Some(rule) => {
            if let Some(name) = req.name {
                rule.name = name;
            }
            if let Some(dsl) = req.dsl {
                let (conditions, actions) = parse_dsl(&dsl);
                rule.conditions = conditions;
                rule.actions = actions;
                rule.priority = extract_priority(&dsl);
                rule.dsl = dsl;
            }
            if let Some(enabled) = req.enabled {
                rule.enabled = enabled;
            }
            rule.updated_at = chrono::Utc::now().to_rfc3339();
            let updated = rule.clone();
            let _ = save_rule_file(&state.rules_dir, &updated);
            (
                StatusCode::OK,
                Json(serde_json::json!({"success": true, "rule": updated})),
            )
                .into_response()
        }
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"success": false, "message": "Not found"})),
        )
            .into_response(),
    }
}

async fn delete_rule(
    axum::extract::Path(rule_id): axum::extract::Path<String>,
    State(state): State<Arc<RuleEngineState>>,
) -> impl IntoResponse {
    let mut rules = state.rules.lock().unwrap();
    if rules.remove(&rule_id).is_some() {
        (StatusCode::OK, Json(serde_json::json!({"success": true})))
    } else {
        (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"success": false})),
        )
    }
}

async fn test_rules(
    State(state): State<Arc<RuleEngineState>>,
    Json(req): Json<TestRuleRequest>,
) -> impl IntoResponse {
    let rules = state.rules.lock().unwrap();
    let mut results = Vec::new();

    for rule in rules.values() {
        if !rule.enabled {
            continue;
        }
        if evaluate(&rule.conditions, &req.facts) {
            results.push(serde_json::json!({
                "rule": rule.name,
                "triggered": true,
                "actions": rule.actions,
            }));
        }
    }

    Json(serde_json::json!({
        "triggered": results.len(),
        "total": rules.len(),
        "results": results,
    }))
}

async fn reload_rules(State(state): State<Arc<RuleEngineState>>) -> impl IntoResponse {
    let dir = PathBuf::from(&state.rules_dir);
    if !dir.exists() {
        std::fs::create_dir_all(&dir).unwrap();
        return Json(serde_json::json!({"loaded": 0}));
    }

    let mut loaded = 0;
    let mut rules = state.rules.lock().unwrap();
    rules.clear();

    for entry in std::fs::read_dir(&dir).unwrap().flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "dsl") {
            continue;
        }
        if let Ok(content) = std::fs::read_to_string(&path) {
            for rule in parse_dsl_file(&content) {
                rules.insert(rule.rule_id.clone(), rule);
                loaded += 1;
            }
        }
    }

    Json(serde_json::json!({"loaded": loaded}))
}

async fn rule_stats(State(state): State<Arc<RuleEngineState>>) -> impl IntoResponse {
    let rules = state.rules.lock().unwrap();
    let total = rules.len();
    let enabled = rules.values().filter(|r| r.enabled).count();
    Json(serde_json::json!({"total": total, "enabled": enabled}))
}

// ============================================================
// DSL解析
// ============================================================

fn parse_dsl(dsl: &str) -> (Vec<ConditionDto>, Vec<ActionDto>) {
    let mut conditions = Vec::new();
    let mut actions = Vec::new();
    let mut in_when = false;
    let mut in_then = false;

    for line in dsl.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line == "WHEN:" {
            in_when = true;
            in_then = false;
            continue;
        }
        if line == "THEN:" {
            in_when = false;
            in_then = true;
            continue;
        }
        if line.starts_with("RULE:")
            || line.starts_with("ID:")
            || line.starts_with("PRIORITY:")
            || line.starts_with("ENABLED:")
        {
            continue;
        }

        if in_when {
            if let Some(c) = parse_condition(line) {
                conditions.push(c);
            }
        } else if in_then {
            if let Some(a) = parse_action(line) {
                actions.push(a);
            }
        }
    }
    (conditions, actions)
}

fn parse_condition(line: &str) -> Option<ConditionDto> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() < 3 {
        return None;
    }
    let dot = parts[0].find('.')?;
    Some(ConditionDto {
        entity: parts[0][..dot].to_string(),
        attribute: parts[0][dot + 1..].to_string(),
        operator: parts[1].to_string(),
        value: parts[2..].join(" ").trim_matches('"').to_string(),
    })
}

fn parse_action(line: &str) -> Option<ActionDto> {
    let parts: Vec<&str> = line.splitn(2, '=').collect();
    if parts.len() < 2 {
        return None;
    }
    let dot = parts[0].trim().find('.')?;
    let attr = parts[0].trim();
    Some(ActionDto {
        entity: attr[..dot].to_string(),
        attribute: attr[dot + 1..].to_string(),
        value: parts[1].trim().trim_matches('"').to_string(),
    })
}

fn extract_priority(dsl: &str) -> String {
    for line in dsl.lines() {
        if line.trim().starts_with("PRIORITY:") {
            return line.trim()[9..].trim().to_string();
        }
    }
    "Medium".to_string()
}

fn evaluate(conditions: &[ConditionDto], facts: &HashMap<String, HashMap<String, String>>) -> bool {
    conditions.iter().all(|c| {
        facts
            .get(&c.entity)
            .and_then(|m| m.get(&c.attribute))
            .is_some_and(|v| compare(&c.operator, v, &c.value))
    })
}

fn compare(op: &str, a: &str, b: &str) -> bool {
    if let (Ok(a), Ok(b)) = (a.parse::<f64>(), b.parse::<f64>()) {
        match op {
            ">" => a > b,
            "<" => a < b,
            "=" | "==" => (a - b).abs() < f64::EPSILON,
            "!=" => (a - b).abs() > f64::EPSILON,
            ">=" => a >= b,
            "<=" => a <= b,
            _ => false,
        }
    } else {
        match op {
            "=" | "==" => a == b,
            "!=" => a != b,
            "contains" => a.contains(b),
            _ => false,
        }
    }
}

// ============================================================
// 规则导入导出
// ============================================================

#[derive(Debug, Deserialize)]
pub struct ImportRequest {
    pub dsl: String,
    #[serde(default)]
    pub overwrite: bool,
}

#[derive(Debug, Deserialize)]
pub struct ExportFileRequest {
    pub path: String,
}

#[derive(Debug, Deserialize)]
pub struct ImportFileRequest {
    pub path: String,
    #[serde(default)]
    pub overwrite: bool,
}

/// GET /api/rules/export - 导出所有规则为DSL
async fn export_rules(State(state): State<Arc<RuleEngineState>>) -> impl IntoResponse {
    let rules = state.rules.lock().unwrap();
    let mut dsl_output = String::new();
    for rule in rules.values() {
        dsl_output.push_str(&rule.dsl);
        dsl_output.push_str("\n\n");
    }
    Json(serde_json::json!({
        "count": rules.len(),
        "dsl": dsl_output,
        "rules": rules.values().collect::<Vec<_>>(),
    }))
}

/// POST /api/rules/import - 从DSL文本导入规则
async fn import_rules(
    State(state): State<Arc<RuleEngineState>>,
    Json(req): Json<ImportRequest>,
) -> impl IntoResponse {
    let parsed = parse_dsl_file(&req.dsl);
    let mut imported = 0;
    let mut skipped = 0;
    {
        let mut rules = state.rules.lock().unwrap();
        for rule in parsed {
            if req.overwrite || !rules.contains_key(&rule.rule_id) {
                let _ = save_rule_file(&state.rules_dir, &rule);
                rules.insert(rule.rule_id.clone(), rule);
                imported += 1;
            } else {
                skipped += 1;
            }
        }
    }
    Json(serde_json::json!({
        "imported": imported,
        "skipped": skipped,
        "total": state.rules.lock().unwrap().len(),
    }))
}

/// POST /api/rules/export/file - 导出规则到文件
async fn export_rules_to_file(
    State(state): State<Arc<RuleEngineState>>,
    Json(req): Json<ExportFileRequest>,
) -> impl IntoResponse {
    let rules = state.rules.lock().unwrap();
    let mut dsl_output = String::new();
    for rule in rules.values() {
        dsl_output.push_str(&rule.dsl);
        dsl_output.push_str("\n\n");
    }
    match std::fs::write(&req.path, &dsl_output) {
        Ok(_) => Json(serde_json::json!({"success": true, "path": req.path, "count": rules.len()})),
        Err(e) => Json(serde_json::json!({"success": false, "error": e.to_string()})),
    }
}

/// POST /api/rules/import/file - 从文件导入规则
async fn import_rules_from_file(
    State(state): State<Arc<RuleEngineState>>,
    Json(req): Json<ImportFileRequest>,
) -> impl IntoResponse {
    let content = match std::fs::read_to_string(&req.path) {
        Ok(c) => c,
        Err(e) => return Json(serde_json::json!({"success": false, "error": e.to_string()})),
    };
    let parsed = parse_dsl_file(&content);
    let mut imported = 0;
    let mut skipped = 0;
    {
        let mut rules = state.rules.lock().unwrap();
        for rule in parsed {
            if req.overwrite || !rules.contains_key(&rule.rule_id) {
                let _ = save_rule_file(&state.rules_dir, &rule);
                rules.insert(rule.rule_id.clone(), rule);
                imported += 1;
            } else {
                skipped += 1;
            }
        }
    }
    Json(
        serde_json::json!({"success": true, "path": req.path, "imported": imported, "skipped": skipped}),
    )
}

fn save_rule_file(dir: &str, rule: &RuleDto) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    std::fs::write(
        PathBuf::from(dir).join(format!("{}.dsl", rule.rule_id)),
        &rule.dsl,
    )
    .map_err(|e| e.to_string())
}

fn parse_dsl_file(content: &str) -> Vec<RuleDto> {
    let mut rules = Vec::new();
    let mut name = String::new();
    let mut id = String::new();
    let mut lines = Vec::new();
    let mut in_rule = false;

    for line in content.lines() {
        let t = line.trim();
        if t.starts_with("RULE:") {
            if in_rule && !lines.is_empty() {
                let dsl = lines.join("\n");
                let (c, a) = parse_dsl(&dsl);
                rules.push(RuleDto {
                    rule_id: id.clone(),
                    name: name.clone(),
                    dsl,
                    priority: extract_priority(&lines.join("\n")),
                    enabled: true,
                    conditions: c,
                    actions: a,
                    created_at: String::new(),
                    updated_at: String::new(),
                });
            }
            name = t.strip_prefix("RULE:").unwrap_or(t).trim().to_string();
            lines.clear();
            lines.push(line.to_string());
            in_rule = true;
        } else if t.starts_with("ID:") && in_rule {
            id = t[3..].trim().to_string();
            lines.push(line.to_string());
        } else if in_rule {
            lines.push(line.to_string());
        }
    }

    if in_rule && !lines.is_empty() {
        let dsl = lines.join("\n");
        let (c, a) = parse_dsl(&dsl);
        rules.push(RuleDto {
            rule_id: id,
            name,
            dsl,
            priority: extract_priority(&lines.join("\n")),
            enabled: true,
            conditions: c,
            actions: a,
            created_at: String::new(),
            updated_at: String::new(),
        });
    }
    rules
}

// ============================================================
// 主函数
// ============================================================

#[tokio::main]
async fn main() {
    let rules_dir = std::env::var("RULES_DIR").unwrap_or_else(|_| "./rules".to_string());
    let port: u16 = std::env::var("PORT")
        .unwrap_or_else(|_| "7912".to_string())
        .parse()
        .unwrap_or(7912);

    let state = Arc::new(RuleEngineState::new(rules_dir.clone()));

    // 启动时加载规则文件
    {
        let dir = PathBuf::from(&rules_dir);
        if dir.exists() {
            let mut rules = state.rules.lock().unwrap();
            for entry in std::fs::read_dir(&dir).unwrap().flatten() {
                let path = entry.path();
                if path.extension().is_none_or(|e| e != "dsl") {
                    continue;
                }
                if let Ok(content) = std::fs::read_to_string(&path) {
                    for rule in parse_dsl_file(&content) {
                        rules.insert(rule.rule_id.clone(), rule);
                    }
                }
            }
            println!("Loaded {} rules from {}", rules.len(), rules_dir);
        }
    }

    let app = Router::new()
        .route("/api/rules", get(list_rules).post(create_rule))
        .route(
            "/api/rules/:id",
            get(
                |axum::extract::Path(id): axum::extract::Path<String>,
                 State(state): State<Arc<RuleEngineState>>| async move {
                    let rules = state.rules.lock().unwrap();
                    match rules.get(&id) {
                        Some(r) => Json(serde_json::json!({"rule": r})).into_response(),
                        None => (
                            StatusCode::NOT_FOUND,
                            Json(serde_json::json!({"error": "not found"})),
                        )
                            .into_response(),
                    }
                },
            )
            .put(update_rule)
            .delete(delete_rule),
        )
        .route("/api/rules/test", post(test_rules))
        .route("/api/rules/reload", post(reload_rules))
        .route("/api/rules/stats", get(rule_stats))
        .route("/api/rules/export", get(export_rules))
        .route("/api/rules/import", post(import_rules))
        .route("/api/rules/export/file", post(export_rules_to_file))
        .route("/api/rules/import/file", post(import_rules_from_file))
        .with_state(state);

    println!("Rules API server listening on 0.0.0.0:{}", port);
    println!("Endpoints:");
    println!("  GET    /api/rules        - List all rules");
    println!("  POST   /api/rules        - Create rule");
    println!("  GET    /api/rules/:id    - Get rule");
    println!("  PUT    /api/rules/:id    - Update rule");
    println!("  DELETE /api/rules/:id    - Delete rule");
    println!("  POST   /api/rules/test   - Test rules");
    println!("  POST   /api/rules/reload - Hot reload");
    println!("  GET    /api/rules/stats  - Statistics");

    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", port))
        .await
        .unwrap();
    axum::serve(listener, app).await.unwrap();
}
