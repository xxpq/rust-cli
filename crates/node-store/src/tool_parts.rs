//! 工具调用与结果 ↔ Node tool part（编排在 node-records `tool_parts`，docs/specs/rust-cli/node-store.md §4.6）。

#[cfg(test)]
#[path = "tool_parts_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "tool_order_tests.rs"]
mod order_tests;

#[cfg(test)]
#[path = "node_open_tests.rs"]
mod node_open_tests;
