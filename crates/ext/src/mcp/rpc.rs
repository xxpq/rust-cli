//! MCP 的 JSON-RPC 客户端（docs/specs/rust-cli/ext.md §3.3、§3.11）：会话层是官方 Rust SDK rmcp（请求 ID、等待表、服务器
//! 请求与通知的分派、握手的先后），消息经适配器（`link.rs`）走本工程的传输端口。这里只留 rmcp 做不到的部分：
//! - 期限与用户中止在这一层，盖住发送（§3.10.10）：rmcp 自带的期限到了先等取消通知发完才返回（§3.11 的 G3）；
//! - 结局定了之后取消在后台发，结果不等它（§3.10.9）；
//! - 传输给的“回复不会来了”的原因从适配器的表里还原（§3.10.6）。
//!
//! 连接归 `Client` 所有（§3.10.7）：它持有 rmcp 的 `RunningService`，最后的句柄放下时 rmcp 的循环结束、关传输。

use std::sync::Arc;
use std::time::Duration;

use rmcp::model::{
    CancelledNotification, CancelledNotificationParam, ClientRequest, CustomRequest, ErrorCode,
    ProtocolVersion, RequestId,
};
use rmcp::service::{
    ClientInitializeError, PeerRequestOptions, RoleClient, RunningService, ServiceError,
    serve_client,
};
use rust_cli_base::Spawner;
use rust_cli_kernel_api::mcp::{McpConnectionId, McpError, McpTransport};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::answers::Answers;
use super::asks::{AskPorts, Asks, Caller, Registered};
use super::link::{Link, LinkError, LinkState};
use super::notices::{self, NoticeReceiver};

/// JSON-RPC 的“方法不存在”错误码。
pub(super) const METHOD_NOT_FOUND: i64 = ErrorCode::METHOD_NOT_FOUND.0 as i64;
/// 连接关了时请求的失败原因。
const CLOSED: &str = "connection closed";
/// `notifications/cancelled` 的 `reason`：用户中止、到期限、回复不会来了（ext.md §3.3、§3.10.6）。
const CANCEL_INTERRUPTED: &str = "User interrupted";
const CANCEL_TIMED_OUT: &str = "Request timed out";
const CANCEL_REPLY_LOST: &str = "Response stream lost";

/// 一次请求的失败。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum RpcError {
    /// 服务器回了 JSON-RPC 错误。
    #[error("MCP error {code}: {message}")]
    Server { code: i64, message: String },
    #[error("{0}")]
    Closed(String),
    #[error("timed out")]
    TimedOut,
    #[error("cancelled")]
    Cancelled,
    #[error("{0}")]
    Send(String),
    /// 传输说这个请求的回复不会来了（`ReplyLost`，流续不上，ext.md §3.10.6）：文字是传输给的原因。
    #[error("{0}")]
    ReplyLost(String),
    /// 服务器要（重新）授权（OAuth，ext.md §3.12.3）：传输换不来能用的令牌；`scope` 是 403 时服务器要的更多权限。
    #[error("{message}")]
    AuthRequired {
        message: String,
        scope: Option<String>,
    },
}

/// 握手没成：要授权的单列（连接任务记成 NeedsAuth，ext.md §3.12.3），其余是原因的文字。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Refusal {
    AuthRequired { scope: Option<String> },
    Failed(String),
}

impl From<String> for Refusal {
    fn from(reason: String) -> Self {
        Self::Failed(reason)
    }
}

impl Refusal {
    /// 传输端口的错误：要授权的那一种原样认出。
    pub(crate) fn of(error: McpError) -> Self {
        match error {
            McpError::AuthRequired { scope, .. } => Self::AuthRequired { scope },
            other => Self::Failed(other.to_string()),
        }
    }
}

/// 经 rmcp 交回的发送失败：适配器交给它的是 [`LinkError`]，认出要授权的那一种（ext.md §3.12.3）。
fn sent_error(error: &rmcp::transport::DynamicTransportError) -> RpcError {
    match error.error.downcast_ref::<LinkError>() {
        Some(LinkError(McpError::AuthRequired { message, scope })) => RpcError::AuthRequired {
            message: message.clone(),
            scope: scope.clone(),
        },
        _ => RpcError::Send(error.error.to_string()),
    }
}

/// 交给连接任务的事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Notice {
    /// `notifications/<kind>/list_changed`：`tools`、`resources`、`prompts`。
    ListChanged(String),
    Closed {
        reason: Option<String>,
        session_expired: bool,
    },
}

/// 握手成功：客户端、连接任务收通知的一侧、协商到的修订与 `initialize` 的原样回复。
pub(crate) struct Opened {
    pub client: Arc<Client>,
    pub notices: NoticeReceiver,
    pub protocol_version: &'static str,
    pub result: Value,
}

