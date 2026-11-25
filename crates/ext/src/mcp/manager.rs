//! MCP 连接管理（docs/specs/rust-cli/ext.md §3.3）：每个服务器一个连接任务，状态与工具快照只在这里，
//! 注册表每步读当前快照；`list_changed` 重新拉取；HTTP / SSE 断开后退避重连。

use rust_cli_base::sync::LockUnpoisoned;
use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use rust_cli_base::Spawner;
use rust_cli_kernel_api::clock::Clock;
use rust_cli_kernel_api::env::WorkspaceRef;
use rust_cli_kernel_api::env_registry::EnvironmentRegistry;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::listing::ListingEntry;
use rust_cli_kernel_api::manage::{McpServerInfo, McpServerState};
use rust_cli_kernel_api::mcp::{McpAdmission, McpTransport, McpTransportConfig};
use rust_cli_kernel_api::tool::Tool;
use serde_json::Value;
use tokio::sync::{Notify, Semaphore, watch};
use tokio_util::sync::CancellationToken;

use super::config::{McpScope, McpServer, McpServerOptions, McpSettings};
use super::connection::{self, Session, has_capability};
use super::resources;
use super::rpc::{Client, Notice, Refusal};
use super::tool::{McpTool, hidden_from_model, name_too_long, note_long_names};

/// 断开后重连的次数与退避。
const MAX_RECONNECTS: u32 = 5;
const INITIAL_BACKOFF_MS: u64 = 1_000;
const MAX_BACKOFF_MS: u64 = 30_000;

/// 一个服务器的连接状态（ext.md §3.3）。附带的标记只在它所属的变体里，非法组合（例如停用的服务器“在重连”）写不出来。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum McpStatus {
    /// 连接中。
    Pending {
        /// 连上过、断开后在退避重连（HTTP / SSE）；直接调用时不等它（ext.md §3.8）。
        reconnecting: bool,
        /// 这次断开是 HTTP 会话过期（工具调用据此等重连、重试一次）。
        expired: bool,
    },
    /// 已连接。
    Connected {
        /// `-p` 里连上过的 stdio 服务器断开了，工具留着、等下次调用再连（ext.md §3.3.3）。
        dormant: bool,
    },
    Failed {
        reason: String,
    },
    /// 停用（配置里 `enabled: false`，或 `/mcp disable`，docs/specs/rust-cli/tui-manage.md §6）：不连接。
    Disabled,
    /// 工作区声明的服务器没有信任记录（App 形态的准入，docs/specs/rust-cli/ext.md §3.1 与 18-mcp-trust.md §12.4）：不连接，不重连。
    Untrusted,
    /// 远程服务器要（重新）授权（OAuth，ext.md §3.12.3）：不连接、不重连，等 Sign in。`scope` 是服务器 403 时要的更多权限，
    /// 下次授权并进请求的 scope（step-up）。
    NeedsAuth {
        scope: Option<String>,
    },
}

impl McpStatus {
    /// 首次连接（或 `/mcp reconnect`、启用）的连接中。
    pub const CONNECTING: Self = Self::Pending {
        reconnecting: false,
        expired: false,
    };
    /// 已连接、客户端可用。
    pub const CONNECTED: Self = Self::Connected { dormant: false };

    /// 界面与等待用的状态（不带附带的标记）。
    pub fn state(&self) -> McpServerState {
        match self {
            Self::Pending { .. } => McpServerState::Pending,
            Self::Connected { .. } => McpServerState::Connected,
            Self::Failed { .. } => McpServerState::Failed,
            Self::Disabled => McpServerState::Disabled,
            // 只有 App 形态会有它（App 没有 `/mcp`）：界面与等待按“没连上”报，原因在 `server_infos`（18 §12.4）
            Self::Untrusted => McpServerState::Failed,
            Self::NeedsAuth { .. } => McpServerState::NeedsAuth,
        }
    }

    fn connected(&self) -> bool {
        matches!(self, Self::Connected { .. })
    }

    fn pending(&self) -> bool {
        matches!(self, Self::Pending { .. })
    }
}

