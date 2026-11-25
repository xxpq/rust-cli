//! ZCode Protocol v5 的前端（docs/specs/rust-cli/protocol-v5.md，工作线 F141）：Host 经 stdio 驱动的 v5 骨架——握手、能力表、
//! 进程级 `agent` 与每个工作区的 `ws/<key>/agent`、`ping`、订阅一个 topic 的快照与 cursor、`agent.release`、未知方法与命令的
//! 结构化回复；会话列表 `ws/<key>/sessions` 与 `sessions.facts`（§14）；会话领域（§15）；工作区 MCP 的信任（§16）；插件引用目录与 hooks 预信任（§59）；归档会话的永久删除（§65）；插件管理（§72，经入口实现的端口 [`PluginAdmin`]）。与 tui、headless、v4 的协议前端同层，互不依赖；只经
//! 协议框架（protocol-core）、内核的存储查询（kernel-api）与入口注入的端口。CLI→Host 的 `host.*` 请求（§30）：经 Host 取账号凭据
//! （[`Handle::credentials`] 给出 kernel-api 的请求头来源）。
//!
//! 线上类型全部由 zod 生成（[`generated`]，docs/zcode-protocol-v5/11-contract-codegen.md），不手写。连接的一切状态归一个
//! actor 任务（[`start`]）：读循环、入口报告的启动事实、工作区组装的完成都是发给它的消息。

pub mod generated;
pub mod tolerant;

mod account_overlay;
mod agent;
mod attachments;
mod caps;
mod commands;
mod consts;
mod deletion;
mod errors;
mod handshake;
mod hook_pretrust;
mod host_services;
mod mcp_oauth;
mod mcp_trust;
mod outbox;
mod plugin_admin;
mod plugin_ops;
mod plugin_references;
mod plugins;
mod publisher;
mod server;
mod session;
mod sessions;
mod slash;
mod strict;
mod topics;
mod trust_writes;

#[cfg(test)]
mod plugin_admin_fake;
#[cfg(test)]
mod session_tests;
#[cfg(test)]
mod test_link;

pub use consts::MAX_MESSAGE_BYTES;
pub use hook_pretrust::{HookPretrust, HookPretrustRefusal};
pub use mcp_trust::{McpDecision, McpPending, McpTrustRefusal, McpTrusted};
pub use plugin_admin::{
    PluginAdmin, PluginApplied, PluginInventory, PluginOp, PluginRefusal, WorkspacePlugins,
};
pub use plugin_references::PluginReference;
pub use server::{
    AttachmentFile, AttachmentFileError, Handle, Settings, Storage, WorkspaceKernel, Workspaces,
    start,
};
