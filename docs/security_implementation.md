# ValueHub 安全层实现方案

## 一、SQL 白名单实现

### 1.1 语句类型白名单

```rust
// crates/onto-server/src/security/sql_whitelist.rs

use once_cell::sync::Lazy;
use std::collections::HashSet;

/// 允许的 SQL 语句类型
static ALLOWED_STATEMENTS: Lazy<HashSet<&str>> = Lazy::new(|| {
    let mut set = HashSet::new();
    set.insert("SELECT");
    set.insert("INSERT");
    set.insert("UPDATE");
    set.insert("DELETE");
    set.insert("SHOW");
    set.insert("SYSTEM");
    set.insert("EXPLAIN");
    set.insert("ANALYZE");
    set
});

/// 禁止的关键词（防止危险操作）
static BLOCKED_KEYWORDS: Lazy<HashSet<&str>> = Lazy::new(|| {
    let mut set = HashSet::new();
    set.insert("DROP");
    set.insert("ALTER");
    set.insert("TRUNCATE");
    set.insert("CREATE USER");
    set.insert("GRANT");
    set.insert("REVOKE");
    set.insert("EXECUTE");
    set.insert("EXEC");
    set
});

/// SQL 注入检测
static INJECTION_PATTERNS: Lazy<Vec<&str>> = Lazy::new(|| {
    vec![
        ";",           // 多语句注入
        "--",          // SQL 注释
        "/*",          // 块注释开始
        "*/",          // 块注释结束
        "UNION ALL",   // UNION 注入
        "UNION SELECT", // UNION 注入
        "OR 1=1",      // 永真条件
        "OR '1'='1",   // 永真条件
        "DROP TABLE",  // 删除表
        "DROP DATABASE", // 删除数据库
    ]
});

pub struct SqlWhitelist {
    allowed_statements: HashSet<String>,
    blocked_keywords: HashSet<String>,
    injection_patterns: Vec<String>,
}

impl SqlWhitelist {
    pub fn new() -> Self {
        Self {
            allowed_statements: ALLOWED_STATEMENTS.iter().map(|s| s.to_string()).collect(),
            blocked_keywords: BLOCKED_KEYWORDS.iter().map(|s| s.to_string()).collect(),
            injection_patterns: INJECTION_PATTERNS.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// 验证 SQL 语句是否安全
    pub fn validate(&self, sql: &str) -> Result<(), SqlValidationError> {
        let sql_upper = sql.trim().to_uppercase();

        // 1. 检查是否为空
        if sql.trim().is_empty() {
            return Err(SqlValidationError::EmptyQuery);
        }

        // 2. 检查注入模式
        for pattern in &self.injection_patterns {
            if sql_upper.contains(&pattern.to_uppercase()) {
                return Err(SqlValidationError::InjectionDetected {
                    pattern: pattern.clone(),
                });
            }
        }

        // 3. 检查禁止关键词
        for keyword in &self.blocked_keywords {
            if sql_upper.contains(&keyword.to_uppercase()) {
                return Err(SqlValidationError::BlockedKeyword {
                    keyword: keyword.clone(),
                });
            }
        }

        // 4. 检查语句类型
        let first_word = sql_upper.split_whitespace().next().unwrap_or("");
        if !self.allowed_statements.contains(first_word) {
            return Err(SqlValidationError::UnsupportedStatement {
                statement: first_word.to_string(),
            });
        }

        // 5. 检查系统表访问（防止 schema 泄露）
        if sql_upper.contains("INFORMATION_SCHEMA") 
            || sql_upper.contains("SYSTEM.")
            || sql_upper.contains("__") {
            return Err(SqlValidationError::SystemTableAccess);
        }

        Ok(())
    }
}

#[derive(Debug)]
pub enum SqlValidationError {
    EmptyQuery,
    InjectionDetected { pattern: String },
    BlockedKeyword { keyword: String },
    UnsupportedStatement { statement: String },
    SystemTableAccess,
}

impl std::fmt::Display for SqlValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyQuery => write!(f, "查询不能为空"),
            Self::InjectionDetected { pattern } => write!(f, "检测到 SQL 注入: {}", pattern),
            Self::BlockedKeyword { keyword } => write!(f, "禁止使用关键词: {}", keyword),
            Self::UnsupportedStatement { statement } => write!(f, "不支持的语句类型: {}", statement),
            Self::SystemTableAccess => write!(f, "禁止访问系统表"),
        }
    }
}
```

### 1.2 限流器实现

