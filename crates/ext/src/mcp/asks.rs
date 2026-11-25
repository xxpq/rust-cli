//! 服务器的询问（docs/specs/rust-cli/ext.md §3.13.2、§3.14）：`elicitation/create` 与 `sampling/createMessage` 在适配器（`link.rs`）
//! 读的一侧截下，不交给 rmcp；每个请求一个任务（本工程的 Spawner，带这条连接的关闭信号），挂在这条连接上在途的调用上问用户，回答在
//! 后台发。
//!
//! 为什么不交给 rmcp 的 `Service`：rmcp 关连接时先排空在跑的服务器请求处理函数（服务器断开最多 5 s、句柄放下最多 2 s），才取消它们、
//! 释放等待表。等用户回答的处理函数会把关闭与等待方的失败推迟到这个期限（§3.11 的 G6、G7 补过的缺陷）。

use rust_cli_base::Spawner;
use rust_cli_base::sync::LockUnpoisoned;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use rmcp::model::{
    ClientJsonRpcMessage, ClientResult, ElicitationCapability, ErrorData,
    FormElicitationCapability, JsonRpcMessage, RequestId, SamplingCapability,
    UrlElicitationCapability,
};
use rust_cli_kernel_api::clock::Clock;
use rust_cli_kernel_api::default_model::DefaultModel;
use rust_cli_kernel_api::env::WorkspaceRef;
use rust_cli_kernel_api::hooks::{HookContext, HookEvent, HookEventName, HookRunner};
use rust_cli_kernel_api::ids::{AgentId, RunId, SessionId, ToolUseId};
use rust_cli_kernel_api::interaction::Answer;
use rust_cli_kernel_api::mcp::{McpConnectionId, McpTransport};
use rust_cli_kernel_api::model::ModelPort;
use rust_cli_kernel_api::orchestration::{InteractionRequest, Interactions, SessionOps};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::side_request::Requester;
use rust_cli_kernel_api::tool::ToolUseContext;
use serde_json::{Value, json};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use super::config::McpClientTraits;
use super::link::send_detached;
use super::sampling::Grants;

pub(super) const ELICITATION_CREATE: &str = "elicitation/create";
pub(super) const SAMPLING_CREATE: &str = "sampling/createMessage";
/// 服务器取消它发出的请求（MCP 的取消通知）。
pub(super) const CANCELLED: &str = "notifications/cancelled";
/// 网址模式的完成通知（MCP `2025-11-25`）。
pub(super) const ELICITATION_COMPLETE: &str = "notifications/elicitation/complete";

/// 这套内核问用户要用的端口与前端声明的能力（ext.md §3.13.1）：所有连接共用一份（兄弟管理器也共用），入口组装时给。
pub struct AskPorts {
    traits: McpClientTraits,
    pub(super) hooks: Arc<dyn HookRunner>,
    pub(super) model: Arc<dyn ModelPort>,
    /// 去处的会话给不出主模型时用它（同 WebFetch 的旁路请求）。
    pub(super) default_model: DefaultModel,
    pub(super) clock: Arc<dyn Clock>,
    /// sampling 的「本会话一直允许」与频率（§3.14.1）：唯一的写者。
    pub(super) grants: Grants,
}

impl AskPorts {
    pub fn new(
        traits: McpClientTraits,
        hooks: Arc<dyn HookRunner>,
        model: Arc<dyn ModelPort>,
        default_model: DefaultModel,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            traits,
            hooks,
            model,
            default_model,
            clock,
            grants: Grants::default(),
        }
    }

    pub(super) fn traits(&self) -> McpClientTraits {
        self.traits
    }

    /// `initialize` 声明的两项（ext.md §3.13.1）：表单与网址各自声明；sampling 不带 `tools` 与 `context`。
    pub(super) fn capabilities(
        &self,
    ) -> (Option<ElicitationCapability>, Option<SamplingCapability>) {
        let McpClientTraits {
            elicitation_form: form,
            elicitation_url: url,
            sampling,
        } = self.traits;
        let elicitation = (form || url).then(|| {
            let mut capability = ElicitationCapability::new();
            if form {
                capability = capability.with_form(FormElicitationCapability::default());
            }
            if url {
                capability = capability.with_url(UrlElicitationCapability::default());
            }
            capability
        });
        (elicitation, sampling.then(SamplingCapability::default))
    }

    /// 截下这个方法：声明了它（没声明的照旧交给 rmcp，回 Method not found）。
    fn takes(&self, method: &str) -> bool {
        let McpClientTraits {
            elicitation_form,
            elicitation_url,
            sampling,
        } = self.traits;
        match method {
            ELICITATION_CREATE => elicitation_form || elicitation_url,
            SAMPLING_CREATE => sampling,
            _ => false,
        }
    }

    /// 会话释放：它的 sampling 记录一起去掉（§3.14.1）。
    pub fn released(&self, session: &SessionId) {
        self.grants.released(session);
    }
}

