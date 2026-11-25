//! 工具与主循环对会话的读与命令（工作线 F134，审查 K2 / C3），经 `orchestration` 重新导出。
//!
//! 以前会话状态的读取散在两个 trait 的 8 个方法里（`RunSink` 的 `current_mode`、`current_mode_and_plan_flags`、
//! `current_workspace`、`current_goal`，`Interactions` 的 `current_selection` 等）：主循环每一步绕会话 actor 3–4 次，
//! 读到的可能是不同时刻的值；`Interactions` 的 10 个方法里混着询问与会话命令。现在询问只在 `Interactions`
//! （`ask`、`cancel_ask`、`lasting`），会话的读与命令在这里，读是一次往返的 [`RunView`]。
//!
//! 方法都没有缺省体：包在会话外面的一层（子代理的输出端）漏转发一个方法是编译错误。测试替身用
//! `testing::OpsOverride`（全委托给 `testing::NoSession`，只覆盖关心的方法）。

use std::sync::Arc;

use async_trait::async_trait;

use crate::env::{EnvPath, WorkspaceRef};
use crate::ids::{SessionId, ToolUseId};
use crate::machine::CommandEvents;
use crate::message::Message;
use crate::model::ModelSelection;
use crate::monitor::MonitorArmed;
use crate::orchestration::OrchestrationError;
use crate::permission::auto_mode::{AutoVerdict, DenialCounts, DenialLimit};
use crate::permission::{ExecutionState, PermissionMode};
use crate::plan::PlanModeFlags;
use crate::session_prompt::SessionPrompt;

/// 会话在一个时刻的状态：一次往返读出（同一时刻的值，docs/specs/rust-cli/query.md §1、context.md §13.4）。
/// 主循环每一步与每次工具调用前读一次，工具改过的模式与工作目录在同一回合里立即生效（tools-more.md §2.3、§11.4）。
#[derive(Debug, Clone, PartialEq)]
pub struct RunView {
    /// 执行状态（权限模式与 plan 开关）：判决用的模式由它算（[`RunView::mode`]）；自动语义还要看 plan 是从哪个权限模式进的
    /// （docs/specs/rust-cli/auto-classifier.md §16.6）。子代理只记判决用的模式，plan 读成“从 default 进入”。
    pub execution: ExecutionState,
    /// 计划模式的会话标志（与 `mode` 同一个快照）。
    pub plan_flags: PlanModeFlags,
    pub workspace: WorkspaceRef,
    /// 会话现在的主模型（工具与 hook 的旁路请求由它推小模型，model.md §6.1；后台子代理是它这次运行的模型，hooks.md §5.3）；
    /// 输出端不持有会话时为 None，调用方用自己的回退值。
    pub selection: Option<ModelSelection>,
    /// 会话现在的目标条件（`/goal`，goal.md §4）；子代理没有。
    pub goal: Option<String>,
    /// 会话现在的附加工作目录（docs/specs/rust-cli/session-dirs.md §3.3）：判决、环境段与 skill / agent 的发现读这一份；子代理是
    /// 派生时的副本。
    pub additional_directories: Vec<EnvPath>,
    /// 会话级的权限规则层（docs/specs/rust-cli/permission-updates.md §3，来源都是 Session）：判决链接在快照之后。前台子代理是父会话
    /// 现在的，后台子代理是派生时的副本。
    pub session_rules: Vec<crate::permission::SourcedRule>,
    /// 自动模式拦下的次数（auto-classifier.md §16.5）：owner 是会话 actor 与子代理的输出端，判决链据它决定放行时要不要清零。
    pub auto_denials: DenialCounts,
    /// 会话钉住的模型可见输入（docs/specs/rust-cli/context.md §15.3）：owner 是会话 actor（压缩之后在这里换成重钉的那一份）；
    /// 子代理是派生它的运行的（fork 的 system 与父会话逐字相同）。
    pub prompt: Arc<SessionPrompt>,
}

impl RunView {
    /// 这个执行状态与工作区的视图：计划模式的标志是缺省，没有会话的主模型与目标，计数为 0。会话 actor 与子代理用结构更新补上
    /// 自己有的字段（工作线 F137）。单值的模式按旧写法读（`Plan` 是 plan 开、default）。
    pub fn new(execution: impl Into<ExecutionState>, workspace: WorkspaceRef) -> Self {
        Self {
            execution: execution.into(),
            plan_flags: PlanModeFlags::default(),
            workspace,
            selection: None,
            goal: None,
            additional_directories: Vec::new(),
            session_rules: Vec::new(),
            auto_denials: DenialCounts::default(),
            prompt: Arc::default(),
        }
    }

