//! 会话管理（修复线 F29，docs/specs/rust-cli/node-store.md §5.1）：← 项目页与 `/resume` 页的查询、改名与删除。
//!
//! 口径照 App 的任务列表（Node `sessions-index`：`task_type in (interactive, fork, workflow_parent)`、本机
//! `workspace_id is null`、没删除，`order by time_updated desc, id desc`），可以不限目录；改名照 Node
//! `setCustomSessionTitle`，删除照 Node `softDeleteSession`。
use rust_cli_kernel_api::env::EnvPath;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::store::{
    SessionOrder, SessionPage, SessionQuery, SessionRecord, StoreError,
};

use crate::facts;
use crate::rows::Rows;

/// 首条输入最多保留的字符数（Unicode 标量）。
pub const FIRST_PROMPT_CHARS: usize = 500;

/// 首条真实用户输入的第一个文字 part：沿 `message_session_time_created_id_idx` 的顺序找第一条
fn first_prompt(conn: &dyn Rows, session: &str) -> Result<Option<String>, StoreError> {
    let data = conn.first_prompt_part(session)?;
    Ok(data
        .and_then(|data| serde_json::from_str::<serde_json::Value>(&data).ok())
        .and_then(|part| part["text"].as_str().map(str::to_owned))
        .map(|text| text.chars().take(FIRST_PROMPT_CHARS).collect()))
}

pub fn query(conn: &dyn Rows, query: &SessionQuery) -> Result<SessionPage, StoreError> {
    let limit = (query.limit > 0).then(|| i64::try_from(query.limit).unwrap_or(i64::MAX - 1));
    let mut rows = conn.query_sessions(
        query.root.as_ref().map(EnvPath::as_str),
        query.order == SessionOrder::Created,
        limit.map(|limit| limit + 1),
    )?;
    let more = limit.is_some_and(|limit| rows.len() as i64 > limit);
    if let Some(limit) = limit {
        rows.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
    }
    let sessions = rows
        .into_iter()
        .map(|row| {
            Ok(SessionRecord {
                first_prompt: first_prompt(conn, &row.id)?,
                id: SessionId::new(row.id),
                title: row.title,
                root: EnvPath::new(row.directory),
                created_at_ms: u64::try_from(row.time_created).unwrap_or(0),
                updated_at_ms: u64::try_from(row.time_updated).unwrap_or(0),
            })
        })
        .collect::<Result<_, StoreError>>()?;
    Ok(SessionPage { sessions, more })
}

/// Node `setCustomSessionTitle`：`updateSession{title, titleSource: custom}`（不推进活动时间）。会话不存在或已删除时
/// `NotFound`。调用方负责事务。
pub fn rename(
    conn: &dyn Rows,
    session: &SessionId,
    title: &str,
    now: u64,
) -> Result<(), StoreError> {
    facts::live_session(conn, session)?;
    let update = crate::rows::Update {
        id: session.as_str().to_owned(),
        title: Some(title.to_owned()),
        title_source: Some("custom".to_owned()),
        ..Default::default()
    };
    conn.update_session(&update, i64::try_from(now).unwrap_or(i64::MAX))?;
    Ok(())
}

/// Node `softDeleteSession`：`time_deleted = coalesce(time_deleted, now)`，消息与活动时间保留。会话不存在时
/// `NotFound`；已删除时不变。调用方负责事务。
pub fn delete(conn: &dyn Rows, session: &SessionId, now: u64) -> Result<(), StoreError> {
    if conn.session(session.as_str())?.is_none() {
        return Err(StoreError::NotFound {
            session: session.clone(),
        });
    }
    conn.soft_delete_session(session.as_str(), i64::try_from(now).unwrap_or(i64::MAX))?;
    Ok(())
}
