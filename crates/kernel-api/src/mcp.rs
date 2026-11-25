//! MCP 的传输端口与跨端口类型（docs/specs/rust-cli/ext.md §3.2、§3.4）。
//!
//! MCP 传输属于执行环境（README §6）：内核（ext）只发收 JSON-RPC 消息，协议语义（握手、工具清单、
//! 调用、通知）在内核里；本地实现在 env-local（stdio 经 proc，SSE / streamable HTTP 经 net），上云后
//! 换成远端执行环境的实现。

use std::collections::BTreeMap;

use async_trait::async_trait;
use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::env::WorkspaceRef;
pub use crate::mcp_auth::{McpAuthTarget, McpOAuth};

/// 一个服务器的传输配置（stdio / sse / http 三种）。
/// 本机的服务器：`${VAR}` 已由入口层按来源的规则处理（`--mcp-config` 展开，配置文件原样）。绑定到远端机器的
/// 服务器：值是配置的原文，由那台机器的执行器用它自己的环境展开（[`crate::mcp_vars`]，remote-exec.md §22.7）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpTransportConfig {
    Stdio {
        command: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        args: Vec<String>,
        /// 加在执行环境白名单变量之上的变量。
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        env: BTreeMap<String, String>,
        /// 进程的工作目录（Node 配置的 `cwd`）：相对路径相对工作区根，绝对路径原样；None 是工作区根
        /// （docs/specs/rust-cli/ext.md §3.1、§3.2）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
    },
    Sse {
        url: String,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        headers: BTreeMap<String, String>,
        /// 用 OAuth 时的配置（docs/specs/rust-cli/ext.md §3.12.1）；None 是不用（配置了 `Authorization` 头、`client_credentials`、
        /// 执行器那一侧）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        oauth: Option<McpOAuth>,
    },
    Http {
        url: String,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        headers: BTreeMap<String, String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        oauth: Option<McpOAuth>,
    },
}

impl McpTransportConfig {
    /// 本地进程：没有额外的环境变量，工作目录是工作区根。
    pub fn stdio(command: impl Into<String>, args: Vec<String>) -> Self {
        Self::Stdio {
            command: command.into(),
            args,
            env: BTreeMap::new(),
            cwd: None,
        }
    }

    /// SSE 服务器：没有 `Authorization` 头时用缺省的 OAuth（ext.md §3.12.1；判定只在这里与 [`Self::with_oauth`]）。
    pub fn sse(url: impl Into<String>, headers: BTreeMap<String, String>) -> Self {
        let oauth = McpOAuth::applies_to(headers.keys()).then(McpOAuth::default);
        Self::Sse {
            url: url.into(),
            headers,
            oauth,
        }
    }

    /// streamable HTTP 服务器：OAuth 同 [`Self::sse`]。
    pub fn http(url: impl Into<String>, headers: BTreeMap<String, String>) -> Self {
        let oauth = McpOAuth::applies_to(headers.keys()).then(McpOAuth::default);
        Self::Http {
            url: url.into(),
            headers,
            oauth,
        }
    }

    /// 换成配置写的 OAuth（None：不用，例如 `client_credentials`）；配置了 `Authorization` 头时仍不用。stdio 不变。
    #[must_use]
    pub fn with_oauth(mut self, configured: Option<McpOAuth>) -> Self {
        if let Self::Sse { headers, oauth, .. } | Self::Http { headers, oauth, .. } = &mut self {
            *oauth = configured.filter(|_| McpOAuth::applies_to(headers.keys()));
        }
        self
    }

    /// 用 OAuth 时这个服务器的授权目标（ext.md §3.12.1）。
    pub fn auth_target(&self, server: &str) -> Option<McpAuthTarget> {
        match self {
            Self::Sse {
                url,
                oauth: Some(oauth),
                ..
            }
            | Self::Http {
                url,
                oauth: Some(oauth),
                ..
            } => Some(McpAuthTarget {
                server: server.to_owned(),
                url: url.clone(),
                oauth: oauth.clone(),
            }),
            _ => None,
        }
    }
}

/// 建立一条连接。`server` 只用于日志与错误文本。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpConnect {
    pub server: String,
    pub transport: McpTransportConfig,
}

/// 执行侧给出的连接 ID。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct McpConnectionId(String);