/// 询问挂在哪个调用上（ext.md §3.13.2）：在途请求的工具调用上下文里问用户、跑 hooks 要用的部分。
pub(crate) struct Caller {
    pub(super) tool_use: ToolUseId,
    pub(super) interactions: Arc<dyn Interactions>,
    pub(super) session_ops: Arc<dyn SessionOps>,
    /// 调用被取消（停下回合、工具取消）。
    pub(super) cancel: CancellationToken,
    session: SessionId,
    root: Option<SessionId>,
    run: RunId,
    agent: Option<AgentId>,
    workspace: WorkspaceRef,
    mode: PermissionMode,
}

impl Caller {
    pub(crate) fn of(context: &ToolUseContext) -> Self {
        Self {
            tool_use: context.tool_use.clone(),
            interactions: context.interactions.clone(),
            session_ops: context.session_ops.clone(),
            cancel: context.cancel.clone(),
            session: context.session.clone(),
            root: context.root.clone(),
            run: context.run.clone(),
            agent: context.agent.clone(),
            workspace: context.workspace.clone(),
            mode: context.mode,
        }
    }

    /// 顶层会话：sampling 的「本会话一直允许」按它记。
    pub(super) fn top(&self) -> &SessionId {
        self.root.as_ref().unwrap_or(&self.session)
    }

    /// 这个调用里的一个 hook 事件（公共字段取它的会话、工作区、模式与子代理）。
    pub(super) fn hook(&self, name: HookEventName, payload: Value) -> HookEvent {
        HookEvent {
            context: HookContext {
                mode: Some(self.mode),
                agent: self.agent.clone(),
                run: Some(self.run.clone()),
                root: self.root.clone(),
                ..HookContext::default()
            },
            ..HookEvent::new(name, self.session.clone(), self.workspace.clone(), payload)
        }
    }

    /// 旁路请求的发起方（sampling 的模型请求）。
    pub(super) fn requester(&self) -> Requester {
        Requester {
            session: self.session.clone(),
            run: Some(self.run.clone()),
            agent: self.agent.clone(),
            root: self.root.clone(),
        }
    }
}

/// 结局之外的停：连接关了或服务器取消了这个请求（不回答），或者去处的调用被取消（回 `cancel` 或错误），ext.md §3.13.5。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Stopped {
    Gone,
    Called,
}

/// 一个请求的停的几路。
pub(super) struct Stop {
    closed: CancellationToken,
    cancelled: CancellationToken,
    call: Option<CancellationToken>,
}

impl Stop {
    /// 选定去处之后，它的调用被取消也算停。
    pub(super) fn on(mut self, caller: &Caller) -> Self {
        self.call = Some(caller.cancel.clone());
        self
    }

