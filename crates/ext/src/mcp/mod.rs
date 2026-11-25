//! MCP 的语义部分（docs/specs/rust-cli/ext.md §3）：连接管理、握手、工具包装、结果转换、instructions、
//! resources。传输经 kernel-api 的 `McpTransport`（本地实现在 env-local）。

mod answers;
mod asks;
mod config;
mod connection;
mod declaration;
mod elicitation;
mod link;
mod manager;
mod node_config;
mod notices;
mod parse;
mod resources;
mod result;
mod rpc;
mod sampling;
mod sessions;
mod startup;
mod tool;
mod wait_tool;

pub use asks::AskPorts;
pub use config::{
    McpBatch, McpClientTraits, McpIsolation, McpProtocol, McpScope, McpServer, McpServerOptions,
    McpServers, McpSettings, McpSharing, normalize_name, tool_name,
};
pub use declaration::{Declaration, declaration_digest};
pub use manager::{McpManager, McpSet, McpSignIns};
pub use node_config::{NodeMcpServer, anchor_project_cwd, config_dir, parse_node_servers};
pub use parse::{expand_vars, merge_scopes, parse_servers, parse_servers_keeping};
pub use sessions::McpSessions;
pub use startup::{
    DEFAULT_CONNECT_WAIT_MS, DEFAULT_PREWAIT_MS, McpStartup, prewait_ms, required_servers,
    wait_for_deferrable,
};
pub(crate) use wait_tool::WaitForMcpServers;
