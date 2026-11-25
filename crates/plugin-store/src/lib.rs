//! 插件存储（docs/specs/rust-cli/plugin-store.md）：市场、安装、卸载、更新与插件设置的写侧，和读侧的清单。
//!
//! 两块：`core` 是领域核心，不做 IO（id、市场清单、来源、依赖闭包、记录文档、manifest、摘要、配置补丁、私有文件模型、
//! 诊断）；`local` 是本机存储（Node 的文件布局、存储锁、暂存与提交、市场目录、孤儿目录、Node 死事务的收尾、私有文件、
//! 读侧）。与 Node CLI 共用同一批文件、同一把锁（plugin-store.md §3、§4）。

pub mod core;
pub mod local;
