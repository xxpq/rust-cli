//! 连接的 actor（protocol-v5.md §4）：唯一的 owner。读循环按到达顺序交来请求与通知；入口经 [`Handle`] 报告启动事实；
//! 工作区的组装、读库、信任写入在别的任务里做，完成时发消息回来；会话列表的变化流也是它的输入（§14）。actor 只等下一条输入与
//! 出站管道的背压（§4.1 的不变量），不用锁，不用超时对齐状态。

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use futures_util::StreamExt;
use rust_cli_base::{Spawner, Task};
use rust_cli_kernel_api::clock::Clock;
use rust_cli_kernel_api::env::WorkspaceRef;
use rust_cli_kernel_api::external_accounts::ExternalAccounts;
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::provider::HeaderSource;
use rust_cli_kernel_api::session_api::Kernel;
use rust_cli_kernel_api::store::{ArtifactStore, StoreError, StoreQueries};
use rust_cli_protocol_core::connection::{Inbound, Peer};
use rust_cli_protocol_core::rpc::RpcError;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::sync::mpsc;

use crate::agent::{Process, Workspace};
use crate::attachments::{CommitFailure, Uploads};
use crate::caps::Negotiated;
use crate::consts::{INITIAL_CREDIT, INTAKE_QUEUE, MAX_MESSAGE_BYTES};
use crate::errors::{Fault, not_applied, protocol_error, to_rpc};
use crate::generated::contract::{self, FRAME, codes};
use crate::generated::core_v1::{
    Credit, Exit, ExitReason, ProtocolError, QueryEnvelope, Unsubscribe,
};
use crate::hook_pretrust::{HookPretrust, HookPretrustRefusal};
use crate::mcp_trust::{McpDecision, McpPending, McpTrustRefusal, McpTrusted};
use crate::outbox::{Outbox, Outgoing};
use crate::plugin_references::PluginReference;
use crate::publisher::Publisher;
use crate::session::{SessionQuery, Sessions};
use crate::sessions::{Feeds, SESSIONS_FACTS, Seed};
use crate::slash::{SLASH_COMMANDS, SlashReads};
use crate::strict;
use crate::trust_writes::TrustWrites;

/// 入口给的设置（protocol-v5.md §2）。
#[derive(Clone, Default)]
pub struct Settings {
    /// 本进程的 build（`zcode-rs` 的版本）：initialize 结果的 `server.build.version` 与 agent 的诊断，只用于日志。
    /// 不拿它判定两端是否同包：rust（0.1.x）与 App（3.x）是两条发布线；判据是契约指纹（03 §3.7）。
    pub version: String,
    /// 插件管理的端口（§72.1）：交了才声明 `catalog.plugins`。
    pub plugins: Option<Arc<dyn crate::plugin_admin::PluginAdmin>>,
    /// 入口的内核装了 MCP 的授权（§74：进程级的授权登记与它的端口）：装了才声明并认 `mcp.oauth`。
    pub mcp_oauth: bool,
}

impl std::fmt::Debug for Settings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Settings")
            .field("version", &self.version)
            .field("plugins", &self.plugins.is_some())
            .field("mcp_oauth", &self.mcp_oauth)
            .finish()
    }
}

/// 组装好的一个工作区的内核与新会话的缺省（会话命令经它建会话、打开会话，protocol-v5.md §15.1）。
#[derive(Clone)]
pub struct WorkspaceKernel {
    pub kernel: Arc<dyn Kernel>,
    /// 新会话的模型与权限模式（`session.create` 用）。
    pub selection: ModelSelection,
    pub mode: PermissionMode,
}

/// 读面读到的一个本机附件的事实（protocol-v5.md §41.4）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttachmentFile {
    pub size: u64,
    pub mtime_ms: u64,
}

/// 读不出一个本机附件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachmentFileError {
    /// 不存在、不是普通文件。
    NotFound,
    /// 超过读的上限。
    TooLarge {
        size: u64,
    },
    Failed(String),
    /// 这个入口不读本机文件（端口的缺省）。
    Unsupported,
}

