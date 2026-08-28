use onto_plugin::{HookPoint, OntoPlugin, PluginContext, PluginError, PluginHealth, PluginResult};
use serde_json::json;
use tracing::{info, debug};

use crate::config::EmbeddingConfig;
use crate::model::EmbeddingModel;
use crate::vectorizer::SemanticVectorizer;

/// Embedding 插件 — 在 INSERT 时自动生成语义向量。
///
/// # 行为
///
/// 1. 在 `PreInsert` 阶段触发
/// 2. 从文档中提取文本字段（根据 `FieldFilter` 策略）
/// 3. 调用内置 `EmbeddingModel`（ONNX）生成语义向量
/// 4. 将向量写入文档的 `__auto_embedding__` 字段
/// 5. 将向量写入 `PluginContext.shared["embedding"]` 供下游插件使用
///
/// # 依赖
///
/// 无前置依赖。可作为管线的第一个插件。
///
/// # 配置
///
/// 通过 `EmbeddingConfig` 控制：
/// - 模型路径
/// - 启用的类（表）
/// - 字段过滤策略
/// - 最大 token 长度
/// - 是否归一化
pub struct EmbeddingPlugin {
    config: EmbeddingConfig,
    model: Option<EmbeddingModel>,
    vectorizer: SemanticVectorizer,
}

impl EmbeddingPlugin {
    /// 创建 Embedding 插件。
    ///
    /// 模型在 `init()` 时加载。如果模型目录不存在或加载失败，
    /// 插件会在 `init()` 中返回错误。
    pub fn new(config: EmbeddingConfig) -> PluginResult<Self> {
        let vectorizer = SemanticVectorizer::new(config.field_filter.clone());
        Ok(Self {
            config,
            model: None,
            vectorizer,
        })
    }

    /// 延迟加载模型（在 init 时调用）。
    fn load_model(&mut self) -> PluginResult<()> {
        let model = EmbeddingModel::load(
            &self.config.model_dir,
            self.config.max_length,
            self.config.normalize,
        )?;
        info!(
            "embedding model loaded: dim={}, max_length={}",
            model.dimension(),
            self.config.max_length
        );
        self.model = Some(model);
        Ok(())
    }
}

impl OntoPlugin for EmbeddingPlugin {
    fn name(&self) -> &str {
        "embedding"
    }

    fn hooks(&self) -> Vec<HookPoint> {
        vec![HookPoint::PreInsert]
    }

    fn filter(&self, class: &str) -> bool {
        self.config.enabled_classes.contains(&"*".to_string())
            || self.config.enabled_classes.contains(&class.to_string())
    }

    fn dependencies(&self) -> Vec<&str> {
        vec![]  // 无前置依赖
    }

    fn init(&mut self) -> PluginResult<()> {
        self.load_model()
    }

    fn execute(&self, ctx: &mut PluginContext) -> PluginResult<()> {
        let model = self.model.as_ref().ok_or_else(|| {
            PluginError::Execution {
                plugin: "embedding".to_string(),
                message: "model not loaded".to_string(),
            }
        })?;

        // 如果用户已经手动提供了 embedding，跳过自动生成
        if ctx.doc.contains_key("embedding")
            || ctx.doc.contains_key("__auto_embedding__")
        {
            debug!("skipping auto-embedding: user-provided embedding exists");
            return Ok(());
        }

        // 提取文本
        let text = self.vectorizer.extract_text(&ctx.doc);
        if text.is_empty() {
            debug!("skipping auto-embedding: no text fields found");
            return Ok(());
        }

        // 生成 embedding
        let embedding = model.encode(&text)?;

        // 写入文档（供向量索引使用）
        ctx.doc.insert(
            "__auto_embedding__".to_string(),
            json!(embedding),
        );

        // 写入共享上下文（供下游插件如 ValueScorer 使用）
        let serialized = bincode::serialize(&embedding).map_err(|e| {
            PluginError::Serialization(e.to_string())
        })?;
        ctx.set_shared("embedding", serialized);

        debug!(
            "auto-embedding generated: class={}, dim={}, text_len={}",
            ctx.class,
            embedding.len(),
            text.len(),
        );

        Ok(())
    }

    fn health(&self) -> PluginHealth {
        if self.model.is_some() {
            PluginHealth::Healthy
        } else {
            PluginHealth::Unhealthy("model not loaded".to_string())
        }
    }
}
