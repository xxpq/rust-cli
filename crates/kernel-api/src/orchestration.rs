//! 能力之间的编排接口（docs/specs/rust-cli/README.md §3.1 规则 3）。
//!
//! 内核能力 crate 只依赖 kernel-api，彼此通过这里的 trait 调用，由 runtime 组装实现：
//! - query 用 [`ContextBuilder`]、[`ToolRegistry`]、[`ToolExecutor`]、[`Compactor`]、[`HookRunner`]；
//! - tools 的执行管线用 [`PermissionGate`]、[`HookRunner`]、[`Interactions`]、[`SessionOps`]；
//! - agents 与 compact 用 [`QueryRunner`] 跑子循环或摘要请求。

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc;

use crate::abort::Abort;
use crate::env::WorkspaceRef;
use crate::ids::{AgentId, MessageId, RunId, SessionId, ToolUseId};
use crate::mcp_state::{McpConnections, McpView};
use crate::message::{AttachmentMessage, CompactTrigger, ContentBlock, Message};
use crate::model::{BlockDelta, ModelError, ModelSelection, QuerySource, SystemBlock};
use crate::permission::{PermissionDecision, PermissionMode};
use crate::plan::PlanModeFlags;
use crate::store::StoreError;
use crate::tool::{Tool, ToolOutput, ToolProgress, ToolUseContext};

pub use crate::compaction::{
    AutoCompactOutcome, CompactMeter, CompactProgress, CompactRequest, CompactResult,
    CompactTracking, Compactor,
};
pub use crate::hooks::{Absorbed, AbsorbedNotice};
pub use crate::hooks::{HookContext, HookEvent, HookEventName, HookOutcome, HookRunner, Steered};
pub use crate::hooks::{StopCondition, StopVerdict};

/// 一次主循环（或子代理、摘要）所需的输入。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryRequest {
    pub session: SessionId,
    pub run: RunId,
    pub workspace: WorkspaceRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentId>,
    pub messages: Vec<Message>,
    pub selection: ModelSelection,
    pub mode: PermissionMode,
    pub source: QuerySource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u32>,
    /// 这次运行的工具缺省在哪台机器上跑（R3，remote-exec.md §20.6）：子代理派生时 Agent 的 `machine`；主线程是主 env
    /// （不写出）。一次运行里不变（工作线 F134，审查 K2：以前是 `RunSink::default_machine`，主循环每一步与每次工具调用
    /// 各问一次输出端）。
    #[serde(default, skip_serializing_if = "crate::ids::MachineName::is_primary")]
    pub default_machine: crate::ids::MachineName,
    /// 人类输入的 UserPromptSubmit 结果（hook 的附加上下文等，还没有提交）。主循环在运行开始时先产生当步的
    /// attachment（[`ContextBuilder::turn_attachments`]），再提交它们（先产生 attachment、再跑
    /// UserPromptSubmit，docs/specs/rust-cli/query.md §1.1）。其他运行为空（修复线 F10 新增）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prompt_hook_messages: Vec<Message>,
    /// 这次运行所属的顶层会话（按会话拥有的资源——会话级的 MCP 连接——按它取，工作线 F145，
    /// docs/specs/rust-cli/shared-workspaces.md §5）：子代理派生时是派生它的顶层会话；None 是 `session` 自己。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<SessionId>,
    /// 这次运行不许用的工具（docs/specs/rust-cli/session-lifecycle.md §10.15，K-C2）：工具名。主线程是开这一轮的输入带的名单，
    /// 子代理是派生它的运行的名单；名单里的工具不在这次运行的清单里，调用时找不到。serde 缺省为空。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disallowed_tools: Vec<String>,
    /// 这次运行的附加工作目录（docs/specs/rust-cli/session-dirs.md §3.3）：主线程是开回合时会话的，子代理是派生时复制的；
    /// 主循环每一步优先读输出端的 [`RunView`]，输出端不持有会话时用这一份。serde 缺省为空。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub additional_directories: Vec<crate::env::EnvPath>,
    /// 这次运行的会话偏好（docs/specs/rust-cli/app-prefs.md §4.2）：主线程是会话的，子代理是派生它的运行的，摘要请求是压缩请求的。
    /// 主循环按它定这次运行的禁用名单（ToolSearch）与上下文的记忆。serde 缺省不关。
    #[serde(
        default,
        skip_serializing_if = "crate::session_preferences::SessionPreferences::is_default"
    )]
    pub preferences: crate::session_preferences::SessionPreferences,
    /// 这次运行的执行覆盖（session-lifecycle.md §10.39）：主线程是开这一轮的输入带的，子代理是派生时跟随的（agents.md §4.1）；
    /// None 时请求用 `selection`。`selection` 是会话的模型，也是退回目标。只在内存，不序列化。
    #[serde(skip)]
    pub execution: Option<crate::turn_execution::RunExecution>,
    /// 发起这次运行那一刻会话钉住的模型可见输入（docs/specs/rust-cli/context.md §15.3）：输出端不持有会话（压缩的摘要请求）时
    /// 每一步用它；持有会话的每一步读 `RunView.prompt`（压缩之后重钉的那一份）。fork 子代理是派生它的运行的。只在内存。
    #[serde(skip)]
    pub prompt: Arc<crate::session_prompt::SessionPrompt>,
}