pub(crate) struct Client {
    service: RunningService<RoleClient, Answers>,
    state: Arc<LinkState>,
    transport: Arc<dyn McpTransport>,
    connection: McpConnectionId,
    /// 取消在这里后台发（§3.10.9）。
    spawner: Spawner,
    /// 前端声明了服务器询问时这条连接的询问（ext.md §3.13.2）：在途请求的调用方在这里登记。
    asks: Option<Arc<Asks>>,
}

impl Client {
    /// 在已经建好的连接上握手（rmcp 的旧纪元握手：`initialize`、`notifications/initialized`），发 `revisions` 的第一项；
    /// `roots` 是 `roots/list` 的回答（工作区根），`version` 是本工程的版本（`clientInfo`）。`asking` 是这套内核问用户的端口与
    /// 服务器名（前端声明了询问时，ext.md §3.13）。`initialize` 没有自己的期限，也不取消（§3.10.8）：期限只有调用方整个握手的一个，
    /// 到了丢弃这个 future，适配器随之放下、关闭连接（§3.10.7）。
    pub(crate) async fn connect(
        transport: Arc<dyn McpTransport>,
        connection: McpConnectionId,
        spawner: &Spawner,
        roots: Vec<String>,
        revisions: &'static [ProtocolVersion],
        (version, asking): (&str, Option<(&Arc<AskPorts>, &str)>),
    ) -> Result<Opened, Refusal> {
        let state = Arc::new(LinkState::default());
        let (notices, receiver) = notices::channel();
        let asks = asking.map(|(ports, server)| {
            Asks::new(
                ports.clone(),
                server.to_owned(),
                transport.clone(),
                (connection.clone(), spawner.clone(), state.closed.clone()),
            )
        });
        let declared = asking
            .map(|(ports, _)| ports.capabilities())
            .unwrap_or_default();
        let link = Link::new(
            transport.clone(),
            connection.clone(),
            spawner.clone(),
            state.clone(),
            notices,
            (revisions, asks.clone()),
        );
        let answers = Answers::new(roots, &revisions[0], version, declared);
        let started = serve_client(answers, link).await;
        // 握手的结局以适配器的判断为准：它看过 `initialize` 的回复（接受的修订与原样的结果，或不认的修订、续不上的原因）。
        // 它没有判断时，rmcp 的错误就是原因（发送失败、连接关了、服务器回错误）。
        let (service, accepted) = match (started, state.take_handshake()) {
            (Ok(service), Some(Ok(accepted))) => (service, accepted),
            (_, Some(Err(reason))) => return Err(Refusal::Failed(reason)),
            (Err(error), _) => return Err(handshake_error(error)),
            // rmcp 只认适配器交来的 `InitializeResult`，适配器交之前已经记下：握手成功时一定有记录。
            (Ok(_), None) => return Err(Refusal::Failed(CLOSED.to_owned())),
        };
        Ok(Opened {
            client: Arc::new(Self {
                service,
                state,
                transport,
                connection,
                spawner: spawner.clone(),
                asks,
            }),
            notices: receiver,
            protocol_version: accepted.revision.as_str(),
            result: accepted.result,
        })
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.state.closed.is_cancelled()
    }

