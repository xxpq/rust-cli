//! rmcp 与本工程传输端口之间的适配器（docs/specs/rust-cli/ext.md §3.11）：rmcp 的会话层经它发收消息，消息经 kernel-api 的
//! `McpTransport` 端口走本工程的传输（stdio 经 proc，HTTP / SSE 经 net，远端机器经执行器）。
//!
//! rmcp 3.9 的客户端有几处做法与以前修过的行为不同、又不能配置，在这里补（ext.md §3.11.2 的编号）：
//! - G2：先看 `initialize` 回复里的修订，不认时不交给 rmcp、结束事件流，rmcp 就不会发 `notifications/initialized`；
//! - G5：传输说回复不会来了（`ReplyLost`）时，给这个 ID 合成一条错误回复，原因记在连接的表里，包装层据此还原；
//! - G6、G7：对服务器请求的回答与关传输都交给后台、立即返回，rmcp 关闭时的排空没有可等的，等待方先失败；
//! - G9：发出之前删掉 rmcp 给每个请求加的 `_meta.progressToken`；
//! - G10：握手以外的回复原样交成 `CustomResult`，上层照旧拿 `Value`；
//! - G11：通知在读的一侧按到达顺序直接交给连接任务，不经 rmcp 的回调。
//!
//! 前端声明了的服务器询问（`elicitation/create`、`sampling/createMessage`）也在这里截下，交给 `asks.rs`，不交给 rmcp（ext.md §3.13.2）；
//! 服务器对它们的取消与网址模式的完成通知同样交过去。
//!
//! 读这条连接的事件只在 `receive` 里：拿到一个事件之后只做同步的事再返回，rmcp 在 `select!` 里丢下这个 future 时不丢事件。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use rmcp::model::{
    ClientJsonRpcMessage, ClientRequest, CustomRequest, CustomResult, ErrorCode, ErrorData,
    InitializeResult, JsonRpcMessage, PingRequest, ProtocolVersion, RequestId, ServerCapabilities,
    ServerJsonRpcMessage, ServerRequest, ServerResult,
};
use rmcp::service::RoleClient;
use rmcp::transport::Transport;
use rust_cli_base::Spawner;
use rust_cli_base::sync::LockUnpoisoned;
use rust_cli_kernel_api::mcp::{McpConnectionId, McpError, McpTransport, McpTransportEvent};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::asks::{self, Asks};
use super::notices::NoticeSender;
use super::rpc::Notice;

/// 事件流自己结束（没有 `Closed`）、或连接被放下时交给连接任务的原因。
const STREAM_ENDED: &str = "event stream ended";
/// rmcp 给每个请求加的进度令牌的键（G9）。
const PROGRESS_TOKEN: &str = "progressToken";
const META: &str = "_meta";
const PING: &str = "ping";

/// 发送失败：传输端口的错误原样（要授权的那一种由包装层认出，ext.md §3.12.3）。
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub(super) struct LinkError(pub(super) McpError);

impl From<McpError> for LinkError {
    fn from(error: McpError) -> Self {
        Self(error)
    }
}

/// 握手接受的结果：协商到的修订与 `initialize` 的原样回复（能力、instructions 由连接任务从这里取，G10）。
pub(super) struct Handshake {
    pub(super) revision: &'static ProtocolVersion,
    pub(super) result: Value,
}

/// 适配器与包装层（`rpc::Client`）共用的这条连接的状态。
#[derive(Default)]
pub(super) struct LinkState {
    /// 连接关了（事件流结束、关闭、放下）：后台的发送随之丢下，之后的请求立即失败。
    pub(super) closed: CancellationToken,
    /// 传输说回复不会来了的请求：ID → 传输给的原因（G5）。只由传输的 `ReplyLost` 写，服务器写不进来。
    lost: Mutex<HashMap<RequestId, String>>,
    /// 适配器对 `initialize` 回复的判断：接受的结果，或不接受、续不上的原因。
    handshake: Mutex<Option<Result<Handshake, String>>>,
}

impl LinkState {
    pub(super) fn take_lost(&self, id: &RequestId) -> Option<String> {
        self.lost.lock_unpoisoned().remove(id)
    }

    pub(super) fn take_handshake(&self) -> Option<Result<Handshake, String>> {
        self.handshake.lock_unpoisoned().take()
    }

    fn settle(&self, outcome: Result<Handshake, String>) {
        *self.handshake.lock_unpoisoned() = Some(outcome);
    }
}

/// 握手走到哪一步：握手中只有 `initialize` 在等回复。
enum Phase {
    Starting,
    Initializing(RequestId),
    Open,
}

/// 一个回复交给谁：握手中只有 `initialize` 在等，由适配器先看（G2）；之后交给 rmcp 的等待表（按 ID 配对，没有等待方的
/// 它丢掉）。
enum Waiter {
    Handshake,
    Session,
}