impl QueryRequest {
    /// 这次运行所属的顶层会话（[`Self::root`]，没有时是 `session`）。
    pub fn root(&self) -> &SessionId {
        self.root.as_ref().unwrap_or(&self.session)
    }

    /// 一次运行的输入：不是子代理、没有轮数上限、缺省机器是主 env、没有待提交的 UserPromptSubmit 消息（各自的调用方用
    /// 结构更新填上）。
    pub fn new(
        session: SessionId,
        run: RunId,
        workspace: WorkspaceRef,
        messages: Vec<Message>,
        selection: ModelSelection,
        mode: PermissionMode,
        source: QuerySource,
    ) -> Self {
        Self {
            session,
            run,
            workspace,
            agent: None,
            messages,
            selection,
            mode,
            source,
            max_turns: None,
            default_machine: crate::ids::MachineName::primary(),
            prompt_hook_messages: Vec::new(),
            root: None,
            disallowed_tools: Vec::new(),
            additional_directories: Vec::new(),
            preferences: Default::default(),
            execution: None,
            prompt: Default::default(),
        }
    }
}

/// 主循环产生的、不需要持久化的事件（流式增量与工具进度）。完整的消息经
/// [`RunSink::commit_message`] 提交。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum QueryEvent {
    /// 模型流里一个块开始了（模型的 [`crate::model::ModelEvent::BlockStart`] 原样转出，早于这个块的增量）：tool_use 的
    /// ID 与名字在这里（工作线 F147，docs/specs/rust-cli/headless.md §3.7）。`message` 同 [`Self::Delta`]。
    BlockStart {
        message: usize,
        index: usize,
        block: ContentBlock,
    },
    /// 模型流的增量；`message` 是正在生成的 assistant 消息的位置序号（本次运行内）。`id` 是这条 assistant 消息的 id
    /// （流开始之前就定了，落库的就是它）；`tool_use` 是工具输入的增量属于哪个调用（别的增量为 None）。前端按这两个
    /// 把增量对到它最后落成的行（docs/specs/rust-cli/session-lifecycle.md §10.13，K-I3）。
    Delta {
        message: usize,
        id: MessageId,
        index: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_use: Option<ToolUseId>,
        delta: BlockDelta,
    },
    ToolStarted {
        tool_use: ToolUseId,
        name: String,
        input: Value,
        /// 执行器开工时读的时钟（Unix 毫秒）：与这次调用的计量（`ToolCall.started_at_ms`）是同一次读
        /// （docs/specs/rust-cli/session-lifecycle.md §10.37）。
        started_at: u64,
    },
    ToolProgress {
        tool_use: ToolUseId,
        progress: ToolProgress,
    },
    ToolFinished {
        tool_use: ToolUseId,
        output: ToolOutput,
        /// 因停止而结束（用户中断、退出，或提交新输入时可取消的工具）：结果是拒绝的文字，不是工具自己的失败
        /// （docs/specs/rust-cli/session-lifecycle.md §10.17）。serde 缺省 false。
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        cancelled: bool,
    },
    /// 模型端口的通知（重试、停顿、恢复），只供显示（docs/specs/rust-cli/stall-visibility.md §1）。
    ModelNotice { notice: crate::model::ModelNotice },
    /// 这一轮的执行覆盖退回了（docs/specs/rust-cli/session-lifecycle.md §10.48）：主循环换成会话的模型的那一刻发，`reason` 是命中的规则的原因。
    /// 会话 actor 把它记进回合记录、换成 `KernelEvent::ExecutionFellBack`。只有带退回规则的运行会发（TUI 与 `-p` 没有）。
    ExecutionFellBack { reason: String },
    /// 只给前端显示的提示：不进模型请求、不落库（Stop hook 连续阻塞到上限的警告，docs/specs/rust-cli/query.md §3 的 Stop hooks）。
    /// 会话 actor 换成 `KernelEvent::SystemNotice`（种类原样带过去）。
    Notice {
        level: crate::session_api::NoticeLevel,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<crate::session_api::NoticeKind>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum QueryStop {
    Completed,
    MaxTurns,
    Aborted,
    /// 工具的 hook 要求停止（PreToolUse / PostToolUse 的 `continue: false`，docs/specs/rust-cli/hooks.md §6）：本批的结果
    /// 与停止说明都已提交，不再请求模型（终止原因 `hook_stopped`，query.md §3）。
    HookStopped,
    /// Stop hook（子代理里 SubagentStop）阻止继续（`prevent_continuation`）：结束说明已提交，不再请求模型（终止原因
    /// `stop_hook_prevented`，docs/specs/rust-cli/query.md §3 的 Stop hooks，工作线 F134）。
    StopHookPrevented,
    /// UserPromptSubmit 拦下了开回合的输入（只由会话的人类输入回合给出，输入没有提交）；会话记为
    /// [`crate::session_api::TurnOutcome::PromptBlocked`]。
    Blocked {
        message: String,
    },
    /// 恢复分支用尽后以 API 错误结束（终止原因 `api_error` / `malformed_tool_use_exhausted`；
    /// `message` 是给用户的错误消息）。
    ApiError {
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
pub enum QueryError {
    #[error(transparent)]
    Model(#[from] ModelError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("not implemented: {what}")]
    Unsupported { what: String },
}

/// 主循环的输出端，由会话 actor 实现（子代理、压缩各有自己的实现）。
///
/// 提交屏障：`commit_message` 与 `commit_intent` 在存储事务成功后才返回，主循环拿到 `Ok` 才继续
/// 下一步（执行工具、发下一次请求）。这样崩溃后，存储里的事实总是领先于外部副作用。
/// 待回答的交互（[`Interactions`]）与会话的读和命令（[`SessionOps`]）也经这里：会话 actor 登记后通知前端（只在内存，
/// 工作线 F130）。
///
/// 方法都没有缺省体（工作线 F134，审查 K6）：以前 8 个缺省体只服务测试替身，包装层漏转发一个也能编译。测试替身用
/// `testing::SinkOverride`（全委托给一个内层输出端，只覆盖关心的方法）。
#[async_trait]
pub trait RunSink: Interactions + SessionOps {
    /// 输入消息与输入账本的提升同事务（pg-store.md §4）；没有持久输入账本的输出端按 [`Self::commit_message`] 提交。
    async fn commit_input(
        &self,
        message: Message,
        commands: Vec<crate::ids::CommandId>,
    ) -> Result<(), StoreError>;

    /// 一条完整的消息（assistant、tool_result、attachment、system）：提交、进入会话历史、通知前端。
    async fn commit_message(&self, message: Message) -> Result<(), StoreError>;

    /// 不需要持久化的事件；返回 false 表示接收方已经关闭，主循环按中止处理。
    async fn emit(&self, event: QueryEvent) -> bool;

    /// 一组必须一起成立的消息（压缩的边界、摘要与附件，docs/specs/rust-cli/compact.md §6）：在一个
    /// 存储事务里提交。没有事务的输出端（只收集的、子代理的）逐条提交。
    async fn commit_messages(&self, messages: Vec<Message>) -> Result<(), StoreError>;

    /// 一条消息与它的计量事实在一个存储事务里提交（docs/specs/rust-cli/usage.md §2.5）：assistant 消息带这次
    /// 模型请求，工具结果带这次工具调用；`message` 为 None 时只提交计量（没有内容的失败请求、丢弃的响应）。
    /// 没有存储的输出端只提交消息、丢弃计量。
    async fn commit_metered(
        &self,
        message: Option<Message>,
        metered: crate::metering::Metered,
    ) -> Result<(), StoreError>;

    /// 主循环开始一次压缩（自动或反应式），前端据此显示进度；没有前端的输出端什么都不做。
    async fn compact_started(&self, trigger: CompactTrigger);

    /// 开始了的压缩失败了、回合不因此结束（docs/specs/rust-cli/compact.md §4.1），前端据此收起进度；没有前端的
    /// 输出端什么都不做。
    async fn compact_failed(&self, trigger: CompactTrigger, message: String);

    /// 工具边界并入的插话（`Delivery::Steer`，带各自 UserPromptSubmit 的消息）与发给这次运行的通知，按到达顺序
    /// （L18，docs/specs/rust-cli/query.md §4、§5）。取走即属于当前回合；本回合已中止时插话不取，继续排队。顶层会话的
    /// 通知由会话 actor 在回合结束后开新回合；子代理收到它派生的后台子代理的通知（agents.md §5.1）。工作线 F134 以前
    /// 另有 `take_steering`、`take_steered`、`take_notices` 三个缺省方法拼出它，主循环只调这一个。
    async fn take_absorbed(&self) -> Vec<Absorbed>;

    /// 报告一次目标评估（goal.md §3）：actor 按判定提交反馈与 `goal_status`、改目标，返回已提交的
    /// 消息（主循环并入自己的历史）。`condition` 不是当前目标时什么都不做；没有目标的输出端（子代理）不提交。
    async fn goal_checked(
        &self,
        condition: String,
        verdict: crate::hooks::StopVerdict,
    ) -> Result<Vec<Message>, StoreError>;
}

#[async_trait]
pub trait QueryRunner: Send + Sync {
    /// 跑到结束、中断或出错。消息经 `sink` 提交，返回值只给出停止原因。
    async fn run(
        &self,
        request: QueryRequest,
        sink: Arc<dyn RunSink>,
        abort: Abort,
    ) -> Result<QueryStop, QueryError>;
}

/// 组装一次模型请求的上下文所需的输入。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextRequest {
    pub session: SessionId,
    pub workspace: WorkspaceRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentId>,
    pub source: QuerySource,
    pub mode: PermissionMode,
    pub tool_names: Vec<String>,
    /// 这一步请求的模型：决定模型身份段、知识截止时间与提示词变体。
    pub selection: ModelSelection,
    /// 计划模式的会话标志，与 `mode` 来自会话 actor 的同一个快照（docs/specs/rust-cli/context.md
    /// §13.4）。没有会话 owner 的输出端（子代理）为默认值。
    #[serde(default)]
    pub plan_flags: PlanModeFlags,
    /// 这一步模型的能力（生效配置，docs/specs/rust-cli/model.md §11，修复线 F23）：system 里提醒的措辞看它是否
    /// 支持对话中间的 system 消息。模型端口答不出时为 None（按模型卡片回退）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<crate::provider::ModelCapabilities>,
    /// 这一步的 MCP 状态，与 `tool_names` 出自同一份工具快照（[`ToolRegistry::snapshot`]，修复线 F36）：延迟工具
    /// 清单里的连接中服务器与 `mcp_instructions_delta` 看它（context.md §4.2）。没有快照的调用方为 None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp: Option<McpView>,
    /// 这次运行的缺省机器（远端执行 F98，docs/specs/rust-cli/remote-exec.md §25.2.1）：子代理派生时 Agent 的 `machine`
    /// （[`QueryRequest::default_machine`]）；主 env 不写出。机器清单提醒按它写“不写 `machine` 时在哪台机器上”。
    #[serde(default, skip_serializing_if = "crate::ids::MachineName::is_primary")]
    pub default_machine: crate::ids::MachineName,
    /// 这次运行所属的顶层会话（[`QueryRequest::root`]，工作线 F145）：延迟工具清单与 MCP instructions 按它的工具视图
    /// （[`ToolRegistry::view`]）；None 是 `session` 自己。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<SessionId>,
    /// 这一步会话的附加工作目录（docs/specs/rust-cli/session-dirs.md §3.3）：环境段、nested memory 与 skill / agent 清单按它，
    /// 与 `mode`、`workspace` 出自同一个 [`RunView`]。serde 缺省为空。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub additional_directories: Vec<crate::env::EnvPath>,
    /// 这次运行的会话偏好（[`QueryRequest::preferences`]）：关了自动记忆时没有记忆段与 MEMORY.md（app-prefs.md §4.3）。serde 缺省不关。
    #[serde(
        default,
        skip_serializing_if = "crate::session_preferences::SessionPreferences::is_default"
    )]
    pub preferences: crate::session_preferences::SessionPreferences,
    /// 这一步会话钉住的模型可见输入（docs/specs/rust-cli/context.md §15.3）：system 段读 `system`，前端上下文与 skill 提及读
    /// `live`；与 `mode`、`workspace` 出自同一个 [`RunView`]。只在内存。
    #[serde(skip)]
    pub prompt: Arc<crate::session_prompt::SessionPrompt>,
}

impl ContextRequest {
    /// 这次运行所属的顶层会话（[`Self::root`]，没有时是 `session`）。
    pub fn root(&self) -> &SessionId {
        self.root.as_ref().unwrap_or(&self.session)
    }

    /// 一步请求的上下文输入：没有子代理、工具名、计划模式标志、能力、MCP 状态与缺省机器（主循环按这一步的快照
    /// 用结构更新填上）。
    pub fn new(
        session: SessionId,
        workspace: WorkspaceRef,
        source: QuerySource,
        mode: PermissionMode,
        selection: ModelSelection,
    ) -> Self {
        Self {
            session,
            workspace,
            agent: None,
            source,
            mode,
            tool_names: Vec::new(),
            selection,
            plan_flags: PlanModeFlags::default(),
            capabilities: None,
            mcp: None,
            default_machine: crate::ids::MachineName::default(),
            root: None,
            additional_directories: Vec::new(),
            preferences: Default::default(),
            prompt: Default::default(),
        }
    }
}

/// system prompt 段落，以及这一步要追加的 attachment。
///
/// attachment 发给模型时的位置由 normalize 的规则决定，不在这里表达：
/// [`AttachmentMessage::role`] 为 user 的排在本轮人类输入之前、并进同一条 user 消息；为 system
/// 的折叠成紧跟其后的 `role: system` 消息（docs/specs/rust-cli/context.md §3）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BuiltContext {
    pub system: Vec<SystemBlock>,
    /// 这一步新增的 attachment；会作为消息落库，resume 后原样重放。
    pub attachments: Vec<AttachmentMessage>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OrchestrationError {
    #[error("{message}")]
    Failed { message: String },
    #[error("not implemented: {what}")]
    Unsupported { what: String },
    /// 会话已经关闭（会话 actor 收不下请求了；工作线 F130，以前各处写 `Failed { "the session is closed" }`）。
    #[error("the session is closed")]
    Closed,
}

#[async_trait]
pub trait ContextBuilder: Send + Sync {
    /// 一步的 system 段与这一步的 attachment。实现了 [`Self::turn_attachments`] 的，这里只给每一步的上下文
    /// 宣布（规则文件、会话上下文、日期、署名）。
    async fn build(
        &self,
        request: ContextRequest,
        history: &[Message],
    ) -> Result<BuiltContext, OrchestrationError>;

    /// 当步的 attachment（环境、模型、清单、nested memory、计划模式、剩余 token 等）。
    /// 主循环在运行开始（人类输入之后、UserPromptSubmit 的结果之前）与每个工具轮结束时调用，排在下一次自动
    /// 压缩之前（docs/specs/rust-cli/query.md §1.1，修复线 F10 新增）。缺省为空：没有拆开的实现在 `build`
    /// 里给出全部 attachment。
    async fn turn_attachments(
        &self,
        _request: ContextRequest,
        _history: &[Message],
    ) -> Result<Vec<AttachmentMessage>, OrchestrationError> {
        Ok(Vec::new())
    }

    /// 一次运行开始时的当步 attachment。主循环在运行开始时调它（不是 [`Self::turn_attachments`]），工具轮结束时
    /// 仍调 `turn_attachments`。缺省同 `turn_attachments`（主线程两处相同）；子代理给出进入子循环
    /// 前产生的那几项（延迟工具、环境、模型、skill，docs/specs/rust-cli/agents.md §4.3，修复线 F16 新增）。
    async fn start_attachments(
        &self,
        request: ContextRequest,
        history: &[Message],
    ) -> Result<Vec<AttachmentMessage>, OrchestrationError> {
        self.turn_attachments(request, history).await
    }
}

/// 新工作目录里还没宣布过的规则文件（`/cd` 给模型的说明的后半，docs/specs/rust-cli/tui-manage.md §7.1）。
/// context 实现；会话 actor 判定可以移动后用自己的活动历史调用（不复制历史）。
#[async_trait]
pub trait DirectoryRules: Send + Sync {
    /// `workspace.root` 的规则文件文字（`instructions` 的格式）；没有新文件时为 None。
    async fn directory_rules(
        &self,
        workspace: &WorkspaceRef,
        history: &[Message],
    ) -> Option<String>;
}

/// 没有规则文件的来源（测试与嵌入）。
pub struct NoDirectoryRules;

#[async_trait]
impl DirectoryRules for NoDirectoryRules {
    async fn directory_rules(
        &self,
        _workspace: &WorkspaceRef,
        _history: &[Message],
    ) -> Option<String> {
        None
    }
}

/// 当前可用的工具（内置 + MCP + 插件），按名字或别名查找。
///
/// 方法都没有缺省体（工作线 F134，modularity.md §7 F132-2）：装饰链（内置 → 扩展 → 判决链过滤 → Agent 等工具 →
/// 子代理的过滤）的每一层显式决定每个方法转发还是自己回答，漏写是编译错误（以前有的层不转发 `hides` / `withheld`
/// 只是因为位置刚好）。测试替身用 `testing::RegistryOverride`。
pub trait ToolRegistry: Send + Sync {
    fn tools(&self) -> Vec<Arc<dyn Tool>>;
    fn find(&self, name: &str) -> Option<Arc<dyn Tool>>;

    /// 注册了、但按暴露规则不出现在 [`Self::tools`] 里的工具（有 Bash 时的 Glob / Grep，内嵌搜索，
    /// docs/specs/rust-cli/tools-files.md §1.1）。子代理的工具清单点名它们、又没有 Bash 时补回（
    /// agents.md §4.2，修复线 F16 新增）。
    fn withheld(&self, name: &str) -> Option<Arc<dyn Tool>>;

    /// 叠在这个注册表上的工具里，名字为 `name` 的要不要藏起来（整个工具的 deny 规则把它从清单里去掉，
    /// docs/specs/rust-cli/tools-files.md §1.1.1，工作线 F69 新增）。外层注册表（Agent 等工具）按它过滤自己的工具。
    fn hides(&self, name: &str) -> bool;

    /// [`Self::tools`] 的顺序就是发给模型的顺序（子代理按点名的工具清单排，agents.md §4.2，修复线 F16 新增）。
    /// 为 false 时主循环发请求前按名字排序（tools-more.md §3.3）。
    fn keeps_order(&self) -> bool;

    /// 同一时点的工具与 MCP 状态（docs/specs/rust-cli/query.md §1.3，修复线 F36）：主循环在运行开始与每个工具批
    /// 之后各取一次，当步 attachment 与之后的请求都用它。`session` 是
    /// 这次运行的会话（WaitForMcpServers 的 declared 按会话，ext.md §3.8）。没有 MCP 的注册表是 [`Self::tools`]、
    /// 没有 MCP 状态。
    fn snapshot(&self, session: &SessionId) -> ToolSnapshot;

    /// MCP 连接（ToolSearch、WaitForMcpServers 与直接调用连接中服务器的工具时等待，ext.md §3.8，修复线 F36）。
    fn mcp(&self) -> Option<Arc<dyn McpConnections>>;

    /// 这个注册表在顶层会话 `root` 的运行里的样子（工作线 F145，docs/specs/rust-cli/shared-workspaces.md §5）：按会话
    /// 拥有的工具（会话级的 MCP 连接）只出现在视图里。主循环在运行开始时取一次（`QueryRequest::root`），之后的快照、
    /// 查找与 MCP 等待都在视图上。没有按会话部分的注册表给出与自己等价的视图；装饰层取内层的视图再包一层。
    fn view(self: Arc<Self>, root: &SessionId) -> Arc<dyn ToolRegistry>;
}

/// [`ToolRegistry::snapshot`] 的结果。
#[derive(Clone, Default)]
pub struct ToolSnapshot {
    pub tools: Vec<Arc<dyn Tool>>,
    /// 注册表接了 MCP 时，与 `tools` 同一时点的连接状态；没有时 None。
    pub mcp: Option<McpView>,
}

/// 执行管线的完整结果：工具结果加上 hook 产生的消息（docs/specs/rust-cli/hooks.md §6）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolExecution {
    pub output: ToolOutput,
    /// 排在 tool_result 之前提交的消息（PreToolUse 的附加上下文）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub before: Vec<Message>,
    /// 排在 tool_result 之后提交的消息（PostToolUse 的附加上下文、阻塞错误、停止说明）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub after: Vec<Message>,
    /// hook 要求这批工具结束后停止本轮（`continue: false`）。
    #[serde(default)]
    pub prevent_continuation: bool,
    /// PermissionRequest hook 的拒绝要求中止整个回合（`interrupt: true`，docs/specs/rust-cli/hooks.md §6.2）：执行器中止回合，
    /// 同用户的停止（这次调用也按被中止交付）。
    #[serde(default)]
    pub interrupt: bool,
}

