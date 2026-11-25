//! ext 测试共用的替身：执行环境事实、内存里的 MCP 服务器（传输端口的替身）。

use rust_cli_base::sync::LockUnpoisoned;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use rust_cli_kernel_api::env::{EnvFacts, EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::EnvironmentId;
use rust_cli_kernel_api::mcp::{
    McpConnect, McpConnectionId, McpError, McpTransport, McpTransportEvent,
};
use serde_json::{Value, json};
use tokio::sync::mpsc;

pub(crate) const CWD: &str = "/Users/dev/project";
pub(crate) const HOME: &str = "/Users/dev";

pub(crate) fn facts() -> EnvFacts {
    EnvFacts {
        cwd: EnvPath::new(CWD),
        platform: "darwin".to_owned(),
        os_version: "Darwin 24".to_owned(),
        shell: "zsh".to_owned(),
        is_git_repo: false,
        home: Some(EnvPath::new(HOME)),
        uid: None,
    }
}

pub(crate) fn workspace() -> WorkspaceRef {
    WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new(CWD),
    }
}

/// 服务器对一条请求的回答：`Ok(result)` 或 `Err((code, message))`；`None` 表示不回答（挂起）。
pub(crate) type Handler =
    dyn Fn(&str, &Value) -> Option<Result<Value, (i64, String)>> + Send + Sync;

struct Connection {
    events: Option<mpsc::UnboundedReceiver<McpTransportEvent>>,
    sender: mpsc::UnboundedSender<McpTransportEvent>,
    server: String,
}

/// 内存里的 MCP 服务器：每个服务器名一个处理函数；记录收到的消息。
#[derive(Default)]
pub(crate) struct FakeMcp {
    handlers: Mutex<HashMap<String, Arc<Handler>>>,
    connections: Mutex<HashMap<String, Connection>>,
    received: Mutex<Vec<(String, Value)>>,
    next: AtomicUsize,
    /// 连接失败的服务器。
    refuse: Mutex<Vec<String>>,
    /// [`FakeMcp::release`] 过的服务器的 `initialize` 结果：之后（含重连）的握手直接回它。
    released: Mutex<HashMap<String, Value>>,
    /// 每次连接的（服务器名，工作区根）。
    roots: Mutex<Vec<(String, String)>>,
    /// 这些方法的请求不回答，传输改送 `ReplyLost{id, 原因}`（流续不上，ext.md §3.10.6）：方法 → 原因。
    losing: Mutex<HashMap<String, String>>,
    /// 这些方法的消息记下之后，发送一直不结束（网络不通，出口在等连接期限，ext.md §3.10.9）。
    stalling: Mutex<Vec<String>>,
    /// 对服务器请求的回答（没有 `method`）记下之后，发送一直不结束：服务器收下回答的 POST、不回响应头（ext.md §3.10.11）。
    stalling_answers: AtomicBool,
    /// 每收到一条消息通知一次：等后台发出的消息（例如取消）。
    arrived: tokio::sync::Notify,
    /// 要授权的服务器（OAuth，ext.md §3.12.3）：它的请求的发送报 `AuthRequired`，带这里的 scope（403 时服务器要的）。
    unauthorized: Mutex<HashMap<String, Option<String>>>,
}

/// 一个能握手、有给定能力与 instructions 的服务器；其余请求交给 `rest`。
pub(crate) fn server(
    capabilities: Value,
    instructions: Option<&str>,
    rest: impl Fn(&str, &Value) -> Option<Result<Value, (i64, String)>> + Send + Sync + 'static,
) -> Arc<Handler> {
    let instructions = instructions.map(str::to_owned);
    Arc::new(move |method: &str, params: &Value| match method {
        "initialize" => {
            let mut result = json!({
                "protocolVersion": "2025-06-18",
                "capabilities": capabilities,
                "serverInfo": {"name": "fake", "version": "1"},
            });
            if let Some(text) = &instructions {
                result["instructions"] = json!(text);
            }
            Some(Ok(result))
        }
        other => rest(other, params),
    })
}

/// 握手不回答的服务器（慢服务器，ext.md §3.3.1）：[`FakeMcp::release`] 时才回 `initialize`；其余请求交给 `rest`。
pub(crate) fn held(
    rest: impl Fn(&str, &Value) -> Option<Result<Value, (i64, String)>> + Send + Sync + 'static,
) -> Arc<Handler> {
    Arc::new(move |method: &str, params: &Value| match method {
        "initialize" => None,
        other => rest(other, params),
    })
}