/// 读到一个事件之后怎么办。
enum Step {
    Deliver(Box<ServerJsonRpcMessage>),
    Skip,
    End(Notice),
}

/// 发一条消息：等传输发完（请求、通知），或交给后台（对服务器请求的回答，G6）。
enum Outgoing {
    Wait(Value),
    Detached(Value),
}

pub(super) struct Link {
    transport: Arc<dyn McpTransport>,
    connection: McpConnectionId,
    /// 关闭时先放下：传输关闭时要送出最后的 `Closed`，事件通道满着又没人读时它会一直等（§3.10.7）。
    events: Option<BoxStream<'static, McpTransportEvent>>,
    state: Arc<LinkState>,
    notices: NoticeSender,
    spawner: Spawner,
    /// 这个服务器可用的修订（§3.10.2）。
    revisions: &'static [ProtocolVersion],
    phase: Phase,
    /// 已经把关闭交给连接任务。
    noticed: bool,
    /// 已经在后台关传输。
    closing: bool,
    /// 前端声明了服务器询问时截下它们（ext.md §3.13.2）；什么都没声明时为 None。
    asks: Option<Arc<Asks>>,
}

impl Link {
    pub(super) fn new(
        transport: Arc<dyn McpTransport>,
        connection: McpConnectionId,
        spawner: Spawner,
        state: Arc<LinkState>,
        notices: NoticeSender,
        (revisions, asks): (&'static [ProtocolVersion], Option<Arc<Asks>>),
    ) -> Self {
        let events = transport.events(&connection);
        Self {
            transport,
            connection,
            events: Some(events),
            state,
            notices,
            spawner,
            revisions,
            phase: Phase::Starting,
            noticed: false,
            closing: false,
            asks,
        }
    }

    fn outgoing(&mut self, item: ClientJsonRpcMessage) -> Result<Outgoing, LinkError> {
        if let JsonRpcMessage::Request(request) = &item
            && matches!(request.request, ClientRequest::InitializeRequest(_))
        {
            self.phase = Phase::Initializing(request.id.clone());
        }
        let answer = matches!(item, JsonRpcMessage::Response(_) | JsonRpcMessage::Error(_));
        let mut message = serde_json::to_value(&item).map_err(|error| {
            LinkError(McpError::Io {
                message: error.to_string(),
            })
        })?;
        strip_progress_token(&mut message);
        Ok(if answer {
            Outgoing::Detached(message)
        } else {
            Outgoing::Wait(message)
        })
    }

    fn step(&mut self, event: Option<McpTransportEvent>) -> Step {
        match event {
            None => Step::End(Notice::Closed {
                reason: Some(STREAM_ENDED.to_owned()),
                session_expired: false,
            }),
            Some(McpTransportEvent::Closed {
                reason,
                session_expired,
            }) => Step::End(Notice::Closed {
                reason,
                session_expired,
            }),
            Some(McpTransportEvent::ReplyLost { id, reason }) => self.lost(&id, reason),
            Some(McpTransportEvent::Message { message }) => self.message(&message),
        }
    }

    fn message(&mut self, message: &Value) -> Step {
        let method = message.get("method").and_then(Value::as_str);
        let id = message.get("id").filter(|id| !id.is_null());
        match (method, id) {
            (None, Some(id)) => self.response(id, message),
            (Some(method), Some(id)) => match &self.asks {
                Some(asks) if asks.takes(method) => {
                    asks.start(id, method, message.get("params").cloned());
                    Step::Skip
                }
                _ => server_request(method, id, message),
            },
            (Some(method), None) => {
                self.notification(method, message.get("params"));
                Step::Skip
            }
            (None, None) => {
                tracing::debug!("ignored an MCP message without method or id");
                Step::Skip
            }
        }
    }

    /// 这个 ID 的回复交给谁；握手中别的 ID 没有等待方（rmcp 的握手会把它当成错误，以前是丢掉）。
    fn waiter(&self, id: &RequestId) -> Option<Waiter> {
        match &self.phase {
            Phase::Initializing(initialize) if initialize == id => Some(Waiter::Handshake),
            Phase::Open => Some(Waiter::Session),
            Phase::Starting | Phase::Initializing(_) => None,
        }
    }

    fn response(&mut self, id: &Value, message: &Value) -> Step {
        let Some(id) = own_id(id) else {
            tracing::debug!(%id, "MCP response for an unknown request");
            return Step::Skip;
        };
        match self.waiter(&id) {
            Some(Waiter::Handshake) if message.get("error").is_none() => {
                return self.handshake(id, message);
            }
            Some(_) => {}
            None => {
                tracing::debug!(%id, "MCP response for an unknown request");
                return Step::Skip;
            }
        }
        Step::Deliver(Box::new(match message.get("error") {
            Some(error) => JsonRpcMessage::error(error_data(error), Some(id)),
            // G10：原样交出，rmcp 不按类型解析（它会丢掉不认识的字段）。
            None => JsonRpcMessage::response(
                ServerResult::CustomResult(CustomResult(
                    message.get("result").cloned().unwrap_or(Value::Null),
                )),
                id,
            ),
        }))
    }

    /// `initialize` 的回复（G2）：修订不在可用修订里时不交给 rmcp，结束事件流，原因是 SDK 的同一句话（§3.10.2）。
    /// 接受时交给 rmcp 一个只带修订的 `InitializeResult`：rmcp 只用修订（旧纪元就按旧纪元处理之后的请求）；能力与
    /// instructions 由连接任务从原样的回复里取，服务器少给 `serverInfo` 一类字段时照样连上（以前就这样）。
    fn handshake(&mut self, id: RequestId, message: &Value) -> Step {
        let result = message.get("result").cloned().unwrap_or(Value::Null);
        let reply = result.get("protocolVersion").and_then(Value::as_str);
        let Some(revision) = self
            .revisions
            .iter()
            .find(|revision| Some(revision.as_str()) == reply)
        else {
            let reason = format!(
                "Server's protocol version is not supported: {}",
                reply.unwrap_or("none")
            );
            self.state.settle(Err(reason.clone()));
            return Step::End(Notice::Closed {
                reason: Some(reason),
                session_expired: false,
            });
        };
        self.state.settle(Ok(Handshake { revision, result }));
        self.phase = Phase::Open;
        let initialized = InitializeResult::new(ServerCapabilities::default())
            .with_protocol_version(revision.clone());
        Step::Deliver(Box::new(JsonRpcMessage::response(
            ServerResult::InitializeResult(initialized),
            id,
        )))
    }

    /// 传输说这个请求的回复不会来了（G5，§3.10.6 的“续不上时”）。rmcp 的传输接口没有这种事件：给这个 ID 合成一条错误
    /// 回复（同 rmcp 自己的 streamable HTTP 传输的做法），让等待方立即结束；原因记进表里，包装层认出它是传输说的。
    /// `initialize` 的回复续不上时握手以这个原因失败。
    fn lost(&mut self, id: &Value, reason: String) -> Step {
        let Some(id) = own_id(id) else {
            tracing::debug!(%id, "MCP reply lost for an unknown request");
            return Step::Skip;
        };
        match self.waiter(&id) {
            Some(Waiter::Handshake) => {
                self.state.settle(Err(reason.clone()));
                return Step::End(Notice::Closed {
                    reason: Some(reason),
                    session_expired: false,
                });
            }
            Some(Waiter::Session) => {}
            None => {
                tracing::debug!(%id, "MCP reply lost for an unknown request");
                return Step::Skip;
            }
        }
        self.state
            .lost
            .lock_unpoisoned()
            .insert(id.clone(), reason.clone());
        Step::Deliver(Box::new(JsonRpcMessage::error(
            ErrorData::new(ErrorCode::INTERNAL_ERROR, reason, None),
            Some(id),
        )))
    }

    /// G11：`list_changed` 按到达顺序交给连接任务（按种类合并，`notices.rs`）；服务器询问的取消与完成交给询问（ext.md §3.13.5）；
    /// 其余只进日志。
    fn notification(&self, method: &str, params: Option<&Value>) {
        let kind = method
            .strip_prefix("notifications/")
            .and_then(|rest| rest.strip_suffix("/list_changed"));
        match (kind, &self.asks) {
            (Some(kind), _) => self.notices.send(Notice::ListChanged(kind.to_owned())),
            (None, Some(asks)) if method == asks::CANCELLED => asks.cancelled(params),
            (None, Some(asks)) if method == asks::ELICITATION_COMPLETE => asks.completed(params),
            (None, _) => tracing::debug!(method, "MCP notification"),
        }
    }

    /// 连接结束：标记关闭，把原因交给连接任务（只一次）。
    fn finish(&mut self, notice: Notice) {
        self.state.closed.cancel();
        if !self.noticed {
            self.noticed = true;
            self.notices.send(notice);
        }
    }

    /// 放下事件流，在后台关传输（G7：rmcp 的等待表在它的循环结束时才释放，关传输不能挡在前面；stdio 的关闭要关 stdin、
    /// TERM 后 KILL 进程树）。传输的关闭是幂等的。
    fn shut(&mut self) {
        self.finish(Notice::Closed {
            reason: Some(STREAM_ENDED.to_owned()),
            session_expired: false,
        });
        self.events = None;
        if self.closing {
            return;
        }
        self.closing = true;
        let (transport, connection) = (self.transport.clone(), self.connection.clone());
        drop(self.spawner.spawn("mcp-close", async move {
            if let Err(error) = transport.close(&connection).await {
                tracing::debug!(%error, "closing an MCP connection failed");
            }
        }));
    }
}

impl Transport<RoleClient> for Link {
    type Error = LinkError;