    /// 等 `future`，或先停（`biased`：连接关了、服务器取消在调用取消之前看：那时不必回答）。
    pub(super) async fn until<F: Future>(&self, future: F) -> Result<F::Output, Stopped> {
        let call = async {
            match &self.call {
                Some(call) => call.cancelled().await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            biased;
            () = self.closed.cancelled() => Err(Stopped::Gone),
            () = self.cancelled.cancelled() => Err(Stopped::Gone),
            () = call => Err(Stopped::Called),
            output = future => Ok(output),
        }
    }
}

/// 回给服务器的。
pub(super) enum Reply {
    Result(ClientResult),
    Error(ErrorData),
}

/// 一条连接上的询问。
pub(super) struct Asks {
    pub(super) ports: Arc<AskPorts>,
    pub(super) server: String,
    transport: Arc<dyn McpTransport>,
    connection: McpConnectionId,
    pub(super) spawner: Spawner,
    /// 这条连接关了（适配器的关闭信号）。
    closed: CancellationToken,
    /// 在途的、有调用方的请求，按登记的先后（ext.md §3.13.2）。
    callers: Mutex<Vec<(u64, Arc<Caller>)>>,
    next: AtomicU64,
    /// 在问的服务器请求：ID → 服务器取消它时的信号。
    requests: Mutex<HashMap<RequestId, CancellationToken>>,
    /// 问过的网址模式：`elicitationId` → 问它的调用（完成通知的 hook 用它的会话）。完成通知到了就去掉。
    urls: Mutex<HashMap<String, Arc<Caller>>>,
    /// 一次问一个：同一个调用同时只有一个询问（§3.13.2）。
    turn: Semaphore,
}

impl Asks {
    pub(super) fn new(
        ports: Arc<AskPorts>,
        server: String,
        transport: Arc<dyn McpTransport>,
        (connection, spawner, closed): (McpConnectionId, Spawner, CancellationToken),
    ) -> Arc<Self> {
        Arc::new(Self {
            ports,
            server,
            transport,
            connection,
            spawner,
            closed,
            callers: Mutex::default(),
            next: AtomicU64::new(0),
            requests: Mutex::default(),
            urls: Mutex::default(),
            turn: Semaphore::new(1),
        })
    }

    pub(super) fn takes(&self, method: &str) -> bool {
        self.ports.takes(method)
    }

    /// 登记一个在途请求的调用方，守卫放下时去掉（结局定了）。
    pub(super) fn register(self: &Arc<Self>, caller: Caller) -> Registered {
        let key = self.next.fetch_add(1, Ordering::Relaxed);
        self.callers.lock_unpoisoned().push((key, Arc::new(caller)));
        Registered {
            asks: self.clone(),
            key,
        }
    }

    /// 去处：最早登记的、还在途的那个（ext.md §3.13.2）。
    pub(super) fn caller(&self) -> Option<Arc<Caller>> {
        self.callers
            .lock_unpoisoned()
            .first()
            .map(|(_, caller)| caller.clone())
    }

    /// 截下一个服务器请求：起一个任务问，回答在后台发。ID 不是整数或字符串（JSON-RPC 不许）时没法回答，丢掉（同 `link.rs`）。
    pub(super) fn start(self: &Arc<Self>, id: &Value, method: &str, params: Option<Value>) {
        let Ok(id) = serde_json::from_value::<RequestId>(id.clone()) else {
            tracing::debug!(%id, method, "ignored an MCP server request with an invalid id");
            return;
        };
        let cancelled = CancellationToken::new();
        self.requests
            .lock_unpoisoned()
            .insert(id.clone(), cancelled.clone());
        let stop = Stop {
            closed: self.closed.clone(),
            cancelled,
            call: None,
        };
        let asks = self.clone();
        let method = method.to_owned();
        drop(self.spawner.spawn("mcp-ask", async move {
            let reply = asks.serve(&method, params, stop).await;
            asks.requests.lock_unpoisoned().remove(&id);
            if let Some(reply) = reply {
                asks.send(id, reply);
            }
        }));
    }

    async fn serve(
        self: &Arc<Self>,
        method: &str,
        params: Option<Value>,
        stop: Stop,
    ) -> Option<Reply> {
        // 排队的时候也看停：服务器取消、连接关了就不再问。
        let Ok(Ok(_turn)) = stop.until(self.turn.acquire()).await else {
            return None;
        };
        if method == ELICITATION_CREATE {
            super::elicitation::serve(self, params, stop).await
        } else {
            super::sampling::serve(self, params, stop).await
        }
    }

    /// 服务器的 `notifications/cancelled`：在问的那个请求撤回（规范：被取消的请求不回答）。
    pub(super) fn cancelled(&self, params: Option<&Value>) {
        let Some(id) = params
            .and_then(|params| params.get("requestId"))
            .and_then(|id| serde_json::from_value::<RequestId>(id.clone()).ok())
        else {
            return;
        };
        if let Some(cancelled) = self.requests.lock_unpoisoned().get(&id) {
            cancelled.cancel();
        }
    }

