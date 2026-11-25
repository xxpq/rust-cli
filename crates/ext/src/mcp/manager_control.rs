//! `/mcp reconnect|enable|disable`（docs/specs/rust-cli/tui-manage.md §6.1）：连接任务的唯一 owner 是
//! [`McpManager`]。重连与停用时代号加一、发取消消息并关闭旧客户端，旧任务之后的更新因代号不符被丢弃；
//! 等待用状态变化的 watch，不用超时。

use rust_cli_base::sync::LockUnpoisoned;
use std::collections::BTreeSet;
use std::sync::Arc;

use futures_util::StreamExt;
use rust_cli_kernel_api::env_registry::EnvironmentRegistry;
use rust_cli_kernel_api::manage::{McpAction, McpServerInfo};

use super::{McpManager, McpServer, McpStatus, Server};

impl McpManager {
    /// 兄弟管理器（工作线 F145，ext.md §3.9）：同一个传输、设置、时钟、任务的启动处与“状态变了”的通知，自己的服务器表
    /// 与连接（一个顶层会话的那一份）。远端机器的注册表照抄、不跟着它的变化重连（App 没有远端机器；跟随任务只在内核级
    /// 那一份上跑一个）。
    pub fn sibling(&self) -> McpManager {
        let inner = &self.0;
        let machines = std::sync::OnceLock::new();
        if let Some(registry) = inner.machines.get() {
            let _ = machines.set(registry.clone());
        }
        // 会话级的服务器过同一道准入（18-mcp-trust.md §12.4）
        let admission = std::sync::OnceLock::new();
        if let Some(port) = inner.admission.get() {
            let _ = admission.set(port.clone());
        }
        // 同一份进程级的授权登记（ext.md §3.12.14）：兄弟也登记，授权之后它的连接一起重连
        let sign_ins = std::sync::OnceLock::new();
        if let Some(port) = inner.sign_ins.get() {
            let _ = sign_ins.set(port.clone());
        }
        // 问用户的端口也是同一份（sampling 的会话记录只有一个写者，ext.md §3.14.1）
        let asking = std::sync::OnceLock::new();
        if let Some(ports) = inner.asking.get() {
            let _ = asking.set(ports.clone());
        }
        let sibling = Arc::new(super::Inner {
            transport: inner.transport.clone(),
            machines,
            admission,
            sign_ins,
            asking,
            spawner: inner.spawner.clone(),
            settings: inner.settings.clone(),
            servers: std::sync::Mutex::default(),
            changed: inner.changed.clone(),
            workspace: std::sync::Mutex::default(),
            clock: inner.clock.clone(),
            declared: std::sync::Mutex::default(),
            noted_names: std::sync::Mutex::default(),
            print: std::sync::atomic::AtomicBool::new(
                inner.print.load(std::sync::atomic::Ordering::Relaxed),
            ),
            resources_seen: std::sync::atomic::AtomicBool::new(false),
        });
        if let Some(sign_ins) = sibling.sign_ins.get() {
            sign_ins.register(&sibling);
        }
        McpManager(sibling)
    }

    /// 停掉全部连接（一个顶层会话释放时，ext.md §3.9）：取消连接任务、清空服务器表，再关闭各自的客户端（stdio 服务器
    /// 的进程树随之结束）。之后的视图里没有它们。
    pub async fn shutdown(&self) {
        let clients: Vec<Arc<super::Client>> = {
            let mut servers = self.0.servers();
            let clients = servers
                .values_mut()
                .filter_map(|server| {
                    server.cancel.cancel();
                    server.client.take()
                })
                .collect();
            servers.clear();
            clients
        };
        self.0.changed.send_modify(|version| *version += 1);
        for client in clients {
            client.close().await;
        }
    }

    /// 启动时停用的服务器（配置里 `enabled: false`）：登记为 disabled，不连接；`/mcp enable` 时才连。
    pub fn register_disabled(&self, servers: Vec<McpServer>) {
        {
            let mut known = self.0.servers();
            for config in servers {
                known
                    .entry(config.name.clone())
                    .or_insert_with(|| Server::new(config, McpStatus::Disabled));
            }
        }
        self.0.changed.send_modify(|version| *version += 1);
    }