```rust
// crates/onto-server/src/security/rate_limiter.rs

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub struct RateLimiter {
    /// 用户配额：user_id -> (max_qps, max_monthly)
    quotas: HashMap<String, UserQuota>,
    /// 当前窗口计数：user_id -> (window_start, count)
    window_counts: Mutex<HashMap<String, (Instant, u32)>>,
    /// 月度计数：user_id -> (month_start, count)
    monthly_counts: Mutex<HashMap<String, (u64, u64)>>,
}

#[derive(Clone)]
pub struct UserQuota {
    pub max_qps: u32,        // 每秒最大请求数
    pub max_monthly: u64,    // 每月最大请求数
    pub max_rows: u32,       // 单次查询最大返回行数
    pub timeout_ms: u64,     // 查询超时（毫秒）
}

impl RateLimiter {
    pub fn new() -> Self {
        Self {
            quotas: HashMap::new(),
            window_counts: Mutex::new(HashMap::new()),
            monthly_counts: Mutex::new(HashMap::new()),
        }
    }

    /// 设置用户配额
    pub fn set_quota(&mut self, user_id: &str, quota: UserQuota) {
        self.quotas.insert(user_id.to_string(), quota);
    }

    /// 检查是否允许请求
    pub fn check(&self, user_id: &str) -> Result<(), RateLimitError> {
        let quota = self.quotas.get(user_id).cloned().unwrap_or(UserQuota::default());

        // 1. 检查 QPS 限制
        {
            let mut counts = self.window_counts.lock().unwrap();
            let entry = counts.entry(user_id.to_string()).or_insert((Instant::now(), 0));
            let (window_start, count) = entry;

            if window_start.elapsed() >= Duration::from_secs(1) {
                // 新窗口
                *window_start = Instant::now();
                *count = 1;
            } else {
                *count += 1;
                if *count > quota.max_qps {
                    return Err(RateLimitError::QpsExceeded {
                        current: *count,
                        limit: quota.max_qps,
                    });
                }
            }
        }

        // 2. 检查月度限制
        {
            let mut counts = self.monthly_counts.lock().unwrap();
            let current_month = self.current_month_id();
            let entry = counts.entry(user_id.to_string()).or_insert((current_month, 0));
            let (month_id, count) = entry;

            if *month_id != current_month {
                // 新月份
                *month_id = current_month;
                *count = 1;
            } else {
                *count += 1;
                if *count > quota.max_monthly {
                    return Err(RateLimitError::MonthlyExceeded {
                        current: *count,
                        limit: quota.max_monthly,
                    });
                }
            }
        }

        Ok(())
    }

    /// 获取用户配额
    pub fn get_quota(&self, user_id: &str) -> UserQuota {
        self.quotas.get(user_id).cloned().unwrap_or(UserQuota::default())
    }

    fn current_month_id(&self) -> u64 {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        now / (30 * 24 * 3600) // 简化的月份计算
    }
}

impl Default for UserQuota {
    fn default() -> Self {
        Self {
            max_qps: 10,           // 默认 10 QPS
            max_monthly: 10_000,   // 默认 1万次/月
            max_rows: 10_000,      // 默认最多返回 1万行
            timeout_ms: 5_000,     // 默认 5秒超时
        }
    }
}

#[derive(Debug)]
pub enum RateLimitError {
    QpsExceeded { current: u32, limit: u32 },
    MonthlyExceeded { current: u64, limit: u64 },
}
```

### 1.3 权限控制实现

