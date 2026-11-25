//! 会话 actor 的循环：命令、主循环的提交请求、回合收尾都在这一个任务里按序处理（会话状态的
//! 唯一 owner）。回合内的逻辑见 [`crate::turn`]。

use std::collections::VecDeque;
use std::sync::Arc;

use rust_cli_base::Task;
use rust_cli_kernel_api::abort::{Abort, AbortReason};
use rust_cli_kernel_api::admission::{Activity, Admission, AdmissionView, Refusal, admit};
use rust_cli_kernel_api::agents::AgentSignal;
use rust_cli_kernel_api::ids::{CommandId, InteractionId, MessageId, RunId, ToolUseId, TurnId};
use rust_cli_kernel_api::message::Message;
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::orchestration::{
    InteractionAnswer, InteractionRequest, OrchestrationError,
};
use rust_cli_kernel_api::run_ledger::RunLedger;
use rust_cli_kernel_api::session_api::{
    Ack, Command, CommandEnvelope, CommandResult, Delivery, InputFate, KernelEvent, SessionView,
};
use rust_cli_kernel_api::store::{SessionMeta, StoreError, WorkspaceCheckpoint};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::SessionDeps;
use crate::agent_signals::AgentStream;
use crate::cron::CronStream;
use crate::log::EventLog;
use crate::notices::{Notices, next_notice};
use crate::sink::SinkRequest;
use crate::title::{TitleState, title_done};
use crate::turn::TurnInput;

pub(crate) enum Request {
    Command {
        envelope: CommandEnvelope,
        reply: oneshot::Sender<Ack>,
    },
    View {
        reply: oneshot::Sender<SessionView>,
    },
    /// 此刻判决用的权限模式（子代理跟随父会话，agents.md §4.1.3）：比 `View` 轻，不复制历史。
    Mode {
        reply: oneshot::Sender<rust_cli_kernel_api::permission::PermissionMode>,
    },
    Meta {
        reply: oneshot::Sender<SessionMeta>,
    },
    /// 一项后台工作的种类与状态（后台工作表，§10.44）与会话的工作区：输出详情在调用方的任务里读文件。
    Work {
        work: String,
        reply: oneshot::Sender<(
            Option<(
                rust_cli_kernel_api::manage::BackgroundKind,
                rust_cli_kernel_api::env_shell::BackgroundStatus,
            )>,
            rust_cli_kernel_api::env::WorkspaceRef,
        )>,
    },
    RewindOptions {
        reply: oneshot::Sender<crate::rewind::Snapshot>,
    },
    /// 回退文件的预览（§10.53）：target 起的 checkpoint 与工作区；target 不是可选的输入时为 None。
    RewindPreview {
        target: rust_cli_kernel_api::ids::MessageId,
        reply: oneshot::Sender<Option<crate::rewind_preview::Undo>>,
    },
    /// 一个回合的文件 diff 的快照（`SessionHandle::turn_file_diffs`，session-lifecycle.md §10.45）。
    TurnFiles {
        turn: rust_cli_kernel_api::ids::TurnId,
        reply: oneshot::Sender<crate::turn_files::TurnFiles>,
    },
    Goal {
        reply: oneshot::Sender<rust_cli_kernel_api::goal::GoalView>,
    },
    /// 关闭会话（`SessionHandle::close`）：中止前台工作，等它收尾后回复并退出（spec §3.3）。`reason` 是中止在跑的
    /// 回合用的原因：退出是 `Shutdown`，`/clear`、`/resume` 是 `User`（修复线 F38）。
    Close {
        reply: oneshot::Sender<()>,
        reason: AbortReason,
    },
    /// 空闲时才关（`SessionHandle::close_idle`，spec §3.3.2）：空闲就进入关闭（`closed` 在退出时回复，同 `Close`）并回 meta；
    /// 不空闲回 None，什么都不改。判与进入关闭在这一步里，之间不开新工作。
    CloseIdle {
        reply: oneshot::Sender<Option<SessionMeta>>,
        closed: oneshot::Sender<()>,
    },
    /// 会话级的询问（`Interactions::lasting`，agents.md §4.1.1）：不属于哪个回合，回合结束与中断都不取消它。
    Ask {
        request: InteractionRequest,
        reply: oneshot::Sender<Result<InteractionAnswer, OrchestrationError>>,
    },
    /// 撤回会话级的询问（工具被取消；与 `Ask` 走同一个通道，所以总排在它的 `Ask` 之后）。
    CancelAsk {
        tool_use: ToolUseId,
    },
    /// 替用户回答会话级的询问（PermissionRequest hook 先有结论，hooks.md §6.1；与 `Ask` 走同一个通道，总排在它之后）。
    /// 回复 true：这个回答先到、询问已解决；false：已回答。
    AnswerAsk {
        tool_use: ToolUseId,
        answer: rust_cli_kernel_api::orchestration::Answer,
        reply: oneshot::Sender<bool>,
    },
    /// 后台子代理的询问没有人能回答、用了 hook 的结论：落它的权限更新（permission-updates.md §5）。
    ApplyUpdates {
        updates: Vec<rust_cli_kernel_api::permission_updates::PermissionUpdate>,
        reply: oneshot::Sender<Result<(), rust_cli_kernel_api::store::StoreError>>,
    },
}

/// 待回答的交互属于谁：回合里的询问随回合结束取消；会话级的询问只随会话关闭取消。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AskScope {
    Turn,
    Session,
}

