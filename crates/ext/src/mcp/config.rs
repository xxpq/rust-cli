//! MCP 服务器的配置快照（docs/specs/rust-cli/ext.md §3.1）与名字规范化（§3.4）。
//!
//! 入口层读配置文件、展开 `${VAR}`、按作用域合并，交给 ext 的只是这份快照。

use rust_cli_base::units;
use rust_cli_kernel_api::ids::MachineName;
use rust_cli_kernel_api::mcp::McpTransportConfig;

/// 配置作用域。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpScope {
    Enterprise,
    User,
    Local,
    Project,
    /// `--mcp-config`。
    Dynamic,
    /// 插件提供（ext.md §4）。
    Plugin,
}

/// 一个服务器。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServer {
    pub name: String,
    pub scope: McpScope,
    pub transport: McpTransportConfig,
    pub options: McpServerOptions,
    /// 服务器绑定的机器（R5，remote-exec.md §22.2：入口按 rust-cli 的旁路文件设）；缺省是主 env。绑定到额外机器时
    /// `transport` 是配置的原文（`${VAR}` 由那台机器的执行器展开，§22.7）。
    pub machine: MachineName,
}

/// 按服务器的选项（Node 配置的 `timeoutMs`、`alwaysLoad`、`isolation`、`protocolVersion`，ext.md §3.1）；`--mcp-config`
/// 与插件的服务器用缺省值。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct McpServerOptions {
    /// 这个服务器的连接（握手与清单）、工具调用与资源读取的期限；None 时用 [`McpSettings`] 的缺省值。
    pub timeout_ms: Option<u64>,
    /// 这个服务器的工具都不延迟（工具自己的 `_meta` 标记也不延迟，`tool.rs` 的 `ALWAYS_LOAD_KEYS`）。
    pub always_load: bool,
    /// 连接能不能在会话之间共用（Node `McpServerIsolation`，ext.md §3.9）；只在 [`McpSharing::PerSession`] 下看。
    pub isolation: McpIsolation,
    /// 握手可以用哪个纪元的修订（Node `McpProtocolVersion`，ext.md §3.10.2）。
    pub protocol: McpProtocol,
}

/// 服务器配置的 `protocolVersion`（Node `McpProtocolVersion`，ext.md §3.10.2）。Node 的 `auto` 先探测新纪元，本客户端
/// 没有新纪元，所以 `auto` 与 `legacy` 相同，只剩两种。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum McpProtocol {
    /// 没写、`auto` 或 `legacy`：`initialize` 握手，按本客户端与传输可用的修订协商。
    #[default]
    Initialize,
    /// `2026-07-28`：只用新纪元、不回退（Node 的钉死）。本客户端还不会新纪元，这个服务器不连接。
    Modern,
}

/// 服务器的连接能不能在会话之间共用（Node 的 `isolation`，工作线 F145）：缺省每个会话一份；`Workspace` 是配置显式
/// 声明的无状态服务器，同一工作区的会话共用一份。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum McpIsolation {
    #[default]
    Session,
    Workspace,
}

/// 连接归谁（入口按前端给，ext.md §3.9）：`Process` 全部服务器在内核级一份（缺省）；`PerSession` 声明
/// [`McpIsolation::Workspace`] 的在内核级，其余每个顶层会话一份，会话打开时连、会话释放时断。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum McpSharing {
    #[default]
    Process,
    PerSession,
}

pub type McpServers = Vec<McpServer>;

/// 前端撑得起的服务器询问（数据，入口按前端给，ext.md §3.13.1）：ext 据此在 `initialize` 声明 `elicitation`、`sampling`，并截下
/// 声明了的请求。缺省什么都不声明（没声明的请求照旧回 Method not found）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct McpClientTraits {
    /// elicitation 的表单模式。
    pub elicitation_form: bool,
    /// elicitation 的网址模式（前端能打开浏览器）。
    pub elicitation_url: bool,
    /// sampling（前端能让人确认）。
    pub sampling: bool,
}

