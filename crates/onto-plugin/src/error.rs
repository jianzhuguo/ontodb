// Copyright (c) 2024-2026 OntoDB Team
// Licensed under the Business Source License 1.1 (BUSL-1.1).
// See LICENSE for details. Change Date: 2031-09-15.
// On the Change Date, this file will be licensed under Apache License 2.0.
use thiserror::Error;

/// 插件错误类型。
#[derive(Error, Debug)]
pub enum PluginError {
    /// 插件配置错误。
    #[error("plugin config error: {0}")]
    Config(String),

    /// 插件执行失败。
    #[error("plugin '{plugin}' execution failed: {message}")]
    Execution { plugin: String, message: String },

    /// 依赖的前置插件未执行。
    #[error("plugin '{plugin}' requires '{dependency}' to run first")]
    DependencyMissing { plugin: String, dependency: String },

    /// 模型加载失败。
    #[error("model load failed: {0}")]
    ModelLoad(String),

    /// 推理失败。
    #[error("inference failed: {0}")]
    Inference(String),

    /// 序列化/反序列化错误。
    #[error("serialization error: {0}")]
    Serialization(String),

    /// 序列化/反序列化错误。
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    /// 其他错误。
    #[error("{0}")]
    Other(String),
}

impl From<bincode::Error> for PluginError {
    fn from(e: bincode::Error) -> Self {
        PluginError::Serialization(e.to_string())
    }
}

/// 插件操作结果类型。
pub type PluginResult<T> = Result<T, PluginError>;