/// 工作区的内核（入口实现，F145 的工作区层）。
#[async_trait]
pub trait Workspaces: Send + Sync {
    /// 组装这个工作区的内核（同一个工作区的并发组装只做一次，缓存到释放）。失败时给原因。
    async fn open(&self, key: &str) -> Result<WorkspaceKernel, String>;
    /// 关掉这个工作区的内核（01 §17.1 的 `agent.release`）。没有打开过时返回假。
    fn release(&self, key: &str) -> bool;
    /// 这个 key 指的工作区（本机环境 + 根目录）：会话事实按它分工作区（protocol-v5.md §14.7）。环境 id 由入口定，v5 不自己拼。
    fn workspace(&self, key: &str) -> WorkspaceRef;
    /// 这个工作区等用户信任的项目级 MCP 声明（18 §12.5）：现在读一次信任记录；工作区的内核还没组装时为空。通道打开会话时用它
    /// 建 `mcpAdmission`。
    async fn mcp_pending(&self, key: &str) -> Vec<McpPending>;
    /// `mcp.trust.set`（18 §12.3）：在信任记录的锁里按磁盘上的声明重验 digest，再写记录；成功时给写入之后的待信任声明。
    async fn trust_mcp(
        &self,
        key: &str,
        decision: McpDecision,
    ) -> Result<McpTrusted, McpTrustRefusal>;
    /// 这个工作区的插件引用目录（protocol-v5.md §59.1，ext.md §4.7）：工作区内核加载的插件（组装中就等它，没有就组装）。
    /// 缺省没有插件（不服务插件的入口与测试的替身）。
    async fn plugin_references(&self, _key: &str) -> Vec<PluginReference> {
        Vec::new()
    }
    /// `hooks.trustGrant`（§59.2，config.md §4.5）：写这一条声明的预信任；接受了就重读这个工作区的 hooks、替换内核的快照。
    /// 缺省拒绝（不服务预信任的入口与测试的替身）。
    async fn pretrust_hooks(
        &self,
        _key: &str,
        _request: HookPretrust,
    ) -> Result<(), HookPretrustRefusal> {
        Err(HookPretrustRefusal::WorkspaceHooksConfigUnreadable)
    }
    /// 读面 stat 一个本机附件（protocol-v5.md §41.4）：这个工作区的主 env 上这个绝对路径；不存在、不是普通文件 → `NotFound`。
    async fn stat_attachment(
        &self,
        _key: &str,
        _path: &str,
    ) -> Result<AttachmentFile, AttachmentFileError> {
        Err(AttachmentFileError::Unsupported)
    }
    /// 读面读一个本机附件的全部字节（§41.4）：同 [`Self::stat_attachment`] 的文件，超过 `limit` → `TooLarge`。
    async fn read_attachment(
        &self,
        _key: &str,
        _path: &str,
        _limit: u64,
    ) -> Result<Vec<u8>, AttachmentFileError> {
        Err(AttachmentFileError::Unsupported)
    }
}

/// 存储的启动事实（进程级 `agent` 的 `storage`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Storage {
    /// 开始打开会话库；`database_id` 是不带路径的不透明 ID。
    Checking {
        database_id: String,
    },
    Ready,
    /// `code` 是存储启动错误码（例如 `open_failed`）。
    Failed {
        code: String,
    },
}

