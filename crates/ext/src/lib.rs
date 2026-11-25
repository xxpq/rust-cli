//! 扩展：skill、MCP、插件（docs/specs/rust-cli/ext.md）。
//!
//! - skill（[`skills`]）：来源与优先级、SKILL.md frontmatter、清单与预算、Skill 工具（参数替换、变量、
//!   `!` 内联命令）、`paths` 条件激活；
//! - MCP（[`mcp`]）：连接管理（握手、工具清单、调用、通知、重连）、工具命名与包装、结果转换与截断、
//!   instructions、resources；传输经 kernel-api 的 [`McpTransport`]，本地实现在 env-local；
//! - 插件（[`plugins`]）：来源与启用判定、目录与 manifest，提供 skill、MCP 服务器、agent 与 hooks。
//!
//! 不做 IO：文件经执行环境端口，MCP 消息经传输端口，任务经 base 的 [`Spawner`]。runtime 组装：
//! [`Ext::registry`] 包住基础工具注册表（加上 Skill 与 MCP 工具），[`Ext::listings`] 是 skill 清单与
//! MCP instructions 的来源，[`Ext::start`] 在内核建好后开始连接 MCP 服务器。

#[cfg(test)]
mod clock_tests;
pub mod mcp;
pub mod plugins;
pub mod skills;
#[cfg(test)]
mod test_support;

use std::sync::Arc;

use async_trait::async_trait;
use rust_cli_base::Spawner;
use rust_cli_kernel_api::clock::{Clock, Ids};
use rust_cli_kernel_api::default_model::DefaultModel;
use rust_cli_kernel_api::env::WorkspaceRef;
use rust_cli_kernel_api::hooks::HookRunner;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::listing::{ListingEntry, ListingSource, Listings};
use rust_cli_kernel_api::machine::{EnvInfo, Files};
use rust_cli_kernel_api::mcp::{McpAdmission, McpTransport};
use rust_cli_kernel_api::mcp_state::McpConnections;
use rust_cli_kernel_api::message::Message;
use rust_cli_kernel_api::model::ModelPort;
use rust_cli_kernel_api::orchestration::{
    ContextRequest, OrchestrationError, ScopedGate, ToolRegistry, ToolSnapshot,
};
use rust_cli_kernel_api::tool::Tool;
use rust_cli_kernel_api::tool_names::WAIT_FOR_MCP_SERVERS;

pub use mcp::{
    AskPorts, McpClientTraits, McpIsolation, McpManager, McpScope, McpServer, McpServers,
    McpSessions, McpSet, McpSettings, McpSharing, McpSignIns,
};
pub use plugins::{Plugin, PluginAgentFile, PluginEntry, PluginSet, PluginSources};
use rust_cli_kernel_api::tool_names;
pub use skills::{Skill, SkillCatalog, SkillCommands, SkillSource, SkillTool};

/// ext 用到的端口，由 runtime 注入。
#[derive(Clone)]
pub struct ExtPorts {
    /// skill 的文件经主 env 的端口读（runtime 从注册表的 `primary_machine()` 取，deployment.md §8）；插件在组装时已经经同一个
    /// 端口读好（ext.md §4.0）。
    pub info: Arc<dyn EnvInfo>,
    pub files: Arc<dyn Files>,
    pub ids: Arc<dyn Ids>,
    /// Skill 的 `!` 命令用判决链（带 skill 的 `allowed-tools`，ext.md §2.5）。
    pub gate: Arc<dyn ScopedGate>,
    pub mcp: Arc<dyn McpTransport>,
    /// 远端机器的注册表（R5，remote-exec.md §22.6）：绑定到远端机器的 MCP 服务器从这里取那台机器的管道；None 时
    /// （只给测试的组装）它们都连不上。
    pub machines: Option<Arc<dyn rust_cli_kernel_api::env_registry::EnvironmentRegistry>>,
    /// MCP 的连接任务在这里启动。
    pub spawner: Spawner,
    /// MCP 等待的截止时刻（ext.md §3.3.1、§3.8）。
    pub clock: Arc<dyn Clock>,
    /// 工作区声明的 MCP 服务器的准入（App 形态，docs/specs/rust-cli/ext.md §3.1 与 18-mcp-trust.md §12.4）；None 时项目级服务器照常连
    /// （TUI、`-p`）。
    pub admission: Option<Arc<dyn McpAdmission>>,
    /// 进程级的授权登记（OAuth，docs/specs/rust-cli/ext.md §3.12.6、§3.12.14）：一个进程里所有内核共用一份；None 时不能 Sign in
    /// （入口不装它：v4 的 App、serve；测试）。
    pub sign_ins: Option<Arc<McpSignIns>>,
    /// 服务器询问的 hooks（Elicitation、ElicitationResult、Notification，ext.md §3.13.6）：与工具共用同一个引擎。
    pub hooks: Arc<dyn HookRunner>,
    /// sampling 的模型请求（ext.md §3.14）与会话给不出主模型时用的缺省模型。
    pub model: Arc<dyn ModelPort>,
    pub default_model: DefaultModel,
    /// 文件变化的订阅（ext.md §2.1.1，全局 watcher 的接口）：有时 skill 发现的结果记下来、目录变了才重新发现；None 时每一步
    /// 照旧 stat。
    pub watch: Option<Arc<dyn rust_cli_kernel_api::file_watch::FileWatch>>,
}

