//! App 协议的方法表（docs/specs/rust-cli/app-protocol.md §3.4、§10.3）：P1 的方法；其余回 `-32601`。
//! 命令在 [`crate::commands`]，订阅在 [`crate::subscribe`]。

use rust_cli_base::sync::LockUnpoisoned;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{EnvironmentId, SessionId};
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::session_api::{EndReason, Kernel, KernelError};
use rust_cli_protocol_core::connection::{Reply, Service};
use rust_cli_protocol_core::rpc::RpcError;
use rust_cli_protocol_core::sessions::{Bridged, SessionBridge};
use serde_json::{Value, json};

use crate::context::Context;
use crate::inbound::Inbound;
use crate::inbox::Remembered;
use crate::index::Topics;
use crate::live::Live;
use crate::projection::state::Followup;
use crate::workspaces::{Hosted, Workspaces};

/// 会话不可用（`zcodeProtocolErrorCodes.sessionUnavailable`）。
pub const SESSION_UNAVAILABLE: i64 = -32004;

/// 会话事件攒成一批再发的窗口。
const EVENT_BATCH_WINDOW: std::time::Duration = std::time::Duration::from_millis(30);

/// 新会话的缺省（入口给出）：模型是进程级的；模式按工作区（[`Hosted::mode`]）。
#[derive(Debug, Clone)]
pub struct Defaults {
    pub selection: ModelSelection,
    pub environment: EnvironmentId,
}

/// 会话请求找不到内核：没有登记工作区，或打开失败。
pub(crate) enum LiveError {
    /// 这个会话没有登记到工作区（app-protocol.md §15.2：不回落到进程 cwd 或第一个工作区）。
    Unrouted,
    Kernel(KernelError),
}

impl LiveError {
    pub(crate) fn rpc(&self, session: &SessionId) -> RpcError {
        match self {
            Self::Unrouted => unrouted(session),
            Self::Kernel(error) => kernel_error(error),
        }
    }

    pub(crate) fn message(&self, session: &SessionId) -> String {
        match self {
            Self::Unrouted => unrouted_text(session),
            Self::Kernel(error) => kernel_error(error).message,
        }
    }
}

fn unrouted_text(session: &SessionId) -> String {
    format!(
        "Session {} is not routed to a workspace in this agent",
        session.as_str()
    )
}

/// 会话没有登记工作区（`-32602`，与 Host 拒绝找不到工作区的反向请求同一个码）。
pub(crate) fn unrouted(session: &SessionId) -> RpcError {
    RpcError::invalid_params(unrouted_text(session))
}

/// 工作区级的开关（Host 推来的偏好；P1 只保存与回声）。
#[derive(Debug, Clone, Default)]
struct Preferences {
    ask_auto_resolution: bool,
    full_retention: bool,
}

pub struct Server {
    pub(crate) context: Arc<Context>,
    /// 每个工作区一套内核（app-protocol.md §15）。
    pub(crate) workspaces: Arc<dyn Workspaces>,
    pub(crate) bridge: SessionBridge<Live>,
    pub(crate) topics: Arc<Topics>,
    pub(crate) remembered: Mutex<Remembered>,
    pub(crate) followups: Mutex<HashMap<SessionId, Followup>>,
    pub(crate) defaults: Defaults,
    /// 本进程的实例标识（时钟探测的 `clock.instanceId`）。
    pub(crate) instance: String,
    preferences: Mutex<Preferences>,
}

pub(crate) fn kernel_error(error: &KernelError) -> RpcError {
    match error {
        KernelError::NotFound { session } => RpcError::new(
            SESSION_UNAVAILABLE,
            format!("Session unavailable: {}", session.as_str()),
        ),
        other => RpcError::internal(other.to_string()),
    }
}

