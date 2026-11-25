//! 用量事实 → Node 用量表（编排在 node-records `usage`）。

#[cfg(test)]
#[path = "usage_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "usage_observe_tests.rs"]
mod observe_tests;

#[cfg(test)]
#[path = "usage_compaction_tests.rs"]
mod compaction_tests;
