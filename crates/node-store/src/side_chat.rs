//! 侧聊的新会话（编排在 node-records `side_chat`，docs/specs/rust-cli/session-lifecycle.md §10.58）：本机在一个
//! `begin immediate` 事务里写完。
use rusqlite::{Connection, TransactionBehavior};
use rust_cli_kernel_api::session_facts::SessionFacts;
use rust_cli_kernel_api::store::{ForkReceipt, SideChatFork, StoreError};

use crate::ids::NodeIds;
use crate::rows::Sqlite;

/// 侧聊不是列表会话（`LISTED_TASK_TYPES`），写它不改会话列表的事实：第二项总是 None。
pub(crate) fn side_chat(
    conn: &mut Connection,
    fork: &SideChatFork,
    ids: &NodeIds,
) -> Result<(ForkReceipt, Option<SessionFacts>), StoreError> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(crate::storage)?;
    let uuid = || ids.uuid();
    let receipt =
        rust_cli_node_records::side_chat::side_chat(&Sqlite(&tx), fork, (ids.now_ms(), &uuid))?;
    tx.commit().map_err(crate::storage)?;
    Ok((receipt, None))
}

#[cfg(test)]
#[path = "side_chat_tests.rs"]
mod tests;