impl McpConnectionId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// 连接上的事件：收到的一条 JSON-RPC 消息，一个请求的回复不会来了，或连接已关闭（之后不再有事件）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpTransportEvent {
    Message {
        message: Value,
    },
    /// 这条连接上不会再有请求 `id`（发出的 JSON-RPC 请求 ID，原样）的回复，连接照常：传输明确知道它的流断了、续不上，
    /// 或 POST 的响应里没有回复（streamable HTTP，docs/specs/rust-cli/ext.md §3.10.6 的“续不上时”与“POST 的响应里没有
    /// 回复”）。等它的请求立即以 `reason` 失败。
    ReplyLost {
        id: Value,
        reason: String,
    },
    Closed {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        /// HTTP 传输上服务器说会话已过期（404 且带会话 ID）：重连后可以重试。
        #[serde(default)]
        session_expired: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum McpError {
    /// 进程起不来、连不上、HTTP 错误状态等。
    #[error("{message}")]
    Connect { message: String },
    /// 连接已关闭或不存在。
    #[error("connection closed: {message}")]
    Closed { message: String },
    #[error("{message}")]
    Io { message: String },
    #[error("not implemented: {what}")]
    Unsupported { what: String },
    /// 服务器要（重新）授权（OAuth，ext.md §3.12.3）：401 且令牌换不了，或 403 要更多权限（`scope` 是它要的）。
    #[error("{message}")]
    AuthRequired {
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scope: Option<String>,
    },
}

/// MCP 传输。实现方保证：一条连接的事件按到达顺序送出；`events` 只能取一次（取走缓冲的事件与之后的
/// 事件），再取得到空流；`close` 之后送出 `Closed` 并结束事件流。
#[async_trait]
pub trait McpTransport: Send + Sync {
    async fn connect(
        &self,
        workspace: &WorkspaceRef,
        request: McpConnect,
    ) -> Result<McpConnectionId, McpError>;

    /// 发一条 JSON-RPC 消息（请求、通知或对服务器请求的响应）。
    async fn send(&self, connection: &McpConnectionId, message: Value) -> Result<(), McpError>;

    fn events(&self, connection: &McpConnectionId) -> BoxStream<'static, McpTransportEvent>;

    async fn close(&self, connection: &McpConnectionId) -> Result<(), McpError>;
}

/// 没有 MCP 传输时的空实现：连接一律失败。
pub struct NoMcpTransport;

fn unsupported() -> McpError {
    McpError::Unsupported {
        what: "MCP transport".to_owned(),
    }
}

#[async_trait]
impl McpTransport for NoMcpTransport {
    async fn connect(
        &self,
        _workspace: &WorkspaceRef,
        _request: McpConnect,
    ) -> Result<McpConnectionId, McpError> {
        Err(unsupported())
    }

    async fn send(&self, _connection: &McpConnectionId, _message: Value) -> Result<(), McpError> {
        Err(unsupported())
    }

    fn events(&self, _connection: &McpConnectionId) -> BoxStream<'static, McpTransportEvent> {
        futures_util::stream::empty().boxed()
    }

    async fn close(&self, _connection: &McpConnectionId) -> Result<(), McpError> {
        Ok(())
    }
}

/// 工作区声明的 MCP 服务器的准入（App 形态的工作区 MCP 信任，docs/specs/rust-cli/ext.md §3.1 与 18-mcp-trust.md §12.4）：ext 的连接任务连一个
/// 项目级服务器之前问一次，连接任务之后的重连沿用这次结论。实现按将要执行的那份声明（组装时的快照）的 digest 查现在的信任记录；
/// 记录读不出时不准入（fail closed）。只有 App 的组装注入它；TUI 与 `-p` 没有，项目级服务器照常连（18 §10 审定第 4 条）。
#[async_trait]
pub trait McpAdmission: Send + Sync {
    async fn admits(&self, server: &str) -> bool;
}

/// MCP 工具的来源（服务器信息与 `alwaysLoad`）：延迟加载与规则匹配用。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpToolInfo {
    /// 配置里的服务器名（未规范化）。
    pub server: String,
    /// 服务器给出的工具原名。
    pub tool: String,
    /// 服务器配置的 `alwaysLoad` 或工具 `_meta` 里的不延迟标记为 true（docs/specs/rust-cli/ext.md §3.1）：不延迟加载。
    #[serde(default)]
    pub always_load: bool,
}

#[cfg(test)]
#[path = "mcp_tests.rs"]
mod tests;
