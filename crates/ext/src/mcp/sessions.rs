//! 会话级的 MCP 连接（工作线 F145，docs/specs/rust-cli/ext.md §3.9）：`McpSharing::PerSession` 时，没有声明
//! `isolation: "workspace"` 的服务器每个顶层会话一份。会话 actor 启动时建（[`McpSessions::opened`]），会话释放时
//! 拿走、断开（[`McpSessions::released`]）；不用计时器。`Process`（TUI、`-p`）时全部服务器在内核级，这里什么都不做。

use rust_cli_base::sync::LockUnpoisoned;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use futures_util::FutureExt;
use rust_cli_base::Spawner;
use rust_cli_kernel_api::env::WorkspaceRef;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::manage::{McpOAuthServer, McpServerInfo, McpSignIn, McpSignInOutcome};
use rust_cli_kernel_api::mcp_auth::McpAuthTarget;

use super::manager::{NO_AUTHORIZER, SIGN_IN_ENDED, settled};
use super::{McpIsolation, McpManager, McpServer, McpSet, McpSharing};

/// 会话级的服务器与每个顶层会话那一份连接的表。
pub struct McpSessions {
    shared: Arc<McpManager>,
    sharing: McpSharing,
    /// 会话级的服务器与它们的工作区：内核开始连接时（`Ext::start`）定下一次。
    servers: OnceLock<(WorkspaceRef, Vec<McpServer>)>,
    owned: Mutex<HashMap<SessionId, Arc<McpManager>>>,
    /// 释放时关闭连接的任务在这里起（内核的作用域）。
    spawner: Spawner,
}

impl McpSessions {
    pub fn new(shared: Arc<McpManager>, sharing: McpSharing, spawner: Spawner) -> Self {
        Self {
            shared,
            sharing,
            servers: OnceLock::new(),
            owned: Mutex::default(),
            spawner,
        }
    }

    /// 内核开始连接时：`PerSession` 下把会话级的服务器记下，返回内核级要连的（声明 `workspace` 的）；`Process` 下全部在
    /// 内核级。
    pub fn split(&self, workspace: &WorkspaceRef, servers: Vec<McpServer>) -> Vec<McpServer> {
        if self.sharing == McpSharing::Process {
            return servers;
        }
        let (shared, owned): (Vec<_>, Vec<_>) = servers
            .into_iter()
            .partition(|server| server.options.isolation == McpIsolation::Workspace);
        if self.servers.set((workspace.clone(), owned)).is_err() {
            tracing::error!("the session MCP servers were split twice; keeping the first");
        }
        shared
    }

    /// 顶层会话 `root` 的运行看到的连接：内核级的，加上它自己的（有时）。
    pub fn set(&self, root: &SessionId) -> Arc<McpSet> {
        let own = self.owned.lock_unpoisoned().get(root).cloned();
        Arc::new(McpSet::new(self.shared.clone(), own))
    }

    /// 内核级的连接（没有会话的那一份）。
    pub fn kernel_set(&self) -> Arc<McpSet> {
        Arc::new(McpSet::new(self.shared.clone(), None))
    }

    /// 会话 actor 启动：建它的那一份并开始连接（不等）。同一个会话再次启动（关闭之后重开）时是新的一份。
    pub fn opened(&self, session: &SessionId) {
        let Some((workspace, servers)) = self.servers.get() else {
            return;
        };
        if servers.is_empty() {
            return;
        }
        let mut owned = self.owned.lock_unpoisoned();
        if owned.contains_key(session) {
            return;
        }
        let manager = Arc::new(self.shared.sibling());
        manager.start(workspace.clone(), servers.clone());
        tracing::info!(event = "mcp.session.opened", session = %session, servers = servers.len(), "session MCP connections started");
        owned.insert(session.clone(), manager);
    }

    /// 会话释放：从表里拿走它的那一份，在内核的作用域里断开（不阻塞会话的关闭）。
    pub fn released(&self, session: &SessionId) {
        let Some(manager) = self.owned.lock_unpoisoned().remove(session) else {
            return;
        };
        tracing::info!(event = "mcp.session.released", session = %session, "session MCP connections closing");
        drop(self.spawner.spawn("mcp-session-close", async move {
            manager.shutdown().await;
        }));
    }

    /// 打开着的会话级连接有几份（测试与诊断）。
    pub fn open_sets(&self) -> usize {
        self.owned.lock_unpoisoned().len()
    }

    /// 内核级的与每个会话的管理器。
    fn managers(&self) -> Vec<Arc<McpManager>> {
        let mut managers = vec![self.shared.clone()];
        managers.extend(self.owned.lock_unpoisoned().values().cloned());
        managers
    }

    /// 会话级的这个名字的配置（`PerSession` 下内核开始连接时记下的，ext.md §3.9）。
    fn session_config(&self, name: &str) -> Option<McpServer> {
        let (_, servers) = self.servers.get()?;
        servers.iter().find(|server| server.name == name).cloned()
    }