impl Server {
    pub fn new(
        context: Arc<Context>,
        workspaces: Arc<dyn Workspaces>,
        spawner: rust_cli_base::Spawner,
        defaults: Defaults,
    ) -> Self {
        let topics = Arc::new(Topics::new(context.clone(), &spawner));
        Self {
            instance: context.fresh("instance"),
            bridge: SessionBridge::new(spawner, EVENT_BATCH_WINDOW),
            context,
            workspaces,
            topics,
            remembered: Mutex::new(Remembered::default()),
            followups: Mutex::new(HashMap::new()),
            defaults,
            preferences: Mutex::new(Preferences::default()),
        }
    }

    pub(crate) fn workspace(&self, path: &str) -> WorkspaceRef {
        WorkspaceRef {
            environment: self.defaults.environment.clone(),
            root: EnvPath::new(path),
        }
    }

    /// 工作区的那一套内核（第一次见到时组装）；路径为空时是进程 cwd 的那一套。
    pub(crate) async fn hosted(&self, root: &str) -> Result<Hosted, KernelError> {
        if root.is_empty() {
            return Ok(self.workspaces.process());
        }
        self.workspaces
            .open(root)
            .await
            .map_err(|message| KernelError::Failed { message })
    }

    /// 只读会话库用的内核：会话登记了、它的工作区已经组装时用那一套，否则用进程 cwd 的那一套（不开会话 actor）。
    pub(crate) fn reader(&self, session: &SessionId) -> Arc<dyn Kernel> {
        self.context
            .workspace_of(session.as_str())
            .and_then(|root| self.workspaces.existing(&root))
            .unwrap_or_else(|| self.workspaces.process())
            .kernel
    }

    /// 会话库的只读查询与前端记录（每套内核共用一个库）。
    pub(crate) fn store(&self) -> Arc<dyn rust_cli_kernel_api::store::StoreQueries> {
        self.workspaces.process().kernel.queries()
    }

    /// 打开会话并接上投影（已经打开时直接给出）：按路由表找它的工作区的那一套内核；没有登记时拒绝。
    pub(crate) async fn live(&self, session: &SessionId) -> Result<Arc<Bridged<Live>>, LiveError> {
        if let Some(live) = self.bridge.get(session) {
            return Ok(live);
        }
        let root = self
            .context
            .workspace_of(session.as_str())
            .ok_or(LiveError::Unrouted)?;
        let hosted = self.hosted(&root).await.map_err(LiveError::Kernel)?;
        let followup = self
            .followups
            .lock_unpoisoned()
            .get(session)
            .copied()
            .unwrap_or(Followup::Queue);
        let (context, topics) = (self.context.clone(), self.topics.clone());
        self.bridge
            .open(&hosted.kernel, session, move |_, view| {
                Live::new(context, topics, view, (followup, root))
            })
            .await
            .map_err(LiveError::Kernel)
    }