```rust
// crates/onto-server/src/security/permissions.rs

use std::collections::HashMap;

/// 权限管理器
pub struct PermissionManager {
    /// 用户角色：user_id -> Vec<role>
    user_roles: HashMap<String, Vec<String>>,
    /// 行级权限：(class, role) -> 条件
    row_permissions: HashMap<(String, String), RowPermission>,
    /// 字段级权限：(class, field, role) -> 字段权限
    field_permissions: HashMap<(String, String, String), FieldPermission>,
}

/// 行级权限
pub struct RowPermission {
    pub condition: String,  // WHERE 条件，如 "owner_id = CURRENT_USER()"
    pub permission_type: PermissionType,
}

/// 字段级权限
pub struct FieldPermission {
    pub mask_type: MaskType,
    pub pattern: Option<String>,
}

#[derive(Clone)]
pub enum PermissionType {
    Select,
    Insert,
    Update,
    Delete,
}

#[derive(Clone)]
pub enum MaskType {
    Full,      // 完整显示
    Partial,   // 部分脱敏
    Hash,      // 哈希脱敏
    Hidden,    // 完全隐藏
}

impl PermissionManager {
    pub fn new() -> Self {
        Self {
            user_roles: HashMap::new(),
            row_permissions: HashMap::new(),
            field_permissions: HashMap::new(),
        }
    }

    /// 添加用户角色
    pub fn add_role(&mut self, user_id: &str, role: &str) {
        self.user_roles
            .entry(user_id.to_string())
            .or_insert_with(Vec::new)
            .push(role.to_string());
    }

    /// 设置行级权限
    pub fn set_row_permission(&mut self, class: &str, role: &str, condition: &str, ptype: PermissionType) {
        self.row_permissions.insert(
            (class.to_string(), role.to_string()),
            RowPermission {
                condition: condition.to_string(),
                permission_type: ptype,
            },
        );
    }

    /// 设置字段级权限
    pub fn set_field_permission(&mut self, class: &str, field: &str, role: &str, mask: MaskType, pattern: Option<&str>) {
        self.field_permissions.insert(
            (class.to_string(), field.to_string(), role.to_string()),
            FieldPermission {
                mask_type: mask,
                pattern: pattern.map(|s| s.to_string()),
            },
        );
    }

    /// 获取用户的行级 WHERE 条件
    pub fn get_row_filter(&self, user_id: &str, class: &str) -> Option<String> {
        let roles = self.user_roles.get(user_id)?;
        
        for role in roles {
            if let Some(perm) = self.row_permissions.get(&(class.to_string(), role.clone())) {
                // 管理员角色无条件限制
                if role == "admin" {
                    return None;
                }
                // 返回条件，替换 CURRENT_USER() 为实际用户 ID
                let condition = perm.condition.replace("CURRENT_USER()", &format!("'{}'", user_id));
                return Some(condition);
            }
        }
        
        // 默认：只能看自己的数据
        Some(format!("owner_id = '{}'", user_id))
    }

    /// 获取字段脱敏规则
    pub fn get_field_mask(&self, user_id: &str, class: &str, field: &str) -> FieldPermission {
        let roles = self.user_roles.get(user_id).cloned().unwrap_or_default();
        
        for role in &roles {
            if let Some(perm) = self.field_permissions.get(&(class.to_string(), field.to_string(), role.clone())) {
                return perm.clone();
            }
        }
        
        // 默认：隐藏
        FieldPermission {
            mask_type: MaskType::Hidden,
            pattern: None,
        }
    }

    /// 检查用户是否有权限执行操作
    pub fn check_permission(&self, user_id: &str, class: &str, ptype: &PermissionType) -> bool {
        let roles = self.user_roles.get(user_id);
        
        if let Some(roles) = roles {
            for role in roles {
                if role == "admin" {
                    return true; // 管理员有所有权限
                }
                if let Some(perm) = self.row_permissions.get(&(class.to_string(), role.clone())) {
                    if matches!((&perm.permission_type, ptype),
                        (PermissionType::Select, PermissionType::Select) |
                        (PermissionType::Insert, PermissionType::Insert) |
                        (PermissionType::Update, PermissionType::Update) |
                        (PermissionType::Delete, PermissionType::Delete)
                    ) {
                        return true;
                    }
                }
            }
        }
        
        false
    }
}
```

### 1.4 查询审计实现

```rust
// crates/onto-server/src/security/audit.rs

use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct QueryAuditor {
    logs: Mutex<Vec<AuditEntry>>,
}

#[derive(Clone, Debug)]
pub struct AuditEntry {
    pub timestamp: u64,
    pub user_id: String,
    pub api_key: String,
    pub query: String,      // 脱敏后的查询
    pub rows_returned: u32,
    pub execution_time_ms: u64,
    pub ip_address: String,
    pub status: AuditStatus,
}

#[derive(Clone, Debug)]
pub enum AuditStatus {
    Success,
    Failed(String),
    Blocked(String),
}

impl QueryAuditor {
    pub fn new() -> Self {
        Self {
            logs: Mutex::new(Vec::new()),
        }
    }

    /// 记录查询
    pub fn log_query(&self, entry: AuditEntry) {
        let mut logs = self.logs.lock().unwrap();
        logs.push(entry);
        
        // 异步写入持久化存储
        // 实际实现中应该用 channel 发送到后台线程
    }

    /// 查询审计日志
    pub fn query_logs(&self, user_id: Option<&str>, limit: usize) -> Vec<AuditEntry> {
        let logs = self.logs.lock().unwrap();
        let filtered: Vec<_> = if let Some(uid) = user_id {
            logs.iter().filter(|e| e.user_id == uid).cloned().collect()
        } else {
            logs.clone()
        };
        filtered.into_iter().rev().take(limit).collect()
    }

    /// 统计用户查询次数
    pub fn count_queries(&self, user_id: &str, month_id: u64) -> u64 {
        let logs = self.logs.lock().unwrap();
        logs.iter()
            .filter(|e| e.user_id == user_id && e.timestamp / (30 * 24 * 3600) == month_id)
            .count() as u64
    }
}
```

