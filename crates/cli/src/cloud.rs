use std::sync::Arc;

use rust_cli_kernel_api::clock::{Clock, Ids};
use rust_cli_kernel_api::env_registry::EnvironmentRegistry;
use rust_cli_kernel_api::model::ModelPort;
use rust_cli_kernel_api::store::{ArtifactStore, StoreError};
use rust_cli_node_store::UuidSource;
use rust_cli_pg_store::PgDatabase;
use rust_cli_pg_store::{StoreAccessContext, StoreProject};
use rust_cli_runtime::{
    CompactConfig, ContextConfig, Defaults, Extensions, LocalKernel, MemoryTaskLists, Ports,
};

/// 宿主已经验证的用户身份与控制面工作区归属，不提供客户端 JSON 反序列化入口。
pub struct CloudSessionConfig {
    pub access: StoreAccessContext,
    pub projects: Vec<StoreProject>,
    pub defaults: Defaults,
    pub context: ContextConfig,
}

/// 无默认本机实现：执行环境、模型与 Artifact 都必须由云宿主提供。
pub struct CloudPorts {
    pub model: Arc<dyn ModelPort>,
    pub environments: Arc<dyn EnvironmentRegistry>,
    pub artifacts: Arc<dyn ArtifactStore>,
    pub clock: Arc<dyn Clock>,
    pub ids: Arc<dyn Ids>,
    /// 会话库写记录用的 uuid 来源（node-records 的 `UuidSource`）：与本机的会话库同一种写法，生产给 node-store 的 `NodeIds`
    /// （同一个实例也可以当 `ids`）。
    pub uuids: Arc<dyn UuidSource>,
}

/// 同一租户的同一会话必须复用此 Kernel，直到宿主确认旧实例已停止再交接。
pub fn kernel(
    database: &PgDatabase,
    mut config: CloudSessionConfig,
    ports: CloudPorts,
) -> Result<LocalKernel, StoreError> {
    if config.projects.len() != 1 {
        return Err(StoreError::Storage {
            message: "a cloud session host requires one explicit project binding".into(),
        });
    }
    let store = Arc::new(database.bind(
        config.access,
        config.projects,
        ports.clock.clone(),
        ports.uuids,
    )?);
    // 本期没有持久 TaskListStore，不能把工具返回的成功误解为跨重启持久。
    config.defaults.task_tools = Some(false);
    Ok(LocalKernel::with_extensions(
        Ports {
            model: ports.model,
            environments: ports.environments,
            store: store.clone(),
            artifacts: ports.artifacts,
            clock: ports.clock,
            ids: ports.ids,
        },
        config.defaults,
        config.context,
        CompactConfig::default(),
        Arc::new(MemoryTaskLists::default()),
        // 输入账本就是会话库；会话的策略与 `zcode-rs` 用共用库时相同（pg-store 的 `SESSION_POLICY`）。
        Extensions {
            journal: store,
            policy: rust_cli_pg_store::SESSION_POLICY,
            ..Extensions::none()
        },
    ))
}

#[cfg(test)]
mod tests;