impl ToolExecution {
    pub fn output(output: ToolOutput) -> Self {
        Self {
            output,
            before: Vec::new(),
            after: Vec::new(),
            prevent_continuation: false,
            interrupt: false,
        }
    }
}

/// 工具执行管线（validate → PreToolUse → 权限 → call → PostToolUse），主循环逐个调用。
#[async_trait]
pub trait ToolExecutor: Send + Sync {
    /// 执行一个工具调用：结果，加上 hook 的消息与停止请求。
    async fn execute(
        &self,
        tool: Arc<dyn Tool>,
        input: Value,
        context: ToolUseContext,
        progress: mpsc::Sender<ToolProgress>,
    ) -> ToolExecution;
}

/// 通用权限判决链（deny 规则 → 工具自身检查 → 模式 → allow 规则 → 询问）。
///
/// 方法都没有缺省体（F125）：缺陷 D1 就是包在判决链外面的一层漏转发了当时的 `ask_once`，落到缺省的 `false` 也能编译。
/// 现在漏转发、漏实现都是编译错误；测试替身用 `testing::FixedGate` 或各自写全。“只批准这一次”是判决结果
/// （`PermissionDecision::Ask.once`，工作线 F137），不再是单独的方法。
#[async_trait]
pub trait PermissionGate: Send + Sync {
    async fn decide(
        &self,
        tool: &dyn Tool,
        input: &Value,
        context: &ToolUseContext,
    ) -> PermissionDecision;