pub(crate) enum Event {
    Storage(Storage),
    /// 进程级的部件建好了：之后可以组装工作区、读会话列表（存储查询是进程级的一份，全部工作区共用）。
    Ready {
        workspaces: Arc<dyn Workspaces>,
        queries: Arc<dyn StoreQueries>,
        /// 会话的 Artifact（上传的附件写进它、读面读它，protocol-v5.md §39）：与 `queries` 是同一个存储。
        artifacts: Arc<dyn ArtifactStore>,
    },
    Opened {
        key: String,
        generation: u64,
        result: Result<(), String>,
    },
    /// 读种子的任务读完一个工作区的会话列表（§14.4 的 ①②）。
    Seeded {
        key: String,
        result: Result<Seed, StoreError>,
    },
    /// 查询的任务做完一次查询（`sessions.facts` §14.6、插件引用目录 §59.1）：JSON-RPC 的 id 原样带回。
    Queried {
        reply: Value,
        result: Result<Value, Fault>,
    },
    /// 信任工作任务做完一条 `mcp.trust.set`（protocol-v5.md §16）。
    McpTrusted {
        reply: Value,
        key: String,
        result: Result<McpTrusted, McpTrustRefusal>,
    },
    /// 工作区级写入的工作任务做完一条 `sessionData.deletionExecute`（protocol-v5.md §65）：JSON-RPC 的 id、工作区、根会话与内核的结果。
    Deleted {
        reply: Value,
        key: String,
        root: String,
        result: Result<rust_cli_kernel_api::deletion::DeletionResult, String>,
    },
    /// 信任写入的工作任务做完一条 `hooks.trustGrant`（protocol-v5.md §59.2）。
    HooksPretrusted {
        reply: Value,
        result: Result<(), HookPretrustRefusal>,
    },
    /// 提交任务做完一个上传（protocol-v5.md §39.3）。
    AttachmentCommitted {
        upload: String,
        outcome: Result<crate::generated::attachments_v1::AttachmentCommitResult, CommitFailure>,
    },
    /// 读面任务做完一条 `attachment.read` / `stat`（§39.4）：JSON-RPC 的 id 原样带回。
    AttachmentRead {
        reply: Value,
        result: Result<Value, Fault>,
    },
    /// 读视图的任务做完一条没有物化的会话的只读查询（protocol-v5.md §40.2）、一次经内核的读或一页的重建：JSON-RPC 的 id 原样带回。
    SessionRead {
        reply: Value,
        result: Result<Value, Fault>,
    },
    /// `rows.range` 往前补的一段读回来了（protocol-v5.md §73.3）：挂进投影再答。
    SessionExtended(Box<crate::session::extend::Extended>),
    /// 通道的读做完一条要会话运行时的查询（§43.2）：带会话 id，表项上的在途数减一（在途的读挡着热会话的回收，§69.3）。
    RuntimeRead {
        sid: String,
        reply: Value,
        result: Result<Value, Fault>,
    },
    /// 查询的任务做完一次 `slash.commands`（protocol-v5.md §60）：JSON-RPC 的 id 原样带回。
    SlashCommands {
        reply: Value,
        result: Result<Value, Fault>,
    },
    /// 插件管理的读与写任务回来了（protocol-v5.md §72.2）。
    Plugins(Box<crate::plugins::PluginEvent>),
    /// 一个会话的通道来的消息（protocol-v5.md §15.1；`generation` 区分新旧通道）。
    Lane {
        sid: String,
        generation: u64,
        event: Box<crate::session::lane::LaneEvent>,
    },
    /// 取凭据的任务要发一个 `host.*` 请求（protocol-v5.md §30；进程级的，§74.1）：actor 写出、交回等待，不等回复。
    Service {
        target: crate::host_services::ServiceTarget,
        service: &'static str,
        params: Value,
        reply: crate::host_services::Reply,
    },
    /// 取凭据的任务要用户完成官方版本的请求安全校验（§35）：会话的投影开交互，应答经应答口回到任务。
    Verify(Box<crate::session::verify::Verify>),
    /// 取凭据的任务放弃了（回合停了、到了截止时刻）：交互还在就撤回（§31.2）。
    Withdraw {
        session: String,
        id: String,
    },
    /// `mcp.oauth.*` 的任务做完了（§74.2）：直接写出回复。
    McpOAuthDone {
        reply: Value,
        result: Result<Value, Fault>,
    },
}

