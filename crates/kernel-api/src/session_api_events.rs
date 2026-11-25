//! 内核事件与回合结局（docs/specs/rust-cli/README.md §7）；从 `session_api.rs` 拆出（800 行上限，工作线 F131），
//! 经 `session_api` 重新导出，路径不变。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::Delivery;
use crate::env::WorkspaceRef;
use crate::ids::{CommandId, InteractionId, MessageId, ToolUseId, TurnId};
use crate::message::{CompactTrigger, ContentBlock, Message, Usage};
use crate::model::{BlockDelta, ModelSelection};
use crate::orchestration::InteractionRequest;
use crate::permission::PermissionMode;
use crate::store::SessionMeta;
use crate::tool::{ToolOutput, ToolProgress};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum TurnOutcome {
    Success,
    Interrupted,
    /// 工具的 hook 要求停止（[`crate::orchestration::QueryStop::HookStopped`]）：不是失败，前端按成功结束显示；`-p` 的
    /// `terminal_reason` 是 `hook_stopped`（docs/specs/rust-cli/headless.md §3.6）。
    HookStopped,
    /// Stop hook 阻止继续（[`crate::orchestration::QueryStop::StopHookPrevented`]）：同 `HookStopped`，不是失败；`-p` 的
    /// `terminal_reason` 是 `stop_hook_prevented`（headless.md §3.6，工作线 F134）。
    StopHookPrevented,
    /// UserPromptSubmit 拦下了开回合的输入（[`crate::orchestration::QueryStop::Blocked`]，docs/specs/rust-cli/session-lifecycle.md
    /// §10.51）：输入没有落库、不调模型，`message` 是 hook 给的那句警告。不是失败（不扣住排队的输入）；TUI 与 `-p` 照回合失败
    /// 显示它，App 照成功结束的一轮显示。
    PromptBlocked {
        message: String,
    },
    /// `cause` 是让回合失败的模型错误（docs/specs/rust-cli/session-lifecycle.md §10.9）：前端按它的类别分支，不解析文字；
    /// 不是模型错误的失败为 None。serde 缺省 None。
    Failed {
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cause: Option<crate::model::ModelError>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChange {
    pub path: String,
    pub additions: u64,
    pub deletions: u64,
    /// 文件所在的机器（远端执行 F98，docs/specs/rust-cli/remote-exec.md §25.4.5：`/rewind` 把额外机器的写成
    /// `dev-a:README.md`）；主 env 不写出。
    #[serde(default, skip_serializing_if = "crate::ids::MachineName::is_primary")]
    pub machine: crate::ids::MachineName,
}

/// 一个回合里一个文件的改动（docs/specs/rust-cli/session-lifecycle.md §10.42）：这个回合里对它最早一次写入之前 → 最后一次写入之后的增删行数，
/// 写了几次、哪些工具写的（排序、去重）。回合记录（[`crate::store::TurnEnded::files`]）与 [`KernelEvent::TurnFinished`] 带同一份。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnFileChange {
    #[serde(flatten)]
    pub change: FileChange,
    pub writes: u64,
    pub tools: Vec<String>,
}

/// 回合里一个文件的 diff（docs/specs/rust-cli/session-lifecycle.md §10.45，v5 的 `session.fileChanges` 的明细）：`file` 是回合记录里的那一项；
/// `hunks` 是用同一组 checkpoint 的 artifact、同一个 diff 算的 jsdiff `structuredPatch` 的 hunk（上下文 4 行）。编辑距离超过上限、
/// artifact 读不出、checkpoint 已被代码回退撤销时为空。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnFileDiff {
    pub file: TurnFileChange,
    pub hunks: Vec<rust_cli_base::line_diff::Hunk>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueuedInput {
    pub command: CommandId,
    pub text: String,
    pub delivery: Delivery,
    /// 接受时的正文之外的事实（docs/specs/rust-cli/session-lifecycle.md §10.14）：开回合时用它，不再问前端。
    #[serde(
        default,
        skip_serializing_if = "crate::input_extras::InputExtras::is_empty"
    )]
    pub extras: crate::input_extras::InputExtras,
    /// 排队的是一次压缩，不是输入（docs/specs/rust-cli/session-lifecycle.md §10.23，`Command::CompactOrQueue`）：`text` 为空。serde 缺省 false。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub compact: bool,
    /// 前端的编辑器锁住了这一项（session-lifecycle.md §10.59，App 的 Composer 编辑态）：锁着的项不出队、不被取回、挪动或提升。锁是项的字段，
    /// 与项同生同灭。serde 缺省 None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edit_lock: Option<QueueEditLock>,
}