    /// “总是允许”的规则已与交互的解决同一批写进 `workspace` 的本机项目规则集（docs/specs/rust-cli/shell-permissions.md
    /// §5）：判决链按 `/permissions` 的同一条路径读回库值、换上 LocalSettings 层，同一进程里的子代理与之后的会话都
    /// 看到它（共享的权限上下文）。
    async fn local_rules_committed(&self, workspace: &crate::env::WorkspaceRef);

    /// “总是允许”写进会话自己的规则集（群会话，或云端的共享规则集）之后：按会话读回（policy 的会话规则来源）；没有会话
    /// 来源时同 [`Self::local_rules_committed`]。
    async fn session_rules_committed(
        &self,
        session: &SessionId,
        workspace: &crate::env::WorkspaceRef,
    );

    /// hook 放行或要求询问时仍然生效的检查：
    /// deny / ask 规则、工具自身检查的拒绝、规则与安全路径的询问。没有命中时为 None
    /// （docs/specs/rust-cli/hooks.md §6）。
    async fn check_rules(
        &self,
        tool: &dyn Tool,
        input: &Value,
        context: &ToolUseContext,
    ) -> Option<PermissionDecision>;
}

/// 带一组只对这一次判决生效的 allow 规则的判决（skill 的 `allowed-tools`，docs/specs/rust-cli/ext.md §2.5）：deny、ask
/// 规则仍然优先。policy 实现，ext 的 skill `!` 命令用。F125（T5）：以前 ext 为这一个方法依赖具体类型 `PolicyGate`。
#[async_trait]
pub trait ScopedGate: Send + Sync {
    async fn decide_with_allowed(
        &self,
        tool: &dyn Tool,
        input: &Value,
        context: &ToolUseContext,
        allowed: &[crate::permission::PermissionRule],
    ) -> PermissionDecision;
}