    /// 重新连一个服务器（重连与启用共用）。
    pub(super) async fn restart(&self, name: &str) {
        let Some((config, client, generation, cancel)) = self.0.retire(name, McpStatus::CONNECTING)
        else {
            return;
        };
        self.0.changed.send_modify(|version| *version += 1);
        if let Some(client) = client {
            client.close().await;
        }
        let workspace = self.0.workspace.lock_unpoisoned().clone();
        let Some(workspace) = workspace else {
            return;
        };
        // 重连与启用不占首次连接的名额（ext.md §3.3.4，同对照实现）。
        let task = self
            .0
            .clone()
            .run(workspace, config, (generation, None), cancel);
        self.0.spawner.spawn_resident("mcp-connection", task);
    }

    /// 本进程刚写下这个工作区声明的信任（18-mcp-trust.md §12.4 的补连）：它是 `Untrusted` 时重启连接任务一次，任务再问一次准入；
    /// 别的状态不动（连着的不断开）。不等它连上。
    pub async fn readmit(&self, name: &str) {
        if self.status_of(name) == Some(McpStatus::Untrusted) {
            self.restart(name).await;
        }
    }

    /// 跟着注册表的连接变化（F98，remote-exec.md §25.5）：一台机器进注册表时（`/machines add` / `connect` 连上、失败之后
    /// 再连上），绑定到它、现在是失败状态的服务器重连一次（同 `/mcp reconnect`）。ssh 抖动时机器一直在注册表里，不触发。
    ///
    /// 缺陷修复（Docker 试用 L6）：以前机器后来连上时，启动时因为它不在清单里而失败的服务器一直是“未连接”，要手动
    /// `/mcp reconnect`。
    pub(super) async fn follow_machines(&self, registry: Arc<dyn EnvironmentRegistry>) {
        let names = |registry: &dyn EnvironmentRegistry| -> BTreeSet<String> {
            registry.machines().names().into_iter().collect()
        };
        let mut seen = names(registry.as_ref());
        let mut changes = registry.changes();
        while changes.next().await.is_some() {
            let now = names(registry.as_ref());
            let arrived: Vec<String> = now.difference(&seen).cloned().collect();
            seen = now;
            for machine in arrived {
                let failed: Vec<String> = self
                    .0
                    .servers()
                    .iter()
                    .filter(|(_, server)| {
                        server.config.machine.remote() == Some(machine.as_str())
                            && matches!(server.status, McpStatus::Failed { .. })
                    })
                    .map(|(name, _)| name.clone())
                    .collect();
                for name in failed {
                    tracing::info!(server = %name, %machine, "reconnecting an MCP server whose machine connected");
                    self.restart(&name).await;
                }
            }
        }
    }

    async fn stop(&self, name: &str) {
        let Some((_, client, _, _)) = self.0.retire(name, McpStatus::Disabled) else {
            return;
        };
        self.0.changed.send_modify(|version| *version += 1);
        if let Some(client) = client {
            client.close().await;
        }
    }

    pub(super) fn status_of(&self, name: &str) -> Option<McpStatus> {
        self.0
            .servers()
            .get(name)
            .map(|server| server.status.clone())
    }

    /// 对这些服务器做一个动作，等重连与启用的服务器离开 connecting，返回它们改动后的状态。
    /// reconnect 跳过停用的；enable 只动停用的；disable 跳过已经停用的。不认识的名字不在结果里。
    pub async fn control(&self, action: McpAction, names: &[String]) -> Vec<McpServerInfo> {
        for name in names {
            let Some(status) = self.status_of(name) else {
                continue;
            };
            match (action, status) {
                (McpAction::Reconnect, McpStatus::Disabled)
                | (
                    McpAction::Enable,
                    McpStatus::Pending { .. }
                    | McpStatus::Connected { .. }
                    | McpStatus::Failed { .. }
                    | McpStatus::Untrusted
                    | McpStatus::NeedsAuth { .. },
                )
                | (McpAction::Disable, McpStatus::Disabled) => {}
                (McpAction::Reconnect | McpAction::Enable, _) => self.restart(name).await,
                (McpAction::Disable, _) => self.stop(name).await,
            }
        }
        let mut changes = self.0.changed.subscribe();
        loop {
            let pending = names
                .iter()
                .any(|name| self.status_of(name).is_some_and(|status| status.pending()));
            if !pending || changes.changed().await.is_err() {
                break;
            }
        }
        self.server_infos_with_auth()
            .await
            .into_iter()
            .filter(|info| names.contains(&info.name))
            .collect()
    }
}

#[cfg(test)]
#[path = "manager_control_tests.rs"]
mod tests;
