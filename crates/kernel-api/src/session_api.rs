//! 前端使用的内核会话 API（docs/specs/rust-cli/README.md §3、§7）。
//!
//! TUI 与 headless 只经这里驱动内核；第二阶段的 ZCode Protocol 前端也一样。事件覆盖 spec §7
//! 列出的 V4 适配层输入，每个事件带会话内单调递增的 seq，并能从持久事实重建（§6 规则 13）。

use std::sync::Arc;

use async_trait::async_trait;
use futures_util::{StreamExt, stream::BoxStream};
use serde::{Deserialize, Serialize};

use crate::env::WorkspaceRef;
use crate::ids::{AgentId, CommandId, InteractionId, MessageId, SessionId, TurnId};
use crate::store::{SessionFilter, SessionSummary};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Ack {
    Accepted {
        #[serde(default, skip_serializing_if = "CommandResult::is_done")]
        result: CommandResult,
    },
    /// 同一个 ID 的命令已经执行过（docs/specs/rust-cli/session-lifecycle.md §10.3）：`original` 是它原来的回执；从效果记录认出的
    /// （进程重启之前执行的）是 `Accepted`。
    Duplicate { original: Box<Ack> },
    /// 未确认提交结果；调用方保留原命令 ID，查询回执后再继续。
    OutcomeUnknown { reason: String },
    /// 拒绝：`refusal` 是闭集的原因（前端按它分支，docs/specs/rust-cli/session-lifecycle.md §10.4），`reason` 是给人看的文字
    /// （只作诊断）。
    Rejected {
        refusal: crate::admission::Refusal,
        reason: String,
    },
}

// 会话的视图在分出的文件里（800 行上限，SMALL-6 合入 HOOK-ROWS 时挪去；路径与名字不变）。
pub use view::{SessionTail, SessionView};

// 会话命令放在分出的文件里（800 行上限；路径与名字不变）。
#[path = "session_api_commands.rs"]
mod commands;
pub use commands::{
    Command, CommandEnvelope, Delivery, EditGuard, QueuedAttachments, QueuedEdit, RewindScope,
};

// 内核事件与回合结局放在分出的文件里（800 行上限，工作线 F131；路径与名字不变）。
#[path = "session_api_events.rs"]
mod events;
pub use events::{
    DropReason, FileChange, InputEnd, InputFate, Joined, KernelEvent, NoticeKind, NoticeLevel,
    PlanSwitch, QueueEditLock, QueueHold, QueuedInput, Running, SequencedEvent, TurnFileChange,
    TurnFileDiff, TurnOrigin, TurnOutcome, TurnStart, TurnTime,
};

// 命令的结果放在分出的文件里（800 行上限，工作线 F134）。
#[path = "session_api_results.rs"]
mod results;
pub use results::{CommandResult, Forked, RewoundFile};

// 会话生命周期的类型（建会话的请求与落库时机、SessionStart / SessionEnd 的来源、rewind 选择器的条目）放在分出的文件里（400 行上限，
// L24 合并后拆出；建会话的两个类型在 PROJ-FACTS × QUERIES-2 合入时挪来，800 行上限；路径与名字不变）。
#[path = "session_api_lifecycle.rs"]
mod lifecycle;
pub use lifecycle::{CreateSession, EndReason, Persistence, RewindOption, StartSource};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum KernelError {
    #[error("session not found: {session}")]
    NotFound { session: SessionId },
    #[error("{message}")]
    Failed { message: String },
    #[error("not implemented: {what}")]
    Unsupported { what: String },
    /// 建会话时调用方给的 id 已经是另一个会话（种类、父会话或工作区不同，docs/specs/rust-cli/session-facts.md §6）。
    #[error("session already exists: {session}")]
    Exists { session: SessionId },
}

