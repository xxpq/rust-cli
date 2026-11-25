//! 会话 actor：一个会话的状态的唯一 owner（docs/specs/rust-cli/README.md §3、§8）。
//!
//! 接收命令（按 CommandId 幂等）、排队、驱动主循环、把主循环事件转成持久事实（先提交再发事件）、
//! 给事件分配 seq。前端只经 [`SessionHandle`] 访问。
//!
//! 已实现：回合、排队、中断、切换模型与模式、主循环的提交屏障（[`sink`]）、待回答的交互
//! （只在内存，工作线 F130）；会话侧 hooks、文件 checkpoint、rewind、fork 与中断后续跑
//! （docs/specs/rust-cli/session-lifecycle.md）。

mod actor;
mod agent_signals;
mod append;
mod bash;
mod checkpoints;
mod closing;
mod commit;
mod compact;
mod cron;
mod detached;
mod dirs;
#[cfg(test)]
mod durable_input_tests;
mod durable_inputs;
mod duration;
mod fork;
mod fork_dirs;
mod fork_input;
mod gate;
mod goal;
mod hook_rows;
mod hooks;
mod inbox;
mod input;
#[cfg(test)]
mod input_tests;
#[cfg(test)]
mod input_upload_tests;
mod lane;
mod launch;
mod ledger;
mod local_record;
mod log;
mod loop_fold;
mod loop_tick;
#[cfg(test)]
mod metering_tests;
mod mode;
mod monitors;
mod note;
mod notices;
mod peer_receipts;
mod peer_restore;
mod peers;
mod permission_updates;
mod prompt_pin;
mod queue_edit;
mod rename;
mod rename_text;
mod replace;
mod replace_works;
mod resume;
mod resume_sentinel;
mod rewind;
mod rewind_machines;
mod rewind_plan;
mod rewind_preview;
mod side_chat;
mod sink;
mod slash;
mod steering;
mod subagent_rows;
mod tasks;
#[cfg(test)]
mod test_support;
mod title;
mod title_model;
mod title_parse;
mod turn;
mod turn_files;
mod view;
mod watched;
mod works;
mod workspace;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures_util::stream::BoxStream;
use rust_cli_base::{Spawner, Task};
use rust_cli_kernel_api::agents::AgentSignals;
use rust_cli_kernel_api::clock::{Clock, Ids};
use rust_cli_kernel_api::env::Operations;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::message::Message;
use rust_cli_kernel_api::model::{ModelPort, ModelSelection};
use rust_cli_kernel_api::orchestration::{Compactor, HookRunner, QueryRunner, ToolRegistry};
use rust_cli_kernel_api::session_api::{
    Ack, CommandEnvelope, EndReason, KernelError, KernelEvent, RewindOption, SequencedEvent,
    SessionHandle, SessionView, StartSource,
};
use rust_cli_kernel_api::store::{ArtifactStore, SessionMeta, SessionStore, WorkspaceCheckpoint};

pub use prompt_pin::PromptPin;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::actor::{Actor, Request};
pub use crate::detached::DetachedAsks;
pub use crate::fork_dirs::{ForkDirectories, Inherited};
use crate::log::EventLog;
pub use crate::peers::PeerDeps;
pub use crate::rewind::reverted_prompts;
pub use crate::rewind_preview::rewind_preview;
pub use crate::title_model::TitleModel;
pub use crate::turn_files::turn_file_diffs;

/// 命令队列的缓冲；满了时发送方等待。
const REQUESTS: usize = 64;

