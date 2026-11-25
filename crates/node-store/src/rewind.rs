//! 回退游标与文件 checkpoint（编排在 node-records `rewind`）。
pub use rust_cli_node_records::rewind::*;

#[cfg(test)]
#[path = "rewind_tests.rs"]
mod tests;
