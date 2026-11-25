//! 一个服务器的连接与握手、清单拉取（docs/specs/rust-cli/ext.md §3.3）：工具、资源与 prompt 命令。

use std::sync::Arc;
use std::time::Duration;

use rmcp::model::ProtocolVersion;
use rust_cli_base::Spawner;
use rust_cli_kernel_api::env::WorkspaceRef;
use rust_cli_kernel_api::env_registry::EnvironmentRegistry;
use rust_cli_kernel_api::ids::MachineName;
use rust_cli_kernel_api::mcp::{McpConnect, McpTransport};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::McpServer;
use super::config::McpProtocol;
use super::rpc::{Client, Refusal, RpcError};

/// 本客户端认的 MCP 修订（docs/specs/rust-cli/ext.md §3.10.2），都用 `initialize` 握手，新的在前。SDK 的表里还有发布前
/// 的草稿 `2024-10-07`，不认；rmcp 认的新纪元 `2026-07-28` 没有 `initialize`，本客户端还不会（§3.10.4 第 3 项）。
///
/// 三种传输都可用（v5r-mcp2 线）：streamable HTTP 以前只到 `2025-06-18`，因为 `2025-11-25` 起服务器可以在回复之前断开
/// SSE 流（SEP-1699），而传输那时不会续读；传输补上续读之后（§3.10.6）放开。
pub(super) static REVISIONS: [ProtocolVersion; 4] = [
    ProtocolVersion::V_2025_11_25,
    ProtocolVersion::V_2025_06_18,
    ProtocolVersion::V_2025_03_26,
    ProtocolVersion::V_2024_11_05,
];
/// 新纪元的第一个修订（没有 `initialize`，§3.10.1）：Node 配置的 `protocolVersion` 可以钉死它，本客户端还不会。
pub(crate) const MODERN_REVISION: &str = "2026-07-28";
/// 服务器 instructions 与工具描述的上限。
pub(crate) const MAX_DESCRIPTION: usize = 2048;
/// 清单翻页的上限（防止服务器一直给 cursor）。
const MAX_PAGES: usize = 100;

/// 握手的结果。
pub(crate) struct Session {
    pub client: Arc<Client>,
    pub notices: super::notices::NoticeReceiver,
    pub capabilities: Value,
    pub instructions: Option<String>,
    /// 这条连接协商到的修订（只属于这条连接，重连时重新协商）。
    pub protocol_version: &'static str,
}

/// 这个服务器可用的修订，新的在前（§3.10.2：本客户端认的、配置允许的）；`Err` 是配置要的修订本客户端不会，不连接。
fn revisions(server: &McpServer) -> Result<&'static [ProtocolVersion], String> {
    if server.options.protocol == McpProtocol::Modern {
        return Err(format!(
            "protocolVersion \"{MODERN_REVISION}\" requires the {MODERN_REVISION} protocol revision, which this client does not support yet"
        ));
    }
    Ok(&REVISIONS)
}

/// 超过 2048 字符时截断加 `… [truncated]`。
pub(crate) fn truncate_description(text: &str) -> String {
    if text.chars().count() > MAX_DESCRIPTION {
        let mut cut: String = text.chars().take(MAX_DESCRIPTION).collect();
        cut.push_str("… [truncated]");
        cut
    } else {
        text.to_owned()
    }
}

/// 服务器的管道开在哪里（R5，remote-exec.md §22.6）：本机的传输与会话的工作区，或者服务器绑定的那台机器的。
pub(crate) struct Pipe<'a> {
    pub local: &'a Arc<dyn McpTransport>,
    /// 远端机器的注册表；没有时（只给测试的组装）绑定到远端的服务器都连不上。
    pub machines: Option<&'a Arc<dyn EnvironmentRegistry>>,
    pub workspace: &'a WorkspaceRef,
}

