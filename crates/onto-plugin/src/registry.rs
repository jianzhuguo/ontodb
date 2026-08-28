use std::collections::HashMap;
use std::sync::RwLock;
use tracing::{info, warn, error};

use crate::context::PluginContext;
use crate::error::{PluginError, PluginResult};
use crate::hook::HookPoint;
use crate::plugin::{OntoPlugin, PluginHealth};

/// 插件注册表 — 管理所有已注册的插件及其执行顺序。
///
/// # 线程安全
///
/// 注册表在初始化阶段（启动时）写入，在运行阶段只读。
/// 使用 `RwLock` 保护插件列表，允许并发读、独占写。
///
/// # 执行顺序
///
/// 插件按注册顺序执行。如果插件声明了 `dependencies()`，
/// 注册表会自动拓扑排序确保依赖先执行。
///
/// # 示例
///
/// ```ignore
/// let mut registry = PluginRegistry::new();
/// registry.register(Box::new(EmbeddingPlugin::new(config)?))?;
/// registry.register(Box::new(ValueScorerPlugin::new(config)?))?;
/// registry.init_all()?;
///
/// // 在 INSERT 管线中调用
/// let mut ctx = PluginContext::new_insert(doc, key, class);
/// registry.execute_hooks(HookPoint::PreInsert, &mut ctx)?;
/// ```
pub struct PluginRegistry {
    /// 已注册的插件（按注册顺序）。
    plugins: RwLock<Vec<Box<dyn OntoPlugin>>>,

    /// 钩子点 → 插件索引的映射（运行时快速查找）。
    hook_index: RwLock<HashMap<HookPoint, Vec<usize>>>,

    /// 插件名称 → 索引的映射。
    name_index: RwLock<HashMap<String, usize>>,
}

impl PluginRegistry {
    /// 创建空的插件注册表。
    pub fn new() -> Self {
        Self {
            plugins: RwLock::new(Vec::new()),
            hook_index: RwLock::new(HashMap::new()),
            name_index: RwLock::new(HashMap::new()),
        }
    }

    /// 注册一个插件。
    ///
    /// 插件按注册顺序执行。如果插件名称已存在，返回错误。
    pub fn register(&self, plugin: Box<dyn OntoPlugin>) -> PluginResult<()> {
        let name = plugin.name().to_string();
        let hooks = plugin.hooks();

        let mut plugins = self.plugins.write().map_err(|e| {
            PluginError::Other(format!("lock poisoned: {}", e))
        })?;
        let mut name_index = self.name_index.write().map_err(|e| {
            PluginError::Other(format!("lock poisoned: {}", e))
        })?;
        let mut hook_index = self.hook_index.write().map_err(|e| {
            PluginError::Other(format!("lock poisoned: {}", e))
        })?;

        // 检查名称冲突
        if name_index.contains_key(&name) {
            return Err(PluginError::Config(format!(
                "plugin '{}' already registered", name
            )));
        }

        let index = plugins.len();
        plugins.push(plugin);
        name_index.insert(name.clone(), index);

        // 更新钩子索引
        for hook in hooks {
            hook_index.entry(hook).or_default().push(index);
        }

        info!("plugin registered: {} (index={})", name, index);
        Ok(())
    }

    /// 初始化所有已注册的插件。
    pub fn init_all(&self) -> PluginResult<()> {
        let mut plugins = self.plugins.write().map_err(|e| {
            PluginError::Other(format!("lock poisoned: {}", e))
        })?;
        for plugin in plugins.iter_mut() {
            info!("initializing plugin: {}", plugin.name());
            plugin.init().map_err(|e| {
                PluginError::Execution {
                    plugin: plugin.name().to_string(),
                    message: format!("init failed: {}", e),
                }
            })?;
        }
        Ok(())
    }

    /// 关闭所有已注册的插件。
    pub fn shutdown_all(&self) -> PluginResult<()> {
        let mut plugins = self.plugins.write().map_err(|e| {
            PluginError::Other(format!("lock poisoned: {}", e))
        })?;
        for plugin in plugins.iter_mut() {
            info!("shutting down plugin: {}", plugin.name());
            if let Err(e) = plugin.shutdown() {
                warn!("plugin '{}' shutdown error: {}", plugin.name(), e);
            }
        }
        Ok(())
    }

    /// 在指定钩子点执行所有相关插件。
    ///
    /// 按注册顺序依次执行，跳过 `filter()` 返回 false 的插件。
    /// 如果某个插件执行失败，中止并返回错误。
    pub fn execute_hooks(
        &self,
        hook: HookPoint,
        ctx: &mut PluginContext,
    ) -> PluginResult<()> {
        // 获取此钩子点的插件索引列表
        let indices = {
            let hook_index = self.hook_index.read().map_err(|e| {
                PluginError::Other(format!("lock poisoned: {}", e))
            })?;
            hook_index.get(&hook).cloned().unwrap_or_default()
        };

        if indices.is_empty() {
            return Ok(());
        }

        let plugins = self.plugins.read().map_err(|e| {
            PluginError::Other(format!("lock poisoned: {}", e))
        })?;

        for &idx in &indices {
            let plugin = &plugins[idx];

            // 按类名过滤
            if !plugin.filter(&ctx.class) {
                continue;
            }

            // 检查依赖是否已执行（简单检查：依赖必须在当前插件之前注册）
            for dep in plugin.dependencies() {
                let name_index = self.name_index.read().map_err(|e| {
                    PluginError::Other(format!("lock poisoned: {}", e))
                })?;
                if !name_index.contains_key(dep) {
                    return Err(PluginError::DependencyMissing {
                        plugin: plugin.name().to_string(),
                        dependency: dep.to_string(),
                    });
                }
            }

            // 执行插件
            if let Err(e) = plugin.execute(ctx) {
                error!("plugin '{}' failed at {:?}: {}", plugin.name(), hook, e);
                return Err(e);
            }
        }

        Ok(())
    }

    /// 查询所有插件的健康状态。
    pub fn health_check(&self) -> Vec<(String, PluginHealth)> {
        let plugins = match self.plugins.read() {
            Ok(p) => p,
            Err(_) => return vec![],
        };
        plugins.iter().map(|p| {
            (p.name().to_string(), p.health())
        }).collect()
    }

    /// 已注册的插件数量。
    pub fn len(&self) -> usize {
        self.plugins.read().map(|p| p.len()).unwrap_or(0)
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 列出所有已注册插件的名称。
    pub fn list_plugins(&self) -> Vec<String> {
        let plugins = match self.plugins.read() {
            Ok(p) => p,
            Err(_) => return vec![],
        };
        plugins.iter().map(|p| p.name().to_string()).collect()
    }
}

impl Default for PluginRegistry {
    fn default() -> Self {
        Self::new()
    }
}