/// 入口层给出的配置快照。
#[derive(Debug, Clone, Default)]
pub struct ExtConfig {
    /// MCP 服务器、连接与调用的超时、输出上限（ext.md §3.1）。
    pub mcp: McpSettings,
    /// 这套内核的插件（入口层在组装的第二段加载好，ext.md §4.0）：skill 根、命令根、MCP 服务器与引用目录都从它算。
    pub plugins: Arc<PluginSet>,
    /// MCP 服务器与插件所在的工作区（stdio 服务器的 cwd）；None 时不连接 MCP、不加载插件。
    pub workspace: Option<WorkspaceRef>,
    /// 配置里停用的 MCP 服务器（`enabled: false`）：登记为 disabled、不连接，`/mcp enable` 时才连
    /// （docs/specs/rust-cli/tui-manage.md §6.1）。
    pub mcp_disabled: Vec<McpServer>,
    /// MCP 连接归谁（ext.md §3.9，工作线 F145）：缺省全部在内核级一份；`PerSession` 时每个顶层会话一份。
    pub mcp_sharing: McpSharing,
    /// 前端撑得起的服务器询问（elicitation、sampling，ext.md §3.13.1）：缺省什么都不声明。
    pub mcp_client: McpClientTraits,
}

pub struct Ext {
    skills: Arc<SkillCatalog>,
    commands: Arc<SkillCommands>,
    mcp: Arc<McpManager>,
    /// 会话级的连接（`McpSharing::PerSession`，ext.md §3.9）。
    sessions: Arc<McpSessions>,
    registry: Arc<ExtRegistry>,
    config: ExtConfig,
}

impl Ext {
    /// `base` 是基础工具注册表（文件、Bash 等）；Skill 的 `!` 命令用其中的 `Bash`。
    pub fn new(ports: ExtPorts, base: Arc<dyn ToolRegistry>, config: ExtConfig) -> Self {
        let skills = Arc::new(
            SkillCatalog::new(ports.info.clone(), ports.files.clone())
                .with_plugins(config.plugins.clone())
                .with_watch(ports.watch.clone()),
        );
        let runner = base
            .find(tool_names::BASH)
            .map(|bash| skills::runner(bash, ports.gate.clone(), ports.ids.clone()));
        // 用户调用（`/<名字>`，ext.md §2.7）与 Skill 工具共用目录与 `!` 命令的执行者。
        let commands = Arc::new(SkillCommands::new(skills.clone(), runner.clone()));
        let skill_tool: Arc<dyn Tool> =
            Arc::new(SkillTool::new(skills.clone(), ports.ids.clone(), runner));
        let mcp = Arc::new(McpManager::new(
            ports.mcp.clone(),
            ports.spawner.clone(),
            config.mcp.clone(),
            ports.clock.clone(),
        ));
        if let Some(machines) = ports.machines.clone() {
            mcp.set_machines(machines);
        }
        if let Some(admission) = ports.admission.clone() {
            mcp.set_admission(admission);
        }
        if let Some(sign_ins) = ports.sign_ins.clone() {
            mcp.set_sign_ins(sign_ins);
        }
        // 前端声明的服务器询问（ext.md §3.13.1）：这套内核的连接都用这一份端口。
        mcp.set_asking(Arc::new(AskPorts::new(
            config.mcp_client,
            ports.hooks.clone(),
            ports.model.clone(),
            ports.default_model.clone(),
            ports.clock.clone(),
        )));
        let sessions = Arc::new(McpSessions::new(
            mcp.clone(),
            config.mcp_sharing,
            ports.spawner,
        ));
        let registry = Arc::new(ExtRegistry::over(base, skill_tool, sessions.clone(), None));
        Self {
            skills,
            commands,
            mcp,
            sessions,
            registry,
            config,
        }
    }