/// 连接与调用的设置（入口层从环境变量读，缺省值见 ext.md §3.3）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpSettings {
    pub servers: McpServers,
    /// `MCP_TIMEOUT`：连接与握手。
    pub connect_timeout_ms: u64,
    /// `MCP_TOOL_TIMEOUT`：一次工具调用。
    pub tool_timeout_ms: u64,
    /// `MAX_MCP_OUTPUT_TOKENS`：结果截断的上限。
    pub max_output_tokens: u64,
    /// `initialize` 的 `clientInfo.version`：入口给的本工程版本（docs/specs/rust-cli/version.md §4）；
    /// ext 不读自己的包版本。缺省为空串。
    pub client_version: String,
    /// `-p` 的启动等待（ext.md §3.3.1，修复线 F36）：入口按环境变量与参数算好。
    pub startup: super::startup::McpStartup,
    /// 首次连接同时进行的上限（ext.md §3.3.4）：本机（stdio）`MCP_SERVER_CONNECTION_BATCH_SIZE`，远端
    /// `MCP_REMOTE_SERVER_CONNECTION_BATCH_SIZE`。
    pub batch: McpBatch,
    /// 在远程 shell 里（有 SSH，或 Linux 上没有图形界面；入口按同 TUI 的规则算）：授权伪工具 authenticate 的结果末尾是远程会话那一段
    /// （浏览器的回调到不了这台机器，请用户把地址贴回来，ext.md §3.12.16）。
    pub remote_shell: bool,
}

/// 首次连接同时进行的上限（ext.md §3.3.4，对照实现的缺省值）。都至少 1。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct McpBatch {
    pub local: usize,
    pub remote: usize,
}

/// 本机服务器首次连接的缺省上限（`MCP_SERVER_CONNECTION_BATCH_SIZE` 没设时）。
pub const DEFAULT_LOCAL_BATCH: usize = 3;
/// 远端服务器首次连接的缺省上限（`MCP_REMOTE_SERVER_CONNECTION_BATCH_SIZE` 没设时）。
pub const DEFAULT_REMOTE_BATCH: usize = 20;

impl Default for McpBatch {
    fn default() -> Self {
        Self {
            local: DEFAULT_LOCAL_BATCH,
            remote: DEFAULT_REMOTE_BATCH,
        }
    }
}

impl McpSettings {
    /// 这个服务器的连接期限：`timeoutMs`，没有时 `MCP_TIMEOUT`。
    pub fn connect_timeout_for(&self, options: McpServerOptions) -> u64 {
        options.timeout_ms.unwrap_or(self.connect_timeout_ms)
    }

    /// 这个服务器的工具调用与资源读取期限：`timeoutMs`，没有时 `MCP_TOOL_TIMEOUT`。
    pub fn tool_timeout_for(&self, options: McpServerOptions) -> u64 {
        options.timeout_ms.unwrap_or(self.tool_timeout_ms)
    }
}

/// 连接一个服务器的缺省超时（`MCP_TIMEOUT`）。
const DEFAULT_CONNECT_TIMEOUT_MS: u64 = 30 * units::MS_PER_SECOND;
/// 一次工具调用的缺省超时（`MCP_TOOL_TIMEOUT`，约 27.8 小时，实际不限）。
const DEFAULT_TOOL_TIMEOUT_MS: u64 = 100_000_000;
/// 工具结果的缺省 token 上限（`MAX_MCP_OUTPUT_TOKENS`）。
const DEFAULT_MAX_OUTPUT_TOKENS: u64 = 25_000;

impl Default for McpSettings {
    fn default() -> Self {
        Self {
            servers: Vec::new(),
            connect_timeout_ms: DEFAULT_CONNECT_TIMEOUT_MS,
            tool_timeout_ms: DEFAULT_TOOL_TIMEOUT_MS,
            max_output_tokens: DEFAULT_MAX_OUTPUT_TOKENS,
            client_version: String::new(),
            startup: super::startup::McpStartup::default(),
            batch: McpBatch::default(),
            remote_shell: false,
        }
    }
}

/// `[^a-zA-Z0-9_-]` 换成 `_`。规则只有
/// 一份，在 kernel-api（ToolSearch 与直接调用时的等待也用它，F36）。
pub use rust_cli_kernel_api::mcp_state::normalize_name;

/// `mcp__<server>__<tool>`，两段都规范化（拼法只在 kernel-api 的 `mcp_name`）。
pub fn tool_name(server: &str, tool: &str) -> String {
    rust_cli_kernel_api::mcp_name::compose(&normalize_name(server), &normalize_name(tool))
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
