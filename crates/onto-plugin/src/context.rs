use std::collections::HashMap;
use serde_json::Value as JsonValue;

/// 插件上下文 — 在钩子执行期间在插件间传递数据。
///
/// 生命周期：一次 INSERT/UPDATE/DELETE 操作创建一个 `PluginContext`，
/// 在所有相关钩子执行完毕后销毁。
///
/// # 插件间数据传递
///
/// 插件通过 `shared` 字典传递中间数据。例如：
/// - `EmbeddingPlugin` 将生成的向量写入 `shared["embedding"]`
/// - `ValueScorerPlugin` 从 `shared["embedding"]` 读取向量并评分
///
/// 约定 key：
/// - `"embedding"` — 语义向量 `Vec<f32>` 的 bincode 序列化
/// - `"hidden_state"` — 模型中间层输出（供下游使用）
/// - `"value_output"` — 五维价值评估结果
#[derive(Debug)]
pub struct PluginContext {
    /// 当前文档（PreInsert 阶段可修改）。
    pub doc: serde_json::Map<String, JsonValue>,

    /// 实体锚点键（`{class}::{pk}` 格式）。
    pub entity_key: Vec<u8>,

    /// 类名（表名）。
    pub class: String,

    /// 插件间共享的中间数据。
    /// Key 约定见模块文档。
    pub shared: HashMap<String, Vec<u8>>,

    /// 操作类型标记。
    pub operation: Operation,
}

/// 操作类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    Insert,
    Update,
    Delete,
}

impl PluginContext {
    /// 创建 INSERT 操作的上下文。
    pub fn new_insert(
        doc: serde_json::Map<String, JsonValue>,
        entity_key: Vec<u8>,
        class: String,
    ) -> Self {
        Self {
            doc,
            entity_key,
            class,
            shared: HashMap::new(),
            operation: Operation::Insert,
        }
    }

    /// 创建 UPDATE 操作的上下文。
    pub fn new_update(
        doc: serde_json::Map<String, JsonValue>,
        entity_key: Vec<u8>,
        class: String,
    ) -> Self {
        Self {
            doc,
            entity_key,
            class,
            shared: HashMap::new(),
            operation: Operation::Update,
        }
    }

    /// 创建 DELETE 操作的上下文。
    pub fn new_delete(entity_key: Vec<u8>, class: String) -> Self {
        Self {
            doc: serde_json::Map::new(),
            entity_key,
            class,
            shared: HashMap::new(),
            operation: Operation::Delete,
        }
    }

    /// 获取实体锚点字符串（`{class}::{pk}`）。
    pub fn entity_id(&self) -> Option<String> {
        std::str::from_utf8(&self.entity_key).ok().map(|s| s.to_string())
    }

    /// 向 shared 字典写入数据。
    pub fn set_shared(&mut self, key: impl Into<String>, value: Vec<u8>) {
        self.shared.insert(key.into(), value);
    }

    /// 从 shared 字典读取数据。
    pub fn get_shared(&self, key: &str) -> Option<&Vec<u8>> {
        self.shared.get(key)
    }
}