/// 一个打开着的会话（前端经它发命令、订阅事件、取快照）。
///
/// 方法都没有缺省体（工作线 F134，审查 K6）：以前 9 个缺省 `Unsupported` 或空实现只服务测试替身。测试替身用
/// `testing::HandleOverride`（只写 ID、命令、订阅与视图，其余是中性的缺省）。
#[async_trait]
pub trait SessionHandle: Send + Sync {
    fn id(&self) -> &SessionId;

    async fn send(&self, command: CommandEnvelope) -> Ack;

    /// 订阅 `after` 之后的事件；`after` 为空时从当前位置开始。
    fn subscribe(&self, after: Option<u64>) -> BoxStream<'static, SequencedEvent>;

    async fn view(&self) -> Result<SessionView, KernelError>;

    /// 前端离开会话时调用：运行 SessionEnd hook，返回只给用户看的提示（失败的 hook）。
    async fn close(&self, reason: EndReason) -> Vec<String>;

    /// 空闲时才关（App 的热会话按上限回收，docs/specs/rust-cli/session-lifecycle.md §3.3.2）：会话 actor 在自己的一步里判空闲，空闲就照
    /// [`SessionHandle::close`] 关、返回 true；不空闲什么都不改、返回 false。前端看到的事件有滞后、也看不到还没成事件的工作，所以由 actor 判，
    /// 判完与「不再开新工作」之间没有窗口。别的调用方已经在关时等它关完、返回 true。
    async fn close_idle(&self, reason: EndReason) -> bool;

    /// rewind 选择器的条目（活动历史里可选的 user 消息，按顺序）。
    async fn rewind_options(&self) -> Result<Vec<RewindOption>, KernelError>;

    /// 回退文件的预览（docs/specs/rust-cli/session-lifecycle.md §10.53）：`Rewind{target, Code}` 会恢复的文件与现在能不能恢复。只读。
    /// `target` 不是活动历史里可选的输入时为 None。
    async fn rewind_preview(
        &self,
        target: &MessageId,
    ) -> Result<Option<crate::rewind_preview::RewindPreview>, KernelError>;

    /// 一个回合的文件 diff（docs/specs/rust-cli/session-lifecycle.md §10.45）：回合记录里的文件改动，每个文件带 hunk；没有这个回合、回合还在跑时为空。
    /// 只读：actor 一次处理给出记录与 checkpoint，读 artifact 在调用方的任务里。
    async fn turn_file_diffs(&self, turn: &TurnId) -> Result<Vec<TurnFileDiff>, KernelError>;

    /// `/goal` 面板的数据：会话 actor 的当前目标与最近一次达成的结果（docs/specs/rust-cli/goal.md §6.2）。
    async fn goal(&self) -> Result<crate::goal::GoalView, KernelError>;

    /// 会话里还在跑的后台命令与后台子代理（`/tasks` 与 footer 的计数，tui-display.md §2.3、§3.1）。
    async fn background_tasks(&self) -> Result<Vec<crate::manage::BackgroundEntry>, KernelError>;

    /// 这个顶层会话里本进程启动过的子代理（含嵌套的），按启动顺序（tui-panels.md §1.1，L30）。
    async fn subagents(&self) -> Result<Vec<crate::panels::SubagentEntry>, KernelError>;

    /// 子代理的状态有变化时产生一项（多次变化可能合并成一项）；收到后重取 [`SessionHandle::tasks_snapshot`]
    /// （修复线 F34）。
    fn subagent_changes(&self) -> BoxStream<'static, ()>;

    /// 子代理列表与后台任务的同一份快照（修复线 F34，tui-panels.md §0、§3.1）：一次查询、一次回执。前端在子代理的变化
    /// 通知与后台任务相关的事件之后都重取它，不再分别取 [`SessionHandle::subagents`] 与
    /// [`SessionHandle::background_tasks`]。
    async fn tasks_snapshot(&self) -> Result<crate::panels::TasksSnapshot, KernelError>;