impl QueuedInput {
    /// 一条排队的输入：正文之外没有事实、不是压缩、没有锁。别的字段用结构体更新语法给。
    pub fn new(command: CommandId, text: impl Into<String>, delivery: super::Delivery) -> Self {
        Self {
            command,
            text: text.into(),
            delivery,
            extras: crate::input_extras::InputExtras::default(),
            compact: false,
            edit_lock: None,
        }
    }
}

/// 排队项的编辑锁（session-lifecycle.md §10.59）：`editor` 是持锁的编辑器（前端给的不透明 id，内核只比相等）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueEditLock {
    pub editor: String,
}

/// 回合的来源（§10.8）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnOrigin {
    /// 用户的输入（文字、带内容块的、bash 模式、SessionStart 给的首条输入）。
    Input,
    /// 后台命令、子代理、Monitor 的结果。
    Notice,
    /// 跨会话的消息（cross-session.md）。
    Peer,
    /// 定时任务（tools-more.md §10.4）。
    Scheduled,
    /// `/compact`。
    Compaction,
}

/// 一个回合的开始：[`KernelEvent::TurnStarted`] 的五个事实（session-lifecycle.md §10.33、§10.50）。视图里在跑的回合带它（[`Running::Turn`]），
/// 回合记录（[`crate::store::TurnRecord`]）里也是同样五个值。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnStart {
    pub turn: TurnId,
    /// 开这个回合的命令：人类输入、bash 模式与压缩的回合有，通知、定时任务、同伴消息开的回合没有。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<CommandId>,
    pub origin: TurnOrigin,
    /// 开回合时 actor 的时钟（Unix 毫秒）。
    pub started_at: u64,
    /// 后台结果开的回合：开它的那几项后台工作（按通知到达的先后，§10.50）；别的回合为空。serde 缺省为空。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub works: Vec<crate::background::NoticedWork>,
}

/// 一个回合结束的时刻与用时（session-lifecycle.md §10.36）：会话 actor 在回合结束时算一次，[`KernelEvent::TurnFinished`] 与回合记录
/// （[`crate::store::TurnEnded::time`]）带同一份，所以活会话与冷读是同一个值。墙上时间是 `ended_at` 减回合的 `started_at`，由读的一方算。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnTime {
    /// 回合结束时 actor 的时钟（Unix 毫秒）。
    pub ended_at: u64,
    /// 回合开始到结束，扣掉有待回答的交互的时间（毫秒；同回合结束那一行的用时，docs/specs/rust-cli/tui-display.md §2.7）。
    pub active_ms: u64,
}

/// 视图取的时刻在跑的是什么（[`super::SessionView::running`]，session-lifecycle.md §10.35）：在不在跑与跑的是什么是同一个事实，只留这一份。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Running {
    /// 会话 actor 在跑的回合与它的开始（与 `TurnStarted` 同值）。
    Turn(TurnStart),
    /// 子代理的一次运行（agents 的只读快照）：子循环不开内核的回合，没有回合的开始。
    Subagent,
}

impl Running {
    /// 会话的回合的开始；子代理的运行没有。
    pub fn turn(&self) -> Option<&TurnStart> {
        match self {
            Self::Turn(start) => Some(start),
            Self::Subagent => None,
        }
    }
}

