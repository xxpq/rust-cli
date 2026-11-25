//! fork 复制消息与目标状态（编排在 node-records `fork_clone`）。
#[cfg(test)]
use crate::{facts::Commit, rows::Sqlite};
#[cfg(test)]
use rust_cli_node_records::fork_clone::Mapping;
pub use rust_cli_node_records::fork_clone::*;

#[cfg(test)]
#[path = "fork_clone_tests.rs"]
mod tests;