    /// 子代理的只读快照：子会话里已提交的消息；`running` 是它是否在跑（tui-panels.md §1.4）。
    async fn subagent_view(&self, agent: &AgentId) -> Result<SessionView, KernelError>;

    /// 一个后台命令的详情（Shell 详情，tui-panels.md §3.3）；不认识时为 None。
    async fn shell_detail(
        &self,
        task: &str,
    ) -> Result<Option<crate::panels::ShellDetail>, KernelError>;

    /// 一项后台命令（Shell、Monitor）的状态与输出文件最后至多 `tail_bytes` 字节（docs/specs/rust-cli/session-lifecycle.md §10.44）：
    /// 状态取会话 actor 的后台工作表（结束之后也记着终态），后台工作表里没有它、或它是子代理时为 None。
    async fn work_output(
        &self,
        work: &str,
        tail_bytes: usize,
    ) -> Result<Option<crate::background::WorkOutput>, KernelError>;
}

#[async_trait]
pub trait Kernel: Send + Sync {
    async fn create_session(&self, request: CreateSession) -> Result<SessionId, KernelError>;

    async fn open_session(
        &self,
        session: &SessionId,
    ) -> Result<Arc<dyn SessionHandle>, KernelError>;

    async fn list_sessions(
        &self,
        filter: SessionFilter,
    ) -> Result<Vec<SessionSummary>, KernelError>;

    /// 同 [`Kernel::create_session`]，另给 SessionStart 的来源（`/clear` 之后的新会话是 `Clear`）。
    async fn create_session_as(
        &self,
        request: CreateSession,
        _source: StartSource,
    ) -> Result<SessionId, KernelError> {
        self.create_session(request).await
    }

    /// 同 [`Kernel::open_session`]，另给 SessionStart 的来源（刚 fork 出的会话是 `Fork`）。
    /// 会话已经打开时来源不起作用。
    async fn open_session_as(
        &self,
        session: &SessionId,
        _source: StartSource,
    ) -> Result<Arc<dyn SessionHandle>, KernelError> {
        self.open_session(session).await
    }

    /// 同 [`Kernel::open_session_as`]，另给冷打开时的起始状态：接在内核缺省之后的附加工作目录与会话偏好（docs/specs/rust-cli/session-dirs.md
    /// §12.4、app-prefs.md §4.1：App 里是 Host 给的，都不落库）。会话已经打开时它们不起作用（运行实例存在时不刷新）。缺省不收。
    async fn open_session_with(
        &self,
        session: &SessionId,
        source: StartSource,
        _start: crate::session_preferences::OpenStart,
    ) -> Result<Arc<dyn SessionHandle>, KernelError> {
        self.open_session_as(session, source).await
    }

    /// 上下文用量（`/context`，docs/specs/rust-cli/context-usage.md）：只读，不改会话。
    async fn context_usage(
        &self,
        _query: crate::context_usage::ContextUsageQuery,
    ) -> Result<crate::context_usage::ContextUsage, KernelError> {
        Err(KernelError::Unsupported {
            what: "context usage".to_owned(),
        })
    }

    /// `@` 补全的文件清单（docs/specs/rust-cli/tui-input.md §1.2）：相对 shell 当前目录的文件路径。
    async fn workspace_files(
        &self,
        _workspace: &crate::env::WorkspaceRef,
    ) -> Result<Vec<String>, KernelError> {
        Err(KernelError::Unsupported {
            what: "workspace files".to_owned(),
        })
    }

    /// 这个工作区里用户可调用的斜杠命令（skill，TUI 的补全；ext.md §2.7，L22）。`additional` 是会话的附加工作目录
    /// （docs/specs/rust-cli/session-dirs.md §5.4）。默认没有。
    async fn user_commands(
        &self,
        _workspace: &crate::env::WorkspaceRef,
        _additional: &[crate::env::EnvPath],
    ) -> Vec<crate::user_command::UserCommand> {
        Vec::new()
    }

