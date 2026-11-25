//! 本机项目规则集与会话规则（编排在 node-records `project_rules`）。
pub use rust_cli_node_records::project_rules::*;

use rusqlite::Connection;
use rust_cli_kernel_api::env::EnvPath;
use rust_cli_kernel_api::permission::ProjectRules;
use rust_cli_kernel_api::store::StoreError;

use crate::rows::Sqlite;

/// `/permissions` 的修改（tui-manage.md §1.1）：不经会话，在一个 `begin immediate` 事务里读、改、upsert，
/// 返回落库后的规则集。
pub(crate) fn change_in(
    conn: &mut Connection,
    project: &str,
    now: u64,
    change: &rust_cli_kernel_api::store::ProjectRulesChange,
) -> Result<ProjectRules, StoreError> {
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(crate::storage)?;
    let rules = rust_cli_node_records::project_rules::change(&Sqlite(&tx), project, now, change)?;
    tx.commit().map_err(crate::storage)?;
    Ok(rules)
}

/// 没有会话时记住目录（`LocalSettings::remember_directories`）：一个 `begin immediate` 事务里读、改、upsert。
pub(crate) fn remember_directories(
    conn: &mut Connection,
    project: &str,
    now: u64,
    added: &[EnvPath],
) -> Result<(), StoreError> {
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(crate::storage)?;
    remember_in(&Sqlite(&tx), project, now, added)?;
    tx.commit().map_err(crate::storage)
}

#[cfg(test)]
#[path = "project_rules_tests.rs"]
mod tests;