/// 会话 actor 依赖的能力。
#[derive(Clone)]
pub struct SessionDeps {
    pub store: Arc<dyn SessionStore>,
    /// 输入账本（pg-store.md §4）：接纳、取消与重开后的待处理输入都经它；没有持久账本的入口注入 `NoJournal`。
    pub journal: Arc<dyn rust_cli_kernel_api::input_journal::InputJournal>,
    /// 随部署形态变的行为（恢复时怎样启动、bash 输入何时提交）：组装根算好，这里只读。
    pub policy: rust_cli_kernel_api::session_policy::SessionPolicy,
    pub ids: Arc<dyn Ids>,
    pub runner: Arc<dyn QueryRunner>,
    /// 手动压缩（`Command::Compact`，docs/specs/rust-cli/compact.md §4.2）。
    pub compactor: Arc<dyn Compactor>,
    /// 执行环境的注册表（R3，remote-exec.md §20.7）：会话在它的主 env 上（订阅后台命令的终态通知、查询后台工作，
    /// docs/specs/rust-cli/query.md §5；shell 当前目录、代码回退，§15.8.1），斜杠命令的工具上下文带它的 controller 服务；
    /// 远端机器上的后台通知、Monitor、代码回退与会话关闭经它找到那台机器。工作线 F131：以前另存一份主 env 与 controller
    /// 的服务，与注册表给的是同一份。
    pub machines: Arc<dyn rust_cli_kernel_api::env_registry::EnvironmentRegistry>,
    /// 主线程的工具注册表：斜杠命令、`@` 提及与 bash 模式的工具上下文带它在这个会话的视图（`ToolUseContext::tools`，
    /// 没有禁用名单，tools-more.md §3.5）。
    pub tools: Arc<dyn ToolRegistry>,
    /// 每次提问最多几轮（`--max-turns`）。
    pub max_turns: Option<u32>,
    /// 子代理的信号：后台子代理的终态通知、任务清单变化（docs/specs/rust-cli/agents.md §5）。
    pub agents: Arc<dyn AgentSignals>,
    /// 会话内定时任务的唤醒与到期任务（docs/specs/rust-cli/tools-more.md §10.4）；`-p` 是 `NoCron`。
    pub cron: Arc<dyn rust_cli_kernel_api::cron::CronSignals>,
    /// 会话侧 hooks（UserPromptSubmit、SessionStart、SessionEnd、Notification）。
    pub hooks: Arc<dyn HookRunner>,
    /// 用户输入的斜杠命令（skill 的用户调用，docs/specs/rust-cli/ext.md §2.7）：回合任务先展开再跑 UPS。
    pub commands: Arc<dyn rust_cli_kernel_api::user_command::UserCommands>,
    /// 用户输入的 `@` 提及与 bash 模式（docs/specs/rust-cli/tui-input.md §1.1、§2.2）：回合任务在斜杠展开之后调用。
    pub inputs: Arc<dyn rust_cli_kernel_api::user_input::UserInputs>,
    /// checkpoint 的 artifact（rewind 读回写入前后的内容）。
    pub artifacts: Arc<dyn ArtifactStore>,
    /// fork 出的会话的创建时间；会话标题请求的用量计时（工作线 F73）。
    pub clock: Arc<dyn Clock>,
    /// 操作上下文的工厂（执行环境的当前实例 + `ids`；回退事务、停任务、停 monitor、`!` 命令，F134：以前另带一个
    /// `generation`）。
    pub operations: Operations,
    /// 会话自己的任务作用域：actor 与主循环都在这里启动。
    pub spawner: Spawner,
    /// 关闭任务的作用域（内核的根作用域，session-lifecycle.md §3.3.1）：关闭不依赖调用方的 future；会话自己的作用域
    /// 在关闭的最后一步释放，关闭任务不能在里面跑。
    pub closing: Spawner,
    /// 会话标题的生成（交互形态才有；`-p` 为 None，docs/specs/rust-cli/titles.md §2）。
    pub titles: Option<TitleModel>,
    /// 会话 actor 自己发的旁路请求用的模型端口（不带参数的 `/rename` 起名，titles.md §8；`-p` 也有）。
    pub side_model: Arc<dyn ModelPort>,
    /// 关闭之后由组装方释放的按会话状态（session-lifecycle.md §3.3）。
    pub released: Arc<dyn Released>,
    /// `/cd` 的说明里新目录的规则文件（tui-manage.md §7.1）：actor 用自己的活动历史调用。
    pub directory_rules: Arc<dyn rust_cli_kernel_api::orchestration::DirectoryRules>,
    /// 本机跨会话通信（docs/specs/rust-cli/cross-session.md §2.4）。
    pub peers: PeerDeps,
    /// 父会话关闭之后旧会话的询问入口（进程级，agents.md §4.1.2，工作线 F72）；没有时那些询问按会话已关闭失败。
    pub detached: Option<Arc<DetachedAsks>>,
    /// 回合被停止或失败之后扣住排队的输入（session-lifecycle.md §10.6）：v5 前端打开，TUI 与 `-p` 关着。
    pub hold_queue: bool,
    /// fork 出的会话继承的附加工作目录（docs/specs/rust-cli/session-dirs.md §12.5）：fork 写进存储之后记下父会话的，组装方冷加载子会话时
    /// 取走。一个内核一份。
    pub forks: Arc<ForkDirectories>,
    /// 前端层（docs/specs/rust-cli/context.md §15.5）：一条输入带的引用与页面状态给模型的写法。没有时（测试、嵌入）这两种材料
    /// 不给模型。
    pub frontend_prompt: Option<Arc<dyn rust_cli_kernel_api::frontend_prompt::FrontendPrompt>>,
}

