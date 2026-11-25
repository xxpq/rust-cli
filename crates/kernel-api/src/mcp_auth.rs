//! 远程（HTTP / SSE）MCP 服务器的 OAuth（docs/specs/rust-cli/ext.md §3.12）：配置、要授权的目标与两个端口。
//!
//! - [`McpTokens`]：每个请求的令牌、401 之后刷新。只有本机的 MCP 传输用它（令牌属于控制面，执行器的传输没有，§3.12.8）。
//! - [`McpAuthorizer`]：用户发起的交互授权、Sign out、有没有存下的令牌。ext 用它（`Kernel::mcp_sign_in`、`Kernel::mcp_sign_out`）。
//!
//! 两个端口的本机实现都在 platform（令牌在 `credentials.json`，与 Node 同一份）；内核不知道令牌放在哪里、怎样换来。

use async_trait::async_trait;
use rust_cli_base::proto::http::header::AUTHORIZATION;
use rust_cli_base::secret::RedactedString;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};

use crate::mcp::McpError;

/// HTTP / SSE 服务器的 OAuth 配置（Node 配置 `oauth` 的 `authorization_code`；没写 `oauth` 时是全空的缺省）。
/// 字段决定令牌存放的键（`clientId`、`scope`、`redirectPath`，ext.md §3.12.4），变了就要重新授权。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpOAuth {
    /// 预先注册的 client；没有时每次授权动态注册（RFC 7591）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<RedactedString>,
    /// 注册时的 `client_name`；没有时是 `ZCode <服务器名>`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// 本机回调的路径；没有时 `/oauth/callback/mcp/<服务器名>`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redirect_path: Option<String>,
    /// 固定的回调端口（`mcpServers` 写法的 `callbackPort`，ext.md §3.12.15）：有时回调地址是 `http://localhost:<端口>/callback`。不进令牌的键。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub callback_port: Option<u16>,
    /// 授权服务器元数据的地址（`mcpServers` 写法的 `authServerMetadataUrl`，https）：有时不做受保护资源与按路径的发现，只取它。不进令牌的键。
    #[serde(
        default,
        rename = "authServerMetadataUrl",
        skip_serializing_if = "Option::is_none"
    )]
    pub metadata_url: Option<String>,
}

impl McpOAuth {
    /// 这组 header 下用不用 OAuth：有 `Authorization`（不分大小写）时不用，头原样发出（只在没有静态凭据时走 OAuth，ext.md
    /// §3.12.1）。
    pub fn applies_to<'a>(headers: impl IntoIterator<Item = &'a String>) -> bool {
        !headers
            .into_iter()
            .any(|name| name.eq_ignore_ascii_case(AUTHORIZATION))
    }
}

/// 一个要授权的服务器：令牌按它找（名字、URL 与 [`McpOAuth`] 决定存放的键）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpAuthTarget {
    /// 配置里的服务器名（未规范化）。
    pub server: String,
    pub url: String,
    pub oauth: McpOAuth,
}

/// 每个请求的令牌（ext.md §3.12.3）。
#[async_trait]
pub trait McpTokens: Send + Sync {
    /// 发请求用的 access token：没有存过时 None（照发，等 401）；快过期且有 refresh token 时先刷新，刷新的网络失败时照用旧的。
    async fn token(&self, target: &McpAuthTarget) -> Result<Option<RedactedString>, McpError>;

    /// 服务器拒绝了 `rejected`（401）：在共享的刷新锁里换一个。存下的已经不是 `rejected` 时直接给它；不能换（没有 refresh
    /// token、`invalid_grant`）时 [`McpError::AuthRequired`]；刷新的网络失败是 [`McpError::Connect`]（临时错误）。
    async fn refreshed(
        &self,
        target: &McpAuthTarget,
        rejected: Option<&RedactedString>,
    ) -> Result<RedactedString, McpError>;
}

/// 用户发起的交互授权（ext.md §3.12.6）。
#[async_trait]
pub trait McpAuthorizer: Send + Sync {
    /// 发现、注册、开本机回调的监听，给出授权地址。`scope` 是服务器上次要的更多权限（403 的 step-up），并进请求的 scope。
    async fn begin(
        &self,
        target: &McpAuthTarget,
        scope: Option<&str>,
    ) -> Result<Box<dyn McpPendingSignIn>, McpError>;

    /// 删掉这个服务器存下的令牌与发现的结果（不去服务器上撤销）。
    async fn sign_out(&self, target: &McpAuthTarget) -> Result<(), McpError>;

    /// 存下了令牌（只看键在不在，不解密）。
    async fn signed_in(&self, target: &McpAuthTarget) -> bool;
}

/// 进行中的一次授权：等浏览器回来、换令牌、存下。放下它就关掉本机的监听。
#[async_trait]
pub trait McpPendingSignIn: Send {
    fn url(&self) -> &str;

    /// 等回调（有上限）、换令牌、发布。`pasted` 是用户贴回来的回调地址（授权伪工具 `complete_authentication`，ext.md §3.12.16）：
    /// 与本机的回调同一套规则，每一个当场回答收不收（state 是这一次的、带 `code` 或 `error`）；收下的就当作回调。
    async fn finish(
        self: Box<Self>,
        pasted: mpsc::Receiver<PastedCallback>,
    ) -> Result<(), McpError>;
}

/// 用户贴回来的一个回调地址与「收不收」的回答口。
#[derive(Debug)]
pub struct PastedCallback {
    pub url: String,
    pub accepted: oneshot::Sender<bool>,
}

#[cfg(test)]
#[path = "mcp_auth_tests.rs"]
mod tests;