pub(crate) struct Server {
    config: McpServer,
    status: McpStatus,
    client: Option<Arc<Client>>,
    tools: Vec<Arc<dyn Tool>>,
    instructions: Option<String>,
    resources: bool,
    prompts: Vec<Value>,
    /// 叫醒等下次调用的连接任务。
    wake: Arc<Notify>,
    /// 连接任务的代号：重连、停用时加一；旧任务的状态更新因代号不符被丢弃（tui-manage.md §6.1）。
    generation: u64,
    /// 停掉当前连接任务的取消消息。
    cancel: CancellationToken,
}

impl Server {
    fn new(config: McpServer, status: McpStatus) -> Self {
        Self {
            config,
            status,
            client: None,
            tools: Vec::new(),
            instructions: None,
            resources: false,
            prompts: Vec::new(),
            wake: Arc::new(Notify::new()),
            generation: 0,
            cancel: CancellationToken::new(),
        }
    }
}

pub(crate) struct Inner {
    transport: Arc<dyn McpTransport>,
    /// 远端机器的注册表（R5）：绑定到远端的服务器从这里取那台机器的 `mcp()`（remote-exec.md §22.6）。
    machines: OnceLock<Arc<dyn EnvironmentRegistry>>,
    /// 工作区声明的准入（App 形态，18-mcp-trust.md §12.4）：项目级服务器连接之前问它；没有时照常连（TUI、`-p`）。
    pub(super) admission: OnceLock<Arc<dyn McpAdmission>>,
    /// 进程级的授权登记（OAuth，ext.md §3.12.6、§3.12.14）：TUI、`-p` 与 v5 的 App 有；没有时（入口不装它、测试）不能 Sign in。
    pub(super) sign_ins: OnceLock<Arc<McpSignIns>>,
    /// 问用户的端口与前端声明的询问（ext.md §3.13）：组装时给一次；没有时（只给测试的组装）什么都不声明。兄弟管理器共用这一份。
    pub(super) asking: OnceLock<Arc<super::asks::AskPorts>>,
    spawner: Spawner,
    settings: McpSettings,
    servers: Mutex<BTreeMap<String, Server>>,
    /// 每次状态变化加一；等待方据此醒来重新检查。兄弟管理器（[`McpManager::sibling`]，会话级的连接）共用这一个，
    /// 等一个集合的状态时一次订阅就够（工作线 F145）。
    changed: Arc<watch::Sender<u64>>,
    /// `start` 给的工作区（重连与启用时用）。
    workspace: Mutex<Option<WorkspaceRef>>,
    /// 等待的截止时刻用注入的时钟算（ext.md §3.3.1、§3.8）。
    clock: Arc<dyn Clock>,
    /// 主线程的请求带过 WaitForMcpServers 的会话（declared，ext.md §3.8）；会话关闭时去掉。
    declared: Mutex<HashSet<SessionId>>,
    /// 名字太长、记过日志的工具（每个名字只记一次，ext.md §3.4）。
    noted_names: Mutex<HashSet<String>>,
    /// print 会话（`-p`、stream-json）：连上过的 stdio 服务器断开时等下次调用再连（ext.md §3.3.3）。
    print: AtomicBool,
    /// 有 resources 能力的服务器连上过：之后三个资源工具一直在（ext.md §3.3.3）。
    resources_seen: AtomicBool,
}