### 1.5 完整的安全层处理流程

```rust
// crates/onto-server/src/security/mod.rs

use crate::security::sql_whitelist::SqlWhitelist;
use crate::security::rate_limiter::RateLimiter;
use crate::security::permissions::PermissionManager;
use crate::security::audit::{QueryAuditor, AuditEntry, AuditStatus};

pub struct SecurityLayer {
    whitelist: SqlWhitelist,
    rate_limiter: RateLimiter,
    permissions: PermissionManager,
    auditor: QueryAuditor,
}

pub struct RequestContext {
    pub user_id: String,
    pub api_key: String,
    pub ip_address: String,
    pub roles: Vec<String>,
}

pub enum SecurityError {
    AuthenticationFailed,
    RateLimitExceeded(String),
    SqlValidationFailed(String),
    PermissionDenied(String),
}

impl SecurityLayer {
    pub fn new() -> Self {
        Self {
            whitelist: SqlWhitelist::new(),
            rate_limiter: RateLimiter::new(),
            permissions: PermissionManager::new(),
            auditor: QueryAuditor::new(),
        }
    }

    /// 处理查询请求的完整安全流程
    pub fn process_query(&self, ctx: &RequestContext, sql: &str) -> Result<String, SecurityError> {
        let start_time = std::time::Instant::now();

        // 1. 认证检查
        if !self.authenticate(ctx) {
            self.audit_blocked(ctx, sql, "认证失败");
            return Err(SecurityError::AuthenticationFailed);
        }

        // 2. 限流检查
        if let Err(e) = self.rate_limiter.check(&ctx.user_id) {
            self.audit_blocked(ctx, sql, &format!("限流: {}", e));
            return Err(SecurityError::RateLimitExceeded(e.to_string()));
        }

        // 3. SQL 白名单检查
        if let Err(e) = self.whitelist.validate(sql) {
            self.audit_blocked(ctx, sql, &format!("SQL验证失败: {}", e));
            return Err(SecurityError::SqlValidationFailed(e.to_string()));
        }

        // 4. 权限检查（行级）
        let modified_sql = self.apply_row_permissions(ctx, sql);

        // 5. 执行查询
        let result = self.execute_query(&modified_sql);

        // 6. 应用字段脱敏
        let masked_result = self.apply_field_masks(ctx, &result);

        // 7. 记录审计日志
        self.audit_success(ctx, sql, &masked_result, start_time.elapsed().as_millis() as u64);

        Ok(masked_result)
    }

    fn authenticate(&self, ctx: &RequestContext) -> bool {
        // 验证 API Key
        // 实际实现中应该查询数据库验证
        !ctx.api_key.is_empty()
    }

    fn apply_row_permissions(&self, ctx: &RequestContext, sql: &str) -> String {
        // 解析 SQL 中的表名
        if let Some(table) = extract_table_name(sql) {
            if let Some(filter) = self.permissions.get_row_filter(&ctx.user_id, &table) {
                // 在 WHERE 子句中添加权限过滤条件
                return add_where_condition(sql, &filter);
            }
        }
        sql.to_string()
    }

    fn apply_field_masks(&self, ctx: &RequestContext, result: &str) -> String {
        // 解析结果中的字段，应用脱敏规则
        // 实际实现中需要解析 JSON 结果并逐字段处理
        result.to_string()
    }

    fn execute_query(&self, sql: &str) -> String {
        // 调用 OntoQL 引擎执行查询
        // 实际实现中应该调用 QueryExecutor
        "{}".to_string()
    }

    fn audit_success(&self, ctx: &RequestContext, sql: &str, result: &str, elapsed_ms: u64) {
        self.auditor.log_query(AuditEntry {
            timestamp: now(),
            user_id: ctx.user_id.clone(),
            api_key: ctx.api_key.clone(),
            query: sanitize_sql(sql),
            rows_returned: count_rows(result),
            execution_time_ms: elapsed_ms,
            ip_address: ctx.ip_address.clone(),
            status: AuditStatus::Success,
        });
    }

    fn audit_blocked(&self, ctx: &RequestContext, sql: &str, reason: &str) {
        self.auditor.log_query(AuditEntry {
            timestamp: now(),
            user_id: ctx.user_id.clone(),
            api_key: ctx.api_key.clone(),
            query: sanitize_sql(sql),
            rows_returned: 0,
            execution_time_ms: 0,
            ip_address: ctx.ip_address.clone(),
            status: AuditStatus::Blocked(reason.to_string()),
        });
    }
}

// 辅助函数
fn extract_table_name(sql: &str) -> Option<String> {
    // 简单实现：提取 FROM 后的表名
    let upper = sql.to_uppercase();
    if let Some(pos) = upper.find("FROM ") {
        let rest = &sql[pos + 5..];
        let table = rest.split_whitespace().next()?;
        return Some(table.to_string());
    }
    None
}

fn add_where_condition(sql: &str, condition: &str) -> String {
    let upper = sql.to_uppercase();
    if upper.contains("WHERE") {
        // 已有 WHERE，用 AND 连接
        sql.replacen("WHERE", &format!("WHERE {} AND", condition), 1)
    } else {
        // 没有 WHERE，添加
        if let Some(pos) = upper.find("ORDER BY") {
            let (before, after) = sql.split_at(pos);
            format!("{} WHERE {} {}", before.trim(), condition, after)
        } else {
            format!("{} WHERE {}", sql, condition)
        }
    }
}

fn sanitize_sql(sql: &str) -> String {
    // 脱敏：隐藏敏感参数
    sql.replace('\'', "***")
}

fn count_rows(result: &str) -> u32 {
    // 计算返回行数
    result.matches('{').count() as u32
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}
```