    /// MCP 服务器的状态（`/mcp`、`/status`，tui-display.md §3.4，L28）。默认没有。
    async fn mcp_servers(&self) -> Vec<crate::manage::McpServerInfo> {
        Vec::new()
    }

    /// `/plan open`（tui-manage.md §8，L29）：会话的计划文件位置，以及它是否有内容。
    async fn plan_file(
        &self,
        _query: crate::manage::PlanFileQuery,
    ) -> Result<crate::manage::PlanFile, KernelError> {
        Err(KernelError::Unsupported {
            what: "plan file".to_owned(),
        })
    }

    /// `/cd <path>`（tui-manage.md §7，L29）：解析与检查路径；有会话时经会话 actor 换工作目录。错误是给用户看的
    /// 文字（`Failed`）。
    async fn change_directory(
        &self,
        _request: crate::manage::ChangeDirectory,
    ) -> Result<crate::manage::DirectoryChanged, KernelError> {
        Err(KernelError::Unsupported {
            what: "changing the working directory".to_owned(),
        })
    }

    /// `/add-dir <path>` 的确认框之前（docs/specs/rust-cli/permission-updates.md §6.1）：解析、检查路径与是否已在工作目录里，不加。
    /// 用不了的原因是给用户看的文字（`Failed`；有会话时已记进对话）。
    async fn check_directory(
        &self,
        _request: crate::manage::AddDirectory,
    ) -> Result<crate::manage::DirectoryChecked, KernelError> {
        Err(KernelError::Unsupported {
            what: "checking a working directory".to_owned(),
        })
    }

    /// `/add-dir <path>`（docs/specs/rust-cli/session-dirs.md §5.1）：解析与检查路径；有会话时经会话 actor 加进去。没加进去的
    /// 原因是给用户看的文字（`Failed`）。`remember` 时另存进本机项目规则集（permission-updates.md §6.1）。
    async fn add_directory(
        &self,
        _request: crate::manage::AddDirectory,
    ) -> Result<crate::manage::DirectoryAdded, KernelError> {
        Err(KernelError::Unsupported {
            what: "adding a working directory".to_owned(),
        })
    }

    /// `/mcp reconnect|enable|disable`（tui-manage.md §6，L29）：经 ext 的连接管理改连接，返回这些服务器
    /// 改动后的状态。默认 `Unsupported`。
    async fn mcp_control(
        &self,
        _control: crate::manage::McpControl,
    ) -> Result<Vec<crate::manage::McpServerInfo>, KernelError> {
        Err(KernelError::Unsupported {
            what: "MCP server control".to_owned(),
        })
    }

    /// 开始一个远程 MCP 服务器的交互授权（OAuth，docs/specs/rust-cli/ext.md §3.12.6）：返回授权地址与完成的 future（完成后
    /// ext 重连这个服务器）。不用 OAuth、这个内核不能授权时报错。默认 `Unsupported`。
    async fn mcp_sign_in(&self, _server: String) -> Result<crate::manage::McpSignIn, KernelError> {
        Err(KernelError::Unsupported {
            what: "MCP sign-in".to_owned(),
        })
    }

    /// 一次交互授权（按开始时给的地址认）的结局（docs/specs/rust-cli/ext.md §3.12.14，App 的 `mcp.oauth.signInOutcome`）：还在进行就等它。
    /// 不用 OAuth、这个内核不能授权时报错。默认 `Unsupported`。
    async fn mcp_sign_in_outcome(
        &self,
        _server: String,
        _url: String,
    ) -> Result<crate::manage::McpSignInOutcome, KernelError> {
        Err(KernelError::Unsupported {
            what: "MCP sign-in".to_owned(),
        })
    }

    /// 这套内核里用 OAuth 的服务器的授权状态（docs/specs/rust-cli/ext.md §3.12.9，App 的 `mcp.oauth.servers`）：内核级与会话级的配置都算。
    /// 这个内核不能授权时没有。默认没有。
    async fn mcp_oauth_servers(&self) -> Vec<crate::manage::McpOAuthServer> {
        Vec::new()
    }