impl Inner {
    fn servers(&self) -> MutexGuard<'_, BTreeMap<String, Server>> {
        self.servers.lock_unpoisoned()
    }

    /// 只改仍是这个代号的服务器：重连或停用之后，旧连接任务的更新不覆盖新的状态。
    fn update(&self, name: &str, generation: u64, change: impl FnOnce(&mut Server)) {
        if let Some(server) = self
            .servers()
            .get_mut(name)
            .filter(|server| server.generation == generation)
        {
            change(server);
        }
        self.changed.send_modify(|version| *version += 1);
    }

    pub(crate) fn settings(&self) -> &McpSettings {
        &self.settings
    }

    /// 这个服务器的选项（Node 配置的 `timeoutMs`、`alwaysLoad`，ext.md §3.1）；不认识的服务器用缺省值。
    pub(crate) fn options(&self, name: &str) -> McpServerOptions {
        self.servers()
            .get(name)
            .map(|server| server.config.options)
            .unwrap_or_default()
    }

    /// 已连接的服务器的客户端。
    pub(crate) fn client(&self, name: &str) -> Option<Arc<Client>> {
        self.servers()
            .get(name)
            .filter(|server| server.status.connected())
            .and_then(|server| server.client.clone())
    }

    /// `-p` 里断开、等下次调用的服务器：叫醒连接任务，等它重新连上（返回新的客户端）或失败（None）。不在等
    /// 下次调用时也是 None。
    pub(crate) async fn reconnect_dormant(&self, name: &str) -> Option<Arc<Client>> {
        let wake = {
            let servers = self.servers();
            let server = servers
                .get(name)
                .filter(|server| server.status == McpStatus::Connected { dormant: true })?;
            server.wake.clone()
        };
        wake.notify_one();
        let mut changes = self.changed.subscribe();
        loop {
            match self
                .servers()
                .get(name)
                .map(|server| (&server.status, &server.client))
            {
                Some((McpStatus::Connected { dormant: false }, Some(client))) => {
                    return Some(client.clone());
                }
                Some((McpStatus::Connected { .. }, _)) => {}
                Some((
                    McpStatus::Pending { .. }
                    | McpStatus::Failed { .. }
                    | McpStatus::Disabled
                    | McpStatus::Untrusted
                    | McpStatus::NeedsAuth { .. },
                    _,
                ))
                | None => return None,
            }
            if changes.changed().await.is_err() {
                return None;
            }
        }
    }

    pub(crate) fn expired(&self, name: &str) -> bool {
        self.servers()
            .get(name)
            .is_some_and(|server| matches!(server.status, McpStatus::Pending { expired: true, .. }))
    }

    /// 等服务器换上新客户端（重连成功）或不再重连。
    pub(crate) async fn wait_reconnected(&self, name: &str, old: &Arc<Client>) {
        let mut changes = self.changed.subscribe();
        loop {
            let settled = match self.servers().get(name) {
                Some(server) => match &server.status {
                    McpStatus::Connected { .. } => server
                        .client
                        .as_ref()
                        .is_some_and(|client| !Arc::ptr_eq(client, old)),
                    McpStatus::Failed { .. }
                    | McpStatus::Disabled
                    | McpStatus::Untrusted
                    | McpStatus::NeedsAuth { .. } => true,
                    McpStatus::Pending { .. } => false,
                },
                None => true,
            };
            if settled || changes.changed().await.is_err() {
                return;
            }
        }
    }

    async fn tools_of(self: &Arc<Self>, server: &str, client: &Client) -> Vec<Arc<dyn Tool>> {
        let options = self.options(server);
        let timeout = Duration::from_millis(self.settings.connect_timeout_for(options));
        match connection::list(client, "tools/list", "tools", timeout).await {
            Ok(listed) => {
                let tools: Vec<Arc<dyn Tool>> = listed
                    .iter()
                    .filter(|item| {
                        let hidden = hidden_from_model(item);
                        if hidden {
                            let tool = item.get("name").and_then(Value::as_str).unwrap_or_default();
                            tracing::debug!(
                                server,
                                tool,
                                "the MCP tool is kept from the model: its _meta.ui.visibility omits \"model\""
                            );
                        }
                        !hidden
                    })
                    .filter_map(|item| {
                        McpTool::from_listed(
                            server,
                            item,
                            options.always_load,
                            Arc::downgrade(self),
                        )
                    })
                    .map(|tool| Arc::new(tool) as Arc<dyn Tool>)
                    .collect();
                note_long_names(server, &tools, &self.noted_names);
                tools
            }
            Err(error) => {
                tracing::warn!(server, %error, "MCP tools/list failed");
                Vec::new()
            }
        }
    }

    async fn prompts_of(&self, server: &str, client: &Client) -> Vec<Value> {
        let timeout =
            Duration::from_millis(self.settings.connect_timeout_for(self.options(server)));
        connection::list(client, "prompts/list", "prompts", timeout)
            .await
            .unwrap_or_else(|error| {
                tracing::warn!(server, %error, "MCP prompts/list failed");
                Vec::new()
            })
    }

    /// 连上之后：拉取清单、发布，处理通知直到连接关闭；返回关闭原因与是否会话过期。
    async fn serve(
        self: &Arc<Self>,
        name: &str,
        generation: u64,
        session: Session,
    ) -> (Option<String>, bool) {
        let Session {
            client,
            mut notices,
            capabilities,
            instructions,
            protocol_version,
        } = session;
        let tools = if has_capability(&capabilities, "tools") {
            self.tools_of(name, &client).await
        } else {
            Vec::new()
        };
        let prompts = if has_capability(&capabilities, "prompts") {
            self.prompts_of(name, &client).await
        } else {
            Vec::new()
        };
        let resources = has_capability(&capabilities, "resources");
        if resources {
            self.resources_seen.store(true, Ordering::Relaxed);
        }
        let published = client.clone();
        self.update(name, generation, move |server| {
            server.status = McpStatus::CONNECTED;
            server.client = Some(published);
            server.tools = tools;
            server.prompts = prompts;
            server.instructions = instructions;
            server.resources = resources;
        });
        tracing::info!(server = name, protocol_version, "MCP server connected");
        while let Some(notice) = notices.recv().await {
            match notice {
                Notice::ListChanged(kind) if kind == "tools" => {
                    let tools = self.tools_of(name, &client).await;
                    self.update(name, generation, |server| server.tools = tools);
                }
                Notice::ListChanged(kind) if kind == "prompts" => {
                    let prompts = self.prompts_of(name, &client).await;
                    self.update(name, generation, |server| server.prompts = prompts);
                }
                // resources 不缓存清单：ListMcpResourcesTool 每次实时拉取。
                Notice::ListChanged(_) => {}
                Notice::Closed {
                    reason,
                    session_expired,
                } => return (reason, session_expired),
            }
        }
        (None, false)
    }

    /// 一个服务器的连接任务：收到取消消息（重连、停用）时停下（tui-manage.md §6.1）。`slots` 是 `start` 这一批的
    /// 并发上限（ext.md §3.3.4），只罩住首次连接；重连、`/mcp reconnect`、启用不经它（None）。
    pub(super) async fn run(
        self: Arc<Self>,
        workspace: WorkspaceRef,
        config: McpServer,
        (generation, slots): (u64, Option<Arc<Semaphore>>),
        cancel: CancellationToken,
    ) {
        tokio::select! {
            () = cancel.cancelled() => {}
            () = self.clone().admit_and_connect(workspace, config, generation, slots) => {}
        }
    }

    /// 先过工作区声明的准入（18-mcp-trust.md §12.4），再连接。
    ///
    /// 根因（MCP-TRUST 线跑过）：Node CLI 对工作区声明的 MCP 逐条要求信任，rust 以前没有这道门，App 里打开一个仓库就执行了仓库声明
    /// 的命令；止血时 App 形态干脆不连项目级服务器（18 §6）。修法：判定放在起进程的这一方，连接之前按将要执行的那份声明问注入的
    /// 准入端口；没有记录就不 spawn、不发请求，状态 `Untrusted`。只问这一次：之后的重连沿用这次结论（准入在连接开始时固定，同 Node）。
    async fn admit_and_connect(
        self: Arc<Self>,
        workspace: WorkspaceRef,
        config: McpServer,
        generation: u64,
        slots: Option<Arc<Semaphore>>,
    ) {
        let gate = self
            .admission
            .get()
            .filter(|_| config.scope == McpScope::Project);
        if let Some(admission) = gate
            && !admission.admits(&config.name).await
        {
            tracing::info!(
                event = "mcp.project.untrusted",
                server = config.name,
                "the workspace has not trusted this MCP server declaration; not connecting it"
            );
            self.update(&config.name, generation, |server| {
                server.status = McpStatus::Untrusted;
            });
            return;
        }
        self.connect(workspace, config, generation, slots).await;
    }

    /// 连接与重连（ext.md §3.3）：首次连接失败即失败；连上后断开时 stdio 失败，HTTP / SSE 退避重连。首次连接先取
    /// `slots` 的一个名额，连上（握手完成）或失败就还回去，之后拉清单不占名额（§3.3.4）。
    async fn connect(
        self: Arc<Self>,
        workspace: WorkspaceRef,
        config: McpServer,
        generation: u64,
        mut slots: Option<Arc<Semaphore>>,
    ) {
        let remote = !matches!(config.transport, McpTransportConfig::Stdio { .. });
        let timeout = Duration::from_millis(self.settings.connect_timeout_for(config.options));
        let name = config.name.clone();
        let mut connected_once = false;
        let mut attempts = 0;
        loop {
            let pipe = connection::Pipe {
                local: &self.transport,
                machines: self.machines.get(),
                workspace: &workspace,
            };
            // 只有第一次尝试取名额（`take`）；信号量不会关闭，取不到只在任务被取消时（随任务一起放下）。
            let slot = match slots.take() {
                Some(slots) => slots.acquire_owned().await.ok(),
                None => None,
            };
            // 这个授权目标的代号（ext.md §3.12.14）：结果是要授权、而这期间有过一次授权时直接再连一次
            let auth_generation = self.auth_generation(&config);
            let opened = connection::open(
                &pipe,
                &self.spawner,
                &config,
                timeout,
                (&self.settings.client_version, self.asking.get()),
            )
            .await;
            drop(slot);
            let reason = match opened {
                Ok(session) => {
                    connected_once = true;
                    attempts = 0;
                    let (reason, expired) = self.serve(&name, generation, session).await;
                    // `-p` 里 stdio 断开：工具留着，等下次调用再连（ext.md §3.3.3）。
                    if !remote && self.print.load(Ordering::Relaxed) {
                        let mut wake = None;
                        self.update(&name, generation, |server| {
                            server.status = McpStatus::Connected { dormant: true };
                            server.client = None;
                            wake = Some(server.wake.clone());
                        });
                        let Some(wake) = wake else { return };
                        tracing::info!(
                            server = name,
                            "MCP server closed; reconnecting on the next call"
                        );
                        wake.notified().await;
                        continue;
                    }
                    self.update(&name, generation, |server| {
                        // stdio 不重连：下面马上记为失败。
                        server.status = McpStatus::Pending {
                            reconnecting: remote,
                            expired,
                        };
                        server.client = None;
                        server.tools.clear();
                    });
                    reason.unwrap_or_else(|| "connection closed".to_owned())
                }
                // 修 bug（竞态，ext.md §3.12.14）：这次连接没带令牌、它的刷新在发布之前读过文件，状态却在授权的通知之后才写成
                // NeedsAuth，通知那一刻它还是 Pending、没被重连，就一直停在要授权。代号变了说明这期间授权过：再连一次（下一轮记下
                // 新的代号，所以只一次），不进 NeedsAuth。
                Err(Refusal::AuthRequired { .. })
                    if self.auth_generation(&config) != auth_generation =>
                {
                    tracing::info!(
                        event = "mcp.oauth.signed_in_meanwhile",
                        server = name,
                        "the MCP server was signed in while connecting; connecting again"
                    );
                    continue;
                }
                // 要授权（OAuth，ext.md §3.12.3）：不退避重连（重连也是同一个 401），等 Sign in 或 `/mcp reconnect`。
                Err(Refusal::AuthRequired { scope }) => {
                    tracing::info!(
                        event = "mcp.oauth.needs_auth",
                        server = name,
                        "MCP server needs authorization"
                    );
                    let stubs = self.auth_stubs(&config);
                    self.update(&name, generation, |server| {
                        server.status = McpStatus::NeedsAuth { scope };
                        server.client = None;
                        server.tools = stubs;
                    });
                    return;
                }
                Err(Refusal::Failed(reason)) => reason,
            };
            if !remote || !connected_once || attempts >= MAX_RECONNECTS {
                tracing::warn!(
                    server = name,
                    reason = logged_reason(&reason),
                    "MCP server failed"
                );
                self.update(&name, generation, |server| {
                    server.status = McpStatus::Failed { reason };
                    server.client = None;
                    server.tools.clear();
                });
                return;
            }
            let backoff = (INITIAL_BACKOFF_MS << attempts).min(MAX_BACKOFF_MS);
            attempts += 1;
            tracing::info!(
                server = name,
                attempts,
                backoff,
                "reconnecting the MCP server"
            );
            tokio::time::sleep(Duration::from_millis(backoff)).await;
        }
    }
}

