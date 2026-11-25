//! 远程服务器的 OAuth 在连接管理里的部分（docs/specs/rust-cli/ext.md §3.12）：要授权的状态、交互授权完成后重连、Sign out、
//! `/mcp` 看的有没有令牌。令牌本身不在这里（授权端口背后的凭据文件）；连接的唯一 owner 仍是连接任务。

use std::sync::Arc;

use rust_cli_kernel_api::manage::{McpAction, McpAuthState, McpServerInfo};
use rust_cli_kernel_api::mcp_auth::McpAuthTarget;
use tokio_util::sync::CancellationToken;

use rust_cli_kernel_api::tool::Tool;

use super::sign_ins::AuthChange;
use super::{Client, Inner, McpManager, McpServer, McpSignIns, McpStatus};

impl Inner {
    /// 停掉当前连接任务并换成 `status`（重连、停用、要授权共用）：代号加一、发取消、清掉工具；返回旧客户端（由调用方关闭）与
    /// 新的代号和取消消息。旧任务之后的更新因代号不符被丢弃（tui-manage.md §6.1）。
    pub(super) fn retire(
        &self,
        name: &str,
        status: McpStatus,
    ) -> Option<(McpServer, Option<Arc<Client>>, u64, CancellationToken)> {
        let mut servers = self.servers();
        let server = servers.get_mut(name)?;
        server.cancel.cancel();
        server.generation += 1;
        server.cancel = CancellationToken::new();
        server.status = status;
        server.tools.clear();
        server.prompts.clear();
        server.instructions = None;
        server.resources = false;
        let client = server.client.take();
        Some((
            server.config.clone(),
            client,
            server.generation,
            server.cancel.clone(),
        ))
    }

    /// 工具调用时服务器要（重新）授权（ext.md §3.12.3）：停掉连接（工具离开快照，下一步撤回），状态 NeedsAuth。传输已经刷新并
    /// 重发过一次，到这里说明令牌换不来了，所以不再重连。
    pub(crate) async fn needs_auth(self: &Arc<Self>, name: &str, scope: Option<String>) {
        let Some((config, client, generation, _)) =
            self.retire(name, McpStatus::NeedsAuth { scope })
        else {
            return;
        };
        let stubs = self.auth_stubs(&config);
        self.update(name, generation, |server| server.tools = stubs);
        tracing::info!(
            event = "mcp.oauth.needs_auth",
            server = name,
            "MCP server needs authorization"
        );
        self.changed.send_modify(|version| *version += 1);
        if let Some(client) = client {
            client.close().await;
        }
    }

    /// 这个服务器的授权目标：用 OAuth、且在本机连（远端机器的令牌不在这里，ext.md §3.12.8）。
    fn auth_target(&self, name: &str) -> Option<McpAuthTarget> {
        let servers = self.servers();
        let config = &servers.get(name)?.config;
        if config.machine.remote().is_some() {
            return None;
        }
        config.transport.auth_target(&config.name)
    }
}

impl Inner {
    /// 要授权的服务器在交互会话（不是 print）里的两个授权伪工具（ext.md §3.12.16）；print 会话、没有授权登记时没有。
    pub(super) fn auth_stubs(self: &Arc<Self>, config: &McpServer) -> Vec<Arc<dyn Tool>> {
        if self.print.load(std::sync::atomic::Ordering::Relaxed) || self.sign_ins.get().is_none() {
            return Vec::new();
        }
        super::auth_tool::stubs(config, self)
    }

    /// 这个服务器的授权目标在进程级登记里的代号（ext.md §3.12.14）；不用 OAuth、没有登记时 None。
    pub(super) fn auth_generation(&self, config: &McpServer) -> Option<u64> {
        let sign_ins = self.sign_ins.get()?;
        let target = config.transport.auth_target(&config.name)?;
        Some(sign_ins.generation(&target))
    }
}

impl McpManager {
    /// 进程级的授权登记（ext.md §3.12.6、§3.12.14）：`start` 之前给一次，这个管理器登记进去（授权之后由它通知重连）。
    pub fn set_sign_ins(&self, sign_ins: Arc<McpSignIns>) {
        if self.0.sign_ins.set(sign_ins.clone()).is_ok() {
            sign_ins.register(&self.0);
        }
    }

    pub(crate) fn sign_ins(&self) -> Option<&Arc<McpSignIns>> {
        self.0.sign_ins.get()
    }

    /// 这个名字的授权目标（用 OAuth、在本机连）。
    pub(crate) fn auth_target_of(&self, name: &str) -> Option<McpAuthTarget> {
        self.0.auth_target(name)
    }

    /// 这个名字要授权时上次 403 要的更多权限（step-up，ext.md §3.12.6）。
    pub(crate) fn needs_auth_scope(&self, name: &str) -> Option<Option<String>> {
        match self.status_of(name) {
            Some(McpStatus::NeedsAuth { scope }) => Some(scope),
            _ => None,
        }
    }

    /// `/mcp` 的快照，加上用 OAuth 的服务器有没有存下的令牌（只看键，不解密；没有授权登记时不填）。
    pub async fn server_infos_with_auth(&self) -> Vec<McpServerInfo> {
        let mut infos = self.server_infos();
        let Some(sign_ins) = self.0.sign_ins.get() else {
            return infos;
        };
        for info in &mut infos {
            if let Some(target) = self.0.auth_target(&info.name) {
                info.auth = Some(if sign_ins.authorizer().signed_in(&target).await {
                    McpAuthState::SignedIn
                } else {
                    McpAuthState::SignedOut
                });
            }
        }
        infos
    }

    /// 这个名字的状态快照（带有没有令牌）；没有这个服务器时 None。
    pub(crate) async fn server_info(&self, name: &str) -> Option<McpServerInfo> {
        self.server_infos_with_auth()
            .await
            .into_iter()
            .find(|info| info.name == name)
    }

    /// 授权变了（ext.md §3.12.14）：这个目标的连接里，登录了的重连 NeedsAuth 的；退出了的重连还连着、在连、失败的（之后是要授权）。
    /// 走 `/mcp reconnect` 的路（不占首次连接的名额），等它们离开 Pending。
    pub(crate) async fn auth_changed(&self, target: McpAuthTarget, change: AuthChange) {
        let names: Vec<String> = {
            let servers = self.0.servers();
            servers
                .values()
                .filter(|server| {
                    server.config.machine.remote().is_none()
                        && server
                            .config
                            .transport
                            .auth_target(&server.config.name)
                            .as_ref()
                            == Some(&target)
                })
                .filter(|server| match change {
                    AuthChange::SignedIn => matches!(server.status, McpStatus::NeedsAuth { .. }),
                    AuthChange::SignedOut => !matches!(
                        server.status,
                        McpStatus::NeedsAuth { .. } | McpStatus::Disabled | McpStatus::Untrusted
                    ),
                })
                .map(|server| server.config.name.clone())
                .collect()
        };
        if names.is_empty() {
            return;
        }
        tracing::info!(
            event = "mcp.oauth.reconnect",
            servers = names.len(),
            signed_in = change == AuthChange::SignedIn,
            "reconnecting the MCP servers whose authorization changed"
        );
        self.control(McpAction::Reconnect, &names).await;
    }
}

#[cfg(test)]
#[path = "manager_auth_tests.rs"]
mod tests;