/// 入口报告启动事实的句柄。
#[derive(Clone)]
pub struct Handle {
    events: mpsc::UnboundedSender<Event>,
    accounts: ExternalAccounts,
}

impl Handle {
    pub fn storage(&self, storage: Storage) {
        let _ = self.events.send(Event::Storage(storage));
    }

    pub fn ready(
        &self,
        workspaces: Arc<dyn Workspaces>,
        queries: Arc<dyn StoreQueries>,
        artifacts: Arc<dyn ArtifactStore>,
    ) {
        let _ = self.events.send(Event::Ready {
            workspaces,
            queries,
            artifacts,
        });
    }

    /// Host 下发的外部账号层（protocol-v5.md §36）：入口交给平台层（`Controller::app`），`account.overlay.set` 写它。
    pub fn accounts(&self) -> ExternalAccounts {
        self.accounts.clone()
    }

    /// 经 Host 取账号凭据的请求头来源（protocol-v5.md §30）：入口交给平台层（`Controller::app`）。`clock` 给需要用户时交互的截止时刻
    /// （`expiresAt`，§31.2）。
    pub fn credentials(&self, clock: Arc<dyn Clock>) -> Arc<dyn HeaderSource> {
        crate::host_services::HostCredentials::source(self.events.clone(), clock)
    }

    /// 经 Host 加解密凭据的值（protocol-v5.md §74、`host.credentials.cipher`）：入口交给 MCP OAuth 的凭据库（委托模式，platform.md §13.13）。
    pub fn cipher(&self) -> Arc<dyn rust_cli_kernel_api::credential_cipher::CredentialCipher> {
        crate::host_services::HostCipher::port(self.events.clone())
    }
}

/// 起 actor：返回入口的句柄、读循环的入口（[`rust_cli_protocol_core::connection::serve_in_order`] 的 `intake`）与任务。
/// 读循环结束（对端关掉输入）或对端说了再见时任务结束。
pub fn start(
    peer: Peer,
    settings: Settings,
    spawner: &Spawner,
) -> (Handle, mpsc::Sender<Inbound>, Task<()>) {
    let (intake, inbound) = mpsc::channel(INTAKE_QUEUE);
    let (events, receiver) = mpsc::unbounded_channel();
    let actor = Actor::new(peer, settings, spawner, events.clone());
    let accounts = actor.accounts.clone();
    let task = spawner.spawn("protocol-v5", actor.run(inbound, receiver));
    (Handle { events, accounts }, intake, task)
}

pub(crate) struct Actor {
    pub(crate) peer: Peer,
    pub(crate) settings: Settings,
    /// 本进程的 epoch（所有 topic 共用；进程重启即换）。
    pub(crate) epoch: String,
    pub(crate) connection: String,
    /// 握手之后的协商结果；之前为 None。
    pub(crate) negotiated: Option<Negotiated>,
    /// 这条链路严格读（03 §3.8）：initialize 带了契约指纹且相等。
    pub(crate) strict: bool,
    /// Host 在握手里告知它应答的 `host.*` 服务（`initialize.hostServices`，protocol-v5.md §30）；没告知的不发。
    pub(crate) host_services: Vec<String>,
    /// shutdown 之后（03 §3.4）：不再接受请求。
    closing: bool,
    pub(crate) topics: BTreeMap<String, Publisher>,
    pub(crate) process: Process,
    /// 工作区（键是解码后的工作区路径）。
    pub(crate) workspaces: BTreeMap<String, Workspace>,
    /// 会话列表：等进程级部件的工作区，与已物化的工作区的变化流（§14）。
    pub(crate) feeds: Feeds,
    /// 会话领域：每个会话的投影与通道（§15）。
    pub(crate) sessions: Sessions,
    /// 信任记录的写入（§16 的 `mcp.trust.set`、§59.2 的 `hooks.trustGrant`）：本链路唯一的工作任务与在途的条数。
    pub(crate) trusts: TrustWrites,
    /// 外部账号层的格子（§36）：`account.overlay.set` 写，入口交给平台层读。
    pub(crate) accounts: ExternalAccounts,
    /// 附件的上传与读面（§39）：暂存、已提交的上传、在途的任务数。
    pub(crate) attachments: Uploads,
    /// `/` 面板的命令目录（§60）：在途的查询数。
    pub(crate) slash: SlashReads,
    /// 插件管理（§72）：端口、两种 topic 的读、写操作。
    pub(crate) plugins: crate::plugins::Plugins,
    /// `shutdown` 来时还有会话命令在通道里：回复等它们的 ACK 都排进队列（§15.9 最后一行）。
    shutdown: Option<Value>,
    pub(crate) outbox: Outbox,
    pub(crate) events: mpsc::UnboundedSender<Event>,
    pub(crate) spawner: Spawner,
    /// 对端发了 exit（或严格链路上解不出通知）：不再处理消息。
    finished: bool,
}