/// 一次 `start` 的首次连接名额（ext.md §3.3.4，按对照实现）：stdio 是本机，
/// 别的传输是远端；alwaysLoad 的服务器另有一套（对照实现的 headless 里它们单独一组），不被慢的普通服务器挡住。
struct Slots {
    local: Arc<Semaphore>,
    remote: Arc<Semaphore>,
    always_local: Arc<Semaphore>,
    always_remote: Arc<Semaphore>,
}

impl Slots {
    fn new(batch: super::config::McpBatch) -> Self {
        let pool = |size: usize| Arc::new(Semaphore::new(size.max(1)));
        Self {
            local: pool(batch.local),
            remote: pool(batch.remote),
            always_local: pool(batch.local),
            always_remote: pool(batch.remote),
        }
    }

    fn of(&self, config: &McpServer) -> Arc<Semaphore> {
        let local = matches!(config.transport, McpTransportConfig::Stdio { .. });
        match (config.options.always_load, local) {
            (false, true) => self.local.clone(),
            (false, false) => self.remote.clone(),
            (true, true) => self.always_local.clone(),
            (true, false) => self.always_remote.clone(),
        }
    }
}

/// MCP 连接管理。
pub struct McpManager(Arc<Inner>);

impl McpManager {
    pub fn new(
        transport: Arc<dyn McpTransport>,
        spawner: Spawner,
        settings: McpSettings,
        clock: Arc<dyn Clock>,
    ) -> Self {
        let (changed, _) = watch::channel(0);
        Self(Arc::new(Inner {
            transport,
            machines: OnceLock::new(),
            admission: OnceLock::new(),
            sign_ins: OnceLock::new(),
            asking: OnceLock::new(),
            spawner,
            settings,
            servers: Mutex::default(),
            changed: Arc::new(changed),
            workspace: Mutex::default(),
            clock,
            declared: Mutex::default(),
            noted_names: Mutex::default(),
            print: AtomicBool::new(false),
            resources_seen: AtomicBool::new(false),
        }))
    }