    /// 发一个请求并等它的结果；超时、取消或回复不会来了（`ReplyLost`）时发 `notifications/cancelled`：连接还在，服务器
    /// 还当请求在跑（ext.md §3.10.6 的“续不上时”）。取消在后台发，结果不等它（§3.10.9）。
    ///
    /// 期限从调用起算，盖住交给 rmcp 与传输的发送（§3.10.10）。不用 rmcp 自带的期限：它到期时先等取消通知发完才返回，
    /// 取消的 POST 卡住时请求一直不返回（§3.11 的 G3，就是 v5r-mcp4 线修掉的缺陷）。
    pub(crate) async fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
        cancel: &CancellationToken,
    ) -> Result<Value, RpcError> {
        if self.is_closed() {
            return Err(RpcError::Closed(CLOSED.to_owned()));
        }
        let mut sent = None;
        let outcome = tokio::select! {
            outcome = self.exchange(method, params, &mut sent) => outcome,
            () = tokio::time::sleep(timeout) => Err(RpcError::TimedOut),
            () = cancel.cancelled() => Err(RpcError::Cancelled),
        };
        let reason = match &outcome {
            Err(RpcError::Cancelled) => CANCEL_INTERRUPTED,
            Err(RpcError::TimedOut) => CANCEL_TIMED_OUT,
            Err(RpcError::ReplyLost(_)) => CANCEL_REPLY_LOST,
            _ => return outcome,
        };
        // 还没交给 rmcp（没有 ID）时服务器不知道这个请求，不用取消。
        if let Some(id) = sent {
            self.cancel(id, reason);
        }
        outcome
    }

    /// 为一次工具调用发请求（[`Self::request`]）：结局定之前，这条连接上服务器的询问挂在这个调用上（ext.md §3.13.2）。
    pub(crate) async fn request_for(
        &self,
        caller: Caller,
        method: &str,
        params: Value,
        (timeout, cancel): (Duration, &CancellationToken),
    ) -> Result<Value, RpcError> {
        let _registered: Option<Registered> = self.asks.as_ref().map(|asks| asks.register(caller));
        self.request(method, params, timeout, cancel).await
    }

    /// 交给 rmcp 发出（它分 ID、登记等待方，发送在它的循环里并发跑），等回复。回复先于发送结束到达时 rmcp 立即交回，
    /// 发送照样跑到结束（§3.11 的 G4）。请求都发成 `CustomRequest`、回复原样交回（G10）：结果的转换在上层。
    async fn exchange(
        &self,
        method: &str,
        params: Value,
        sent: &mut Option<RequestId>,
    ) -> Result<Value, RpcError> {
        let request = ClientRequest::CustomRequest(CustomRequest::new(method, Some(params)));
        let handle = self
            .service
            .peer()
            .send_request_with_option(request, PeerRequestOptions::no_options())
            .await
            .map_err(|error| self.failure(None, error))?;
        let id = handle.id.clone();
        *sent = Some(id.clone());
        match handle.await_response().await {
            // 适配器把回复交成 `CustomResult`，它按原样序列化。
            Ok(result) => {
                serde_json::to_value(result).map_err(|error| RpcError::Send(error.to_string()))
            }
            Err(error) => Err(self.failure(Some(&id), error)),
        }
    }

    fn failure(&self, id: Option<&RequestId>, error: ServiceError) -> RpcError {
        match error {
            // 适配器为 `ReplyLost` 合成的错误回复：原因在它的表里（服务器写不进这张表）。
            ServiceError::McpError(error) => match id.and_then(|id| self.state.take_lost(id)) {
                Some(reason) => RpcError::ReplyLost(reason),
                None => RpcError::Server {
                    code: i64::from(error.code.0),
                    message: error.message.into_owned(),
                },
            },
            ServiceError::TransportSend(error) => sent_error(&error),
            ServiceError::TransportClosed => RpcError::Closed(CLOSED.to_owned()),
            // 本工程不用 rmcp 的期限、订阅与 MRTR，其余的错误照原文报出。
            other => RpcError::Send(other.to_string()),
        }
    }

    /// 在后台告诉服务器不用再做请求 `id`（ext.md §3.10.9）。尽力而为：失败只记 debug，不重试；连接关了时丢下还没完成
    /// 的发送。任务只拿 rmcp 的 Peer（不持有 `RunningService`），不推迟连接的释放（§3.10.7）。经 rmcp 发出：它发完之后
    /// 从自己的等待表里去掉这个请求。
    fn cancel(&self, id: RequestId, reason: &'static str) {
        let peer = self.service.peer().clone();
        let closed = self.state.closed.clone();
        drop(self.spawner.spawn("mcp-cancel", async move {
            let notification = CancelledNotification::new(CancelledNotificationParam::new(
                Some(id),
                Some(reason.to_owned()),
            ));
            let sent = closed
                .run_until_cancelled(peer.send_notification(notification.into()))
                .await;
            if let Some(Err(error)) = sent {
                tracing::debug!(%error, "could not send an MCP cancel");
            }
        }));
    }

    /// 显式关闭（`/mcp reconnect`、停用、会话释放、退出）：停掉 rmcp 的循环（等待中的请求随之失败），等传输关完
    /// （stdio 的进程树结束）。
    pub(crate) async fn close(&self) {
        self.state.closed.cancel();
        self.service.cancellation_token().cancel();
        if let Err(error) = self.transport.close(&self.connection).await {
            tracing::debug!(%error, "closing an MCP connection failed");
        }
    }
}

/// 握手失败的原因，照以前的写法：服务器的错误是 `MCP error <码>: <文字>`，发送失败是传输的原文，连接关了是
/// `connection closed`。
fn handshake_error(error: ClientInitializeError) -> Refusal {
    match error {
        ClientInitializeError::JsonRpcError(error) => Refusal::Failed(
            RpcError::Server {
                code: i64::from(error.code.0),
                message: error.message.into_owned(),
            }
            .to_string(),
        ),
        // `initialize` 的 POST 被拒且令牌换不来（OAuth，ext.md §3.12.3）。
        ClientInitializeError::TransportError { error, .. } => match sent_error(&error) {
            RpcError::AuthRequired { scope, .. } => Refusal::AuthRequired { scope },
            other => Refusal::Failed(other.to_string()),
        },
        ClientInitializeError::ConnectionClosed(_) => Refusal::Failed(CLOSED.to_owned()),
        other => Refusal::Failed(other.to_string()),
    }
}

#[cfg(test)]
#[path = "rpc_tests.rs"]
mod tests;
