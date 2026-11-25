//! 会话列表的事实（docs/specs/rust-cli/session-facts.md §2、§3；读法在 node-records 的 `session_facts`）：本机库的查询与
//! 写事务里的变化（[`Watch`]）。
use rusqlite::Connection;
use rust_cli_kernel_api::env::WorkspaceRef;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::session_facts::SessionFacts;
use rust_cli_kernel_api::store::StoreError;
use rust_cli_node_records::session_facts::{listed, of};

use crate::repo::sessions;
use crate::rows::Sqlite;
use crate::{LOCAL_ENVIRONMENT, facts, storage};

/// 这些会话的事实，按给出的顺序；库里没有的不给出。
pub fn lookup(conn: &Connection, ids: &[SessionId]) -> Result<Vec<SessionFacts>, StoreError> {
    rust_cli_node_records::session_facts::lookup(&Sqlite(conn), ids)
}

/// 这个工作区最近活动的列表会话（含软删除的）的事实。
pub fn recent(
    conn: &Connection,
    workspace: &WorkspaceRef,
    limit: usize,
) -> Result<Vec<SessionFacts>, StoreError> {
    if workspace.environment.as_str() != LOCAL_ENVIRONMENT {
        return Err(facts::unsupported("sessions of a remote environment"));
    }
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    sessions::recent(conn, workspace.root.as_str(), limit)
        .map_err(storage)?
        .into_iter()
        .map(|row| of(&Sqlite(conn), row))
        .collect()
}

/// 一次写事务里的事实变化（session-facts.md §3）：开始时读这个会话的列表事实，写完、提交之前 [`Self::finish`] 再读一次，
/// 不同时给出新值，由调用方在事务提交之后发出（`NodeStore::write`）。读失败时整个写事务失败：不会出现“写成功、事实没发”。
pub(crate) struct Watch {
    session: String,
    before: Option<SessionFacts>,
}

impl Watch {
    pub(crate) fn start(conn: &Connection, session: &str) -> Result<Self, StoreError> {
        Ok(Self {
            session: session.to_owned(),
            before: listed(&Sqlite(conn), session)?,
        })
    }

    pub(crate) fn finish(self, conn: &Connection) -> Result<Option<SessionFacts>, StoreError> {
        Ok(listed(&Sqlite(conn), &self.session)?
            .filter(|after| self.before.as_ref() != Some(after)))
    }
}

#[cfg(test)]
#[path = "session_facts_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "fact_changes_tests.rs"]
mod change_tests;