/// 不属于回合的后台工作（SessionStart、代码回退）的结果，回到 actor 后才提交（spec §2）。
pub(crate) enum JobDone {
    Startup(Box<rust_cli_kernel_api::hooks::HookOutcome>),
    Rewind(Box<crate::rewind::Finished>),
}

pub(crate) struct Job {
    pub(crate) task: Task<JobDone>,
    /// SessionStart 可以被中断；回退事务不能中途放弃，不取消。
    pub(crate) cancel: Option<CancellationToken>,
}

/// 等待用户回答的交互（只在内存：进程退出后无法再回答，不落库，D4）。
pub(crate) struct Pending {
    pub(crate) id: InteractionId,
    pub(crate) request: InteractionRequest,
    pub(crate) reply: oneshot::Sender<Result<InteractionAnswer, OrchestrationError>>,
    /// 权限询问的 Notification 计时；交互被回答或取消（丢弃）时取消。
    pub(crate) _notify: Option<tokio_util::sync::DropGuard>,
    pub(crate) scope: AskScope,
}

pub(crate) struct Running {
    pub(crate) turn: TurnId,
    /// 这次运行（主循环的回合）；压缩没有（它的子循环自己取）。会话标题的请求用它（工作线 F64）。
    pub(crate) run: Option<RunId>,
    /// 这是压缩的回合（session-lifecycle.md §10.23：一次只有一个压缩在跑或在等）。
    pub(crate) compaction: bool,
    pub(crate) abort: Abort,
    pub(crate) requests: mpsc::Receiver<SinkRequest>,
    pub(crate) task: Task<crate::launch::TurnRun>,
    /// 提交失败的原因；有值时本回合以失败收尾，之后的提交一律拒绝。
    pub(crate) failure: Option<StoreError>,
    /// 开启本回合的 user 消息：checkpoint 的 `messageId`（spec §4.2）。
    pub(crate) prompt: Option<MessageId>,
    /// 这个回合里的输入与它们的消息（效果记录，session-lifecycle.md §10.3）：开回合的输入、成批的后续输入、工具边界并入的
    /// 插话。提交这条消息的那一批带它的命令 id（Node `anchor.sourceCommandId`）。
    pub(crate) inputs: Vec<(MessageId, CommandId)>,
    /// 这个回合里输入的显示事实（§10.14、§10.59）：开回合的这一条与成批的后续输入的，各与它的消息同一批提交，提交时取走。
    pub(crate) records: Vec<rust_cli_kernel_api::input_extras::InputRecord>,
    /// 这个回合里因停止而取消的工具调用（§10.17）：结果落库之后记下，回合结束时写进回合记录。
    pub(crate) cancelled: Vec<rust_cli_kernel_api::ids::ToolUseId>,
    /// 回合结束那一行的计时（修复线 F118，duration.rs）。
    pub(crate) timing: crate::duration::TurnTiming,
}

enum Next {
    Request(Request),
    Sink(Option<SinkRequest>),
    Notice(rust_cli_kernel_api::machine_notices::MachineNotice),
    Agent(AgentSignal),
    Cron,
    Job(Result<JobDone, rust_cli_base::TaskError>),
    Title(crate::title::TitleDone),
    Name(Result<Option<String>, rust_cli_base::TaskError>),
    /// 跨会话的入站（消息或回执，cross-session.md §2.4）。
    Peer(rust_cli_kernel_api::peers::PeerSignal),
    /// 扣住的消息里最早的到期时刻到了（醒来后按时钟判断）。
    PeerDeadline,
    /// Monitor 的输出事件与截止时刻（工作线 F83，monitors.rs）。
    Monitor(
        rust_cli_kernel_api::ids::ShellTaskId,
        Result<rust_cli_kernel_api::env::CommandEvent, rust_cli_kernel_api::env::EnvError>,
    ),
    MonitorDue,
}

/// 正在运行的回合的提交请求；没有回合时一直等。
async fn sink_request(running: &mut Option<Running>) -> Option<SinkRequest> {
    match running {
        Some(running) => running.requests.recv().await,
        None => std::future::pending().await,
    }
}

/// 后台工作的结果；没有时一直等。
async fn job_done(job: &mut Option<Job>) -> Result<JobDone, rust_cli_base::TaskError> {
    match job {
        Some(job) => (&mut job.task).await,
        None => std::future::pending().await,
    }
}