/// initialize 成功之前只允许 initialize 与 exit（03 §3.3）。
fn unauthorized() -> ProtocolError {
    protocol_error(
        codes::HANDSHAKE_UNAUTHORIZED,
        "only initialize and exit are allowed before initialize succeeds",
    )
}

/// 协议违约（没有副作用）：外壳解码失败（01 §10）、成功之后再 initialize、shutdown 之后的请求、严格链路上多余的键。
pub(crate) fn violation(message: impl Into<String>) -> Fault {
    Box::new(not_applied(protocol_error(
        codes::LINK_PROTOCOL_VIOLATION,
        message,
    )))
}

/// params 缺省是空对象（`ping` 可以不带 params）。
fn or_empty(params: Value) -> Value {
    if params.is_null() {
        Value::Object(serde_json::Map::new())
    } else {
        params
    }
}

/// 读成生成的类型（宽松：未知字段忽略、带降级的枚举进 Unrecognized）。严格链路上先过 [`Actor::check_strict`]。
pub(crate) fn decode<T: DeserializeOwned>(what: &str, params: Value) -> Result<T, Fault> {
    serde_json::from_value(or_empty(params))
        .map_err(|error| violation(format!("invalid {what}: {error}")))
}

impl Actor {
    /// 还没跑起来的 actor（进程级 `agent` 已登记）：`events` 是发给它的内部消息的入口。负载测量（`session/load_tests.rs`）直接用它，
    /// 一步一步地交消息。
    pub(crate) fn new(
        peer: Peer,
        settings: Settings,
        spawner: &Spawner,
        events: mpsc::UnboundedSender<Event>,
    ) -> Self {
        let epoch = format!("rs-{:016x}", rust_cli_base::random::u64());
        let mut actor = Self {
            peer,
            connection: format!("{epoch}-c"),
            process: Process::new(),
            plugins: crate::plugins::Plugins::new(settings.plugins.clone()),
            settings,
            epoch,
            negotiated: None,
            strict: false,
            host_services: Vec::new(),
            closing: false,
            topics: BTreeMap::new(),
            workspaces: BTreeMap::new(),
            feeds: Feeds::default(),
            sessions: Sessions::default(),
            trusts: TrustWrites::default(),
            accounts: ExternalAccounts::new(),
            attachments: Uploads::default(),
            slash: SlashReads::default(),
            shutdown: None,
            outbox: Outbox::new(INITIAL_CREDIT, MAX_MESSAGE_BYTES),
            events,
            spawner: spawner.clone(),
            finished: false,
        };
        actor.register_process_topic();
        actor
    }

