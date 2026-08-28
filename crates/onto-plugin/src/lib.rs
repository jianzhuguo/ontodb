//! OntoDB 插件框架
//!
//! 提供可扩展的 INSERT/UPDATE/DELETE 管线钩子系统。
//! 插件通过 `OntoPlugin` trait 定义，在指定钩子点自动执行。
//!
//! # 设计原则
//!
//! - OntoDB 核心不依赖任何具体插件
//! - 插件通过 feature flag 按需编译
//! - 插件间通过 `PluginContext` 共享数据
//! - 钩子执行顺序由注册顺序决定

mod context;
mod error;
mod hook;
mod plugin;
mod registry;

pub use context::PluginContext;
pub use error::{PluginError, PluginResult};
pub use hook::HookPoint;
pub use plugin::{OntoPlugin, PluginHealth};
pub use registry::PluginRegistry;