pub(crate) struct Actor {
    pub(crate) resume_queue_paused: bool,
    deferred_start: Option<rust_cli_kernel_api::session_api::StartSource>,
    pub(crate) write_blocked: std::sync::atomic::AtomicBool,
    pub(crate) journaled: crate::durable_inputs::Journaled,
    pub(crate) deps: SessionDeps,
    pub(crate) log: Arc<EventLog>,
    pub(crate) meta: SessionMeta,
    pub(crate) messages: Vec<Message>,
    /// 冷读取时只供显示的、最后一个压缩点之前的消息（工作线 F102，compact.md §5.1）：打开后不变，只进 `view()`。模型侧的一切（主循环、压缩、token、标题、rewind、fork、goal 与任务的恢复）只看 `messages`。另一个只读的读者：
    /// 打开时旧会话的跨会话 entry 在这里按文字找一次是否并入过（peer_restore.rs，F139）。
    pub(crate) earlier: Vec<Message>,
    pub(crate) selection: ModelSelection,
    /// 执行状态：权限模式 × plan 开关（修复线 F20，shell-permissions.md §4.6）。判决用的模式是 [`Self::mode`]。
    pub(crate) execution: rust_cli_kernel_api::permission::ExecutionState,
    /// 计划模式的两个会话标志（已退出计划模式、待发退出提示）：与 `mode` 同一个
    /// owner，改模式时更新，提交 reentry / exit 时清掉；不持久化（context.md §13.4）。
    pub(crate) plan_flags: rust_cli_kernel_api::plan::PlanModeFlags,
    /// 自动模式拦下的次数（auto-classifier.md §16.5）：主线程的唯一 owner，经 `SessionOps::note_auto_verdict` 改，只在内存。
    pub(crate) auto_denials: rust_cli_kernel_api::permission::auto_mode::DenialCounts,
    /// 输入调度：排队的输入与 next 通道（插话输入与后台通知的到达顺序，优先级 `next`），一条输入只存一份（inbox.rs，
    /// 工作线 F130）。
    pub(crate) inbox: crate::inbox::Inbox,
    /// 已从任务表取走、还没开回合的定时任务（排在通道与 `Queue` 输入之后）。
    pub(crate) cron_fires: VecDeque<rust_cli_kernel_api::cron::CronFire>,
    pub(crate) pending: Vec<Pending>,
    pub(crate) running: Option<Running>,
    pub(crate) job: Option<Job>,
    /// 没有被代码回退撤销的 checkpoint，按写入顺序（spec §4）。
    pub(crate) checkpoints: Vec<WorkspaceCheckpoint>,
    /// 被代码回退撤销过的 checkpoint（§10.54）：不能再回退；它们的回合的文件改动是「已撤销」，回合的 diff 照样从它们算。
    pub(crate) reverted: Vec<WorkspaceCheckpoint>,
    /// 提交核心（kernel-api `RunLedger`，与子代理的 sink 同一份，工作线 F130）：当前轮（随每个批次交给存储，node-store.md
    /// §4，修复线 F6；冷加载时取 `LoadedSession.turn`）、工具已结束而结果还没提交的 checkpoint 与子代理关联。只在
    /// [`Self::commit`] 与提交消息时改。
    pub(crate) ledger: RunLedger,
    /// 排队的输入扣住了（session-lifecycle.md §10.6）：扣住时回合结束不开下一条输入。只在内存。
    pub(crate) queue_hold: Option<rust_cli_kernel_api::session_api::QueueHold>,
    /// 会话的跟随方式（session-lifecycle.md §10.27，K-S4）：`SubmitFollowUp` 在回合在跑时怎样投递。只在内存（照 v4 的内存事件库）。
    pub(crate) follow_up: Delivery,
    /// 提升的输入（`Command::Promote`，§10.13）：它中止的回合结束后 `start_next` 先开它，不论扣住。只在内存，与 `queue_hold`
    /// 同一个 owner。
    pub(crate) promoted: Option<TurnInput>,
    /// 回合在跑时收到的换掉一条输入（`Command::ReplaceInput`，§10.53）：它中止的回合结束后 `start_next` 先做它，不论扣住。与 `promoted`
    /// 只有一条（后到的被拒），同一个 owner。只在内存。
    pub(crate) replacing: Option<Box<crate::replace::Replacement>>,
    /// 最近一次被接受的「放行并切模式」回答的询问的调用（session-lifecycle.md §10.25、§10.59）：只在内存（只进 `view()`）。
    pub(crate) mode_grant: Option<ToolUseId>,
    /// 最近一次工具切的 plan（§10.25）：只在内存（只进 `view()`），只由 `mode.rs` 的 `apply` 写。
    pub(crate) plan_switch: Option<rust_cli_kernel_api::session_api::PlanSwitch>,
    /// 附加工作目录（docs/specs/rust-cli/session-dirs.md §1）：只在内存，只由 `dirs.rs` 的 `add_directory` 追加；主循环与工具经
    /// `RunView` 读它。
    pub(crate) additional_directories: Vec<rust_cli_kernel_api::env::EnvPath>,
    /// 宿主给的会话偏好（docs/specs/rust-cli/app-prefs.md §4.2）：起始状态里的，之后不变；每次运行的请求带一份副本。
    pub(crate) preferences: rust_cli_kernel_api::session_preferences::SessionPreferences,
    /// 会话钉住的模型可见输入（prompt_pin.rs，context.md §15.3）：只在压缩边界提交之后换；主循环经 `RunView` 读它。
    pub(crate) prompt: crate::prompt_pin::PromptPin,
    /// 会话级的权限规则层（docs/specs/rust-cli/permission-updates.md §3）：只在内存，只由 `permission_updates.rs` 的 `apply_updates`
    /// 改；主循环与工具经 `RunView` 读它，`/permissions` 经 `view()`。
    pub(crate) session_rules: Vec<rust_cli_kernel_api::permission::SourcedRule>,
    /// 回合记录（session-lifecycle.md §10.1）：打开时从库里读到的加上本进程里写的，按写入顺序（只进 `view()`）。
    pub(crate) turns: Vec<rust_cli_kernel_api::store::TurnRecord>,
    /// 输入记录（§10.14）：打开时从库里读到的加上本进程里写的，按写入顺序（只进 `view()`）。
    pub(crate) inputs: Vec<rust_cli_kernel_api::input_extras::InputRecord>,
    /// 输入的消息与它的命令（§10.30）：打开时从库里读到的加上本进程里提交的，按写入顺序（只进 `view()`）。
    pub(crate) sources: Vec<rust_cli_kernel_api::store::InputSource>,
    /// 工具结果的输出头部（§10.33）：打开时从库里读到的加上本进程里提交的，按写入顺序（只进 `view()`）。
    pub(crate) output_heads: Vec<rust_cli_kernel_api::command_output::ToolOutputHead>,
    /// 工具调用的开工与结束（§10.37）：打开时从库里读到的加上本进程里提交的，按写入顺序（只进 `view()`）。
    pub(crate) tool_times: Vec<rust_cli_kernel_api::metering::ToolTime>,
    /// ExitPlanMode 拿给用户批准的计划（§10.43）：打开时从库里读到的加上本进程里结束的调用的，按记下的顺序（只进 `view()`）。
    pub(crate) plans: Vec<rust_cli_kernel_api::plan::ShownPlan>,
    /// 已经开始、还没有落库任何消息的回合：它的记录随它的第一条消息同一批提交（commit.rs）。
    pub(crate) unrecorded: Option<rust_cli_kernel_api::session_api::TurnStart>,
    /// 记下的 hook 运行（hook_rows.rs，§10.51）：只在内存，只进 `view()`。
    pub(crate) hook_rows: crate::hook_rows::HookRows,
    /// 主线程派生的子代理（subagent_rows.rs，§10.56）：只在内存，只进 `view()`。
    pub(crate) subagent_rows: crate::subagent_rows::SubagentRows,
    /// 历史被改写的次数（`HistoryRewritten.generation`）。
    pub(crate) generation: u64,
    /// 会话标题的生成（title.rs）与不带参数的 `/rename` 的起名任务（rename.rs）。
    pub(crate) titles: TitleState,
    /// `/loop` 哨兵展开的状态（loop_tick.rs）：不持久化，主线程压缩后清空。
    pub(crate) loop_ticks: crate::loop_tick::LoopTicks,
    pub(crate) naming: Option<crate::rename::NameJob>,
    /// `/goal` 的目标（goal.rs）：提交 `goal_status` 后才改，冷加载从活动历史恢复。
    pub(crate) goal: crate::goal::Goals,
    /// 回合或压缩在跑时到达的本地命令记录（修复线 L-A，local_record.rs）：收尾后按到达顺序提交。
    pub(crate) held_records: Vec<rust_cli_kernel_api::user_command::LocalCommandRecord>,
    /// 命令账本（ledger.rs，session-lifecycle.md §10.3）：id → 摘要与原结果。
    pub(crate) commands: crate::ledger::CommandLedger,
    /// 续写时补在内存里、还没落库的消息，按历史里的顺序：末尾未结调用的中断结果（session-lifecycle.md §6.3，工作线
    /// F81）与 `No response requested.`（§6.3.1，工作线 F77）。第一批带消息的提交把它们排在最前面，成功后清掉；
    /// `StoreResumeSentinel` 把它们单独提交（F79）。只在 `commit` 与 `store_resume_sentinel` 里改。
    pub(crate) resume_pending: Vec<MessageId>,
    /// 延迟落库的草稿（docs/specs/rust-cli/session-facts.md §4）：还没落库的事实，第一项是会话行。只在 [`Self::commit`] 里改：
    /// 不带消息的批次接在后面，第一批带消息的提交连它一起落库，成功后清掉。
    pub(crate) draft: Option<Vec<rust_cli_kernel_api::store::Fact>>,
    /// 正在关闭：前台工作收尾后回复它并退出循环；期间不开新回合、不接新命令（spec §3.3）。
    pub(crate) closing: Option<oneshot::Sender<()>>,
    /// 收到了 `Shutdown`（进程要退出，修复线 F38，spec §3.5）：之后不再开回合。
    pub(crate) shutting_down: bool,
    stream: Notices,
    agent_stream: AgentStream,
    cron_stream: CronStream,
    /// 跨会话的入站流与状态（peers.rs）。
    peer_stream: crate::watched::Watched<rust_cli_kernel_api::peers::PeerSignal>,
    pub(crate) peers: crate::peers::PeerState,
    /// Monitor 的切行、批、限流与终态的合成（工作线 F83，monitors.rs）：只在 actor 里。
    pub(crate) monitors: crate::monitors::Monitors,
    /// 在跑的后台工作（K-W1，works.rs）：只在内存，冷打开时为空。
    pub(crate) works: crate::works::Works,
    /// 最近一个完成的主回合请求的输出速度（footer 的 TPS，修复线 F105）：只在内存，冷打开时为空。
    pub(crate) output_speed: Option<rust_cli_kernel_api::metering::OutputSpeed>,
    /// 等后台子代理的起点（回合结束那一行，修复线 F118，duration.rs）：只在内存。
    pub(crate) wait_started: Option<u64>,
    /// 自己的请求通道（弱引用）：交给回合的输出端，给出会话级的询问入口；不让 actor 因此不退出。
    asks: mpsc::WeakSender<Request>,
}

