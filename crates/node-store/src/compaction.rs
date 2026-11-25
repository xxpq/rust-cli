//! 压缩的落库与冷读取（编排在 node-records `compaction`，docs/specs/rust-cli/compact.md §6）。

#[cfg(test)]
#[path = "compaction_tests.rs"]
mod tests;
