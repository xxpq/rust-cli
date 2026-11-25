//! 一次运行看到的 MCP 连接（工作线 F145，docs/specs/rust-cli/ext.md §3.9）：内核级的那一份，加上这个顶层会话自己的
//! 那一份（`McpSharing::PerSession`）。没有会话的那一份时，每个方法都原样交给内核级的管理器：TUI 与 `-p` 的行为与
//! 以前逐字节相同。
//!
//! 两份管理器是兄弟（[`McpManager::sibling`]）：共用“状态变了”的通知，所以等这个集合的状态只订阅一次；各有一把锁，
//! 同一个快照里两份各自一致（不保证同一时刻）。

use std::sync::Arc;

use async_trait::async_trait;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::listing::ListingEntry;
use rust_cli_kernel_api::mcp_state::{McpConnections, McpView};
use rust_cli_kernel_api::tool::Tool;

use super::state::{TOOL_WAIT_CAP_MS, view_of};
use super::{McpManager, has_resources, instructions_of, resources, server_tools};

/// 内核级的连接与（有时）一个顶层会话自己的连接。
pub struct McpSet {
    shared: Arc<McpManager>,
    own: Option<Arc<McpManager>>,
}

impl McpSet {
    pub fn new(shared: Arc<McpManager>, own: Option<Arc<McpManager>>) -> Self {
        Self { shared, own }
    }

    /// 有资源工具时先是三个资源工具（覆盖两份里有 resources 能力的服务器），然后是两份已连接的服务器的工具（按名字
    /// 排序）。
    pub fn tools(&self) -> Vec<Arc<dyn Tool>> {
        let Some(own) = &self.own else {
            return self.shared.tools();
        };
        let (shared_resources, mut tools) = {
            let servers = self.shared.0.servers();
            (
                has_resources(&self.shared.0, &servers),
                server_tools(&servers),
            )
        };
        let own_resources = {
            let servers = own.0.servers();
            tools.extend(server_tools(&servers));
            has_resources(&own.0, &servers)
        };
        tools.sort_by(|left, right| left.name().cmp(right.name()));
        if !(shared_resources || own_resources) {
            return tools;
        }
        let mut all =
            resources::tools(vec![Arc::downgrade(&self.shared.0), Arc::downgrade(&own.0)]);
        all.extend(tools);
        all
    }

    pub fn find(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools()
            .into_iter()
            .find(|tool| rust_cli_kernel_api::tool::answers_to(tool.as_ref(), name))
    }

    /// 工具与连接状态（query.md §1.3）：每一份在自己的锁里取。
    pub fn snapshot(&self) -> (Vec<Arc<dyn Tool>>, McpView) {
        match &self.own {
            None => self.shared.snapshot(),
            Some(_) => (self.tools(), self.view()),
        }
    }

    pub fn instructions(&self) -> Vec<ListingEntry> {
        let Some(own) = &self.own else {
            return self.shared.instructions();
        };
        let mut entries = self.shared.instructions();
        entries.extend(instructions_of(&own.0.servers()));
        entries.sort_by(|left, right| left.name.cmp(&right.name));
        entries
    }

    pub fn has_servers(&self) -> bool {
        self.shared.has_servers() || self.own.as_ref().is_some_and(|own| own.has_servers())
    }

    pub fn has_pending(&self) -> bool {
        self.shared.has_pending() || self.own.as_ref().is_some_and(|own| own.has_pending())
    }

    /// 带过 WaitForMcpServers 的会话记在内核级的那一份（ext.md §3.8）。
    pub fn declared(&self, session: &SessionId) -> bool {
        self.shared.declared(session)
    }
}

#[async_trait]
impl McpConnections for McpSet {
    /// 两份的服务器按名字排在一起（名字出自同一份配置，不重复）。
    fn view(&self) -> McpView {
        let Some(own) = &self.own else {
            return self.shared.view();
        };
        let mut view = self.shared.view();
        let mine = view_of(&own.0.servers());
        view.servers.extend(mine.servers);
        view.servers
            .sort_by(|left, right| left.name.cmp(&right.name));
        view.instructions.extend(mine.instructions);
        view.instructions
            .sort_by(|left, right| left.name.cmp(&right.name));
        view
    }

    async fn wait(
        &self,
        wait_ms: u64,
        done: &(dyn for<'v> Fn(&'v McpView) -> bool + Send + Sync),
    ) -> McpView {
        if self.own.is_none() {
            return self.shared.wait(wait_ms, done).await;
        }
        // 兄弟管理器共用“状态变了”的通知：在内核级那一份上等，两份的变化都会叫醒它。
        let deadline = self.shared.0.clock.now_ms().saturating_add(wait_ms);
        self.shared
            .0
            .wait_until(deadline, || done(&self.view()))
            .await;
        self.view()
    }

    fn declare_wait_tool(&self, session: &SessionId) {
        self.shared.declare_wait_tool(session);
    }

    fn tool_wait_ms(&self) -> u64 {
        self.shared
            .0
            .settings
            .connect_timeout_ms
            .min(TOOL_WAIT_CAP_MS)
    }
}