    /// 这个名字的授权目标：内核级的服务器问内核级的管理器，会话级的按记下的配置算（ext.md §3.12.9：设置页不开会话也能授权）。
    fn auth_target(&self, name: &str) -> Option<McpAuthTarget> {
        self.shared.auth_target_of(name).or_else(|| {
            let config = self.session_config(name)?;
            if config.machine.remote().is_some() {
                return None;
            }
            config.transport.auth_target(&config.name)
        })
    }

    fn not_oauth(name: &str) -> String {
        format!("MCP server \"{name}\" does not sign in with OAuth")
    }

    /// 这套内核里这个名字的状态（内核级的先；会话级的取第一个有它的会话）。
    async fn server_info(&self, name: &str) -> Option<McpServerInfo> {
        for manager in self.managers() {
            if let Some(info) = manager.server_info(name).await {
                return Some(info);
            }
        }
        None
    }

    /// 开始一次交互授权（`Kernel::mcp_sign_in`；TUI 与 App 同一条路，ext.md §3.12.6、§3.12.9）：用户发起的，丢下这个目标进行中的那一次。
    /// 上次 403 要的更多权限（任何一个连接的）并进这次的 scope。`done` 等结局，给出这套内核里它之后的状态。
    pub async fn sign_in(self: &Arc<Self>, name: &str) -> Result<McpSignIn, String> {
        let sign_ins = self.shared.sign_ins().ok_or(NO_AUTHORIZER)?.clone();
        let target = self
            .auth_target(name)
            .ok_or_else(|| Self::not_oauth(name))?;
        let scope = self
            .managers()
            .iter()
            .find_map(|manager| manager.needs_auth_scope(name))
            .flatten();
        let started = sign_ins.begin(target, scope, false, &self.spawner).await?;
        let sessions = self.clone();
        let name = name.to_owned();
        let done = async move {
            match settled(started.outcome).await {
                McpSignInOutcome::SignedIn => Ok(sessions.server_info(&name).await),
                McpSignInOutcome::Failed(reason) => Err(reason),
                McpSignInOutcome::Replaced | McpSignInOutcome::Ended => {
                    Err(SIGN_IN_ENDED.to_owned())
                }
            }
        };
        Ok(McpSignIn {
            url: started.url,
            done: done.boxed(),
        })
    }

    /// 一次交互授权（按地址认）的结局（App 的 `mcp.oauth.signInOutcome`，ext.md §3.12.14）：还在进行就等它；已经不在进行时 `Ended`。
    pub async fn sign_in_outcome(&self, name: &str, url: &str) -> Result<McpSignInOutcome, String> {
        let sign_ins = self.shared.sign_ins().ok_or(NO_AUTHORIZER)?.clone();
        let target = self
            .auth_target(name)
            .ok_or_else(|| Self::not_oauth(name))?;
        Ok(match sign_ins.outcome_of(&target, url) {
            Some(outcome) => settled(outcome).await,
            None => McpSignInOutcome::Ended,
        })
    }

    /// Sign out（`Kernel::mcp_sign_out`）：删掉令牌，这个进程里这个目标的连接重连（ext.md §3.12.14），给出这套内核里它之后的状态。
    pub async fn sign_out(&self, name: &str) -> Result<Option<McpServerInfo>, String> {
        let sign_ins = self.shared.sign_ins().ok_or(NO_AUTHORIZER)?.clone();
        let target = self
            .auth_target(name)
            .ok_or_else(|| Self::not_oauth(name))?;
        sign_ins.sign_out(&target).await?;
        Ok(self.server_info(name).await)
    }

    /// 这套内核里用 OAuth 的服务器的授权状态（App 的 `mcp.oauth.servers`，ext.md §3.12.9）：内核级的与会话级的配置，按名字排。
    /// 有没有令牌只看键（不解密）；要授权看这套内核里的连接。没有授权登记时没有。
    pub async fn oauth_servers(&self) -> Vec<McpOAuthServer> {
        let Some(sign_ins) = self.shared.sign_ins().cloned() else {
            return Vec::new();
        };
        let mut names: Vec<String> = self
            .shared
            .server_infos()
            .into_iter()
            .map(|info| info.name)
            .collect();
        if let Some((_, servers)) = self.servers.get() {
            names.extend(servers.iter().map(|server| server.name.clone()));
        }
        names.sort();
        names.dedup();
        let managers = self.managers();
        let mut out = Vec::new();
        for name in names {
            let Some(target) = self.auth_target(&name) else {
                continue;
            };
            out.push(McpOAuthServer {
                signed_in: sign_ins.authorizer().signed_in(&target).await,
                needs_auth: managers
                    .iter()
                    .any(|manager| manager.needs_auth_scope(&name).is_some()),
                pending_url: sign_ins.pending_url(&target),
                name,
            });
        }
        out
    }
}