    async fn run(
        mut self,
        mut inbound: mpsc::Receiver<Inbound>,
        mut events: mpsc::UnboundedReceiver<Event>,
    ) {
        loop {
            // 先处理已经到的内部消息（启动事实、组装完成），再处理会话事实的变化，最后处理对端的消息：对端的一条消息到时，
            // 此前已经提交的变化都已进了 feed（会话列表的测试靠 ping 定点，§14.8）
            tokio::select! {
                biased;
                Some(event) = events.recv() => self.on_event(event).await,
                Some((key, facts)) = self.feeds.changes.next() => self.fact_changed(&key, &facts),
                message = inbound.recv() => match message {
                    Some(message) => self.on_inbound(message).await,
                    None => break,
                },
            }
            // 一次处理一帧（01 §19.3，用户定 R2）：先处理完醒来这一刻已经到的内部事件，再写出站队列，每个订阅只拼一帧。
            // 只取这一刻队列里的条数、不等新的：生产者一直在推时对端的请求也轮得到；不按时间合并
            for _ in 0..events.len() {
                let Ok(event) = events.try_recv() else {
                    break;
                };
                self.on_event(event).await;
            }
            self.flush().await;
            if self.finished {
                break;
            }
        }
    }

    async fn on_inbound(&mut self, message: Inbound) {
        match message {
            Inbound::Request { id, method, params } => {
                if let Some(result) = self.on_request(&id, &method, params) {
                    self.peer.respond(&id, &result).await;
                }
            }
            Inbound::Notification { method, params } => self.on_notification(&method, params).await,
        }
    }

    /// 严格链路上按 core@1 的严格变体校验参数（03 §3.8）；宽松链路不校验，直接读。
    fn check_strict(&self, method: &str, params: &Value) -> Result<(), Fault> {
        if !self.strict {
            return Ok(());
        }
        strict::check(method, &or_empty(params.clone()))
            .map_err(|error| violation(format!("invalid {method} on a strict link: {error}")))
    }

    /// 严格链路上解不出的通知（03 §3.8）：通知不能单独拒绝，丢掉会静默少一份状态（少一格信用 → 订阅卡住），同一份契约
    /// 上重试也修不好。说明原因、不再处理消息；对端关掉输入后进程退出，交给链路的拥有者（Host 的进程管理）。
    async fn abort(&mut self, method: &str, error: &str) {
        tracing::error!(
            event = "protocol.strict.violation",
            method,
            error,
            "a notification does not decode on a strict link; closing it"
        );
        let exit = Exit {
            reason: ExitReason::ProtocolViolation,
        };
        let params = serde_json::to_value(exit).unwrap_or(Value::Null);
        self.peer.notify(contract::EXIT, &params).await;
        self.finished = true;
    }

    /// 一个请求。直接回复的返回结果；排进出站队列的（`subscribed`、命令的 ACK、shutdown 的回复）与交给任务的（读库，§4.1）返回
    /// None，结果回来时再回复。
    fn on_request(
        &mut self,
        id: &Value,
        method: &str,
        params: Value,
    ) -> Option<Result<Value, RpcError>> {
        if method == contract::INITIALIZE {
            return Some(self.initialize(params).map_err(|error| to_rpc(&error)));
        }
        let known = [
            contract::PING,
            contract::SUBSCRIBE,
            contract::UNSUBSCRIBE,
            contract::COMMAND,
            contract::QUERY,
            contract::SHUTDOWN,
        ];
        if !known.contains(&method) {
            let error = not_applied(protocol_error(
                codes::QUERY_UNSUPPORTED,
                format!("unknown method {method}"),
            ));
            let data = serde_json::to_value(&error).unwrap_or(Value::Null);
            return Some(Err(RpcError::method_not_found(method).with_data(data)));
        }
        if self.negotiated.is_none() {
            return Some(Err(to_rpc(&unauthorized())));
        }
        if self.closing {
            return Some(Err(to_rpc(&violation(
                "requests are not accepted after shutdown",
            ))));
        }
        if let Err(error) = self.check_strict(method, &params) {
            return Some(Err(to_rpc(&error)));
        }
        let outcome = match method {
            contract::PING => Ok(Some(Value::Object(Default::default()))),
            contract::SHUTDOWN => {
                self.shutdown(id);
                Ok(None)
            }
            contract::SUBSCRIBE => self.subscribe(id, params).map(|()| None),
            contract::UNSUBSCRIBE => self.unsubscribe(params).map(Some),
            contract::COMMAND => self.command(id, params).map(|()| None),
            _ => self.query(id, params),
        };
        match outcome {
            Ok(Some(result)) => Some(Ok(result)),
            Ok(None) => None,
            Err(error) => Some(Err(to_rpc(&error))),
        }
    }

