//! 工作区信任的记录（编排在 node-records `workspace_trust`）。
pub use rust_cli_node_records::workspace_trust::*;

#[cfg(test)]
#[path = "workspace_trust_tests.rs"]
mod tests;