impl FakeMcp {
    /// 完成 [`held`] 的服务器的握手：回答已经挂起的 `initialize`；还没发来时，之后发来就直接回答。
    pub(crate) fn release(&self, server: &str, capabilities: Value, instructions: Option<&str>) {
        let mut result = json!({
            "protocolVersion": "2025-06-18",
            "capabilities": capabilities,
            "serverInfo": {"name": "fake", "version": "1"},
        });
        if let Some(text) = instructions {
            result["instructions"] = json!(text);
        }
        self.released
            .lock_unpoisoned()
            .insert(server.to_owned(), result.clone());
        let id = self
            .received
            .lock_unpoisoned()
            .iter()
            .rev()
            .find(|(name, message)| name == server && message["method"] == "initialize")
            .map(|(_, message)| message["id"].clone());
        if let Some(id) = id {
            self.push(
                server,
                json!({"jsonrpc": "2.0", "id": id, "result": result}),
            );
        }
    }

    pub(crate) fn with(self, name: &str, handler: Arc<Handler>) -> Self {
        self.handlers
            .lock_unpoisoned()
            .insert(name.to_owned(), handler);
        self
    }

    /// `method` 的请求都“续不上”：传输送出 `ReplyLost{id, reason}`，服务器不回答。
    pub(crate) fn losing(self, method: &str, reason: &str) -> Self {
        self.losing
            .lock_unpoisoned()
            .insert(method.to_owned(), reason.to_owned());
        self
    }

    /// `method` 的消息的发送记下之后一直不结束。
    pub(crate) fn stalling(self, method: &str) -> Self {
        self.stalling.lock_unpoisoned().push(method.to_owned());
        self
    }

    /// 对服务器请求的回答的发送记下之后一直不结束。
    pub(crate) fn stalling_answers(self) -> Self {
        self.stalling_answers.store(true, Ordering::Relaxed);
        self
    }

    /// 等到收到一条满足 `wanted` 的消息并返回它（后台发出的消息，测试不知道它什么时候到）。
    pub(crate) async fn arrival(&self, wanted: impl Fn(&Value) -> bool) -> Value {
        loop {
            // 先登记等待再查：消息在两者之间到达也不会漏掉通知。
            let mut arrived = std::pin::pin!(self.arrived.notified());
            arrived.as_mut().enable();
            let found = self
                .received
                .lock_unpoisoned()
                .iter()
                .find(|(_, message)| wanted(message))
                .map(|(_, message)| message.clone());
            if let Some(found) = found {
                return found;
            }
            arrived.await;
        }
    }

    pub(crate) fn refusing(self, name: &str) -> Self {
        self.refuse.lock_unpoisoned().push(name.to_owned());
        self
    }

    /// 这个服务器之后的请求都要授权（传输换不来令牌，ext.md §3.12.3）；`scope` 是 403 时要的更多权限。
    pub(crate) fn require_auth(&self, name: &str, scope: Option<&str>) {
        self.unauthorized
            .lock_unpoisoned()
            .insert(name.to_owned(), scope.map(str::to_owned));
    }

    /// 授权完成：之后的请求照常。
    pub(crate) fn authorize(&self, name: &str) {
        self.unauthorized.lock_unpoisoned().remove(name);
    }

    /// 收到的消息（服务器名，消息）。
    pub(crate) fn received(&self) -> Vec<(String, Value)> {
        self.received.lock_unpoisoned().clone()
    }

    /// 每次连接的（服务器名，工作区根）。
    pub(crate) fn roots(&self) -> Vec<(String, String)> {
        self.roots.lock_unpoisoned().clone()
    }

    /// 服务器主动发一条消息（通知或请求）。
    pub(crate) fn push(&self, server: &str, message: Value) {
        for connection in self.connections.lock_unpoisoned().values() {
            if connection.server == server {
                let _ = connection.sender.send(McpTransportEvent::Message {
                    message: message.clone(),
                });
            }
        }
    }

    /// 关闭某个服务器的全部连接。
    pub(crate) fn drop_server(&self, server: &str, session_expired: bool) {
        for connection in self.connections.lock_unpoisoned().values() {
            if connection.server == server {
                let _ = connection.sender.send(McpTransportEvent::Closed {
                    reason: Some("gone".to_owned()),
                    session_expired,
                });
            }
        }
    }
}