    /// 网址模式的完成通知：只触发 Notification hook（本工程没有「等完成」的对话框，ext.md §3.13.4）。
    pub(super) fn completed(&self, params: Option<&Value>) {
        let Some(id) = params
            .and_then(|params| params.get("elicitationId"))
            .and_then(Value::as_str)
        else {
            return;
        };
        let Some(caller) = self.urls.lock_unpoisoned().remove(id) else {
            tracing::debug!(server = %self.server, elicitation = id, "Ignoring completion notification for unknown elicitation: {id}");
            return;
        };
        self.notify(
            &caller,
            format!(
                "MCP server \"{}\" confirmed elicitation {id} complete",
                self.server
            ),
            ELICITATION_COMPLETE_NOTIFICATION,
        );
    }

    /// 记下问过的网址模式（等完成通知）。
    pub(super) fn awaiting(&self, elicitation: String, caller: Arc<Caller>) {
        self.urls.lock_unpoisoned().insert(elicitation, caller);
    }

    /// 问用户：停了时撤回询问（取消是一条消息，不靠丢下 `ask` 的 future，同 tools 的 `asking::ask`）。询问失败（会话关闭）是 `Ok(None)`。
    pub(super) async fn ask(
        &self,
        caller: &Caller,
        request: InteractionRequest,
        stop: &Stop,
    ) -> Result<Option<Answer>, Stopped> {
        let tool_use = request.tool_use().clone();
        match stop.until(caller.interactions.ask(request)).await {
            Ok(answer) => Ok(answer.ok().map(|answer| answer.answer)),
            Err(stopped) => {
                caller.interactions.cancel_ask(&tool_use).await;
                Err(stopped)
            }
        }
    }

    /// 跑一个 hook 事件（没有这个事件的 hook 时不跑）；停了时取消 hook。
    pub(super) async fn hooks(
        &self,
        event: HookEvent,
        stop: &Stop,
    ) -> Result<rust_cli_kernel_api::hooks::HookOutcome, Stopped> {
        if !self.ports.hooks.has_hooks(event.name) {
            return Ok(Default::default());
        }
        let cancel = CancellationToken::new();
        let _cancel_on_return = cancel.clone().drop_guard();
        stop.until(self.ports.hooks.run(event, cancel)).await
    }

    /// Notification hook（只通知，不等，ext.md §3.13.6）。
    pub(super) fn notify(&self, caller: &Caller, message: String, kind: &'static str) {
        let hooks = self.ports.hooks.clone();
        if !hooks.has_hooks(HookEventName::Notification) {
            return;
        }
        let event = caller.hook(
            HookEventName::Notification,
            json!({"message": message, "notification_type": kind}),
        );
        drop(self.spawner.spawn("mcp-ask-notification", async move {
            let _ = hooks.run(event, CancellationToken::new()).await;
        }));
    }

    fn send(&self, id: RequestId, reply: Reply) {
        let message: ClientJsonRpcMessage = match reply {
            Reply::Result(result) => JsonRpcMessage::response(result, id),
            Reply::Error(error) => JsonRpcMessage::error(error, Some(id)),
        };
        match serde_json::to_value(&message) {
            Ok(message) => send_detached(
                &self.spawner,
                self.transport.clone(),
                self.connection.clone(),
                self.closed.clone(),
                message,
            ),
            Err(error) => {
                tracing::debug!(%error, "could not encode an answer to an MCP server request")
            }
        }
    }
}

/// Notification hook 的种类（hook 输入的 `notification_type`，ext.md §3.13.6）。
pub(super) const ELICITATION_RESPONSE_NOTIFICATION: &str = "elicitation_response";
const ELICITATION_COMPLETE_NOTIFICATION: &str = "elicitation_complete";

/// 在途请求的调用方登记：放下时去掉。
pub(super) struct Registered {
    asks: Arc<Asks>,
    key: u64,
}

impl Drop for Registered {
    fn drop(&mut self) {
        self.asks
            .callers
            .lock_unpoisoned()
            .retain(|(key, _)| *key != self.key);
    }
}

#[cfg(test)]
#[path = "asks_tests.rs"]
mod tests;