/// 一条输入的结局（§10.7）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "fate", rename_all = "snake_case")]
pub enum InputFate {
    /// 排进了队列（回合在跑，或队列扣住）。
    Queued,
    /// 进了一个回合。
    Started { turn: TurnId, joined: Joined },
    /// 它所在的回合结束了。
    Ended { turn: TurnId, end: InputEnd },
    /// 没有进对话。
    Dropped { why: DropReason },
}

/// 输入怎么进的回合（§10.7）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Joined {
    /// 开了这个回合。
    Opened,
    /// 与开回合的输入成批进来（query.md §4.3）。
    Batched,
    /// 在工具边界并入（插话）。
    Absorbed,
}

/// 输入所在的回合怎么结束（§10.7）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputEnd {
    /// 成功（含 hook 要求停下）。
    Completed,
    /// 被中断。
    Cancelled,
    Failed,
}

/// 输入为什么没有进对话（§10.7）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DropReason {
    /// 用户取回（`Dequeue`）。
    TakenBack,
    /// UserPromptSubmit 拦下了它。
    Blocked,
}

/// 排队的输入为什么扣住（§10.6）：扣住时回合结束不开下一条，输入命令要先等用户决定。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueueHold {
    /// 回合被停止。
    Stopped,
    /// 回合以失败结束。
    TurnFailed,
    /// 用户要求扣住（`SetAutoDrain{enabled: false}`）。
    Requested,
}

/// 工具切的 plan（docs/specs/rust-cli/session-lifecycle.md §10.25）：哪个调用、切成了什么。只在会话 actor 的内存。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanSwitch {
    pub tool_use: ToolUseId,
    pub enabled: bool,
}

/// 只供显示的系统提示的级别（informational system 消息的 `level`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeLevel {
    Info,
    Warning,
}

/// 只供显示的系统提示的种类与参数（docs/specs/rust-cli/query.md §3 的 Stop hooks）：要按语言显示整句的前端据它取文案，`text` 仍是英文原句（TUI 显示它）。
/// 封闭的枚举，参数是类型：没有种类的提示（插话被 UserPromptSubmit 拦下，正文是 hook 的说明）只有原文。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeKind {
    /// Stop hook 连续阻塞到了上限：`blocked` 是这一次是连续第几次阻塞（原句里的次数）。
    StopHookLimit { blocked: u32 },
}

