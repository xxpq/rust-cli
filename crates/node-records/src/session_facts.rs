//! 会话列表的事实（docs/specs/rust-cli/session-facts.md §2）：会话行与它的配置记录 → kernel-api 的 `SessionFacts`，经 [`Rows`]
//! 读（本机与云端共用一份读法）。写事务里的变化（`Watch`）与工作区的最近列表只在本机库（node-store）。
//!
//! 全部取自 Node 已有的列与记录（不加列、不加表）：会话行（标题与来源、时间、结局、删除）、首条真实输入的时刻、首条消息
//! 上的导入标记、`runtime/model_selection` 与执行状态（同冷读取，`load.rs`）。
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{EnvironmentId, SessionId};
use rust_cli_kernel_api::session_facts::{
    MigrationSource, SessionFacts, SessionOutcome, TitleSource,
};
use rust_cli_kernel_api::store::StoreError;
use serde_json::Value;

use crate::rows::{Rows, SessionRow};
use crate::{LOCAL_ENVIRONMENT, load, state};

/// Node 导入的历史在消息与 part 的 `metadata` 里记的来源键（`server-operations.ts` 的 `importedHistory`）。
const MIGRATION_KEY: &str = "migrationSource";
/// Node 导入外部会话历史时写的来源值（v4 `ZCodeTaskMigrationSource` 的唯一取值）。
const IMPORT_SOURCE: &str = "claudeCode";
/// Node 更早版本导入时的固定消息 ID：`msg_import_<数字>` 与 [`LEGACY_IMPORT_ID_PREFIX`] 开头的（Node `isImportedHistoryMessage`）。
const IMPORT_ID_PREFIX: &str = "msg_import_";
const LEGACY_IMPORT_ID_PREFIX: &str = "msg_claude-import-";

/// Node `title_source` 的取值（表上有 CHECK，只有这四个）；NULL 是 Node 建会话的缺省 `first_input`。
fn title_source(value: Option<&str>) -> TitleSource {
    match value.unwrap_or("first_input") {
        "first_input" => TitleSource::FirstInput,
        "generated" => TitleSource::Generated,
        "custom" => TitleSource::Custom,
        _ => TitleSource::Default,
    }
}

/// Node `last_outcome`；认不出的非空值按 error：错了也安全，结局可见（02 §11.4 的降级规则）。
fn outcome(value: Option<&str>) -> Option<SessionOutcome> {
    value.map(|value| match value {
        "completed" => SessionOutcome::Completed,
        "interrupted" => SessionOutcome::Interrupted,
        _ => SessionOutcome::Error,
    })
}

fn millis(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

/// 导入的消息 id（Node `isImportedHistoryMessage` 的 id 规则）。
fn imported_id(id: &str) -> bool {
    id.strip_prefix(IMPORT_ID_PREFIX)
        .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
        || id.starts_with(LEGACY_IMPORT_ID_PREFIX)
}

/// 迁移来源：只看首条消息（session-facts.md §7 差异 1：导入的历史在会话开头）。
fn migration(conn: &dyn Rows, session: &str) -> Result<Option<MigrationSource>, StoreError> {
    let Some((id, data)) = conn.first_message(session)? else {
        return Ok(None);
    };
    let marked = imported_id(&id)
        || serde_json::from_str::<Value>(&data)
            .is_ok_and(|data| data["metadata"][MIGRATION_KEY] == IMPORT_SOURCE)
        || conn.part_metadata_is(&id, MIGRATION_KEY, IMPORT_SOURCE)?;
    Ok(marked.then_some(MigrationSource::ImportedHistory))
}

/// 一个会话行的事实。
pub fn of(conn: &dyn Rows, row: SessionRow) -> Result<SessionFacts, StoreError> {
    let selection = state::selection(conn, &row.id)?
        .as_ref()
        .map(load::kernel_selection);
    Ok(SessionFacts {
        kind: load::kind(&row.task_type),
        parent: row.parent_id.as_deref().map(SessionId::new),
        workspace: WorkspaceRef {
            environment: EnvironmentId::new(
                row.workspace_id.as_deref().unwrap_or(LOCAL_ENVIRONMENT),
            ),
            root: EnvPath::new(row.directory.as_str()),
        },
        title_source: title_source(row.title_source.as_deref()),
        created_at_ms: millis(row.time_created),
        updated_at_ms: millis(row.time_updated),
        last_outcome: outcome(row.last_outcome.as_deref()),
        deleted_at_ms: row.time_deleted.map(millis),
        first_input_at_ms: conn.first_input_at(&row.id)?.map(millis),
        migration: migration(conn, &row.id)?,
        selection,
        execution: load::execution_state(conn, &row)?,
        id: SessionId::new(row.id),
        title: row.title,
    })
}

/// 这些会话的事实，按给出的顺序；库里没有的不给出。
pub fn lookup(conn: &dyn Rows, ids: &[SessionId]) -> Result<Vec<SessionFacts>, StoreError> {
    let wanted: Vec<&str> = ids.iter().map(SessionId::as_str).collect();
    let mut rows = conn.sessions_by_ids(&wanted)?;
    let mut found = Vec::new();
    for id in wanted {
        if let Some(index) = rows.iter().position(|row| row.id == id) {
            found.push(of(conn, rows.swap_remove(index))?);
        }
    }
    Ok(found)
}

/// 列表会话的事实；没有这一行或不是列表会话时 None（先看会话行的种类，子代理的子会话不读别的记录）。
pub fn listed(conn: &dyn Rows, session: &str) -> Result<Option<SessionFacts>, StoreError> {
    match conn.session(session)? {
        Some(row) if row.listed() => of(conn, row).map(Some),
        _ => Ok(None),
    }
}
