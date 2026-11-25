//! 项目的持久权限模式（编排在 node-records `project_mode`）。
pub use rust_cli_node_records::project_mode::*;

#[cfg(test)]
#[path = "project_mode_tests.rs"]
mod tests;