    /// 主线程的工具注册表：基础工具、Skill、MCP 工具（ext.md §1）。
    /// 单次的 `-p`：内置 `/loop` 的 Monitor 超时随它（tools-more.md §17.1）。
    pub fn set_print_session(&self, print: bool) {
        self.skills.set_print_session(print);
        self.mcp.set_print_session(print);
    }

    pub fn registry(&self) -> Arc<dyn ToolRegistry> {
        self.registry.clone()
    }

    /// skill 清单与 MCP instructions 的来源。
    pub fn listings(&self) -> Arc<dyn ListingSource> {
        Arc::new(ExtListings {
            skills: self.skills.clone(),
            sessions: self.sessions.clone(),
        })
    }

    pub fn skills(&self) -> &Arc<SkillCatalog> {
        &self.skills
    }

    /// 用户调用 skill 的展开与斜杠补全（ext.md §2.7）。
    pub fn user_commands(&self) -> Arc<dyn rust_cli_kernel_api::user_command::UserCommands> {
        self.commands.clone()
    }

    /// 内核级的连接（`/mcp` 与 `/status` 看的那一份）。
    pub fn mcp(&self) -> &Arc<McpManager> {
        &self.mcp
    }

    /// 会话 actor 启动（ext.md §3.9）：`PerSession` 时建这个会话的那一份连接、开始连接（不等）。
    pub fn session_opened(&self, session: &SessionId) {
        self.sessions.opened(session);
    }

    /// 会话释放：去掉它的 WaitForMcpServers 声明（ext.md D34）与 sampling 的会话记录（§3.14.1），断开它的那一份连接。
    pub fn session_released(&self, session: &SessionId) {
        self.mcp.release(session);
        self.mcp.asking_released(session);
        self.sessions.released(session);
    }

    /// 会话级的连接表（测试与诊断）。
    pub fn mcp_sessions(&self) -> &Arc<McpSessions> {
        &self.sessions
    }

    /// 这套内核的插件（ext.md §4.0）。
    pub fn plugins(&self) -> &Arc<PluginSet> {
        &self.config.plugins
    }

    /// MCP 服务器与插件所在的工作区（配置给的）；None 时不连接 MCP。
    pub fn workspace(&self) -> Option<&WorkspaceRef> {
        self.config.workspace.as_ref()
    }

    /// 开始连接 MCP 服务器（每个服务器一个连接任务，不等待）：配置的服务器加上插件的（ext.md §4）。
    pub fn start(&self) {
        let Some(workspace) = self.config.workspace.clone() else {
            return;
        };
        let mut servers = self.config.mcp.servers.clone();
        servers.extend(self.config.plugins.mcp_servers());
        // `PerSession`：会话级的服务器留到会话打开时再连（ext.md §3.9）。
        let servers = self.sessions.split(&workspace, servers);
        self.mcp.start(workspace, servers);
        self.mcp.register_disabled(self.config.mcp_disabled.clone());
    }

    /// `-p` 在第一个请求前的等待（ext.md §3.3.1）：alwaysLoad 的服务器与第一条命令前的 prewait，到期就开跑。
    pub async fn wait_for_print(&self) {
        self.mcp.wait_for_print().await;
    }
}

/// 基础工具在前，Skill 其次（有连接中的服务器时再加 WaitForMcpServers），MCP 工具最后（MCP 工具是连接任务发布的
/// 快照）。发给模型前按名字排序（tools-more.md §3.3）。
struct ExtRegistry {
    base: Arc<dyn ToolRegistry>,
    skill: Arc<dyn Tool>,
    /// WaitForMcpServers（ext.md §3.8）：有 Pending 的服务器、或这个会话带过它（declared）时在工具表里。启用 ToolSearch
    /// 时由主循环去掉（tools-more.md §3.3 第 4 条）。
    wait: Arc<dyn Tool>,
    /// 这个注册表看得到的连接：内核的那一份是内核级的；视图（`view`）里另加顶层会话自己的（工作线 F145）。
    mcp: Arc<McpSet>,
    sessions: Arc<McpSessions>,
}

impl ExtRegistry {
    /// `root` 为 None 时是内核级的连接；否则加上这个顶层会话自己的。
    fn over(
        base: Arc<dyn ToolRegistry>,
        skill: Arc<dyn Tool>,
        sessions: Arc<McpSessions>,
        root: Option<&SessionId>,
    ) -> Self {
        let mcp = match root {
            Some(root) => sessions.set(root),
            None => sessions.kernel_set(),
        };
        let connections: Arc<dyn McpConnections> = mcp.clone();
        Self {
            base,
            skill,
            wait: Arc::new(mcp::WaitForMcpServers::new(Arc::downgrade(&connections))),
            mcp,
            sessions,
        }
    }