    /// 判决用的权限模式（plan 开时是 plan）。
    pub fn mode(&self) -> PermissionMode {
        self.execution.effective()
    }
}

/// 会话此刻判决用的权限模式的长期入口（docs/specs/rust-cli/agents.md §4.1.3）：回合结束之后照样能问；会话已经不在时为
/// None。子代理派生时拿走它，每次取视图都现读父会话的模式，不在派生时
/// 定下。
#[async_trait]
pub trait LiveMode: Send + Sync {
    async fn mode(&self) -> Option<PermissionMode>;
}

/// 会话的读与命令。会话 actor 实现（经回合的提交通道，状态的唯一 owner）；子代理的输出端（agents）按前台、后台
/// 转给父会话或自己处理；不持有会话的输出端（压缩的摘要请求）一律没有。
#[async_trait]
pub trait SessionOps: Send + Sync {
    /// 会话现在的状态；输出端不持有会话的状态时为 None，主循环用请求里的值。
    async fn view(&self) -> Option<RunView>;

    /// 接收交互与排队输入的顶层会话（tools-more.md §10.2）：前台子代理转给父会话，其余是 `session` 自己。定时任务与
    /// worktree 的记录按它。
    async fn owner_session(&self, session: &SessionId) -> SessionId;

    /// 会话已提交的消息（这次回复之前的历史）：fork 子代理继承它（修复线 L-A，agents.md §8.1.2）。不持有会话对话
    /// 的输出端（子代理）为 None。
    async fn conversation(&self) -> Option<Vec<Message>>;

    /// 工具请求打开或关掉 plan 开关（EnterPlanMode、ExitPlanMode，修复线 F20），权限模式不变。owner 是会话 actor：
    /// 先提交事实再通知前端，返回后新状态已生效（tools-more.md §2.3）。`by` 是请求它的工具调用：前端据它知道这次切换来自工具
    /// （session-lifecycle.md §10.25）。不持有会话状态的输出端返回 `Unsupported`。
    async fn set_plan(&self, enabled: bool, by: &ToolUseId) -> Result<(), OrchestrationError>;

    /// 改会话的工作目录（EnterWorktree / ExitWorktree）；owner 是会话 actor，它同时设执行环境的 shell cwd
    /// （tools-more.md §11.4）。前台子代理改的是父会话的；不持有会话的返回 `Unsupported`。
    async fn set_workspace(&self, root: EnvPath) -> Result<(), OrchestrationError>;

    /// Monitor 工具起好的 monitor 交给它的 owner（工作线 F83，tools-more.md §19.6）：主线程是会话 actor，子代理里是
    /// agents。`events` 是 `start_command` 返回、工具读过 `Started` 的那条流。不持有的返回 `Unsupported`，调用方停掉
    /// 已经起的命令。
    async fn arm_monitor(
        &self,
        monitor: MonitorArmed,
        events: CommandEvents,
    ) -> Result<(), OrchestrationError>;

    /// 一次 hook 事件里真的跑了的 hook（hooks.md §14.3）：会话 actor 挂在在跑的回合上、发 `HooksRan`；子代理与不持有会话的输出端
    /// 丢掉（子会话的 hook 行不落库，没有投影读它）。
    async fn hooks_ran(&self, invocation: crate::hook_runs::HookInvocation);

    /// Agent 工具派生了一个子代理（session-lifecycle.md §10.56）：会话 actor 记进子代理表、发 `SubagentRecorded`，记下之后才返回（子代理在
    /// 那之后才跑，它的终态通知不会先于开始到 actor）。子代理的输出端与不持有会话的输出端丢掉（嵌套的子代理不进父会话）。
    async fn subagent_started(&self, start: crate::subagent_runs::SubagentStart);

    /// 记一次自动模式的结论（auto-classifier.md §16.5）：owner（主线程是会话 actor，子代理是它这次运行的输出端）按
    /// `DenialCounts::record` 改计数，回到了的上限。不持有会话状态的输出端不计、回 None。
    async fn note_auto_verdict(&self, verdict: AutoVerdict) -> Option<DenialLimit>;

    /// 会话此刻权限模式的长期入口（[`LiveMode`]，agents.md §4.1.3）：Agent 派生子代理时取走。会话 actor 经它的请求通道回答；
    /// 子代理在父会话的入口上套自己声明的模式；不持有会话状态的输出端为 None。
    fn live_mode(&self) -> Option<std::sync::Arc<dyn LiveMode>>;
}