#[async_trait]
impl McpTransport for FakeMcp {
    async fn connect(
        &self,
        workspace: &WorkspaceRef,
        request: McpConnect,
    ) -> Result<McpConnectionId, McpError> {
        self.roots
            .lock_unpoisoned()
            .push((request.server.clone(), workspace.root.as_str().to_owned()));
        if self.refuse.lock_unpoisoned().contains(&request.server) {
            return Err(McpError::Connect {
                message: "spawn failed".to_owned(),
            });
        }
        let id = format!("c{}", self.next.fetch_add(1, Ordering::Relaxed));
        let (sender, events) = mpsc::unbounded_channel();
        self.connections.lock_unpoisoned().insert(
            id.clone(),
            Connection {
                events: Some(events),
                sender,
                server: request.server,
            },
        );
        Ok(McpConnectionId::new(id))
    }

    async fn send(&self, connection: &McpConnectionId, message: Value) -> Result<(), McpError> {
        let (server, sender) = {
            let connections = self.connections.lock_unpoisoned();
            let found = connections
                .get(connection.as_str())
                .ok_or_else(|| McpError::Closed {
                    message: "unknown connection".to_owned(),
                })?;
            (found.server.clone(), found.sender.clone())
        };
        self.received
            .lock_unpoisoned()
            .push((server.clone(), message.clone()));
        self.arrived.notify_waiters();
        let method = message.get("method").and_then(Value::as_str);
        let stalled = match method {
            Some(method) => self
                .stalling
                .lock_unpoisoned()
                .iter()
                .any(|stalled| stalled == method),
            None => self.stalling_answers.load(Ordering::Relaxed),
        };
        if stalled {
            std::future::pending::<()>().await;
        }
        if message.get("id").is_some()
            && let Some(scope) = self.unauthorized.lock_unpoisoned().get(&server).cloned()
        {
            return Err(McpError::AuthRequired {
                message: "the MCP server needs authorization (HTTP 401)".to_owned(),
                scope,
            });
        }
        let (Some(id), Some(method)) = (
            message.get("id").cloned(),
            message.get("method").and_then(Value::as_str),
        ) else {
            return Ok(());
        };
        if let Some(reason) = self.losing.lock_unpoisoned().get(method).cloned() {
            let _ = sender.send(McpTransportEvent::ReplyLost { id, reason });
            return Ok(());
        }
        let handler = self.handlers.lock_unpoisoned().get(&server).cloned();
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        let released = (method == "initialize")
            .then(|| self.released.lock_unpoisoned().get(&server).cloned())
            .flatten();
        let reply = match released {
            Some(result) => Some(Ok(result)),
            None => handler.and_then(|handler| handler(method, &params)),
        };
        let reply = match reply {
            None => return Ok(()),
            Some(Ok(result)) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Some(Err((code, text))) => {
                json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": text}})
            }
        };
        let _ = sender.send(McpTransportEvent::Message { message: reply });
        Ok(())
    }

    fn events(&self, connection: &McpConnectionId) -> BoxStream<'static, McpTransportEvent> {
        let events = self
            .connections
            .lock_unpoisoned()
            .get_mut(connection.as_str())
            .and_then(|found| found.events.take());
        match events {
            Some(mut events) => {
                futures_util::stream::poll_fn(move |context| events.poll_recv(context)).boxed()
            }
            None => futures_util::stream::empty().boxed(),
        }
    }

    async fn close(&self, connection: &McpConnectionId) -> Result<(), McpError> {
        if let Some(found) = self.connections.lock_unpoisoned().get(connection.as_str()) {
            let _ = found.sender.send(McpTransportEvent::Closed {
                reason: None,
                session_expired: false,
            });
        }
        Ok(())
    }
}