    /// 远端机器的注册表（R5，remote-exec.md §22.6）：`start` 之前给一次；绑定到远端机器的服务器从它取管道。F98（§25.5）：
    /// 同时跟着它的连接变化，机器进注册表时重连绑定到它的失败的服务器。
    pub fn set_machines(&self, machines: Arc<dyn EnvironmentRegistry>) {
        if self.0.machines.set(machines.clone()).is_err() {
            return;
        }
        let manager = McpManager(self.0.clone());
        self.0.spawner.spawn_resident("mcp-machines", async move {
            manager.follow_machines(machines).await;
        });
    }

    /// 工作区声明的准入（App 形态，18-mcp-trust.md §12.4）：`start` 之前给一次；之后项目级服务器连接之前问它。
    pub fn set_admission(&self, admission: Arc<dyn McpAdmission>) {
        let _ = self.0.admission.set(admission);
    }

    /// 问用户的端口（ext.md §3.13.1）：`start` 之前给一次。
    pub fn set_asking(&self, asking: Arc<super::asks::AskPorts>) {
        let _ = self.0.asking.set(asking);
    }

    /// 会话释放：它的 sampling 记录去掉（ext.md §3.14.1）。
    pub fn asking_released(&self, session: &SessionId) {
        if let Some(asking) = self.0.asking.get() {
            asking.released(session);
        }
    }