    fn assemble(&self, wait: bool, mcp: Vec<Arc<dyn Tool>>) -> Vec<Arc<dyn Tool>> {
        let mut tools = self.base.tools();
        tools.push(self.skill.clone());
        if wait {
            tools.push(self.wait.clone());
        }
        tools.extend(mcp);
        tools
    }
}

impl ToolRegistry for ExtRegistry {
    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.assemble(self.mcp.has_pending(), self.mcp.tools())
    }

    fn find(&self, name: &str) -> Option<Arc<dyn Tool>> {
        if name == self.skill.name() {
            return Some(self.skill.clone());
        }
        // 快照里带过它、之后服务器连上了，这次调用照常执行（按请求时的工具表找）：有服务器就找得到。
        if name == WAIT_FOR_MCP_SERVERS && self.mcp.has_servers() {
            return Some(self.wait.clone());
        }
        self.base.find(name).or_else(|| self.mcp.find(name))
    }

    fn withheld(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.base.withheld(name)
    }

    /// 内置注册表的规则（工作线 F134：以前是缺省的 false，内置注册表也是 false）。
    fn hides(&self, name: &str) -> bool {
        self.base.hides(name)
    }

    /// 内置工具在前、skill 与 MCP 工具接在后面，发请求前照内置注册表的规则排（同上）。
    fn keeps_order(&self) -> bool {
        self.base.keeps_order()
    }

    /// MCP 工具、连接中的服务器与 instructions 在同一把锁里取（query.md §1.3，修复线 F36）。
    fn snapshot(&self, session: &SessionId) -> ToolSnapshot {
        let (tools, view) = self.mcp.snapshot();
        let wait = !view.pending().is_empty() || self.mcp.declared(session);
        ToolSnapshot {
            tools: self.assemble(wait, tools),
            mcp: Some(view),
        }
    }

    fn mcp(&self) -> Option<Arc<dyn McpConnections>> {
        Some(self.mcp.clone())
    }

    /// 基础工具的视图、同一个 Skill 工具，MCP 换成“内核级 + 顶层会话 `root` 自己的”（工作线 F145）。
    fn view(self: Arc<Self>, root: &SessionId) -> Arc<dyn ToolRegistry> {
        Arc::new(Self::over(
            self.base.clone().view(root),
            self.skill.clone(),
            self.sessions.clone(),
            Some(root),
        ))
    }
}

struct ExtListings {
    skills: Arc<SkillCatalog>,
    sessions: Arc<McpSessions>,
}

#[async_trait]
impl ListingSource for ExtListings {
    async fn listings(&self, request: &ContextRequest) -> Result<Listings, OrchestrationError> {
        self.listings_for(request, &[]).await
    }

    async fn listings_for(
        &self,
        request: &ContextRequest,
        history: &[Message],
    ) -> Result<Listings, OrchestrationError> {
        let listing = self
            .skills
            .listing(
                (&request.workspace, &request.additional_directories),
                history,
                &request.selection.model,
            )
            .await;
        Ok(Listings {
            skills: listing.entries,
            dynamic_skills: listing.dynamic,
            ..Listings::default()
        })
    }

    /// 调用过的 skill（compact.md §3.4）。
    async fn invoked_skills(
        &self,
        request: &ContextRequest,
        history: &[Message],
    ) -> Result<Vec<rust_cli_kernel_api::listing::InvokedSkill>, OrchestrationError> {
        Ok(self
            .skills
            .invoked(
                (&request.workspace, &request.additional_directories),
                history,
            )
            .await)
    }

    /// 输入中间的 skill 提及（ext.md §2.8）。
    async fn skill_mentions(
        &self,
        request: &ContextRequest,
        input: &str,
    ) -> Result<Vec<String>, OrchestrationError> {
        Ok(self
            .skills
            .mentions((&request.workspace, &request.additional_directories), input)
            .await)
    }

    /// 有工具快照时用快照里的（与工具同一时点，query.md §1.3）；否则取现在的。
    async fn mcp_instructions(
        &self,
        request: &ContextRequest,
    ) -> Result<Vec<ListingEntry>, OrchestrationError> {
        Ok(match &request.mcp {
            Some(view) => view.instructions.clone(),
            // 没有快照的调用方：顶层会话的视图（工作线 F145）。
            None => self.sessions.set(request.root()).instructions(),
        })
    }
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "sessions_tests.rs"]
mod sessions_tests;

#[cfg(test)]
#[path = "admission_tests.rs"]
mod admission_tests;
