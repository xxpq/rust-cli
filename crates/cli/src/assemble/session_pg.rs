//! 部署配置选的共用 PG 会话库（docs/specs/rust-cli/deployment.md §6、pg-store.md §11）：配置不全或连不上时报错退出，
//! 不回退本机 SQLite。只用于专用测试库；生产身份由 Harness 注入（pg-store.md §6）。
use std::sync::Arc;

use rust_cli_base::secret::RedactedString;
use rust_cli_kernel_api::clock::Clock;
use rust_cli_kernel_api::env::WorkspaceRef;
use rust_cli_node_store::UuidSource;
use rust_cli_pg_store::{PgDatabase, PgSessionStore};
use rust_cli_pg_store::{StoreAccessContext, StoreProject};
use sha2::{Digest, Sha256};

use crate::commands::flags::Fatal;

/// 共用会话库下会话的策略：pg-store 给出（两个入口共用），在选会话库的那一处注入内核。
pub(crate) use rust_cli_pg_store::SESSION_POLICY as POLICY;

/// 一个 CLI 进程一个池；主会话与子代理共用。
const POOL: u32 = 4;
/// 项目 ID 取摘要的前多少个十六进制字符。
const PROJECT_DIGEST_LEN: usize = 32;

/// 这个进程自己连库（连接串在解析部署时已经取好），再绑定用户。
pub(crate) async fn open(
    url: &RedactedString,
    user_id: &str,
    workspace: &WorkspaceRef,
    clock: Arc<dyn Clock>,
    uuids: Arc<dyn UuidSource>,
) -> Result<Arc<PgSessionStore>, Fatal> {
    let database = connect(url, POOL).await?;
    bind(&database, user_id, workspace, clock, uuids)
}

/// 连库（连接池上限 `pool`）。serve 进程启动时连一次，所有连接、所有用户共用（multi-tenant.md §8.1）。
pub(crate) async fn connect(url: &RedactedString, pool: u32) -> Result<PgDatabase, Fatal> {
    PgDatabase::connect_configured(url, pool)
        .await
        .map_err(|error| fail(error.to_string()))
}

/// 在已经连上的库上绑定这个用户与工作区的项目：只建绑定，不另开连接。
pub(crate) fn bind(
    database: &PgDatabase,
    user_id: &str,
    workspace: &WorkspaceRef,
    clock: Arc<dyn Clock>,
    uuids: Arc<dyn UuidSource>,
) -> Result<Arc<PgSessionStore>, Fatal> {
    let access =
        StoreAccessContext::verified(user_id, u64::MAX).map_err(|error| fail(error.to_string()))?;
    let store = database
        .bind(access, vec![project(workspace)], clock, uuids)
        .map_err(|error| fail(error.to_string()))?;
    Ok(Arc::new(store))
}

fn fail(message: String) -> Fatal {
    Fatal(format!("Error: shared session database: {message}"))
}

/// 项目身份按工作区派生：路径相同的机器共用一个项目，不同路径各自一个（pg-store.md §11）。
pub(crate) fn project(workspace: &WorkspaceRef) -> StoreProject {
    let bytes = serde_json::to_vec(workspace).unwrap_or_default();
    let digest = format!("{:x}", Sha256::digest(bytes));
    StoreProject {
        id: format!("ws_{}", &digest[..PROJECT_DIGEST_LEN]),
        workspace: workspace.clone(),
    }
}

#[cfg(test)]
#[path = "session_pg_tests.rs"]
mod tests;
