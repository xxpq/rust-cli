//! 按库的版本记住的读（docs/specs/rust-cli/node-store.md §5.7）：范围读的历史索引与库里的子代理目录。
//! 只在连接锁里用（[`crate::NodeStore`] 的阻塞线程）。版本相同 = 上一次记下之后库里没有任何写，所以记住的就是现在读会得到的。
//!
//! ```text
//! 连接锁里：v = Version::of(conn)（读之前取）→ memo 里这个会话的版本是 v？ 是 ─► 用记住的；否 ─► 读库 → 记下 (v, 结果)
//! ```

use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use rust_cli_base::sync::LockUnpoisoned;
use rust_cli_kernel_api::agents::StoredSubagent;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_node_records::range::HistoryIndex;

/// 记住多少个会话（最近用过的在后）：界面同时在看与最近看过的会话。值同前端保留的冷投影上限，两者各自定义：一个限
/// 这里的派生读，一个限前端的投影。
const HISTORY_MEMO_SESSIONS: usize = 8;

/// 库的版本：`PRAGMA data_version`（别的连接提交过就变）与 `total_changes()`（本连接改过的行数，本进程的每一次写都让它变）。NodeStore 只有一个连接，
/// 两样都在连接锁里读，所以两次相同 = 中间库里没有任何写。不按会话细分：会话级的戳有同一毫秒、删加相抵的漏（node-store.md §5.7）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Version {
    data: i64,
    changes: i64,
}

impl Version {
    pub(crate) fn of(conn: &Connection) -> rusqlite::Result<Self> {
        Ok(Self {
            data: conn
                .prepare_cached("pragma data_version")?
                .query_row([], |row| row.get(0))?,
            changes: conn
                .prepare_cached("select total_changes()")?
                .query_row([], |row| row.get(0))?,
        })
    }
}

/// 一个会话在某个库版本下读出的东西。
struct Remembered {
    session: SessionId,
    version: Version,
    index: Option<Arc<HistoryIndex>>,
    subagents: Option<Vec<StoredSubagent>>,
}

/// 记住的读：最近用过的在后，超过 [`HISTORY_MEMO_SESSIONS`] 个时丢最前的。
#[derive(Default)]
pub(crate) struct HistoryMemo {
    entries: Mutex<Vec<Remembered>>,
}

impl HistoryMemo {
    /// 这个会话在 `version` 下的历史索引。
    pub(crate) fn index(&self, session: &SessionId, version: Version) -> Option<Arc<HistoryIndex>> {
        self.with(session, version, |entry| entry.index.clone())
    }

    pub(crate) fn remember_index(
        &self,
        session: &SessionId,
        version: Version,
        index: Arc<HistoryIndex>,
    ) {
        self.remember(session, version, |entry| entry.index = Some(index));
    }

    /// 这个会话在 `version` 下的子代理目录。
    pub(crate) fn subagents(
        &self,
        session: &SessionId,
        version: Version,
    ) -> Option<Vec<StoredSubagent>> {
        self.with(session, version, |entry| entry.subagents.clone())
    }

    pub(crate) fn remember_subagents(
        &self,
        session: &SessionId,
        version: Version,
        subagents: Vec<StoredSubagent>,
    ) {
        self.remember(session, version, |entry| entry.subagents = Some(subagents));
    }

    /// 版本相同的项里取一样；用到的挪到最后。
    fn with<T>(
        &self,
        session: &SessionId,
        version: Version,
        take: impl FnOnce(&Remembered) -> Option<T>,
    ) -> Option<T> {
        let mut entries = self.entries.lock_unpoisoned();
        let at = entries
            .iter()
            .position(|entry| &entry.session == session && entry.version == version)?;
        let entry = entries.remove(at);
        let found = take(&entry);
        entries.push(entry);
        found
    }

    /// 记下一样：同一会话版本不同的项整个换掉（旧版本下读出的别的东西也不能再用）。
    fn remember(&self, session: &SessionId, version: Version, set: impl FnOnce(&mut Remembered)) {
        let mut entries = self.entries.lock_unpoisoned();
        let kept = entries
            .iter()
            .position(|entry| &entry.session == session)
            .map(|at| entries.remove(at))
            .filter(|entry| entry.version == version);
        let mut entry = kept.unwrap_or_else(|| Remembered {
            session: session.clone(),
            version,
            index: None,
            subagents: None,
        });
        set(&mut entry);
        entries.push(entry);
        if entries.len() > HISTORY_MEMO_SESSIONS {
            entries.remove(0);
        }
    }
}

#[cfg(test)]
#[path = "memo_tests.rs"]
mod tests;