/// 拒绝的回执：`refusal` 是闭集的原因（session-lifecycle.md §10.4），`reason` 是给前端看的文字（存储的错误按它的原文）。
pub(crate) fn rejected(refusal: Refusal, reason: impl std::fmt::Display) -> Ack {
    Ack::Rejected {
        refusal,
        reason: reason.to_string(),
    }
}

pub(crate) fn accepted() -> Ack {
    Ack::Accepted {
        result: CommandResult::Done,
    }
}

impl Actor {
    /// `resume_pending`：续写时补在内存里的消息（`resume::settle` 的中断结果与 `resume_sentinel::insert` 的占位，按
    /// 历史里的顺序），随第一批带消息的提交落库。
    pub(crate) fn new(
        deps: SessionDeps,
        log: Arc<EventLog>,
        state: crate::SessionState,
        asks: mpsc::WeakSender<Request>,
        resume_pending: Vec<MessageId>,
    ) -> Self {
        // 本机与连上的远端机器的通知（R3，remote-exec.md §20.7.2）。
        let stream = Notices::new(rust_cli_kernel_api::machine_notices::all_shell_notices(
            deps.machines.clone(),
            state.meta.id.clone(),
        ));
        let agent_stream = AgentStream::new(deps.agents.signals(&state.meta.id));
        let cron_stream = CronStream::new(deps.cron.wakeups(&state.meta.id));
        let peer_stream = crate::watched::Watched::new(deps.peers.port.signals(&state.meta.id));
        let start = state.start;
        let restored_inputs = state.pending_inputs;
        let defer_start = deps.policy.resume
            == rust_cli_kernel_api::session_policy::ResumeStart::OnNextInput
            && start == rust_cli_kernel_api::session_api::StartSource::Resume;
        let goal = crate::goal::Goals::restore(&state.messages, deps.clock.now_ms());
        // 命令账本的种子是效果记录里的命令（session-lifecycle.md §10.33）：同一个命令出现几次只记一项。
        let commands = crate::ledger::CommandLedger::seeded(
            state.sources.iter().map(|source| source.command.clone()),
        );
        let mut actor = Self {
            resume_queue_paused: defer_start || !restored_inputs.is_empty(),
            deferred_start: defer_start.then_some(start),
            write_blocked: std::sync::atomic::AtomicBool::new(false),
            journaled: Default::default(),
            deps,
            log,
            meta: state.meta,
            messages: state.messages,
            earlier: state.earlier,
            selection: state.selection,
            execution: state.mode,
            plan_flags: Default::default(),
            auto_denials: Default::default(),
            inbox: crate::inbox::Inbox::default(),
            cron_fires: VecDeque::new(),
            pending: Vec::new(),
            running: None,
            job: None,
            checkpoints: state.checkpoints,
            ledger: RunLedger::new(state.turn),
            turns: state.turns,
            inputs: state.inputs,
            sources: state.sources,
            output_heads: state.output_heads,
            tool_times: state.tool_times,
            plans: state.plans,
            reverted: state.reverted,
            unrecorded: None,
            hook_rows: crate::hook_rows::HookRows::default(),
            subagent_rows: crate::subagent_rows::SubagentRows::default(),
            queue_hold: None,
            follow_up: Delivery::Queue,
            promoted: None,
            replacing: None,
            mode_grant: None,
            plan_switch: None,
            additional_directories: state.additional_directories,
            preferences: state.preferences,
            prompt: state.prompt,
            session_rules: state.session_rules,
            generation: 0,
            titles: TitleState::default(),
            loop_ticks: Default::default(),
            naming: None,
            goal,
            held_records: Vec::new(),
            commands,
            resume_pending,
            draft: state.draft,
            closing: None,
            shutting_down: false,
            stream,
            agent_stream,
            cron_stream,
            peer_stream,
            peers: crate::peers::PeerState::default(),
            monitors: crate::monitors::Monitors::default(),
            works: crate::works::Works::default(),
            output_speed: None,
            wait_started: None,
            asks,
        };
        for input in restored_inputs {
            actor.restore_input(input.envelope);
        }
        // 云端恢复只恢复事实，SessionStart 的模型/命令 hook 也等用户继续。
        if !defer_start {
            actor.start_session(start);
        }
        actor
    }

