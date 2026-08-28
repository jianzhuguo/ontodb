/// 插件钩子点 — 定义插件在 INSERT/UPDATE/DELETE 管线的哪个阶段执行。
///
/// 执行顺序：PreInsert → (LSM 写入) → PostInsert → (事务提交) → PostCommit
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HookPoint {
    /// 文档构建后、写入 LSM 前。
    /// 可修改文档（如添加自动生成的 embedding 字段）。
    PreInsert,

    /// LSM 写入后、事务提交前。
    /// 可读取最终文档，执行副作用（如写入外部系统）。
    PostInsert,

    /// 事务提交后。
    /// 适合异步操作（如更新缓存、发送事件）。
    PostCommit,

    /// UPDATE 前。
    PreUpdate,

    /// UPDATE 后。
    PostUpdate,

    /// DELETE 前。
    PreDelete,

    /// DELETE 后。
    PostDelete,
}