    fn preferences(&self) -> std::sync::MutexGuard<'_, Preferences> {
        self.preferences.lock_unpoisoned()
    }

    /// 斜杠命令与模式按这个工作区的那一套（F145：以前是进程 cwd 的配置）。
    async fn read_presentation(&self, params: &Value) -> Result<Value, RpcError> {
        let workspace = params["workspace"].clone();
        let path = workspace["workspacePath"].as_str().unwrap_or_default();
        let hosted = self
            .hosted(path)
            .await
            .map_err(|error| kernel_error(&error))?;
        let commands: Vec<Value> = hosted
            .kernel
            // v4 的命令不带附加目录（session-dirs.md §8），只有工作区的。
            .user_commands(&self.workspace(path), &[])
            .await
            .into_iter()
            .filter(|command| !command.hidden && !command.name.is_empty())
            .map(|command| {
                let mut value = json!({"name": command.name, "description": command.description,
                    "source": "custom"});
                if let Some(hint) = command.argument_hint {
                    value["inputHint"] = json!(hint);
                }
                value
            })
            .collect();
        Ok(
            json!({"workspace": workspace, "mode": hosted.mode.node_name(),
            "slashCommands": commands}),
        )
    }

    /// 断开投影，再关闭会话 actor（会话级的 MCP 随之释放，app-protocol.md §15.3）。修复（F145）：以前只断开投影，
    /// 会话 actor 与它按会话的东西一直留到进程退出。
    async fn close_session(&self, params: &Value) -> Value {
        let session = SessionId::new(params["sessionId"].as_str().unwrap_or_default());
        if let Some(live) = self.bridge.close(&session) {
            live.handle.close(EndReason::Other).await;
        }
        json!({"closed": true})
    }

    async fn dispatch(&self, method: &str, params: Value) -> Result<Reply, RpcError> {
        let reply = |value: Value| Ok(Reply::new(value));
        match method {
            // 不支持的能力给 false（app-protocol.md §4.3）；按工作区路由支持（§15，F145），记下 Host 的同名位。
            "runtime/capabilities" => {
                if params["hostCapabilities"]["sessionWorkspaceRouting"] == true {
                    self.context.set_host_routes();
                }
                reply(json!({
                    "independentPlanState": false,
                    "historyFirst": false,
                    "sessionDirectorySource": false,
                    "genUiContext": false,
                    "sessionWorkspaceRouting": true,
                    "residentRowsRange": false,
                }))
            }
            // platform 自己读同一批文件；回执版本（app-protocol.md §5.1）。
            "provider/updateAccountConfig" => reply(json!({
                "receivedRevision": params["revision"],
                "providerCount": params["providers"].as_object().map_or(0, serde_json::Map::len),
                "status": "received",
            })),
            "provider/refreshRegistry" => reply(json!({"refreshed": true})),
            "workspace/updateInteractionPreferences" => {
                let enabled = params["preferences"]["askUserQuestionAutoResolutionEnabled"]
                    .as_bool()
                    .unwrap_or(false);
                self.preferences().ask_auto_resolution = enabled;
                reply(json!({"workspace": params["workspace"],
                    "askUserQuestionAutoResolutionEnabled": enabled, "snoozedInteractionCount": 0}))
            }
            "workspace/updateModelIoPreferences" => {
                let enabled = params["preferences"]["fullRetentionEnabled"]
                    .as_bool()
                    .unwrap_or(false);
                self.preferences().full_retention = enabled;
                reply(
                    json!({"workspace": params["workspace"], "fullRetentionEnabled": enabled,
                    "updatedSessionCount": 0}),
                )
            }
            "workspace/updateOffPeakToolPolicy" => {
                reply(json!({"workspace": params["workspace"], "enabled": params["enabled"]}))
            }
            // 动态工作流不移植（P4 决定）：回 disabled 生效。
            "workspace/updateDynamicWorkflowPolicy" => {
                reply(json!({"workspace": params["workspace"], "enabled": false}))
            }
            "workspace/readPresentation" => self.read_presentation(&params).await.map(Reply::new),
            // 目录来源按 sessionIds 查父链：rust 的会话没有要报的父链（子代理不进列表），P1 回空列表。
            "session/list" => reply(json!({"sessions": []})),
            "session/read" => crate::legacy::read(self, &params).await.map(Reply::new),
            "session/close" => reply(self.close_session(&params).await),
            "v4/conversation/subscribe" => crate::subscribe::subscribe(self, &params).await,
            "v4/conversation/unsubscribe" => crate::subscribe::unsubscribe(self, &params).await,
            "v4/conversation/resync" => crate::subscribe::resync(self, &params).await,
            "v4/connection/flow" => crate::subscribe::flow(self, &params).await,
            "v4/sessionsIndex/reconcile" => crate::subscribe::reconcile(self, &params).await,
            "v4/command" => crate::commands::command(self, params).await.map(Reply::new),
            "v4/commands/query" => crate::commands::query(self, &params).await.map(Reply::new),
            other => Err(RpcError::method_not_found(other)),
        }
    }
}

/// [`Server`] 放在连接上的形状（读循环要 `'static`）。
pub struct Handler(pub Arc<Server>);

#[async_trait]
impl Service for Handler {
    async fn request(&self, method: &str, params: Value) -> Result<Reply, RpcError> {
        Inbound::get().method(method, &params)?;
        self.0.dispatch(method, params).await
    }
}