    /// 回合（或命令、压缩）的输出端：请求经 `sender` 交给 actor；会话级的询问入口经自己的请求通道。
    pub(crate) fn sink(&self, sender: mpsc::Sender<SinkRequest>) -> crate::sink::ActorSink {
        crate::sink::ActorSink {
            sender,
            asks: self.asks.clone(),
            session: self.meta.id.clone(),
            detached: self.deps.detached.clone(),
        }
    }

    /// 有回合或后台工作在跑：新输入排队，回退与 TUI 的 fork 拒绝（v5 的 fork 在回合旁也接受，准入见 kernel-api 的 `admit`）。
    pub(crate) fn busy(&self) -> bool {
        self.running.is_some() || self.job.is_some()
    }

    /// 关掉它不中止、不丢下任何工作（spec §3.3.2）：不忙、没有待回答的交互、输入调度里没有项（排队的输入、插话、等着开回合的通知）、
    /// 没有在跑的后台工作、标题与起名任务、以后会到期开回合的定时任务（关掉之后到再打开之前它们不触发），也不是还没落库的草稿。只在回合里
    /// 才有的状态（取走的定时任务、提升与换掉的输入、回合里到的本地命令记录）在不忙时都已处理完。
    pub(crate) fn idle(&self) -> bool {
        !self.busy()
            && self.pending.is_empty()
            && self.inbox.is_empty()
            && self.works.is_empty()
            && !self.titles.in_flight()
            && self.naming.is_none()
            && self.draft.is_none()
            && !self.deps.cron.scheduled(&self.meta.id)
    }