/// 一次工具调用的上下文（替身端口）。
pub(crate) fn tool_context<E: rust_cli_kernel_api::testing::AllPorts + 'static>(
    env: Arc<E>,
    mode: rust_cli_kernel_api::permission::PermissionMode,
) -> rust_cli_kernel_api::tool::ToolUseContext {
    use rust_cli_kernel_api::env::{OperationContext, Origin};
    use rust_cli_kernel_api::ids::{Generation, OperationId, RunId, SessionId};
    use rust_cli_kernel_api::testing::{RecordingSink, controller_services, machine};
    let session = SessionId::new("sess_1");
    let run = RunId::new("run_1");
    rust_cli_kernel_api::tool::ToolUseContext {
        session: session.clone(),
        run: run.clone(),
        workspace: workspace(),
        mode,
        machine: machine(env.clone()),
        controller: controller_services(env),
        interactions: Arc::new(RecordingSink::default()),
        session_ops: Arc::new(RecordingSink::default()),
        operation: OperationContext::new(
            EnvironmentId::new("local"),
            Generation::new("g1"),
            OperationId::new("op_call"),
            Origin::Agent { session, run },
        ),
        ..rust_cli_kernel_api::testing::tool_use_context()
    }
}

/// 记下命令的 Bash 替身：输出 `out:<命令>`；命令含 `fail` 时结果是错误。
#[derive(Default)]
pub(crate) struct EchoBash {
    pub commands: Mutex<Vec<(String, String)>>,
}

#[async_trait]
impl rust_cli_kernel_api::tool::Tool for EchoBash {
    fn classifier_input(
        &self,
        _input: &serde_json::Value,
    ) -> rust_cli_kernel_api::classifier::ClassifierInput {
        rust_cli_kernel_api::classifier::ClassifierInput::Skip
    }

    fn name(&self) -> &str {
        "Bash"
    }

    fn input_schema(&self) -> Value {
        json!({"type": "object"})
    }

    fn description(&self, _context: &rust_cli_kernel_api::tool::PromptContext) -> String {
        "bash".to_owned()
    }

    async fn call(
        &self,
        input: Value,
        context: rust_cli_kernel_api::tool::ToolUseContext,
        _progress: mpsc::Sender<rust_cli_kernel_api::tool::ToolProgress>,
    ) -> Result<rust_cli_kernel_api::tool::ToolOutput, rust_cli_kernel_api::tool::ToolError> {
        let command = input["command"].as_str().unwrap_or_default().to_owned();
        self.commands
            .lock_unpoisoned()
            .push((command.clone(), context.operation.operation.to_string()));
        Ok(rust_cli_kernel_api::tool::ToolOutput {
            content: vec![rust_cli_kernel_api::message::ToolResultContent::Text {
                text: format!("out:{command}"),
            }],
            is_error: command.contains("fail"),
            display: Value::Null,
        })
    }
}

/// skill `!` 命令的判决链替身（kernel-api `ScopedGate`）：命令在 `denied` 里时拒绝，命令匹配一条前缀写法的
/// `Bash(<前缀>:*)` allowed 规则时放行，其余询问。规则本身的语义（deny 与 ask 优先、通配、并空白）在 policy 的测试里
/// （`lib_tests_allowed.rs`）；这里只看 ext 怎样用判决的结论。
#[derive(Default)]
pub(crate) struct AllowedGate {
    pub denied: Vec<String>,
}

#[async_trait]
impl rust_cli_kernel_api::orchestration::ScopedGate for AllowedGate {
    async fn decide_with_allowed(
        &self,
        _tool: &dyn rust_cli_kernel_api::tool::Tool,
        input: &Value,
        _context: &rust_cli_kernel_api::tool::ToolUseContext,
        allowed: &[rust_cli_kernel_api::permission::PermissionRule],
    ) -> rust_cli_kernel_api::permission::PermissionDecision {
        use rust_cli_kernel_api::permission::PermissionDecision;
        use rust_cli_kernel_api::permission::syntax::{ShellRule, prefix_matches};
        let command = input["command"].as_str().unwrap_or_default();
        if self.denied.iter().any(|denied| denied == command) {
            return PermissionDecision::Deny {
                message: "Permission to use Bash has been denied.".to_owned(),
            };
        }
        let admitted = allowed.iter().any(|rule| {
            rule.tool == "Bash"
                && rule.content.as_deref().is_some_and(|content| {
                    matches!(ShellRule::parse(content), ShellRule::Prefix(prefix) if prefix_matches(prefix, command))
                })
        });
        if admitted {
            PermissionDecision::Allow {
                updated_input: None,
            }
        } else {
            PermissionDecision::ask("This command requires approval", Vec::new())
        }
    }
}

