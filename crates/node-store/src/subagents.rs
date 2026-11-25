//! 库里的子代理（`StoreQueries::subagents`，docs/specs/rust-cli/session-lifecycle.md §10.46；编排在 node-records 的
//! `subagents`）。只读。

use rusqlite::Connection;
use rust_cli_kernel_api::agents::StoredSubagent;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::store::StoreError;

use crate::rows::Sqlite;

pub(crate) fn subagents(
    conn: &Connection,
    parent: &SessionId,
) -> Result<Vec<StoredSubagent>, StoreError> {
    rust_cli_node_records::subagents::stored(&Sqlite(conn), parent)
}

#[cfg(test)]
#[path = "subagents_tests.rs"]
mod tests;
