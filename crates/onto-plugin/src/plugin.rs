use crate::context::PluginContext;
use crate::error::PluginResult;
use crate::hook::HookPoint;

/// OntoDB 插件 trait — 所有插件必须实现此接口。
///
/// # 生命周期
///
/// 插件在 OntoDB 启动时初始化（`init`），在关闭时清理（`shutdown`）。
/// 每次 INSERT/UPDATE/DELETE 操作时，按注册顺序依次调用 `execute`。
///
/// # 线程安全
///
/// 插件必须是 `Send + Sync`，因为 OntoDB 使用多线程处理请求。
/// 如果插件需要可变状态，应使用内部可变性（如 `Mutex`）。
///
/// # 示例
///
/// ```ignore
/// struct MyPlugin;
///
/// impl OntoPlugin for MyPlugin {
///     fn name(&self) -> &str { "my_plugin" }
///
///     fn hooks(&self) -> Vec<HookPoint> {
///         vec![HookPoint::PreInsert]
///     }
///
///     fn execute(&self, ctx: &mut PluginContext) -> PluginResult<()> {
///         // 在 INSERT 前修改文档
///         ctx.doc.insert("my_field".to_string(), json!("auto_value"));
///         Ok(())
///     }
/// }
/// ```
pub trait OntoPlugin: Send + Sync {
    /// 插件唯一名称（用于日志和依赖声明）。
    fn name(&self) -> &str;

    /// 声明在哪些钩子点触发。
    fn hooks(&self) -> Vec<HookPoint>;

    /// 执行插件逻辑。
    ///
    /// 在 `PreInsert` 阶段可修改 `ctx.doc`（如添加自动生成的字段）。
    /// 在 `PostInsert` / `PostCommit` 阶段可执行副作用（如写入外部系统）。
    fn execute(&self, ctx: &mut PluginContext) -> PluginResult<()>;

    /// 是否对该类（表）生效。默认对所有类生效。
    ///
    /// 重写此方法可实现按表过滤。例如只对配置了 embedding 的表生成向量。
    fn filter(&self, _class: &str) -> bool {
        true
    }

    /// 此插件依赖的前置插件名称列表。
    ///
    /// 依赖的插件会在本插件之前执行。
    /// 如果依赖的插件未注册或未执行，本插件会收到 `DependencyMissing` 错误。
    fn dependencies(&self) -> Vec<&str> {
        vec![]
    }

    /// 插件初始化（在 OntoDB 启动时调用一次）。
    fn init(&mut self) -> PluginResult<()> {
        Ok(())
    }

    /// 插件关闭（在 OntoDB 关闭时调用一次）。
    fn shutdown(&mut self) -> PluginResult<()> {
        Ok(())
    }

    /// 插件健康检查（可选）。
    fn health(&self) -> PluginHealth {
        PluginHealth::Healthy
    }
}

/// 插件健康状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginHealth {
    /// 正常运行。
    Healthy,
    /// 降级运行（功能受限但不影响整体服务）。
    Degraded(String),
    /// 不可用。
    Unhealthy(String),
}