    /// Sign out 一个远程 MCP 服务器（docs/specs/rust-cli/ext.md §3.12.6）：删掉存下的令牌，这个进程里这个授权目标的连接重连
    /// （§3.12.14），给出这套内核里它之后的状态（要授权；没有它的连接时 None）。
    /// 删不掉时报错。默认 `Unsupported`。
    async fn mcp_sign_out(
        &self,
        _server: String,
    ) -> Result<Option<crate::manage::McpServerInfo>, KernelError> {
        Err(KernelError::Unsupported {
            what: "MCP sign-out".to_owned(),
        })
    }

    /// 当前生效的 hook 配置（`/hooks`，tui-display.md §3.5，L28）：只读。默认没有。
    async fn hooks(&self) -> Vec<crate::manage::HookEntry> {
        Vec::new()
    }

    /// `/diff` 的未提交改动（tui-manage.md §4，L29）：执行环境里 git 的原始输出。
    async fn uncommitted_diff(
        &self,
        _workspace: &crate::env::WorkspaceRef,
    ) -> Result<crate::env_git::GitDiff, KernelError> {
        Err(KernelError::Unsupported {
            what: "uncommitted diff".to_owned(),
        })
    }

    /// 状态栏第 2 行的分支与未提交的增删行数（tui-display.md §2.6）：分支读 `.git/HEAD`，行数是执行环境里
    /// `git diff HEAD --numstat` 的合计。不是 git 仓库时两样都没有（不是错误）。
    async fn working_tree(
        &self,
        _workspace: &crate::env::WorkspaceRef,
    ) -> Result<crate::env_git::WorkingTree, KernelError> {
        Err(KernelError::Unsupported {
            what: "working tree".to_owned(),
        })
    }

    /// `/export <文件名>`（tui-manage.md §3，L29）：经执行环境写文件，返回写入的绝对路径。
    async fn write_export(&self, _file: crate::manage::ExportFile) -> Result<String, KernelError> {
        Err(KernelError::Unsupported {
            what: "exporting the conversation".to_owned(),
        })
    }

    /// `/permissions` 的规则列表（tui-manage.md §1.1，L29）：规则快照、本机项目规则集（现读）与会话规则。
    async fn permission_rules(
        &self,
        _query: crate::permission::PermissionRulesQuery,
    ) -> Result<Vec<crate::permission::SourcedRule>, KernelError> {
        Err(KernelError::Unsupported {
            what: "permission rules".to_owned(),
        })
    }

    /// `/permissions` 的修改（tui-manage.md §1.1）：落库后换上新的规则快照，返回修改后的列表。
    async fn change_permission_rules(
        &self,
        _change: crate::permission::PermissionRulesChange,
    ) -> Result<Vec<crate::permission::SourcedRule>, KernelError> {
        Err(KernelError::Unsupported {
            what: "changing permission rules".to_owned(),
        })
    }

    /// 存储的只读查询与前端记录（审查 K7：`/usage` 的 API 时长、footer 的请求统计、App 的命令事实）：前端直接问存储，
    /// 不经会话 actor。以前是 4 个透传的方法（`session_metering`、`request_stats`、`put_session_record`、
    /// `session_records`）。
    fn queries(&self) -> Arc<dyn crate::store::StoreQueries>;

    /// 活动统计（`/stats`，usage.md §5）：今天与时区偏移取自内核的时钟，聚合在存储。默认 `Unsupported`。
    async fn activity(
        &self,
        _range: crate::metering::ActivityRange,
    ) -> Result<crate::metering::ActivityStats, KernelError> {
        Err(KernelError::Unsupported {
            what: "activity stats".to_owned(),
        })
    }