/// 会话关闭的收尾（docs/specs/rust-cli/session-lifecycle.md §3.3、§3.3.1）：每个会话的关闭任务各调用一次。
pub trait Released: Send + Sync {
    /// 关闭任务的第一步（早于 SessionEnd）：组装方把会话表里的条目标成“关闭中”，之后打开同一个会话的调用等关闭走完。
    fn closing(&self, _session: &SessionId) {}

    /// 空闲时关闭（`SessionHandle::close_idle`，spec §3.3.2）判出不空闲：组装方把「关闭中」的条目换回打开着，等它的打开拿到原句柄。
    fn kept(&self, _session: &SessionId) {}

    /// 关闭任务的最后一步（actor 退出、执行环境与定时任务释放之后）：组装方（runtime）清掉各能力按会话保存的表，
    /// 最后把会话移出会话表、释放它的任务作用域。
    fn released(&self, session: &SessionId);
}

/// 没有按会话的表要释放（测试与嵌入）。
pub struct NothingToRelease;

impl Released for NothingToRelease {
    fn released(&self, _session: &SessionId) {}
}

/// 打开会话时的状态（新建或冷加载）。
pub struct SessionState {
    pub pending_inputs: Vec<rust_cli_kernel_api::input_journal::JournalInput>,
    pub meta: SessionMeta,
    pub messages: Vec<Message>,
    /// 只供显示的、最后一个压缩点之前的消息（冷加载的 `LoadedSession.earlier`，工作线 F102）；新建的会话为空。
    pub earlier: Vec<Message>,
    pub selection: ModelSelection,
    /// 执行状态（权限模式 × plan 开关，修复线 F20）；旧的单值模式经 `From<PermissionMode>`（`Plan` 是 plan 开）。
    pub mode: rust_cli_kernel_api::permission::ExecutionState,
    /// 还没有被代码回退撤销的 checkpoint（冷加载的 `LoadedSession.checkpoints`）。
    pub checkpoints: Vec<WorkspaceCheckpoint>,
    /// SessionStart hook 的来源（session-lifecycle.md §3.2）。
    pub start: StartSource,
    /// 当前轮（冷加载的 `LoadedSession.turn`；新建的会话没有）。
    pub turn: Option<rust_cli_kernel_api::store::TurnAnchor>,
    /// 当前轮以错误结束（冷加载的 `LoadedSession.last_turn_failed`）：续写时不补 assistant（session-lifecycle.md
    /// §6.3.1）。
    pub last_turn_failed: bool,
    /// 回合记录（冷加载的 `LoadedSession.turns`，session-lifecycle.md §10.1）；新建的会话为空。
    pub turns: Vec<rust_cli_kernel_api::store::TurnRecord>,
    /// 输入记录（冷加载的 `LoadedSession.inputs`，§10.14）；新建的会话为空。
    pub inputs: Vec<rust_cli_kernel_api::input_extras::InputRecord>,
    /// 效果记录：输入的消息与它的命令（冷加载的 `LoadedSession.sources`，§10.3、§10.31）：其中的命令进命令账本（§10.33）；
    /// 新建的会话为空。
    pub sources: Vec<rust_cli_kernel_api::store::InputSource>,
    /// 工具结果的输出头部（冷加载的 `LoadedSession.output_heads`，§10.33）；新建的会话为空。
    pub output_heads: Vec<rust_cli_kernel_api::command_output::ToolOutputHead>,
    /// 工具调用的开工与结束（冷加载的 `LoadedSession.tool_times`，§10.37）；新建的会话为空。
    pub tool_times: Vec<rust_cli_kernel_api::metering::ToolTime>,
    /// ExitPlanMode 拿给用户批准的计划（冷加载的 `LoadedSession.plans`，§10.43）；新建的会话为空。
    pub plans: Vec<rust_cli_kernel_api::plan::ShownPlan>,
    /// 被文件回退撤销过的 checkpoint（冷加载的 `LoadedSession.reverted`，§10.54）；新建的会话为空。
    pub reverted: Vec<WorkspaceCheckpoint>,
    /// 延迟落库的草稿（docs/specs/rust-cli/session-facts.md §4）：建会话的事实（第一项是 `Fact::SessionCreated`），随第一批
    /// 带消息的提交落库；None 是已经在库里。
    pub draft: Option<Vec<rust_cli_kernel_api::store::Fact>>,
    /// 起始的附加工作目录（docs/specs/rust-cli/session-dirs.md §3.1）：组装方按内核缺省与建会话带的算好；不落库，冷加载的只有
    /// 内核缺省。
    pub additional_directories: Vec<rust_cli_kernel_api::env::EnvPath>,
    /// 宿主给的会话偏好（docs/specs/rust-cli/app-prefs.md §4.2）：建会话、冷打开时调用方给的；不落库，缺省不关。
    pub preferences: rust_cli_kernel_api::session_preferences::SessionPreferences,
    /// 起始的会话层权限规则（permission-updates.md §3）：不落库，缺省为空；侧聊第一次冷加载时是父会话开侧聊那一刻的快照（session-lifecycle.md §10.58）。
    pub session_rules: Vec<rust_cli_kernel_api::permission::SourcedRule>,
    /// 会话钉住的模型可见输入（docs/specs/rust-cli/context.md §15.3）：组装方按内核缺省与库里的记录建好；缺省是空的那一份（测试）。
    pub prompt: PromptPin,
}