    /// print 会话（`-p`、stream-json）：连上过的 stdio 服务器断开时不记失败，等下次调用再连（ext.md §3.3.3）。
    pub fn set_print_session(&self, print: bool) {
        self.0.print.store(print, Ordering::Relaxed);
    }

    /// 开始连接（每个服务器一个常驻任务，不等待）。同名的服务器只连一次。这一批的首次连接按 [`Slots`] 限制同时进行的数量
    /// （ext.md §3.3.4）。
    pub fn start(&self, workspace: WorkspaceRef, servers: Vec<McpServer>) {
        *self.0.workspace.lock_unpoisoned() = Some(workspace.clone());
        let slots = Slots::new(self.0.settings.batch);
        for config in servers {
            let cancel = {
                let mut known = self.0.servers();
                if known.contains_key(&config.name) {
                    continue;
                }
                let server = Server::new(config.clone(), McpStatus::CONNECTING);
                let cancel = server.cancel.clone();
                known.insert(config.name.clone(), server);
                cancel
            };
            let slots = slots.of(&config);
            let task = self
                .0
                .clone()
                .run(workspace.clone(), config, (0, Some(slots)), cancel);
            self.0.spawner.spawn_resident("mcp-connection", task);
        }
        self.0.changed.send_modify(|version| *version += 1);
    }

