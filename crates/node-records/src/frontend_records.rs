//! 前端记录（缺口 G2，docs/specs/rust-cli/app-protocol.md §5.2、README §5 的例外）：前端给的不透明记录原样写成
//! `session_entry`（`type = kind`、`id = key`，同 key 覆盖，不推进会话的活动时间）。这里不认识任何 kind；
//! 不加 `rust-cli/` 前缀，因为写的是前端与 Node 共用的记录类型（例如 App 协议的命令事实）。
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::store::{FrontendRecord, StoreError};

use crate::rows::Rows;

/// 会话存在且没有删除。
fn require_session(conn: &dyn Rows, session: &str) -> Result<(), StoreError> {
    match conn.session(session)? {
        Some(row) if row.time_deleted.is_none() => Ok(()),
        _ => Err(StoreError::NotFound {
            session: SessionId::new(session),
        }),
    }
}

pub fn put(
    conn: &dyn Rows,
    session: &str,
    record: &FrontendRecord,
    now_ms: u64,
) -> Result<(), StoreError> {
    require_session(conn, session)?;
    let now = i64::try_from(now_ms).unwrap_or(i64::MAX);
    conn.save_entry(&crate::rows::Entry {
        id: record.key.clone(),
        session_id: session.to_owned(),
        kind: record.kind.clone(),
        time_created: now,
        time_updated: now,
        data: record.data.clone(),
        touch_session: false,
    })
}

pub fn list(conn: &dyn Rows, session: &str, kind: &str) -> Result<Vec<FrontendRecord>, StoreError> {
    Ok(conn
        .entries(session, Some(kind))?
        .into_iter()
        .map(|entry| FrontendRecord {
            kind: entry.kind,
            key: entry.id,
            data: entry.data,
        })
        .collect())
}