    fn send(
        &mut self,
        item: ClientJsonRpcMessage,
    ) -> impl Future<Output = Result<(), LinkError>> + Send + 'static {
        let outgoing = self.outgoing(item);
        let (transport, connection) = (self.transport.clone(), self.connection.clone());
        let (spawner, closed) = (self.spawner.clone(), self.state.closed.clone());
        async move {
            match outgoing? {
                Outgoing::Wait(message) => Ok(transport.send(&connection, message).await?),
                Outgoing::Detached(message) => {
                    send_detached(&spawner, transport, connection, closed, message);
                    Ok(())
                }
            }
        }
    }

    async fn receive(&mut self) -> Option<ServerJsonRpcMessage> {
        loop {
            let event = self.events.as_mut()?.next().await;
            match self.step(event) {
                Step::Deliver(message) => return Some(*message),
                Step::Skip => {}
                Step::End(notice) => {
                    self.finish(notice);
                    self.events = None;
                    return None;
                }
            }
        }
    }

    fn close(&mut self) -> impl Future<Output = Result<(), LinkError>> + Send {
        self.shut();
        std::future::ready(Ok(()))
    }
}

/// 握手被丢弃（期限到、连接任务被取消）时 rmcp 不调 `close`，传输随握手的 future 一起放下：照样关连接（§3.10.7）。
impl Drop for Link {
    fn drop(&mut self) {
        self.shut();
    }
}