    /// 登记里的状态：有待处理的交互时是等你，否则看在不在跑（tui-cockpit.md §4）。
    pub(crate) fn status_now(&self) -> rust_cli_kernel_api::peers::PeerStatus {
        use rust_cli_kernel_api::peers::PeerStatus;
        if !self.pending.is_empty() {
            PeerStatus::Waiting
        } else if self.busy() {
            PeerStatus::Busy
        } else {
            PeerStatus::Idle
        }
    }

    pub(crate) async fn run(mut self, mut requests: mpsc::Receiver<Request>) {
        // 跨会话：绑定为主会话并从库里恢复（cross-session.md §2.2）；在处理第一条命令之前。
        self.peers_open().await;
        loop {
            // 修复（R1-1）：以前这里在“所有句柄都已释放”（`recv` 返回 None）时退出，但内核的会话表一直持有
            // 句柄，这个分支永远到不了——关闭后 actor 还活着，收到后台通知就自己开回合。现在由 `Request::Close`
            // 显式关闭：前台工作收尾后在这里退出。句柄只在会话表释放作用域之后才会全部释放，那时 actor 已被中止，
            // 所以 `recv` 的 None 不需要分支（`Some` 模式不匹配时 select! 只是停用这一支）。
            if !self.busy()
                && let Some(reply) = self.closing.take()
            {
                self.peers_close().await;
                let _ = reply.send(());
                return;
            }
            let now_ms = self.deps.clock.now_ms();
            let monitor_due = self.monitors.next_deadline();
            let next = tokio::select! {
                Some(request) = requests.recv() => Next::Request(request),
                request = sink_request(&mut self.running) => Next::Sink(request),
                notice = next_notice(&mut self.stream) => Next::Notice(notice),
                signal = self.agent_stream.next() => Next::Agent(signal),
                () = self.cron_stream.next() => Next::Cron,
                done = job_done(&mut self.job) => Next::Job(done),
                done = title_done(&mut self.titles) => Next::Title(done),
                done = crate::rename::name_done(&mut self.naming) => Next::Name(done),
                signal = self.peer_stream.next() => Next::Peer(signal),
                () = crate::peers::deadline(&self.peers, now_ms) => Next::PeerDeadline,
                (task, event) = crate::monitors::next_event(&mut self.monitors) => Next::Monitor(task, event),
                () = crate::monitors::deadline(monitor_due, now_ms) => Next::MonitorDue,
            };
            match next {
                Next::Request(request) => self.handle(request).await,
                Next::Sink(Some(request)) => self.on_sink(request).await,
                Next::Notice(notice) => self.on_notice(notice).await,
                Next::Agent(signal) => self.on_agent_signal(signal).await,
                Next::Cron => self.on_cron_wakeup().await,
                Next::Job(done) => self.on_job(done).await,
                Next::Title(done) => self.on_title(done).await,
                Next::Name(result) => self.on_name(result).await,
                Next::Peer(signal) => self.on_peer_signal(signal).await,
                Next::PeerDeadline => self.peers_expire(true).await,
                Next::Monitor(task, event) => self.on_monitor_event(task, event).await,
                Next::MonitorDue => self.on_monitor_due().await,
                // 主循环的输出端已全部释放：它已经返回，收尾本回合。
                Next::Sink(None) => self.finish_turn().await,
            }
            // 登记里的状态只看 actor 自己的状态，变化时才写（cross-session.md §2.1）。
            self.peer_status(self.status_now()).await;
        }
    }