/// 判决链当前的规则快照（只读）：工具清单按它去掉整个工具被 deny 的工具（docs/specs/rust-cli/tools-files.md §1.1.1）。
/// 一次清单只取一次，清单前后用同一份。policy 实现。F125（T5）：以前 tools 为此依赖具体类型 `PolicyGate`。
pub trait RuleSnapshots: Send + Sync {
    fn current_rules(&self) -> Arc<crate::permission::PermissionRules>;
}

/// 发起询问的子代理（docs/specs/rust-cli/agents.md §4.1.1）：最里层的子代理写入，外层转发时不改；主线程的询问没有它。
/// 界面据此标出询问的来源（TUI 用类型，App 的前端用全部）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskedBy {
    pub agent: AgentId,
    /// agent 类型（定义的名字，例如 `general-purpose`、`Explore`）。
    pub agent_type: String,
    /// 子代理自己的会话。
    pub session: SessionId,
    /// 启动它的会话。
    pub parent: SessionId,
    /// 启动它的 Agent 调用的 `description`。
    pub description: String,
    /// 这次运行属于的调用：派生时是 Agent 调用，SendMessage 续跑之后是那次 SendMessage 调用；SendMessage 留下的消息续跑之后没有
    /// （docs/specs/rust-cli/agents.md §6）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use: Option<ToolUseId>,
}

