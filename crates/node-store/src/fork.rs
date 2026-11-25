//! `/fork` 的新会话（编排在 node-records `fork`）：本机在一个 `begin immediate` 事务里写完。
use rusqlite::{Connection, TransactionBehavior};
use rust_cli_kernel_api::session_facts::SessionFacts;
use rust_cli_kernel_api::store::{ForkReceipt, SessionFork, StoreError};

use crate::ids::NodeIds;
use crate::rows::Sqlite;
use crate::session_facts::Watch;

#[cfg(test)]
use rust_cli_node_records::fork::CONTINUE;

/// 另给出新会话改了的列表会话事实（session-facts.md §3），调用方在提交之后发出。
pub(crate) fn fork(
    conn: &mut Connection,
    fork: &SessionFork,
    ids: &NodeIds,
) -> Result<(ForkReceipt, Option<SessionFacts>), StoreError> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(crate::storage)?;
    let watch = Watch::start(&tx, fork.child.as_str())?;
    let uuid = || ids.uuid();
    let receipt = rust_cli_node_records::fork::fork(&Sqlite(&tx), fork, (ids.now_ms(), &uuid))?;
    let changed = watch.finish(&tx)?;
    tx.commit().map_err(crate::storage)?;
    Ok((receipt, changed))
}

#[cfg(test)]
#[path = "fork_tests.rs"]
mod tests;