    async fn handle(&mut self, request: Request) {
        match request {
            Request::View { reply } => {
                let _ = reply.send(self.view().await);
            }
            Request::Mode { reply } => {
                let _ = reply.send(self.mode());
            }
            Request::Meta { reply } => {
                let _ = reply.send(self.meta.clone());
            }
            Request::Work { work, reply } => {
                let _ = reply.send((self.works.state(&work), self.meta.workspace.clone()));
            }
            Request::RewindOptions { reply } => {
                let _ = reply.send(self.rewind_snapshot());
            }
            Request::RewindPreview { target, reply } => {
                let _ = reply.send(self.rewind_undo(&target));
            }
            Request::TurnFiles { turn, reply } => {
                let _ = reply.send(self.turn_files(&turn));
            }
            Request::Goal { reply } => {
                let _ = reply.send(self.goal_view());
            }
            Request::Close { reply, reason } => self.close(reply, reason).await,
            Request::CloseIdle { reply, closed } => {
                // 空闲才进入关闭：没有要中止的，原因不起作用；不空闲时丢下 `closed`，什么都不改
                let meta = self.idle().then(|| self.meta.clone());
                if meta.is_some() {
                    self.close(closed, AbortReason::Shutdown).await;
                }
                let _ = reply.send(meta);
            }
            // 关闭中、要退出时不再登记询问：丢下回复，询问方按会话已关闭处理。
            Request::Ask { reply, .. } if self.closing.is_some() || self.shutting_down => {
                drop(reply);
            }
            Request::Ask { request, reply } => self.ask(request, reply, AskScope::Session),
            Request::CancelAsk { tool_use } => self.cancel_ask(&tool_use),
            Request::AnswerAsk {
                tool_use,
                answer,
                reply,
            } => {
                let _ = reply.send(self.answer_ask(&tool_use, answer).await);
            }
            Request::ApplyUpdates { updates, reply } => {
                let _ = reply.send(self.apply_updates(None, &updates).await);
            }
            Request::Command { envelope, reply } => {
                // 认识的 id 在账本里回执（§10.3）。
                let Some(reply) = self.commands.admit(&envelope, reply) else {
                    return;
                };
                // 准入（§10.4）：关闭中不接新命令，「忙」的拒绝都在这里给出。停止的拒绝由 `interrupt` 自己给（空闲时取消了
                // loop 唤醒也算接受，TUI 的行为）。
                let admission = admit(&self.admission(), &envelope.command);
                if let Admission::Deny(refusal) = &admission
                    && !matches!(envelope.command, Command::Interrupt { .. })
                {
                    let text = crate::gate::refused(&envelope.command, refusal);
                    self.commands.settle(reply, rejected(refusal.clone(), text));
                    return;
                }
                // 输入账本（pg-store.md §4）：准入放行之后、执行之前记账，被准入拒绝的输入不进账本。本机的账本不记账，
                // 总是放行；持久账本认出的重复（别的进程接纳过）回它原来的回执。
                if let Some(ack) = self.admit_input(&envelope).await {
                    self.commands.settle(reply, ack);
                    return;
                }
                // 代码回退的回执要等事务结束（spec §5.1），起名的回执要等名字（titles.md §8.2）。
                match envelope.command {
                    Command::Rewind { target, scope } => {
                        return self.rewind(envelope.id, target, scope, reply).await;
                    }
                    // 回执等新回合开了（回合在跑时等它结束之后，§10.53）
                    Command::ReplaceInput {
                        target,
                        text,
                        extras,
                        restore_code,
                        stop_cut_works,
                    } => {
                        let replacement = crate::replace::Replacement {
                            command: envelope.id,
                            target,
                            text,
                            extras: Box::new(extras),
                            restore_code,
                            stop_cut_works,
                            reply,
                        };
                        return self.replace_input(replacement).await;
                    }
                    Command::GenerateName => return self.generate_name(envelope.id, reply),
                    _ => {}
                }
                // 输入命令（Submit、SubmitContent、Bash）记进账本之后不会再被拒绝：准入已经放行，开回合对它们恒为 Ok。
                let ack = self.command(envelope, admission).await;
                self.commands.settle(reply, ack);
            }
        }
    }

    /// 准入要看的状态（kernel-api `admission::AdmissionView`）：在跑的回合、会话的后台工作、是否正在关闭。
    pub(crate) fn admission(&self) -> AdmissionView {
        let activity = match (&self.running, &self.job) {
            (Some(running), _) => Activity::Turn(running.turn.clone()),
            (None, Some(job)) => Activity::Job {
                cancellable: job.cancel.is_some(),
            },
            (None, None) => Activity::Idle,
        };
        AdmissionView {
            activity,
            closing: self.closing.is_some(),
            held: self.queue_is_held(),
            frozen: self.inbox.later_head_locked(),
            compaction: self
                .running
                .as_ref()
                .is_some_and(|running| running.compaction)
                || matches!(self.promoted, Some(TurnInput::Compact { .. }))
                || self.inbox.has_compaction(),
            follow_up: self.follow_up,
        }
    }

