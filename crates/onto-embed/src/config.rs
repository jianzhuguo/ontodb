use std::path::PathBuf;
use serde::{Deserialize, Serialize};

/// Embedding 插件配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingConfig {
    /// 模型目录路径（需包含 model.onnx + tokenizer.json）。
    pub model_dir: PathBuf,

    /// 启用自动生成 embedding 的类（表）列表。
    /// 使用 `"*"` 表示所有类。
    /// 空列表 = 不对任何类生效。
    pub enabled_classes: Vec<String>,

    /// 哪些字段参与 embedding 生成。
    pub field_filter: FieldFilter,

    /// 最大 token 长度（超出部分截断）。
    pub max_length: usize,

    /// 是否对输出向量做 L2 归一化。
    pub normalize: bool,
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            model_dir: PathBuf::from("models/bge-small-zh-v1.5"),
            enabled_classes: vec!["*".to_string()],
            field_filter: FieldFilter::AllString,
            max_length: 512,
            normalize: true,
        }
    }
}

/// 文本字段过滤策略 — 决定哪些字段参与 embedding 生成。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum FieldFilter {
    /// 所有 STRING 类型字段（跳过内部字段 `__xxx__`）。
    AllString,

    /// 只使用白名单中的字段。
    Whitelist(Vec<String>),

    /// 使用白名单中的字段，其余字段忽略。
    /// 与 Whitelist 的区别：Whitelist 是精确匹配，
    /// Blacklist 是排除指定字段。
    Blacklist(Vec<String>),

    /// 不自动生成（禁用）。
    None,
}
