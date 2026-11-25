//! Node 会话库的仓储：打开与迁移、会话、消息与 part、session_entry、`local_setting`、todo、用量与用量的只读报表
//! （复制自 `apps/zcode-cli-rust/crates/state/src/node`，docs/specs/rust-cli/node-store.md §1、§2）。普通实体读写用 ORM，
//! JSON 合并、索引敏感的读取与统计聚合保留原 SQL（node-store.md §2）。
//!
//! 这里使用 ORM 与旧记录编解码；迁移和特殊 JSON 合并保留原文 SQL，不知道内核的事实；事实到记录的映射在 `facts`。
//! 所有函数都是同步的，只在存储的阻塞线程里调用。
pub use rust_cli_node_records::codecs;
pub mod entries;
pub mod messages;
pub mod migrations;
pub mod open;
mod open_error;
pub mod sessions;
pub mod settings;
pub mod usage;
pub mod usage_reports;

#[cfg(test)]
#[path = "repo_tests.rs"]
mod repo_tests;