    /// 有 resources 能力的服务器时先是三个资源工具，然后是已连接的服务器的工具（按名字排序）。
    pub fn tools(&self) -> Vec<Arc<dyn Tool>> {
        tools_of(&self.0, &self.0.servers())
    }

    pub fn find(&self, name: &str) -> Option<Arc<dyn Tool>> {
        // 旧名按 kernel-api 的旧名表也能找到（资源工具的 `ListMcpResources` 等，与内置工具的查找相同）。
        self.tools()
            .into_iter()
            .find(|tool| rust_cli_kernel_api::tool::answers_to(tool.as_ref(), name))
    }

    /// 已连接、有 instructions 的服务器，按名字排序：`## <server>\n<instructions>`（ext.md §3.6）。
    pub fn instructions(&self) -> Vec<ListingEntry> {
        instructions_of(&self.0.servers())
    }

    /// 每个服务器的状态（按名字排序）。
    #[cfg(test)]
    pub(crate) fn status(&self) -> Vec<(String, McpStatus)> {
        self.0
            .servers()
            .values()
            .map(|server| (server.config.name.clone(), server.status.clone()))
            .collect()
    }

    /// `/mcp` 与 `/status` 的快照（docs/specs/rust-cli/tui-display.md §3.4）：状态、失败原因、已连接时的工具数。
    pub fn server_infos(&self) -> Vec<McpServerInfo> {
        self.0
            .servers()
            .values()
            .map(|server| {
                let reason = match &server.status {
                    McpStatus::Failed { reason } => Some(reason.clone()),
                    McpStatus::Untrusted => Some(untrusted_reason(&server.config.name)),
                    McpStatus::Pending { .. }
                    | McpStatus::Connected { .. }
                    | McpStatus::Disabled
                    | McpStatus::NeedsAuth { .. } => None,
                };
                McpServerInfo {
                    name: server.config.name.clone(),
                    state: server.status.state(),
                    reason,
                    // 要授权的服务器的两个授权伪工具不算它的工具（ext.md §3.12.16）
                    tools: if server.status.connected() {
                        server.tools.len()
                    } else {
                        0
                    },
                    machine: server.config.machine.clone(),
                    // 有没有令牌要读凭据文件，由 `server_infos_with_auth` 补（manager_auth.rs）。
                    auth: None,
                }
            })
            .collect()
    }

    /// 服务器的 prompts（`prompts/list` 的原样结果）。
    pub fn prompts(&self, server: &str) -> Vec<Value> {
        self.0
            .servers()
            .get(server)
            .map(|server| server.prompts.clone())
            .unwrap_or_default()
    }

    /// 等全部服务器离开 `Pending`（连上或失败）。
    pub async fn wait_ready(&self) {
        let mut changes = self.0.changed.subscribe();
        loop {
            let pending = self
                .0
                .servers()
                .values()
                .any(|server| server.status.pending());
            if !pending || changes.changed().await.is_err() {
                return;
            }
        }
    }
}

