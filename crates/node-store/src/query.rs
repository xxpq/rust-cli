//! 会话分页、改名与软删除（编排在 node-records `query`）：本机的改名与删除各在一个 `begin immediate` 事务里。
use rusqlite::{Connection, TransactionBehavior};
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::session_facts::SessionFacts;
use rust_cli_kernel_api::store::{SessionPage, SessionQuery, StoreError};

use crate::rows::Sqlite;
use crate::session_facts::Watch;
use crate::{LOCAL_ENVIRONMENT, facts};

/// `role = user`、`semantics.origin = real_user` 的消息，取它的第一个非合成 text part（按 part 的顺序）。
pub(crate) const FIRST_PROMPT: &str = "select p.data from message m join part p on p.message_id = m.id
      where m.session_id = ?1
        and (case when json_valid(m.data) then json_extract(m.data, '$.role') end) = 'user'
        and (case when json_valid(m.data) then json_extract(m.data, '$.semantics.origin') end) = 'real_user'
        and (case when json_valid(p.data) then json_extract(p.data, '$.type') end) = 'text'
        and coalesce(case when json_valid(p.data) then json_extract(p.data, '$.synthetic') end, 0) = 0
      order by m.time_created, m.id, p.sequence is null, p.sequence, p.time_created, p.id
      limit 1";

pub fn query(conn: &Connection, query: &SessionQuery) -> Result<SessionPage, StoreError> {
    if query.environment.as_str() != LOCAL_ENVIRONMENT {
        return Err(facts::unsupported("sessions of a remote environment"));
    }
    rust_cli_node_records::query::query(&Sqlite(conn), query)
}

/// 改名与删除另给出改了的列表会话事实（session-facts.md §3），调用方在提交之后发出。
pub fn rename(
    conn: &mut Connection,
    session: &SessionId,
    title: &str,
    now: u64,
) -> Result<((), Option<SessionFacts>), StoreError> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(crate::storage)?;
    let watch = Watch::start(&tx, session.as_str())?;
    rust_cli_node_records::query::rename(&Sqlite(&tx), session, title, now)?;
    let changed = watch.finish(&tx)?;
    tx.commit().map_err(crate::storage)?;
    Ok(((), changed))
}

pub fn delete(
    conn: &mut Connection,
    session: &SessionId,
    now: u64,
) -> Result<((), Option<SessionFacts>), StoreError> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(crate::storage)?;
    let watch = Watch::start(&tx, session.as_str())?;
    rust_cli_node_records::query::delete(&Sqlite(&tx), session, now)?;
    let changed = watch.finish(&tx)?;
    tx.commit().map_err(crate::storage)?;
    Ok(((), changed))
}

#[cfg(test)]
#[path = "query_tests.rs"]
mod tests;