    /// ← 项目页与 `/resume` 页的会话（修复线 F29，tui-panels.md §2.1）：存储的查询加本进程里打开着的会话的
    /// 实时状态与目录的分支。默认 `Unsupported`。
    async fn project_sessions(
        &self,
        _query: crate::store::SessionQuery,
    ) -> Result<crate::panels::ProjectSessions, KernelError> {
        Err(KernelError::Unsupported {
            what: "project sessions".to_owned(),
        })
    }

    /// 写现在的会话的现场（tui-cockpit.md §4）；跨会话关闭时什么也不做。
    async fn set_presence(&self, _session: &SessionId, _presence: crate::peers::PeerPresence) {}

    /// 让别的进程中止在跑的回合（tui-cockpit.md §1.3）。
    async fn stop_peer(&self, _address: &crate::peers::PeerAddress) -> Result<(), KernelError> {
        Err(KernelError::Unsupported {
            what: "stop peer".to_owned(),
        })
    }

    /// 给会话改名（F29，tui-manage.md §10.6）：打开着的经它的 actor（`Command::Rename`），否则直接写存储。
    async fn rename_session(
        &self,
        _session: &SessionId,
        _title: String,
    ) -> Result<(), KernelError> {
        Err(KernelError::Unsupported {
            what: "renaming sessions".to_owned(),
        })
    }

    /// 删除会话（F29，tui-manage.md §10.6）：打开着的先关掉（中止在跑的回合），再存储软删除；草稿关掉就是删掉
    /// （docs/specs/rust-cli/session-facts.md §5）。
    async fn delete_session(&self, _session: &SessionId) -> Result<(), KernelError> {
        Err(KernelError::Unsupported {
            what: "deleting sessions".to_owned(),
        })
    }

    /// 归档会话的永久删除的预检（docs/specs/rust-cli/session-facts.md §5.1）：根与自有后代、
    /// forks、保留的附件、文件数与阻止；`workspace` 是前端指名的工作区（根在别的工作区时阻止 `OtherWorkspace`）。只读：不占会话、不关会话。
    async fn deletion_preview(
        &self,
        _root: &SessionId,
        _workspace: &WorkspaceRef,
    ) -> Result<crate::deletion::DeletionPreview, KernelError> {
        Err(KernelError::Unsupported {
            what: "permanent deletion".to_owned(),
        })
    }

    /// 永久删除（同上）：重新预检、比 `revision`，占住树里每个会话的条目（在用的阻止、空闲的关掉），写删除凭据（`operation` 是它的
    /// operation id），先删文件再一个事务删行。
    async fn delete_permanently(
        &self,
        _root: &SessionId,
        _workspace: &WorkspaceRef,
        _revision: &str,
        _operation: &str,
    ) -> Result<crate::deletion::DeletionResult, KernelError> {
        Err(KernelError::Unsupported {
            what: "permanent deletion".to_owned(),
        })
    }

    /// 会话的只读快照（F29，`/resume` 页的预览与全屏对话）：打开着的问它的 actor，否则从存储冷加载；不打开会话、
    /// 不启动 actor、不跑 SessionStart。
    async fn session_snapshot(&self, _session: &SessionId) -> Result<SessionView, KernelError> {
        Err(KernelError::Unsupported {
            what: "session snapshots".to_owned(),
        })
    }

    /// 前端冷读只读最后一段的视图（docs/specs/rust-cli/node-store.md §5.6）：打开着的问它的 actor（整段、子代理读库）；否则只读最后 `records` 条存储消息
    /// （补到回合开头，[`crate::store::StoreQueries::range`]），视图只带这一段，模型与模式的缺省、上下文用量、任务清单同 [`Self::session_snapshot`]。
    /// 不打开会话、不跑 SessionStart。缺省是整段：[`Self::session_snapshot`] 加库里的全部子代理（只读整段的内核与以前的冷读相同）。
    async fn session_tail(
        &self,
        session: &SessionId,
        _records: usize,
    ) -> Result<SessionTail, KernelError> {
        let view = self.session_snapshot(session).await?;
        // 库里没有这个会话（没有独立记录的子会话）时没有子代理（同前端冷读以前的读法）
        let subagents = match self.queries().subagents(session).await {
            Err(crate::store::StoreError::NotFound { .. }) => Vec::new(),
            read => read?,
        };
        Ok(SessionTail {
            view,
            subagents,
            earlier: false,
        })
    }

