//! OntoDB Embedding 插件
//!
//! 在数据 INSERT 时自动生成语义向量，支持任意 HuggingFace 兼容的 embedding 模型
//! （ONNX 格式）。
//!
//! # 快速开始
//!
//! ```ignore
//! use onto_embed::{EmbeddingPlugin, EmbeddingConfig};
//!
//! let config = EmbeddingConfig {
//!     model_dir: PathBuf::from("models/bge-small-zh-v1.5"),
//!     enabled_classes: vec!["*".to_string()],  // 所有表
//!     field_filter: FieldFilter::AllString,
//!     max_length: 512,
//!     normalize: true,
//! };
//!
//! let plugin = EmbeddingPlugin::new(config)?;
//! registry.register(Box::new(plugin))?;
//! ```
//!
//! # 模型格式
//!
//! 模型目录需包含：
//! - `model.onnx` — ONNX 格式的模型文件
//! - `tokenizer.json` — HuggingFace tokenizers 格式的分词器
//!
//! 推荐模型（按场景选择）：
//!
//! | 模型 | 维度 | 大小 | 延迟 | 场景 |
//! |------|------|------|------|------|
//! | BGE-small-zh-v1.5 | 384 | ~100MB | ~1ms | 中文通用 |
//! | BGE-base-zh-v1.5 | 768 | ~400MB | ~3ms | 中文高精度 |
//! | all-MiniLM-L6-v2 | 384 | ~80MB | ~0.5ms | 英文通用 |
//! | BGE-m3 | 1024 | ~2GB | ~10ms | 多语言 |

mod config;
mod model;
mod plugin;
mod vectorizer;

pub use config::{EmbeddingConfig, FieldFilter};
pub use model::EmbeddingModel;
pub use plugin::EmbeddingPlugin;
pub use vectorizer::SemanticVectorizer;