impl SessionState {
    /// 新建的会话（还没有历史）。
    pub fn created(
        meta: SessionMeta,
        selection: ModelSelection,
        mode: rust_cli_kernel_api::permission::ExecutionState,
        start: StartSource,
    ) -> Self {
        Self {
            pending_inputs: Vec::new(),
            meta,
            messages: Vec::new(),
            earlier: Vec::new(),
            selection,
            mode,
            checkpoints: Vec::new(),
            start,
            turn: None,
            last_turn_failed: false,
            turns: Vec::new(),
            inputs: Vec::new(),
            sources: Vec::new(),
            output_heads: Vec::new(),
            tool_times: Vec::new(),
            plans: Vec::new(),
            reverted: Vec::new(),
            draft: None,
            additional_directories: Vec::new(),
            preferences: Default::default(),
            session_rules: Vec::new(),
            prompt: PromptPin::created(Default::default()),
        }
    }

    /// 冷加载的会话（工作线 F130，审查 C10：冷加载的恢复在会话这一侧只有这一个入口，以前由 runtime 逐个字段拼）：库里
    /// 没有模型选择时用 `selection`，没有执行状态时用 `mode`（新会话的缺省）。续跑时要补的消息、定时任务与目标的恢复
    /// 在 [`Session::spawn`] 里按 [`Opened::Loaded`] 做。
    pub fn from_loaded(
        loaded: rust_cli_kernel_api::store::LoadedSession,
        selection: &ModelSelection,
        mode: rust_cli_kernel_api::permission::PermissionMode,
        start: StartSource,
    ) -> Self {
        Self {
            pending_inputs: Vec::new(),
            meta: loaded.meta,
            messages: loaded.messages,
            earlier: loaded.earlier,
            selection: loaded.model.unwrap_or_else(|| selection.clone()),
            mode: loaded.mode.map_or_else(
                || mode.into(),
                |stored| {
                    rust_cli_kernel_api::permission::ExecutionState::from_parts(stored, loaded.plan)
                },
            ),
            checkpoints: loaded.checkpoints,
            start,
            turn: loaded.turn,
            last_turn_failed: loaded.last_turn_failed,
            turns: loaded.turns,
            inputs: loaded.inputs,
            sources: loaded.sources,
            output_heads: loaded.output_heads,
            tool_times: loaded.tool_times,
            plans: loaded.plans,
            reverted: loaded.reverted,
            draft: None,
            additional_directories: Vec::new(),
            preferences: Default::default(),
            session_rules: Vec::new(),
            prompt: PromptPin::created(Default::default()),
        }
    }
}