/// 需要用户回答的交互。会话 actor 实现：登记待回答（只在内存，工作线 F130），通知前端，回答后唤醒等待方。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InteractionRequest {
    Permission {
        tool_use: ToolUseId,
        tool: String,
        input: Value,
        message: String,
        /// 判决链建议的规则：用户选"不再询问"时随 `PermissionAnswer::Allow { remember }` 带回。
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        suggestions: Vec<crate::permission::PermissionRule>,
        /// “不再询问”一行的可编辑前缀（种子，docs/specs/rust-cli/shell-permissions.md §4.10）：Bash 且有建议规则时由执行
        /// 管线按判决链的同一套命令分析算出；前端只显示、编辑（F133，审查 U14：以前 TUI 经 runtime 调 policy 自己算）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rule_seed: Option<String>,
        /// 发起询问的子代理（[`AskedBy`]）；主线程的询问为 None。装箱：主线程的询问占多数，事件枚举不因它变大。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        asked_by: Option<Box<AskedBy>>,
        /// 调用在哪台机器上执行（R3，remote-exec.md §20.8）；主 env 不写出。界面的标记在 R4。
        #[serde(default, skip_serializing_if = "crate::ids::MachineName::is_primary")]
        machine: crate::ids::MachineName,
        /// 只批准这一次（guarded 的危险命令，判决的 `PermissionDecision::Ask.once`，shell-permissions.md §4.6.1）：没有建议规则；
        /// App 的前端据此不给「完全访问」（docs/specs/rust-cli/session-lifecycle.md §10.22）。TUI 不读它。serde 缺省 false、为 false 时不写出。
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        once: bool,
    },
    Question {
        tool_use: ToolUseId,
        questions: Value,
        /// 同 `Permission.asked_by`（子代理的提问也标来源）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        asked_by: Option<Box<AskedBy>>,
    },
    PlanApproval {
        tool_use: ToolUseId,
        plan: String,
        /// 同 `Permission.asked_by`。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        asked_by: Option<Box<AskedBy>>,
    },
    /// MCP 服务器要一份输入（`elicitation/create`，docs/specs/rust-cli/ext.md §3.13）。`tool_use` 是服务器请求到达时这条连接上
    /// 在途的调用（询问挂在它上面：停下回合时随它撤回）。装箱：表单比别的询问大。
    McpElicitation {
        tool_use: ToolUseId,
        elicitation: Box<crate::mcp_ask::Elicitation>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        asked_by: Option<Box<AskedBy>>,
    },
    /// MCP 服务器要借用模型（`sampling/createMessage`，ext.md §3.14）：先问人。`tool_use` 同 `McpElicitation`。
    McpSampling {
        tool_use: ToolUseId,
        sampling: Box<crate::mcp_ask::Sampling>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        asked_by: Option<Box<AskedBy>>,
    },
}

