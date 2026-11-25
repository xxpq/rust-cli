//! 事实 → Node 记录（编排在 node-records `facts`）：本机一次 `commit` 在一个 `begin immediate` 事务里提交
//! （docs/specs/rust-cli/node-store.md §4，docs/specs/rust-cli/README.md §5）。
use rusqlite::{Connection, TransactionBehavior};
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::session_facts::SessionFacts;
use rust_cli_kernel_api::store::{CommitReceipt, FactBatch, StoreError};
use rust_cli_node_records::rows::Rows;

use crate::ids::NodeIds;
use crate::rows::Sqlite;
use crate::session_facts::Watch;

pub use rust_cli_node_records::facts::*;

/// 在一个 `begin immediate` 事务里应用整批事实（[`apply_batch`]）。另给出这一批改了的列表会话事实（session-facts.md §3），
/// 调用方在提交之后发出。
///
/// 第一项是 `SessionCreated`（草稿落库，session-facts.md §4）时先建会话行（与 `SessionStore::create` 同一个写法，`version`
/// 是会话行的版本），其余的事实在同一个事务里。
pub fn commit(
    conn: &mut Connection,
    session: &SessionId,
    (batch, version): (&FactBatch, &str),
    ids: &NodeIds,
) -> Result<(CommitReceipt, Option<SessionFacts>), StoreError> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(crate::storage)?;
    let watch = Watch::start(&tx, session.as_str())?;
    if let Some((meta, _)) = created(batch) {
        let now = i64::try_from(ids.now_ms()).unwrap_or(i64::MAX);
        Sqlite(&tx).create_session(&crate::create_input(meta, version)?, now)?;
    }
    let uuid = || ids.uuid();
    let receipt = apply_batch(&Sqlite(&tx), session, batch, (ids.now_ms(), &uuid))?;
    let changed = watch.finish(&tx)?;
    tx.commit().map_err(crate::storage)?;
    Ok((receipt, changed))
}

#[cfg(test)]
#[path = "turn_anchor_tests.rs"]
mod turn_anchor_tests;

#[cfg(test)]
#[path = "session_created_tests.rs"]
mod session_created_tests;