/// 会话是新建的还是从存储加载的；新建时发出 `SessionCreated`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Opened {
    Created,
    Loaded,
}

pub struct Session {
    id: SessionId,
    requests: mpsc::Sender<Request>,
    log: Arc<EventLog>,
    deps: SessionDeps,
    /// 每台远端机器上一份的后台命令（远端执行 F96，remote-exec.md §23.3）：快照的唯一 owner 是会话；重连中的机器用它。
    last_remote: tasks::LastSeen,
    /// 关闭的进度（[`closing`]）：None 还没关；Some 关闭任务已经起了，令牌在它结束时取消。
    closing: Mutex<Option<CancellationToken>>,
    _actor: Task<Option<()>>,
}

impl Session {
    pub fn spawn(deps: SessionDeps, mut state: SessionState, opened: Opened) -> Self {
        let id = state.meta.id.clone();
        let log = Arc::new(EventLog::new());
        let mut pending = Vec::new();
        match opened {
            Opened::Created => {
                log.append(KernelEvent::SessionCreated {
                    meta: state.meta.clone(),
                });
            }
            // 中断后续跑：末尾没有结果的调用补中断结果、更早的去掉 tool_use 块（session-lifecycle.md §6.3，工作线
            // F81）；最后一条 user 消息没有回答时补一条 assistant（§6.3.1）。补的消息由 actor 随第一批带消息的提交
            // 落库（工作线 F77），会话里切过去时由 `Command::StoreResumeSentinel` 马上落库（F79）。
            Opened::Loaded => {
                let ids = deps.ids.clone();
                let (messages, results) =
                    resume::settle(std::mem::take(&mut state.messages), || ids.message());
                state.messages = messages;
                pending = results;
                pending.extend(resume_sentinel::insert(
                    &mut state.messages,
                    state.last_turn_failed,
                    || ids.message(),
                ));
            }
        }
        let (requests, receiver) = mpsc::channel(REQUESTS);
        let spawner = deps.spawner.clone();
        // 从存储打开的会话：进入循环之前先让任务表从活动历史复原会话任务（tools-more.md
        // §18.2），第一次 `take_due` 时任务表已就绪。新建的会话没有历史，不调。
        let resume = (opened == Opened::Loaded).then(|| state.messages.clone());
        let cron = deps.cron.clone();
        let session = id.clone();
        let actor = Actor::new(
            deps.clone(),
            log.clone(),
            state,
            requests.downgrade(),
            pending,
        );
        let task = spawner.spawn_resident("session-actor", async move {
            if let Some(history) = resume {
                cron.resume(&session, &history).await;
            }
            actor.run(receiver).await
        });
        Self {
            id,
            requests,
            log,
            deps,
            last_remote: tasks::LastSeen::default(),
            closing: Mutex::default(),
            _actor: task,
        }
    }

    async fn ask<T>(
        &self,
        request: impl FnOnce(oneshot::Sender<T>) -> Request,
    ) -> Result<T, KernelError> {
        closing::ask(&self.requests, request).await
    }
}

fn closed() -> KernelError {
    KernelError::Failed {
        message: "the session is closed".to_owned(),
    }
}

#[async_trait]
impl SessionHandle for Session {
    fn id(&self) -> &SessionId {
        &self.id
    }

    async fn send(&self, envelope: CommandEnvelope) -> Ack {
        let (reply, answer) = oneshot::channel();
        if self
            .requests
            .send(Request::Command { envelope, reply })
            .await
            .is_err()
        {
            return Ack::Rejected {
                refusal: rust_cli_kernel_api::admission::Refusal::Closed,
                reason: closed().to_string(),
            };
        }
        answer.await.unwrap_or_else(|_| Ack::Rejected {
            refusal: rust_cli_kernel_api::admission::Refusal::Closed,
            reason: closed().to_string(),
        })
    }