impl InteractionRequest {
    /// 发起询问的工具调用（每种询问都带它；同一个调用同时只有一个询问）。
    pub fn tool_use(&self) -> &ToolUseId {
        match self {
            Self::Permission { tool_use, .. }
            | Self::Question { tool_use, .. }
            | Self::PlanApproval { tool_use, .. }
            | Self::McpElicitation { tool_use, .. }
            | Self::McpSampling { tool_use, .. } => tool_use,
        }
    }

    /// 发起询问的子代理（每种询问都可能有）。
    pub fn asked_by(&self) -> Option<&AskedBy> {
        match self {
            Self::Permission { asked_by, .. }
            | Self::Question { asked_by, .. }
            | Self::PlanApproval { asked_by, .. }
            | Self::McpElicitation { asked_by, .. }
            | Self::McpSampling { asked_by, .. } => asked_by.as_deref(),
        }
    }

    /// 转发询问的一层写入来源用（agents 的 `ChildSink`）。
    pub fn asked_by_mut(&mut self) -> &mut Option<Box<AskedBy>> {
        match self {
            Self::Permission { asked_by, .. }
            | Self::Question { asked_by, .. }
            | Self::PlanApproval { asked_by, .. }
            | Self::McpElicitation { asked_by, .. }
            | Self::McpSampling { asked_by, .. } => asked_by,
        }
    }
}