/// 内核事件。新增变体时同步更新前端与 spec §7。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum KernelEvent {
    SessionCreated {
        meta: SessionMeta,
    },
    TitleChanged {
        title: String,
    },
    ModelChanged {
        selection: ModelSelection,
    },
    /// 权限模式变了（修复线 F20 起只带权限模式，不会是 `Plan`；plan 开关见 [`KernelEvent::PlanChanged`]）。
    ModeChanged {
        mode: PermissionMode,
    },
    /// plan 开关变了（修复线 F20）。`by` 是切它的工具调用（EnterPlanMode、ExitPlanMode，docs/specs/rust-cli/session-lifecycle.md §10.25）；
    /// 用户切的、开回合时套用输入的配置为 None。serde 缺省 None。
    PlanChanged {
        enabled: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        by: Option<ToolUseId>,
    },
    /// 权限询问被回答为「放行并切模式」（`Command::ResolveSwitchingMode`，session-lifecycle.md §10.25、§10.59）：`tool_use` 是那个询问的调用。
    /// 会话已经是目标模式时也发（这次回答本身就是授权）。
    ModeGranted {
        tool_use: ToolUseId,
    },
    TurnStarted {
        turn: TurnId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        command: Option<CommandId>,
        /// 回合的来源（docs/specs/rust-cli/session-lifecycle.md §10.8）。
        origin: TurnOrigin,
        /// 开回合时 actor 的时钟（Unix 毫秒），与回合记录里的同值。
        started_at: u64,
        /// 后台结果开的回合：哪几项后台工作的终态通知开的它（docs/specs/rust-cli/session-lifecycle.md §10.50），与回合记录里的同值。serde 缺省为空。
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        works: Vec<crate::background::NoticedWork>,
    },
    TurnFinished {
        turn: TurnId,
        outcome: TurnOutcome,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<Usage>,
        /// 回合结束的时刻与用时，与回合记录里的同值（docs/specs/rust-cli/session-lifecycle.md §10.36）。
        time: TurnTime,
        /// 这个回合的文件改动，与回合记录里的同值（session-lifecycle.md §10.42）。serde 缺省为空。
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        files: Vec<TurnFileChange>,
    },
    /// 一个块开始了（[`crate::orchestration::QueryEvent::BlockStart`]，工作线 F147）：tool_use 的 ID 与名字早于它的输入增量
    /// 到达。只供显示，不落库；`-p` 的 `stream_event` 用它（docs/specs/rust-cli/headless.md §3.7），别的前端不用。
    /// `response` 同 [`Self::StreamDelta`]。
    StreamBlockStart {
        turn: TurnId,
        response: u32,
        index: usize,
        block: ContentBlock,
    },
    /// 正文、thinking、工具输入的流式增量；`response` 是本回合内第几次模型响应。`message` 是正在生成的 assistant 消息的
    /// id（落库的就是它），`tool_use` 是工具输入的增量属于哪个调用（别的增量为 None）：前端按它们把增量对到最后落成的
    /// 行（docs/specs/rust-cli/session-lifecycle.md §10.13，K-I3；`index` 会漂，不能当身份）。
    StreamDelta {
        turn: TurnId,
        response: u32,
        message: MessageId,
        index: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_use: Option<ToolUseId>,
        delta: BlockDelta,
    },
    /// 一条消息已完整并已落库。
    MessageDone {
        message: Message,
    },
    /// 一条输入的显示事实已随它的用户消息落库（docs/specs/rust-cli/session-lifecycle.md §10.14）：同一次提交之后、在那条消息的
    /// [`KernelEvent::MessageDone`] 之前发。
    InputRecorded {
        record: crate::input_extras::InputRecord,
    },
    /// 一条输入的消息已落库（docs/specs/rust-cli/session-lifecycle.md §10.31）：同一次提交之后、在那条消息的 [`KernelEvent::MessageDone`]
    /// 之前发。前端据它认出这条消息是哪条命令的。
    InputCommitted {
        source: crate::store::InputSource,
    },
    ToolStarted {
        tool_use: ToolUseId,
        name: String,
        input: Value,
        /// 执行器开工的时刻（Unix 毫秒）：与落库的 tool part 的 `time.start` 同值（docs/specs/rust-cli/session-lifecycle.md §10.37）。
        started_at: u64,
    },
    ToolProgress {
        tool_use: ToolUseId,
        progress: ToolProgress,
    },
    ToolDone {
        tool_use: ToolUseId,
        output: ToolOutput,
        /// 因停止而结束（[`crate::orchestration::QueryEvent::ToolFinished::cancelled`]，session-lifecycle.md §10.17）：前端显示「已取消」，
        /// 不是「出错」。serde 缺省 false。
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        cancelled: bool,
    },
    /// 一次调用的开工与结束已随它的结果落库（docs/specs/rust-cli/session-lifecycle.md §10.37）：同一次提交之后、在结果消息的
    /// [`KernelEvent::MessageDone`] 之前发；与 tool part 的 `state.time`、视图的 `tool_times` 同值。时刻不放在 `ToolDone` 上：它早于结果的
    /// 提交，没开工就有结果的调用也没有它。
    ToolTimeRecorded {
        time: crate::metering::ToolTime,
    },
    FileChanges {
        turn: TurnId,
        files: Vec<FileChange>,
        rewindable: bool,
    },
    /// 代码回退撤销了这些回合的文件改动（session-lifecycle.md §10.54）：`prompts` 是回合的开回合输入；与 `CheckpointsRestored` 同一次提交之后发，
    /// 视图的 `reverted` 已经带着它们。不加 revision 以外的意义：前端据此把回合的文件改动标成已撤销。
    FilesRestored {
        prompts: Vec<MessageId>,
    },
    InteractionAsked {
        interaction: InteractionId,
        request: InteractionRequest,
    },
    InteractionResolved {
        interaction: InteractionId,
    },
    ContextUsage {
        used_tokens: u64,
        max_tokens: u64,
        /// 生效的自动压缩阈值（[`crate::compaction::ContextOccupancy::compact_at`]，compact.md §2.3）。serde 缺省 None。
        #[serde(default)]
        compact_at: Option<u64>,
        /// 模型的标称窗口（[`crate::compaction::ContextOccupancy::window`]，compact.md §2.1）。serde 缺省 None、为 None 时不写出（旧的形状不变）。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        window: Option<u64>,
    },
    /// 上下文快满的提示（docs/specs/rust-cli/compact.md §2.1）：提交带用量的 assistant 与压缩结果之后重算。
    ContextNotice {
        notice: crate::compaction::ContextNotice,
    },
    /// 完成的主回合请求的输出速度（footer 的 TPS，tui-display.md §2.6，F105）：计量提交之后发，不落库。
    OutputSpeed {
        speed: crate::metering::OutputSpeed,
    },
    /// 会话 actor 的一个模型请求的计量已落库（docs/specs/rust-cli/session-lifecycle.md §10.44）：与 `OutputSpeed` 同一处、先落库再发；
    /// 带完整的计量（含只在内存的首个内容时刻）。只供显示（v5 的开发者面板），不落库、不加 revision。
    ModelCalled {
        call: crate::metering::ModelCall,
    },
    TasksChanged {
        tasks: Value,
    },
    QueueChanged {
        items: Vec<QueuedInput>,
    },
    /// 一条输入的结局变了（docs/specs/rust-cli/session-lifecycle.md §10.7）：`input` 是提交它的命令 id；`at` 是会话 actor
    /// 发出它时的时钟（Unix 毫秒，§10.13：02 §1.5 的 `InputFate.at` 由内核给，前端不另读时钟）。
    InputFate {
        input: CommandId,
        fate: InputFate,
        at: u64,
    },
    /// 排队的输入扣住或放开了（docs/specs/rust-cli/session-lifecycle.md §10.6）：`hold` 为 None 是放开。
    QueueHeld {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hold: Option<QueueHold>,
    },
    /// 会话的跟随方式变了（docs/specs/rust-cli/session-lifecycle.md §10.27，`Command::SetFollowUp`）。
    FollowUpChanged {
        delivery: crate::session_api::Delivery,
    },
    /// 后台工作（命令、Monitor、后台子代理）的终态通知进了会话（工作线 F134：`status` 以前是字符串，tui 另写一份解析）。
    BackgroundWork {
        work: String,
        status: crate::env_shell::BackgroundStatus,
    },
    /// 会话里开始了一项后台工作（K-W1，docs/specs/rust-cli/session-lifecycle.md §10.28）：进了会话 actor 的后台工作表。
    BackgroundStarted {
        work: crate::background::BackgroundStart,
    },
    /// 一项在跑的后台工作结束了（K-W1，§10.28）：离开了后台工作表。每项一次；表里没有的不发。
    BackgroundEnded {
        work: String,
    },
    CompactStarted {
        trigger: CompactTrigger,
    },
    CompactDone {
        boundary: MessageId,
    },
    /// 开始了的压缩失败了（docs/specs/rust-cli/compact.md §4.1）：`message` 是给用户的原因。自动压缩失败时回合照常继续。
    CompactFailed {
        trigger: CompactTrigger,
        message: String,
    },
    /// 在跑回合的回合记录里，主循环的一次压缩开始了或有了结局（docs/specs/rust-cli/session-lifecycle.md §10.52）：`index` 是它在
    /// `TurnRecord.compactions` 里的位置，`compaction` 是记录里的新值。会话 actor 先改记录、再发它。
    CompactionRecorded {
        turn: TurnId,
        index: usize,
        compaction: crate::compaction::LoopCompaction,
    },
    /// 历史被改写（rewind、编辑重发）；前端丢弃旧视图后重新取快照。
    HistoryRewritten {
        generation: u64,
    },
    /// 定时任务触发的界面行（tools-more.md §18.3，L22）：不进模型上下文、不落库，
    /// 紧接着是这次触发开的回合。`at` 是本地时间（`Oct 18 3:04pm`）；`fold` 有值时前端收起上一个 loop tick。
    ScheduledFire {
        at: String,
        kind: crate::cron::CronFireKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fold: Option<crate::cron::LoopFold>,
    },
    /// 只供显示的系统提示，不发给模型、不落库（L18：插话被 UserPromptSubmit 阻塞，
    /// docs/specs/rust-cli/query.md §4.2；Stop hook 到上限，§3）。`kind` 是种类与参数（serde 缺省 None）。
    SystemNotice {
        level: NoticeLevel,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<NoticeKind>,
    },
    /// 会话的工作目录换了（EnterWorktree / ExitWorktree / `/cd`，F1）：前端的工作区只从这里与快照的
    /// `meta.workspace` 取（tools-more.md §11.4、tui-manage.md §7.1）。
    WorkspaceChanged {
        workspace: WorkspaceRef,
    },
    /// 会话多了一个附加工作目录（`/add-dir`，docs/specs/rust-cli/session-dirs.md §3.2）：前端的视图只从这里与快照的
    /// [`SessionView::additional_directories`] 取。排在回执之前。
    DirectoryAdded {
        directory: crate::env::EnvPath,
    },
    /// 会话少了一个附加工作目录（hook 的 `removeDirectories`，docs/specs/rust-cli/permission-updates.md §2.3）：同 `DirectoryAdded`，
    /// 前端从视图里去掉它。
    DirectoryRemoved {
        directory: crate::env::EnvPath,
    },
    /// 扣住的同伴消息变了（cross-session.md §2.4，F28）：整份快照，界面的批准框只从它与
    /// [`SessionView::peer_held`] 取。
    PeerHeldChanged {
        held: Vec<crate::peers::HeldPeerMessage>,
    },
    /// 回合结束那一行的数据（tui-display.md §2.7，F118）：`TurnFinished` 之后、下一个回合之前发；只供显示，不落库。
    TurnDuration(crate::turn_duration::TurnDuration),
    /// 这一轮的执行覆盖退回了（docs/specs/rust-cli/session-lifecycle.md §10.48）：与回合记录里的 `fell_back` 同值；会话 actor 先改记录再发它。
    ExecutionFellBack {
        turn: TurnId,
        fell_back: crate::turn_execution::FellBack,
    },
    /// 一次 hook 事件里跑了的 hook 挂在了回合上（session-lifecycle.md §10.51）：只在内存，视图的 `hooks` 是它们。
    HooksRan {
        hooks: crate::hook_runs::TurnHooks,
    },
    /// 会话 actor 的子代理表里这一项变了（开始、转后台、再跑、结束，session-lifecycle.md §10.56）：`run` 是表里的新值；只在内存，视图的
    /// `subagents` 是表里的全部。
    SubagentRecorded {
        run: crate::subagent_runs::SubagentRun,
    },
    /// 主线程模型请求的通知（重试、停顿、恢复）：不落库，前端用它显示重试与停顿
    /// （docs/specs/rust-cli/stall-visibility.md §3）。
    ModelNotice {
        notice: crate::model::ModelNotice,
        /// 会话 actor 发出它时的时钟（Unix 毫秒，§10.9）：重试的下一次请求是 `at + delay_ms`。
        at: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SequencedEvent {
    pub seq: u64,
    pub event: KernelEvent,
    /// 会话的状态 revision（缺口 G1，docs/specs/rust-cli/app-protocol.md §6.4）：会话 actor 在可见状态变化时加一
    /// （回合开始与结束、消息落定、工具开始与结束、询问、队列、模式、模型、plan、标题、历史改写、压缩、工作区、子代理与
    /// 后台任务）；流式增量、进度、用量与只供显示的提示不加。新会话从 0 开始，只在 actor 的寿命内单调（进程重启后从 0
    /// 重来）。这条事件之后的值；serde 缺省 0。
    #[serde(default)]
    pub revision: u64,
}