    /// 回退文件的预览（docs/specs/rust-cli/session-lifecycle.md §10.53）：打开着的问它的 actor，否则从存储冷加载；不打开会话、不跑
    /// SessionStart。`target` 不是可选的输入时为 None。
    async fn rewind_preview(
        &self,
        _session: &SessionId,
        _target: &MessageId,
    ) -> Result<Option<crate::rewind_preview::RewindPreview>, KernelError> {
        Err(KernelError::Unsupported {
            what: "rewind previews".to_owned(),
        })
    }

    /// 一个回合的文件 diff（session-lifecycle.md §10.45）：打开着的问它的 actor，否则从存储冷加载；不打开会话、不启动 actor、不跑 SessionStart。
    async fn turn_file_diffs(
        &self,
        _session: &SessionId,
        _turn: &TurnId,
    ) -> Result<Vec<TurnFileDiff>, KernelError> {
        Err(KernelError::Unsupported {
            what: "turn file diffs".to_owned(),
        })
    }

    /// 会话可用的工具、agent 与 skill（`-p` 的 stream-json 写 `system/init`，docs/specs/rust-cli/headless.md §3.3，
    /// 修复线 F39）：只读。默认 `Unsupported`。
    async fn session_catalog(
        &self,
        _query: crate::catalog::CatalogQuery,
    ) -> Result<crate::catalog::SessionCatalog, KernelError> {
        Err(KernelError::Unsupported {
            what: "session catalog".to_owned(),
        })
    }

    /// 工作区的 agent 类型（agents.md §8 的 PT 增量）：不要会话，顺序同 agent 目录。缺省空。
    async fn agent_definitions(&self, _workspace: &WorkspaceRef) -> Vec<crate::catalog::AgentInfo> {
        Vec::new()
    }

    /// 旧会话的询问（docs/specs/rust-cli/agents.md §4.1.2，工作线 F72）：先给出还没回答的，再给出之后的变化。缺省
    /// 没有（流不结束）。
    fn detached_asks(&self) -> BoxStream<'static, crate::interaction::DetachedEvent> {
        futures_util::stream::pending().boxed()
    }

    /// 回答旧会话的询问（回答同 [`Command::ResolveInteraction`]）。缺省 `Unsupported`。
    async fn resolve_detached(
        &self,
        _interaction: &InteractionId,
        _answer: crate::interaction::Answer,
    ) -> Result<(), KernelError> {
        Err(KernelError::Unsupported {
            what: "answering questions of closed sessions".to_owned(),
        })
    }

    /// 持久输入的接纳回执（pg-store.md §4）：不依赖连接内存或前端兼容投影。没有输入账本时没有回执。
    async fn input_receipt(
        &self,
        _session: &SessionId,
        _command: &CommandId,
    ) -> Result<Option<Ack>, KernelError> {
        Ok(None)
    }

    /// 工作区的会话（与 [`Kernel::list_sessions`] 同一个来源），合并本进程里打开着的会话的实时状态
    /// （← 面板，tui-panels.md §2.1，L30）。默认没有实时状态。
    async fn workspace_sessions(
        &self,
        filter: SessionFilter,
    ) -> Result<Vec<crate::panels::WorkspaceSession>, KernelError> {
        Ok(self
            .list_sessions(filter)
            .await?
            .into_iter()
            .map(|summary| crate::panels::WorkspaceSession {
                summary,
                activity: None,
                last_reply: None,
            })
            .collect())
    }
}

#[path = "session_view.rs"]
mod view;