/// 服务器发来的请求：`ping` 交成 rmcp 认识的类型（握手中 rmcp 自己回答它）；其余交成 `CustomRequest`，由 `Answers`
/// 按方法名回答（G12）。ID 原样带回；不是整数或字符串的 ID（JSON-RPC 不许）没法回答，丢掉。
fn server_request(method: &str, id: &Value, message: &Value) -> Step {
    let Ok(id) = serde_json::from_value::<RequestId>(id.clone()) else {
        tracing::debug!(%id, method, "ignored an MCP server request with an invalid id");
        return Step::Skip;
    };
    let request = if method == PING {
        ServerRequest::PingRequest(PingRequest {
            method: Default::default(),
            extensions: Default::default(),
        })
    } else {
        ServerRequest::CustomRequest(CustomRequest::new(method, message.get("params").cloned()))
    };
    Step::Deliver(Box::new(JsonRpcMessage::request(request, id)))
}

/// 本客户端请求的 ID：rmcp 从 0 起分的整数。别的 ID（字符串、负数、小数）不是本客户端发的，没有等待方（同以前）。
fn own_id(id: &Value) -> Option<RequestId> {
    id.as_u64()
        .and_then(|id| i64::try_from(id).ok())
        .map(RequestId::Number)
}

/// 服务器的错误回复：没有错误码时是 0，没有文字时是 `Unknown error`（同以前）。JSON-RPC 的错误码在 32 位整数之内
/// （各 MCP SDK 都这样存），超出的按没有错误码。
fn error_data(error: &Value) -> ErrorData {
    let code = error
        .get("code")
        .and_then(Value::as_i64)
        .and_then(|code| i32::try_from(code).ok())
        .unwrap_or_default();
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("Unknown error")
        .to_owned();
    ErrorData::new(ErrorCode(code), message, None)
}

/// G9：rmcp 给经它的 Peer 发的每个请求加 `_meta.progressToken`，没有开关。本客户端不处理进度，以前也不带：删掉它，
/// `_meta` 空了就整个去掉。
fn strip_progress_token(message: &mut Value) {
    let Some(params) = message.get_mut("params").and_then(Value::as_object_mut) else {
        return;
    };
    let Some(meta) = params.get_mut(META).and_then(Value::as_object_mut) else {
        return;
    };
    meta.shift_remove(PROGRESS_TOKEN);
    if meta.is_empty() {
        params.shift_remove(META);
    }
}

/// 在后台发一条不等结果的消息（G6：对服务器请求的回答）。尽力而为：失败只记 debug，不重试；连接关了时丢下还没完成的
/// 发送（§3.10.11）。
pub(super) fn send_detached(
    spawner: &Spawner,
    transport: Arc<dyn McpTransport>,
    connection: McpConnectionId,
    closed: CancellationToken,
    message: Value,
) {
    drop(spawner.spawn("mcp-send", async move {
        let sent = closed
            .run_until_cancelled(transport.send(&connection, message))
            .await;
        if let Some(Err(error)) = sent {
            tracing::debug!(%error, "could not send an answer to an MCP server request");
        }
    }));
}

#[cfg(test)]
#[path = "link_tests.rs"]
mod tests;