pub use crate::interaction::{
    Answer, DetachedAsk, DetachedEvent, InteractionAnswer, PermissionAnswer, PlanApprovalAnswer,
    QuestionAnnotation, QuestionAnswer,
};
pub use crate::session_ops::{RunView, SessionOps};

/// 询问用户（权限、提问、计划审批）。会话的读与命令在 [`SessionOps`]（工作线 F134，审查 C3：以前 10 个方法里混着
/// 会话命令）；各方法的 `session` 参数生产实现都不用，一起删掉（询问总是交给给出入口的会话）。
///
/// 方法都没有缺省体（工作线 F137，审查 K6 的剩余）：每个实现写明撤回与会话级的入口（以前缺省“什么都不做”与 None，
/// 只为约 20 个只回答询问的测试替身存在，包装层漏转发也能编译）。测试替身用 `testing::ScriptedInteractions`、
/// `testing::NoOneToAsk` 或代理 `testing::AskOverride`。
#[async_trait]
pub trait Interactions: Send + Sync {
    async fn ask(
        &self,
        request: InteractionRequest,
    ) -> Result<InteractionAnswer, OrchestrationError>;

    /// 撤回 `tool_use` 还没有回答的询问（工具被取消，docs/specs/rust-cli/session-lifecycle.md §2）：交互的 owner
    /// 清掉它并通知前端关闭对话框，之后到达的回答被拒绝。取消是一条消息，不靠丢弃 `ask` 的 future（README §6
    /// 规则 5）。没有这个询问时什么都不做；不持有交互的实现什么都不做。
    async fn cancel_ask(&self, tool_use: &ToolUseId);

    /// 替用户回答 `tool_use` 还没回答的询问（PermissionRequest hook 先给出了结论，docs/specs/rust-cli/hooks.md §6.1）。交互的
    /// owner 按到达先后裁决，与用户的回答是同一个询问：询问还在时按 `answer` 解决（同用户的回答：通知前端关闭对话框，等着的
    /// `ask` 得到这个回答），返回 true；已经回答、撤回或没有这个询问时什么都不改，返回 false（已回答）。调用方在 `ask` 发出之后
    /// 才调它，两者走同一条通道，所以询问总是先到。不持有交互的实现返回 false。
    async fn answer_ask(&self, tool_use: &ToolUseId, answer: Answer) -> bool;

    /// 没有人能回答、用了 PermissionRequest hook 的结论时，请交互的 owner 落 hook 带的权限更新（docs/specs/rust-cli/
    /// permission-updates.md §5，不能询问时也写）：询问已经有了“没有人能回答”的结局，这里不再碰询问。去处与询问相同：
    /// 会话 actor 一批提交、成功后改内存；`-p` 里没有入口的后台子代理只提交落在本机的那部分。落不了时是错误（调用方记日志）。
    async fn apply_permission_updates(
        &self,
        updates: Vec<crate::permission_updates::PermissionUpdate>,
    ) -> Result<(), OrchestrationError>;

    /// 不随当前回合结束的询问入口（docs/specs/rust-cli/agents.md §4.1.1）：后台工作（交互界面里的后台子代理）
    /// 在父回合结束之后仍经它询问会话的用户。会话 actor 的回合通道给出会话级的入口，它的询问不随回合结束取消、
    /// 只随会话关闭取消；子代理的 sink 转给父会话。不能询问的实现给 None，调用方按“没有人能回答”处理。
    fn lasting(&self) -> Option<Arc<dyn Interactions>>;
}

#[cfg(test)]
#[path = "orchestration_tests.rs"]
mod tests;