/// 测试用的一个插件（`<name>@inline`，在 `root`，没有组件）。
pub(crate) fn plugin(name: &str, root: &str) -> crate::plugins::Plugin {
    crate::plugins::Plugin {
        context: Arc::new(rust_cli_base::plugin::PluginContext {
            id: format!("{name}@inline"),
            name: name.to_owned(),
            root: root.to_owned(),
            ..Default::default()
        }),
        marketplace: "inline".to_owned(),
        ..Default::default()
    }
}

/// 只有这些插件的 `PluginSet`。
pub(crate) fn plugin_set(plugins: Vec<crate::plugins::Plugin>) -> Arc<crate::plugins::PluginSet> {
    Arc::new(crate::plugins::PluginSet::new(
        plugins,
        Vec::new(),
        Vec::new(),
    ))
}

/// 没有任何 hook 的引擎（ExtPorts 的 hooks）。
pub(crate) struct NoHooks;

#[async_trait]
impl rust_cli_kernel_api::hooks::HookRunner for NoHooks {
    async fn run(
        &self,
        _event: rust_cli_kernel_api::hooks::HookEvent,
        _cancel: tokio_util::sync::CancellationToken,
    ) -> rust_cli_kernel_api::hooks::HookOutcome {
        rust_cli_kernel_api::hooks::HookOutcome::default()
    }

    fn has_hooks(&self, _event: rust_cli_kernel_api::hooks::HookEventName) -> bool {
        false
    }
}

pub(crate) fn no_hooks() -> Arc<dyn rust_cli_kernel_api::hooks::HookRunner> {
    Arc::new(NoHooks)
}

/// 不会被调用的模型端口（没有 sampling 的测试）。
pub(crate) fn no_model() -> Arc<dyn rust_cli_kernel_api::model::ModelPort> {
    Arc::new(rust_cli_kernel_api::testing::ScriptedModel::new(Vec::new()))
}

pub(crate) fn default_model() -> rust_cli_kernel_api::default_model::DefaultModel {
    rust_cli_kernel_api::model::ModelSelection::new("zai", rust_cli_base::models::GLM_5_3).into()
}

/// 远端机器的替身（发现、插件加载的往返次数测试，ext.md §2.1 末条、§4.0）。
mod slow {
    use std::time::Duration;

    use async_trait::async_trait;
    use rust_cli_kernel_api::env::{
        EnvError, FileContent, OperationContext, ReadFile, WorkspaceRef, WriteFile, WriteReceipt,
    };
    use rust_cli_kernel_api::env_files::{DirEntry, FileStat, ListDir, StatFile, StatMany};
    use rust_cli_kernel_api::machine::Files;
    use rust_cli_kernel_api::testing::MemoryEnvironment;

    /// 一次往返。
    pub(crate) const RTT: Duration = Duration::from_millis(10);

    /// 每个调用先等一次往返再交给内存环境；`stat_many` 是一次往返（远端机器合成一个请求）。
    pub(crate) struct Slow(pub(crate) MemoryEnvironment);

    #[async_trait]
    impl Files for Slow {
        async fn read_file(
            &self,
            workspace: &WorkspaceRef,
            request: ReadFile,
        ) -> Result<FileContent, EnvError> {
            tokio::time::sleep(RTT).await;
            self.0.read_file(workspace, request).await
        }

        async fn write_file(
            &self,
            workspace: &WorkspaceRef,
            context: OperationContext,
            request: WriteFile,
        ) -> Result<WriteReceipt, EnvError> {
            self.0.write_file(workspace, context, request).await
        }

        async fn stat(
            &self,
            workspace: &WorkspaceRef,
            request: StatFile,
        ) -> Result<Option<FileStat>, EnvError> {
            tokio::time::sleep(RTT).await;
            self.0.stat(workspace, request).await
        }

        async fn stat_many(
            &self,
            workspace: &WorkspaceRef,
            request: StatMany,
        ) -> Result<Vec<Result<Option<FileStat>, EnvError>>, EnvError> {
            tokio::time::sleep(RTT).await;
            self.0.stat_many(workspace, request).await
        }

        async fn list_dir(
            &self,
            workspace: &WorkspaceRef,
            request: ListDir,
        ) -> Result<Vec<DirEntry>, EnvError> {
            tokio::time::sleep(RTT).await;
            self.0.list_dir(workspace, request).await
        }
    }
}

pub(crate) use slow::{RTT, Slow};