    async fn on_notification(&mut self, method: &str, params: Value) {
        match method {
            // 03 §3.4：关之前的最后一句，任何时候都接受、读出什么都关；不回复。对端关掉输入后进程退出
            contract::EXIT => self.finished = true,
            // shutdown 之后照收：回复可能正排在没有信用的订阅后面（03 §3.4 ④）
            contract::CREDIT if self.negotiated.is_some() => {
                let checked = self
                    .check_strict(method, &params)
                    .and_then(|()| decode::<Credit>(method, params));
                match checked {
                    Ok(credit) => self.outbox.credit(&credit.sub, credit.n),
                    Err(error) if self.strict => self.abort(method, &error.message).await,
                    // 宽松链路：丢掉这一条（规则同 11 §5）
                    Err(_) => {}
                }
            }
            other => tracing::debug!(
                event = "protocol.notification.ignored",
                method = other,
                "ignored a notification"
            ),
        }
    }

    /// 排空（03 §3.4）：不再收新请求、不再挂新的帧标记；回复排在已排的一切之后。会话命令在通道里执行（§15.1）、`mcp.trust.set`
    /// 在信任工作任务里执行（§16）、读库在任务里（§4.1）：还有在途的时回复等它们的结果都回到 actor 再排（计数，不计时）；别的请求在
    /// actor 里同步处理完，这时都已回复或已排进队列。
    fn shutdown(&mut self, id: &Value) {
        self.closing = true;
        self.shutdown = Some(id.clone());
        // 插件的写操作：先取消，回复等它们到终态（§72.4）
        self.cancel_plugin_operations();
        self.finish_shutdown();
    }

    /// 在途的会话命令、信任写入与读库都回来了：关出站队列、排 shutdown 的回复。
    pub(crate) fn finish_shutdown(&mut self) {
        if self.sessions.in_flight > 0
            || self.sessions.reads > 0
            || self.trusts.in_flight > 0
            || self.attachments.in_flight > 0
            || self.slash.in_flight > 0
            || self.plugins.busy()
            || self.feeds.busy()
        {
            return;
        }
        if let Some(id) = self.shutdown.take() {
            self.outbox.close();
            self.outbox.reply(id, Ok(Value::Object(Default::default())));
        }
    }

    pub(crate) fn unsubscribe(&mut self, params: Value) -> Result<Value, Fault> {
        let unsubscribe: Unsubscribe = decode("unsubscribe", params)?;
        let topic = self.outbox.topic_of(&unsubscribe.sub).map(str::to_owned);
        // 未知的 sub 也回 {}（已经退订或订阅被拒）；队头可能正被它挡着，flush 会推进（审查 F08）
        self.outbox.unsubscribe(&unsubscribe.sub);
        // 会话 topic 少了一个订阅：没有别的持有者就释放（protocol-v5.md §66）
        if let Some(topic) = topic {
            self.left_topic(&topic);
        }
        Ok(Value::Object(Default::default()))
    }