/// 有 resources 能力的服务器时先是三个资源工具，然后是已连接的服务器的工具（按名字排序）。`servers` 是调用方
/// 持有的锁里的表：快照（manager_state.rs）与工具、状态在同一把锁里取。这是给模型的工具表：名字太长的工具不在里面
/// （主线程、子代理、ToolSearch 与直接调用都从这里取，ext.md §3.4）；`/mcp` 的工具数仍按服务器列出的全部工具。
fn tools_of(inner: &Arc<Inner>, servers: &BTreeMap<String, Server>) -> Vec<Arc<dyn Tool>> {
    let mut tools: Vec<Arc<dyn Tool>> = if has_resources(inner, servers) {
        resources::tools(vec![Arc::downgrade(inner)])
    } else {
        Vec::new()
    };
    let mut own = server_tools(servers);
    own.sort_by(|left, right| left.name().cmp(right.name()));
    tools.extend(own);
    tools
}

/// 资源工具在不在：有 resources 能力的服务器连上过一次之后一直在（服务器断开不撤回，ext.md §3.3.3）。
pub(crate) fn has_resources(inner: &Inner, servers: &BTreeMap<String, Server>) -> bool {
    inner.resources_seen.load(Ordering::Relaxed)
        || servers
            .values()
            .any(|server| server.status.connected() && server.resources)
}

/// 已连接的服务器的工具（没排序；名字太长的不在里面）。
pub(crate) fn server_tools(servers: &BTreeMap<String, Server>) -> Vec<Arc<dyn Tool>> {
    servers
        .values()
        // 要授权的服务器只有它的两个授权伪工具（交互会话，ext.md §3.12.16）
        .filter(|server| {
            server.status.connected() || matches!(server.status, McpStatus::NeedsAuth { .. })
        })
        .flat_map(|server| server.tools.iter())
        .filter(|tool| !name_too_long(tool.name()))
        .cloned()
        .collect()
}

/// 已连接、有 instructions 的服务器，按名字排序：`## <server>\n<instructions>`（ext.md §3.6）。
pub(crate) fn instructions_of(servers: &BTreeMap<String, Server>) -> Vec<ListingEntry> {
    servers
        .values()
        .filter(|server| server.status.connected())
        .filter_map(|server| {
            let text = server.instructions.as_ref()?;
            Some(ListingEntry {
                name: server.config.name.clone(),
                line: format!("## {}\n{text}", server.config.name),
            })
        })
        .collect()
}

/// 已连接、有 resources 能力的服务器与它们的客户端（资源工具用）。
pub(crate) fn resource_servers(inner: &Inner) -> Vec<(String, Arc<Client>)> {
    inner
        .servers()
        .values()
        .filter(|server| server.status.connected() && server.resources)
        .filter_map(|server| Some((server.config.name.clone(), server.client.clone()?)))
        .collect()
}

#[path = "manager_control.rs"]
mod control;

#[path = "manager_state.rs"]
mod state;

#[path = "manager_set.rs"]
mod set;

#[path = "manager_auth.rs"]
mod auth;
#[path = "auth_tool.rs"]
mod auth_tool;
#[path = "sign_ins.rs"]
mod sign_ins;
pub use set::McpSet;
pub use sign_ins::McpSignIns;
pub(crate) use sign_ins::{NO_AUTHORIZER, SIGN_IN_ENDED, settled};

/// 失败原因写进日志的部分：前 300 个字符（与 `tool.failed` 相同，logging.md §4）。
///
/// 缺陷修复 R2-14：原来整段按 warn 记日志；HTTP 错误的原因带最多 64 KiB 的响应体（net
/// `ERROR_BODY_LIMIT`）。界面上的原因（`McpStatus::Failed`）不截断。
///
/// 截取只定义在 base 的 `log_text::head`（F58：以前这里与 query 各写一份）。
pub(crate) fn logged_reason(reason: &str) -> String {
    rust_cli_base::log_text::head(reason)
}

/// `Untrusted` 的服务器在 `/status` 与等待里报的原因（18-mcp-trust.md §12.4）。
fn untrusted_reason(server: &str) -> String {
    format!("MCP server \"{server}\" is not trusted in this workspace")
}

#[cfg(test)]
#[path = "manager_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "manager_names_tests.rs"]
mod names_tests;

#[cfg(test)]
#[path = "manager_dormant_tests.rs"]
mod dormant_tests;

#[cfg(test)]
#[path = "manager_machines_tests.rs"]
mod machines_tests;

#[cfg(test)]
#[path = "manager_batch_tests.rs"]
mod batch_tests;