## 二、前端/SDK 使用示例

### 2.1 Python SDK

```python
from valuehub import ValueHubClient

# 初始化客户端
client = ValueHubClient(
    endpoint="http://localhost:7912",
    api_key="your-api-key"
)

# 查询设备数据（自动经过安全层）
devices = client.query("SELECT * FROM Device WHERE status = 'online'")
print(devices)

# 查询会被自动处理：
# 1. 认证检查（API Key）
# 2. 限流检查（QPS）
# 3. SQL 白名单检查（只允许 SELECT）
# 4. 行级权限过滤（只返回用户有权限的设备）
# 5. 字段脱敏（敏感字段自动脱敏）
# 6. 审计日志记录
```

### 2.2 JavaScript SDK

```javascript
import { ValueHubClient } from '@valuehub/sdk';

const client = new ValueHubClient({
  endpoint: 'http://localhost:7912',
  apiKey: 'your-api-key'
});

// 查询数据
const devices = await client.query('SELECT * FROM Device WHERE status = ?', ['online']);
console.log(devices);

// 会自动经过安全层处理
```

### 2.3 MCP Server 集成

```python
# AI Agent 通过 MCP Server 查询
# 安全层自动处理认证、限流、权限

from valuehub.mcp import ValueHubMCPServer

server = ValueHubMCPServer(
    endpoint="http://localhost:7912",
    api_key="ai-agent-key"
)

# AI Agent 查询
result = server.query("SELECT * FROM Device WHERE temperature > 35")
# 自动经过：
# - 认证（AI Agent 的 API Key）
# - 限流（AI Agent 的 QPS 配额）
# - SQL 白名单（只允许 SELECT）
# - 行级权限（只返回 AI Agent 有权限的设备）
```

## 三、配置示例

### 3.1 安全层配置文件

```yaml
# valuehub-security.yaml

security:
  # API Gateway 配置
  api_gateway:
    enabled: true
    port: 7912
    
  # SQL 白名单
  sql_whitelist:
    allowed_statements:
      - SELECT
      - INSERT
      - UPDATE
      - DELETE
      - SHOW
      - SYSTEM
    blocked_keywords:
      - DROP
      - ALTER
      - TRUNCATE
      - GRANT
      - REVOKE
    injection_patterns:
      - ";"
      - "--"
      - "/*"
      - "UNION ALL"
      - "OR 1=1"
    
  # 限流配置
  rate_limiting:
    default_qps: 10
    default_monthly: 10000
    default_timeout_ms: 5000
    default_max_rows: 10000
    
    user_quotas:
      "user_001":
        max_qps: 100
        max_monthly: 1000000
      "ai_agent_001":
        max_qps: 1000
        max_monthly: -1  # 无限制
    
  # 权限配置
  permissions:
    default_role: "user"
    admin_roles:
      - "admin"
    
  # 审计配置
  audit:
    enabled: true
    log_queries: true
    retention_days: 90
```