    fn subscribe(&self, after: Option<u64>) -> BoxStream<'static, SequencedEvent> {
        self.log.subscribe(after)
    }

    async fn view(&self) -> Result<SessionView, KernelError> {
        self.ask(|reply| Request::View { reply }).await
    }

    /// SessionEnd 在关闭任务里跑（hook 只读 meta），不占用 actor（spec §3.3）。之后关闭 actor、释放执行环境
    /// 与定时任务、交组装方释放按会话的表；只关一次，关闭任务不依赖调用方（[`closing`]，§3.3.1）。
    ///
    /// 修复（R1-1）：以前这里只跑 SessionEnd 与 `cron.session_closed`，actor 一直活着（内核的会话表持有句柄）：
    /// `/clear`、`/resume`、fork 之后旧会话收到后台通知就在用户看不到的地方自己开回合，按会话的表与 EventLog 不释放，
    /// `Commands::close_session` 没有调用方，再打开同一会话拿到旧 actor、不跑 SessionStart。现在：前台工作
    /// 中止，后台工作保留（`/clear`、`/resume` 不 `forget`），actor 退出，组装方把会话移出会话表。
    async fn close(&self, reason: EndReason) -> Vec<String> {
        closing::close(self, reason).await
    }

    async fn close_idle(&self, reason: EndReason) -> bool {
        closing::close_idle(self, reason).await
    }

    /// actor 给出可选消息与 checkpoint 的快照；读 artifact 算统计在调用方的任务里做。
    async fn rewind_options(&self) -> Result<Vec<RewindOption>, KernelError> {
        let snapshot = self.ask(|reply| Request::RewindOptions { reply }).await?;
        Ok(rewind::options(self.deps.artifacts.as_ref(), snapshot).await)
    }

    /// actor 给出 target 起的 checkpoint（一次往返）；读 artifact、读文件在调用方的任务里（§10.53）。
    async fn rewind_preview(
        &self,
        target: &rust_cli_kernel_api::ids::MessageId,
    ) -> Result<Option<rust_cli_kernel_api::rewind_preview::RewindPreview>, KernelError> {
        let target = target.clone();
        let Some(undo) = self
            .ask(|reply| Request::RewindPreview { target, reply })
            .await?
        else {
            return Ok(None);
        };
        Ok(Some(
            rewind_preview::preview(
                self.deps.artifacts.as_ref(),
                self.deps.machines.as_ref(),
                &undo.workspace,
                undo.checkpoints,
            )
            .await,
        ))
    }

    /// actor 给出回合记录与这个回合的 checkpoint（一次往返）；读 artifact 算 hunk 在调用方的任务里（§10.45）。
    async fn turn_file_diffs(
        &self,
        turn: &rust_cli_kernel_api::ids::TurnId,
    ) -> Result<Vec<rust_cli_kernel_api::session_api::TurnFileDiff>, KernelError> {
        let turn = turn.clone();
        let files = self.ask(|reply| Request::TurnFiles { turn, reply }).await?;
        Ok(files.diffs(self.deps.artifacts.as_ref()).await)
    }

    async fn goal(&self) -> Result<rust_cli_kernel_api::goal::GoalView, KernelError> {
        self.ask(|reply| Request::Goal { reply }).await
    }

    /// 后台任务的事实不在 actor 里（执行环境的账本与 agents）：取 meta 后在调用方的任务里汇总
    /// （docs/specs/rust-cli/tui-display.md §3.1）。
    async fn background_tasks(
        &self,
    ) -> Result<Vec<rust_cli_kernel_api::manage::BackgroundEntry>, KernelError> {
        let meta = self.ask(|reply| Request::Meta { reply }).await?;
        Ok(
            tasks::background_entries(&self.deps, &self.last_remote, &meta.workspace, &meta.id)
                .await,
        )
    }

    /// 子代理的事实在 agents 里（tui-panels.md §1）：不经 actor，直接转给它，按这个顶层会话过滤。
    async fn subagents(
        &self,
    ) -> Result<Vec<rust_cli_kernel_api::panels::SubagentEntry>, KernelError> {
        Ok(self.deps.agents.subagents(&self.id).await)
    }

    fn subagent_changes(&self) -> BoxStream<'static, ()> {
        self.deps.agents.subagent_changes()
    }

    /// 子代理列表与后台任务的同一份快照（修复线 F34，tui-panels.md §0）：actor 只给出工作区，命令与子代理在调用方的
    /// 任务里取，一次回执。
    async fn tasks_snapshot(
        &self,
    ) -> Result<rust_cli_kernel_api::panels::TasksSnapshot, KernelError> {
        let meta = self.ask(|reply| Request::Meta { reply }).await?;
        Ok(tasks::snapshot(&self.deps, &self.last_remote, &meta.workspace, &meta.id).await)
    }

    async fn subagent_view(
        &self,
        agent: &rust_cli_kernel_api::ids::AgentId,
    ) -> Result<SessionView, KernelError> {
        self.deps.agents.subagent_view(&self.id, agent).await
    }

    /// Shell 详情：与 `background_tasks` 同一份账本与输出文件（tui-panels.md §3.3）。
    async fn shell_detail(
        &self,
        task: &str,
    ) -> Result<Option<rust_cli_kernel_api::panels::ShellDetail>, KernelError> {
        let meta = self.ask(|reply| Request::Meta { reply }).await?;
        Ok(tasks::shell_detail(&self.deps, &meta.workspace, &meta.id, task).await)
    }

    /// 后台命令的状态与输出末尾（§10.44）：状态由 actor 从后台工作表答，文件在调用方的任务里读，不占用 actor。
    async fn work_output(
        &self,
        work: &str,
        tail_bytes: usize,
    ) -> Result<Option<rust_cli_kernel_api::background::WorkOutput>, KernelError> {
        let work = work.to_owned();
        let (state, workspace) = self
            .ask(|reply| Request::Work {
                work: work.clone(),
                reply,
            })
            .await?;
        let Some((kind, status)) = state else {
            return Ok(None);
        };
        if kind == rust_cli_kernel_api::manage::BackgroundKind::Agent {
            return Ok(None);
        }
        let read = tasks::work_tail(&self.deps, &workspace, &self.id, &work, tail_bytes).await;
        Ok(Some(rust_cli_kernel_api::background::WorkOutput {
            status,
            read,
        }))
    }
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "ledger_tests.rs"]
mod ledger_tests;