impl Pipe<'_> {
    /// 这个服务器的传输与工作区：本机；或者那台机器的 `mcp()` 与它的工作区（stdio 的相对 `cwd`、`roots/list` 都按它）。
    /// 那台机器正在连时等它连完（调用方的握手期限之内）；不在清单里、没在连或连不上时是 R3 的“没连上”。
    async fn resolve(
        &self,
        machine: &MachineName,
    ) -> Result<(Arc<dyn McpTransport>, WorkspaceRef), String> {
        let Some(name) = machine.remote() else {
            return Ok((self.local.clone(), self.workspace.clone()));
        };
        let found = match self.machines {
            Some(registry) => registry.wait_remote(machine).await,
            None => None,
        };
        found
            .map(|(machine, workspace)| (machine.mcp().clone(), workspace))
            .ok_or_else(|| rust_cli_kernel_api::machine::not_connected(name))
    }
}

/// 连接并握手；整个过程受 `MCP_TIMEOUT` 限制（绑定到远端的服务器等它的机器连上也算在里面）。`client_version` 是入口
/// 给的本工程版本（`clientInfo`），`asking` 是问用户的端口（前端声明的询问，§3.13）。握手发可用修订的第一项，服务器回的修订
/// 不在可用修订里时断开（§3.10.2）。
pub(crate) async fn open(
    pipe: &Pipe<'_>,
    spawner: &Spawner,
    server: &McpServer,
    timeout: Duration,
    (client_version, asking): (&str, Option<&Arc<super::asks::AskPorts>>),
) -> Result<Session, Refusal> {
    let revisions = revisions(server)?;
    let handshake = async {
        let (transport, workspace) = pipe.resolve(&server.machine).await?;
        let transport = &transport;
        let workspace = &workspace;
        let connection = transport
            .connect(
                workspace,
                McpConnect {
                    server: server.name.clone(),
                    transport: server.transport.clone(),
                },
            )
            .await
            .map_err(Refusal::of)?;
        let roots = vec![workspace.root.as_str().to_owned()];
        // 出错返回时不用显式关闭：适配器随握手放下，关闭连接（§3.10.7；握手被丢弃时同样）。`initialize` 没有自己的期限
        // （§3.10.8），到期只走下面整个握手的期限。回的修订不认时适配器不让 rmcp 发 initialized（§3.10.2、§3.11 的 G2）。
        let opened = Client::connect(
            transport.clone(),
            connection,
            spawner,
            roots,
            revisions,
            (
                client_version,
                asking.map(|ports| (ports, server.name.as_str())),
            ),
        )
        .await?;
        let instructions = opened
            .result
            .get("instructions")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .map(truncate_description);
        Ok(Session {
            client: opened.client,
            notices: opened.notices,
            capabilities: opened
                .result
                .get("capabilities")
                .cloned()
                .unwrap_or(Value::Null),
            instructions,
            protocol_version: opened.protocol_version,
        })
    };
    // 握手只有这一个期限（§3.10.8）：到了丢弃握手，`client` 随之释放、连接关闭，不发取消。
    match tokio::time::timeout(timeout, handshake).await {
        Ok(result) => result,
        Err(_) => Err(Refusal::Failed(format!(
            "MCP server \"{}\" connection timed out after {}ms",
            server.name,
            timeout.as_millis()
        ))),
    }
}

/// `tools/list` 一类的清单，按 `nextCursor` 翻页，取 `key` 下的数组。
pub(crate) async fn list(
    client: &Client,
    method: &str,
    key: &str,
    timeout: Duration,
) -> Result<Vec<Value>, RpcError> {
    let never = CancellationToken::new();
    let mut items = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let params = match &cursor {
            Some(cursor) => json!({"cursor": cursor}),
            None => json!({}),
        };
        let page = client.request(method, params, timeout, &never).await?;
        if let Some(found) = page.get(key).and_then(Value::as_array) {
            items.extend(found.iter().cloned());
        }
        cursor = page
            .get("nextCursor")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }
    Ok(items)
}

pub(crate) fn has_capability(capabilities: &Value, name: &str) -> bool {
    capabilities.get(name).is_some_and(|value| !value.is_null())
}

#[cfg(test)]
#[path = "connection_tests.rs"]
mod tests;