    async fn command(&mut self, envelope: CommandEnvelope, admission: Admission) -> Ack {
        // 恢复时队列暂停（pg-store.md §4、SessionPolicy::resume）：账本里待处理的输入已按接纳顺序回到队列，新的输入排在
        // 它们之后，不越过它们开回合；推迟的 SessionStart 也在这时开始。
        if self.resume_queue_paused
            && rust_cli_kernel_api::input_journal::is_journal_input(&envelope.command)
        {
            let id = envelope.id.clone();
            self.restore_input(envelope);
            self.input_fate(id, InputFate::Queued);
            self.resume_queue_paused = false;
            if let Some(start) = self.deferred_start.take() {
                self.start_session(start);
            }
            if !self.busy() {
                self.start_next().await;
            }
            return accepted();
        }
        let CommandEnvelope { id, command } = envelope;
        let queued = matches!(admission, Admission::Enqueue(_));
        match command {
            Command::Submit { text, extras, .. } | Command::SubmitFollowUp { text, extras } => {
                self.submit(id, text, extras, admission).await
            }
            Command::SubmitNow { text, extras } => {
                self.submit_now(id, text, extras, admission).await
            }
            Command::SetFollowUp { delivery } => {
                self.set_follow_up(delivery);
                accepted()
            }
            Command::Interrupt { target } => self.interrupt(target, admission).await,
            Command::Shutdown => self.shutdown().await,
            Command::RestoreCancelled => self.restore_cancelled(id).await,
            Command::ResolveInteraction {
                interaction,
                answer,
            } => self.resolve(id, interaction, answer).await,
            Command::SetModel { selection } => match self.change_model(Some(id), selection).await {
                Ok(()) => accepted(),
                Err(reason) => rejected(Refusal::Failed, reason),
            },
            Command::SetMode { mode } => match self.user_mode(id, mode).await {
                Ok(()) => accepted(),
                Err(reason) => rejected(Refusal::Failed, reason),
            },
            Command::SetPlan { enabled } => match self.user_plan(id, enabled).await {
                Ok(()) => accepted(),
                Err(reason) => rejected(Refusal::Failed, reason),
            },
            Command::Compact { instructions } => {
                self.start_compaction(id, instructions);
                accepted()
            }
            Command::Fork { at } => self.fork(id, at).await,
            Command::ForkFromInput { input } => self.fork_from_input(input).await,
            Command::ForkFromInputAs { input, session } => self.fork_into(input, session).await,
            Command::SideChatAs { session } => self.side_chat(session).await,
            Command::Rename { title } => self.rename(id, title, None).await,
            Command::RenameRecorded { title, record } => self.rename(id, title, Some(record)).await,
            Command::StopTask { task } => self.stop_task(task).await,
            Command::ResolvePeerMessage { message, approve } => {
                match self.resolve_peer(&message, approve).await {
                    Ok(()) => accepted(),
                    Err(reason) => rejected(Refusal::NotFound, reason),
                }
            }
            Command::ChangeDirectory { root } => self.change_directory(id, root, None).await,
            Command::ChangeDirectoryRecorded { root, record } => {
                self.change_directory(id, root, Some(record)).await
            }
            Command::RecordLocalCommand { record } => self.record_local_command(id, record).await,
            Command::AddDirectory {
                directory,
                args,
                remember,
            } => self.add_directory(id, directory, args, remember).await,
            Command::UpdatePermissions { updates } => {
                match self.apply_updates(Some(id), &updates).await {
                    Ok(()) => accepted(),
                    Err(reason) => rejected(Refusal::Failed, reason),
                }
            }
            Command::BackgroundForeground { reason } => self.background_foreground(reason).await,
            Command::StoreResumeSentinel => self.store_resume_sentinel(id).await,
            Command::Dequeue { command } => self.dequeue(&command).await,
            Command::PatchNote { message, patch } => self.patch_note(&message, &patch).await,
            Command::SetAutoDrain { enabled } => {
                self.set_auto_drain(enabled).await;
                accepted()
            }
            // 准入已判过「只在空闲时」；扣住不动，所以这一轮之后队列仍扣住（§10.13）。前端看到的扣住的队列要与现在的相符（§10.16）。
            Command::SubmitKeepingQueue {
                text,
                extras,
                expected_held,
            } => match self.held_as_expected(expected_held.as_deref()) {
                Ok(()) => self.submit(id, text, extras, admission).await,
                Err(refused) => refused,
            },
            // 取空扣住的队列再发（§10.16）：查、取空与提交在这一次处理里。
            Command::SubmitDiscardingQueue {
                text,
                extras,
                expected_held,
            } => match self.held_as_expected(expected_held.as_deref()) {
                Ok(()) => {
                    self.discard_held().await;
                    self.submit(id, text, extras, admission).await
                }
                Err(refused) => refused,
            },
            Command::Promote { command, .. } => self.promote(command).await,
            Command::EditQueued {
                command,
                edit,
                guard,
            } => self.edit_queued(&command, &edit, &guard).await,
            Command::LockQueued { command, editor } => self.lock_queued(&command, editor),
            Command::UnlockQueued { command } => self.unlock_queued(&command).await,
            Command::ApplyQueuedConfig { commands, config } => {
                self.apply_queued_config(&commands, &config)
            }
            Command::MoveQueued { command, before } => self.move_queued(&command, before.as_ref()),
            Command::ResolveSwitchingMode { interaction, mode } => {
                self.resolve_switching_mode(id, interaction, mode).await
            }
            Command::CompactOrQueue { selection } => {
                self.compact_or_queue(id, selection, &admission).await
            }
            // 在 `handle` 里处理（回执要等事务结束或名字）。
            Command::Rewind { .. } | Command::ReplaceInput { .. } | Command::GenerateName => {
                rejected(Refusal::Failed, "this command is handled by the actor loop")
            }
            Command::Bash {
                command: line,
                delivery: _,
            } => {
                self.bash_command(id, line, queued);
                accepted()
            }
            Command::SubmitContent {
                content,
                delivery: _,
            } => match self.content_command(id, content, queued).await {
                Ok(()) => accepted(),
                Err(reason) => rejected(Refusal::Failed, reason),
            },
        }
    }

    async fn on_job(&mut self, done: Result<JobDone, rust_cli_base::TaskError>) {
        self.job = None;
        match done {
            Ok(JobDone::Startup(outcome)) => self.finish_startup(*outcome).await,
            Ok(JobDone::Rewind(finished)) => self.finish_rewind(*finished).await,
            Err(error) => {
                tracing::warn!(%error, "a session job ended without a result");
                self.start_next().await;
            }
        }
    }

    /// 一条输入的结局（session-lifecycle.md §10.7）：与对应的状态变化同一步发出。
    pub(crate) fn input_fate(&self, input: CommandId, fate: InputFate) {
        let at = self.deps.clock.now_ms();
        self.log.append(KernelEvent::InputFate { input, fate, at });
    }

    pub(crate) fn emit_queue(&self) {
        self.log.append(KernelEvent::QueueChanged {
            items: self.inbox.queued(),
        });
    }
}

#[cfg(test)]
#[path = "turn_anchor_tests.rs"]
mod turn_anchor_tests;