#[cfg(test)]
#[path = "gate_tests.rs"]
mod gate_tests;

#[cfg(test)]
#[path = "stop_tests.rs"]
mod stop_tests;

#[cfg(test)]
#[path = "hold_tests.rs"]
mod hold_tests;

#[cfg(test)]
#[path = "promote_tests.rs"]
mod promote_tests;

#[cfg(test)]
#[path = "full_access_tests.rs"]
mod full_access_tests;

#[cfg(test)]
#[path = "output_head_tests.rs"]
mod output_head_tests;

#[cfg(test)]
#[path = "shown_plan_tests.rs"]
mod shown_plan_tests;

#[cfg(test)]
#[path = "tool_time_tests.rs"]
mod tool_time_tests;

#[cfg(test)]
#[path = "tool_result_facts_tests.rs"]
mod tool_result_facts_tests;

#[cfg(test)]
#[path = "work_output_tests.rs"]
mod work_output_tests;

#[cfg(test)]
#[path = "race_tests.rs"]
mod race_tests;

#[cfg(test)]
#[path = "execution_tests.rs"]
mod execution_tests;
#[cfg(test)]
#[path = "turn_config_tests.rs"]
mod turn_config_tests;

#[cfg(test)]
#[path = "compact_queue_tests.rs"]
mod compact_queue_tests;

#[cfg(test)]
#[path = "fate_tests.rs"]
mod fate_tests;

#[cfg(test)]
#[path = "origin_tests.rs"]
mod origin_tests;

#[cfg(test)]
#[path = "loop_compaction_tests.rs"]
mod loop_compaction_tests;

#[cfg(test)]
#[path = "failure_tests.rs"]
mod failure_tests;

#[cfg(test)]
#[path = "machines_tests.rs"]
mod machines_tests;

#[cfg(test)]
#[path = "work_events_tests.rs"]
mod work_events_tests;

#[cfg(test)]
#[path = "replace_tests.rs"]
mod replace_tests;

#[cfg(test)]
#[path = "auto_denials_tests.rs"]
mod auto_denials_tests;
