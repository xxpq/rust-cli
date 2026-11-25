//! 冷读取（编排在 node-records `load`）。
use rusqlite::Connection;
use rust_cli_kernel_api::store::{SessionFilter, SessionSummary, StoreError};

use crate::LOCAL_ENVIRONMENT;
use crate::facts::unsupported;
use crate::rows::Sqlite;

pub use rust_cli_node_records::load::*;

/// 本机只有本机环境的会话（远端机器上的会话在执行环境里，remote-exec.md §3）。
pub fn list(conn: &Connection, filter: &SessionFilter) -> Result<Vec<SessionSummary>, StoreError> {
    if filter.workspace.environment.as_str() != LOCAL_ENVIRONMENT {
        return Err(unsupported("sessions of a remote environment"));
    }
    rust_cli_node_records::load::list(&Sqlite(conn), filter)
}

#[cfg(test)]
#[path = "load_turn_tests.rs"]
mod turn_tests;