    /// 一个查询：`sessions.facts` 读库、插件引用目录要工作区内核（§59.1），都交给任务（§4.1），回复等 [`Event::Queried`]；附件的读面（§39.4）同样在任务里读；会话的只读查询
    /// （§40、§43）按会话 topic 答，没有物化的在任务里读，要会话运行时的交给通道；不认识的类型直接回错误。
    fn query(&mut self, id: &Value, params: Value) -> Result<Option<Value>, Fault> {
        let query: QueryEnvelope = decode("query", params)?;
        match query.r#type.as_str() {
            SESSIONS_FACTS => {
                self.query_facts(id, query);
                Ok(None)
            }
            SLASH_COMMANDS => self.slash_commands(id, query).map(|()| None),
            crate::deletion::DELETION_PREVIEW => {
                self.deletion_preview(id, query);
                Ok(None)
            }
            kind if crate::attachments::is_attachment_query(kind) => {
                self.attachment_query(id, query)
            }
            kind if crate::plugin_references::is_reference_query(kind) => {
                self.plugin_references(id, query);
                Ok(None)
            }
            // MCP 的授权（§74）：只在入口装了授权时认它
            kind if self.settings.mcp_oauth && crate::mcp_oauth::is_query(kind) => {
                self.mcp_oauth_query(id, query).map(|()| None)
            }
            // 插件管理（§72）：只在入口交了端口时认它
            crate::plugins::PLUGINS_DESCRIBE if let Some(admin) = self.plugins.admin.clone() => {
                self.plugins_describe(admin, id, query);
                Ok(None)
            }
            other => match SessionQuery::of(other) {
                Some(kind) => self.session_query(id, kind, query),
                None => Err(Box::new(not_applied(protocol_error(
                    codes::QUERY_UNSUPPORTED,
                    format!("unknown query {other}"),
                )))),
            },
        }
    }

    /// 把出站队列里能写的写出去。
    pub(crate) async fn flush(&mut self) {
        for message in self.outbox.pump(&self.topics) {
            match message {
                Outgoing::Notify { method, params } => {
                    self.peer.notify(method, &params).await;
                }
                Outgoing::Frame { params } => {
                    self.peer.notify_text(FRAME, &params).await;
                }
                Outgoing::Respond { id, result } => {
                    self.peer.respond(&id, &result).await;
                }
            }
        }
    }

    async fn on_event(&mut self, event: Event) {
        match event {
            Event::Storage(storage) => self.storage(storage),
            Event::Ready {
                workspaces,
                queries,
                artifacts,
            } => {
                self.attachments.ready(artifacts);
                self.process_ready(workspaces.clone(), queries.clone());
                self.materialize_waiting(&(workspaces, queries));
                self.read_waiting();
            }
            Event::Seeded { key, result } => self.seeded(&key, result),
            Event::Queried { reply, result } => self.queried(reply, result).await,
            Event::McpOAuthDone { reply, result } => self.mcp_oauth_done(reply, result).await,
            Event::Opened {
                key,
                generation,
                result,
            } => self.opened(&key, generation, result),
            Event::McpTrusted { reply, key, result } => self.mcp_trusted(reply, &key, result),
            Event::HooksPretrusted { reply, result } => self.hooks_pretrusted(reply, result),
            Event::Deleted {
                reply,
                key,
                root,
                result,
            } => self.deleted(reply, &key, &root, result),
            Event::AttachmentCommitted { upload, outcome } => {
                self.attachment_committed(&upload, outcome);
            }
            Event::AttachmentRead { reply, result } => self.attachment_read(reply, result).await,
            Event::SessionRead { reply, result } => self.session_read(reply, result).await,
            Event::SessionExtended(extended) => self.session_extended(*extended).await,
            Event::RuntimeRead { sid, reply, result } => {
                self.runtime_read(&sid, reply, result).await;
            }
            Event::SlashCommands { reply, result } => self.slash_answered(reply, result).await,
            Event::Plugins(event) => self.plugins_event(*event),
            Event::Lane {
                sid,
                generation,
                event,
            } => self.on_lane(&sid, generation, *event),
            Event::Service {
                target,
                service,
                params,
                reply,
            } => self.service(target, service, params, reply).await,
            Event::Verify(verify) => self.verify(*verify),
            Event::Withdraw { session, id } => self.withdraw(&session, &id),
        }
    }
}

#[cfg(test)]
#[path = "server_tests.rs"]
mod tests;
