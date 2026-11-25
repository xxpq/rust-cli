//! 本进程里启动过的子代理（ListAgents、SendMessage、TaskStop 与后台列表都查这里）。
//!
//! 子代理的消息只在子会话里（存储是唯一事实源）；这里只记身份、归属、运行状态与邮箱。发给子代理的
//! 通知也记在这里它自己的条目上（docs/specs/rust-cli/agents.md §5.1），不另起状态源。

use rust_cli_base::sync::LockUnpoisoned;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use rust_cli_kernel_api::abort::{Abort, AbortReason};
use rust_cli_kernel_api::clock::Clock;
use rust_cli_kernel_api::env::WorkspaceRef;
use rust_cli_kernel_api::env_shell::BackgroundStatus;
use rust_cli_kernel_api::ids::{AgentId, MachineName, SessionId, ToolUseId};
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::orchestration::AskedBy;
use rust_cli_kernel_api::permission::PermissionMode;

use crate::definitions::AgentDefinition;
use crate::doing::Doing;
use crate::report::{Ending, Report, StoppedBy};

/// 发给某个子代理的一条通知：它派生的后台子代理结束了。
#[derive(Debug, Clone)]
pub(crate) struct Notice {
    pub(crate) from: AgentId,
    pub(crate) status: BackgroundStatus,
    /// `<task-notification>` 信封（不带说明与包装）。
    pub(crate) envelope: String,
}

/// 邮箱里的一项：SendMessage 的消息在工具边界作为插话并入，通知作为 `task-notification` 并入。
#[derive(Debug, Clone)]
pub(crate) enum Mail {
    Message(String),
    Notice(Notice),
}

/// 一次运行的停止句柄：子循环的中止信号，加上谁停的（agents.md §5 的停止者表）。停止者由停止命令进来的地方给（[`Live::stop`]
/// 的调用方：TaskStop 是模型，用户的停止是用户；前台子代理随父调用被取消也是用户），运行结束时由这个句柄带进终态，是停止者唯一的事实源。
#[derive(Debug, Clone, Default)]
pub(crate) struct Stop {
    abort: Abort,
    by: Arc<OnceLock<StoppedBy>>,
}

impl Stop {
    /// 先记停止者、再中止：看到中止的一方一定读得到停止者。停了几次时第一次的停止者生效（同 `Abort` 的原因）。
    pub(crate) fn stop(&self, by: StoppedBy) {
        let _ = self.by.set(by);
        self.abort.abort(AbortReason::User);
    }

    /// 交给子循环的中止信号。
    pub(crate) fn abort(&self) -> &Abort {
        &self.abort
    }

    pub(crate) fn is_stopped(&self) -> bool {
        self.abort.is_aborted()
    }

    /// 谁停的。子循环只因这个句柄中止，运行以中止结束时一定停过；没停过时读到的是 `StoppedBy` 的缺省（用户）。
    pub(crate) fn by(&self) -> StoppedBy {
        self.by.get().copied().unwrap_or_default()
    }
}

/// SendMessage 的去向（[`Live::deliver`]，agents.md §6）。
#[derive(Debug)]
pub(crate) enum Delivery {
    /// 正在跑：排进邮箱，下一个工具边界并入。
    Queued,
    /// 已结束或停放：换成了属于这次 SendMessage 调用的新一次运行。`previous` 是换之前的状态（结果的文字），`agent` 是换之后的快照
    /// （后台运行用它的停止句柄）。
    Restarted {
        previous: BackgroundStatus,
        agent: Box<LiveAgent>,
    },
    /// 用户停过（终态的停止者是用户，或正在因用户的停止收尾）：不续跑（照对照实现的 `stoppedByUser`）。
    StoppedByUser,
    /// 别的停止者（TaskStop、父调用取消）停了、还没退出：不排队（排进去也会在运行结束时丢掉）、不续跑，等它收尾（照对照实现的 still stopping）。
    StillStopping,
}

#[derive(Debug, Clone)]
pub(crate) enum AgentState {
    /// 正在跑；`mailbox` 在下一个工具边界并入。
    Running {
        stop: Stop,
        mailbox: Vec<Mail>,
    },
    /// 跑完了，但还有在跑的后台子代理、monitor 或没交给它的后台命令（keepalive，只在交互模式，agents.md §5.1 第 3 条）。
    /// 记着最后一次运行的报告（被 TaskStop 停止、收尾时的通知用它）与自己的通知送过没有：只为后台子代理与 monitor
    /// 停放时推迟（`false`），只为命令停放时已经送出（`true`，§5.2 第 5 条）。
    Parked {
        report: Report,
        notified: bool,
    },
    Finished(Ending),
}

impl AgentState {
    /// 新的一次运行（新的停止句柄）；`mailbox` 是一开始就排着的。
    pub(crate) fn running(mailbox: Vec<Mail>) -> Self {
        Self::Running {
            stop: Stop::default(),
            mailbox,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct LiveAgent {
    pub(crate) agent: AgentId,
    pub(crate) child: SessionId,
    /// 启动它的会话。
    pub(crate) parent: SessionId,
    /// 顶层会话：任务清单、链路 ID 与“送顶层会话”的通知用它。
    pub(crate) root: SessionId,
    /// 派生它的子代理；主线程派生的没有（通知送 `root`）。
    pub(crate) owner: Option<AgentId>,
    /// 主线程派生的是 1，每往下一层加 1（Agent 工具只给深度小于 3 的子代理）。
    pub(crate) depth: u32,
    /// 这次运行属于的工具调用（通知的 `<tool-use-id>`）：派生时是 Agent 调用；SendMessage 续跑之后是那次 SendMessage 调用
    /// （agents.md §6）；SendMessage 留下的消息续跑之后为 None（续跑不属于哪次工具调用，通知里没有 `<tool-use-id>`，A8）。
    pub(crate) tool_use: Option<ToolUseId>,
    pub(crate) description: String,
    /// 定义（含 system prompt 正文）共享一份：条目每次被查询、快照都要复制（修复线 F6，审查 R5-11）。
    pub(crate) definition: Arc<AgentDefinition>,
    pub(crate) workspace: WorkspaceRef,
    /// 子代理的工具缺省在哪台机器上跑（Agent 的 `machine`，R3，remote-exec.md §20.6）。
    pub(crate) machine: MachineName,
    /// 子会话的模型（落库的那一个）：会话的模型派生出来的，不是加速 provider（agents.md §4.1）。
    pub(crate) selection: ModelSelection,
    /// 派生时跟随的父运行的执行覆盖（agents.md §4.1）：每次运行（含续跑）的请求按它的选择与引用发；没有时用 `selection`。
    pub(crate) execution: Option<rust_cli_kernel_api::turn_execution::TurnExecution>,
    /// 派生那一刻算的模式（Agent 调用上下文里父会话的模式套上定义的 `permissionMode`）：只在父会话不在、`follow` 没有回答时用
    /// （agents.md §4.1.3）。
    pub(crate) mode: PermissionMode,
    /// 父会话此刻的模式（派生时取自 Agent 调用的 `SessionOps::live_mode`）：子代理每次取视图都跟随它现算（agents.md §4.1.3）。
    pub(crate) follow: Option<crate::child::Follow>,
    pub(crate) background: bool,
    /// 父会话的会话级询问入口（交互界面里才有，agents.md §4.1.1）：后台运行（含续跑、停放后的唤醒）经它询问。
    pub(crate) asks: Option<crate::child::Asks>,
    pub(crate) state: AgentState,
    /// 列表显示用的记录（tui-panels.md §1.1）；由 `live::Live` 在每次改动后统一维护。
    pub(crate) shown: Shown,
    /// 转发任务已经交给它的命令通知的来源（命令的任务 ID，agents.md §5.2 第 5 条）：在交出的同一把锁里记。命令通知交出
    /// 之后才 ack，这一段里执行环境的 `shell_state` 还列着它；收尾判断“还有没交给它的命令”时去掉这些。
    pub(crate) delivered: Vec<AgentId>,
    /// fork 子代理继承的前缀（修复线 L-A，agents.md §8.1.3）：不落库，续跑时拼在子会话的冷读取之前；别的子代理为 None。
    pub(crate) fork_prefix: Option<Arc<Vec<rust_cli_kernel_api::message::Message>>>,
    /// 派生它的运行不许用的工具（session-lifecycle.md §10.15）：这个子代理的每次运行（含续跑）都带着，它再派生的也继承。
    pub(crate) disallowed_tools: Vec<String>,
    /// 派生时父调用的附加工作目录（session-dirs.md §3.3）：之后不跟父会话（同 `workspace`），它再派生的也继承。
    pub(crate) additional_directories: Vec<rust_cli_kernel_api::env::EnvPath>,
    /// 派生它的运行的会话偏好（app-prefs.md §4.2）：这个子代理的每次运行（含续跑）都带着，它再派生的也继承。
    pub(crate) preferences: rust_cli_kernel_api::session_preferences::SessionPreferences,
    /// 派生它的运行钉住的模型可见输入（context.md §15.3）：后台运行用它；前台读父会话现在的。只有 fork 子代理的 system 读它。
    pub(crate) prompt: Arc<rust_cli_kernel_api::session_prompt::SessionPrompt>,
    /// 派生时父调用的会话级规则层（permission-updates.md §3）：后台运行用它（之后不跟父会话，D4）；前台读父会话现在的。
    pub(crate) session_rules: Vec<rust_cli_kernel_api::permission::SourcedRule>,
}

/// 列表显示用的记录：启动顺序、这次运行的开始与结束时刻、等待中的交互、子会话的提交次数、当前活动。
#[derive(Debug, Clone, Default)]
pub(crate) struct Shown {
    pub(crate) order: u64,
    pub(crate) started_ms: u64,
    pub(crate) ended_ms: Option<u64>,
    pub(crate) waiting: u32,
    pub(crate) version: u64,
    /// 当前活动与记下它的时刻（L35，stall-visibility.md §4.3）；离开运行时清掉。
    pub(crate) doing: Option<(Doing, u64)>,
    /// 最近一个完成的子代理请求的输出速度（修复线 F112，tui-panels.md §1.7：查看它时 footer 的 TPS）：只在内存里，
    /// 与主会话的速度同一个口径（tui-display.md §2.6）。续跑与结束都不清掉。
    pub(crate) speed: Option<rust_cli_kernel_api::metering::OutputSpeed>,
}

impl LiveAgent {
    /// 子代理此刻的模式（agents.md §4.1.3）：父会话此刻的模式套上定义的 `permissionMode`；父会话没有回答时用派生那一刻的。
    pub(crate) async fn current_mode(&self) -> PermissionMode {
        let parent = match &self.follow {
            Some(follow) => follow.0.mode().await,
            None => None,
        };
        parent.map_or(self.mode, |parent| {
            crate::spawn::mode(parent, self.definition.permission_mode)
        })
    }

    /// 这个子代理的询问带的来源（agents.md §4.1.1）。
    pub(crate) fn asked_by(&self) -> AskedBy {
        AskedBy {
            agent: self.agent.clone(),
            agent_type: self.definition.agent_type.clone(),
            session: self.child.clone(),
            parent: self.parent.clone(),
            description: self.description.clone(),
            tool_use: self.tool_use.clone(),
        }
    }

    pub(crate) fn status(&self) -> BackgroundStatus {
        match &self.state {
            AgentState::Running { .. } => BackgroundStatus::Running,
            // 停放的子代理状态是 completed（带 keepalive）。
            AgentState::Parked { .. } => BackgroundStatus::Completed,
            AgentState::Finished(ending) => ending.status(),
        }
    }

    /// 还在跑或停放。
    pub(crate) fn is_live(&self) -> bool {
        !matches!(self.state, AgentState::Finished(_))
    }
}

pub(crate) type Agents = BTreeMap<AgentId, LiveAgent>;

pub(crate) struct Live {
    agents: Mutex<Agents>,
    clock: Arc<dyn Clock>,
    /// 启动顺序的计数（列表按它排序）。
    order: AtomicU64,
    /// 每次改动后加一（tokio watch：多次改动合并成一次通知，tui-panels.md §0）。
    changes: tokio::sync::watch::Sender<u64>,
}

impl Live {
    pub(crate) fn new(clock: Arc<dyn Clock>) -> Self {
        Self {
            agents: Mutex::default(),
            clock,
            order: AtomicU64::new(0),
            changes: tokio::sync::watch::channel(0).0,
        }
    }

    fn agents(&self) -> MutexGuard<'_, Agents> {
        self.agents.lock_unpoisoned()
    }

    /// 改动条目：锁里改完、记好时刻，放锁后再通知（订阅方收到通知后去查，一定看得到这次改动）。
    fn change<R>(&self, work: impl FnOnce(&mut Agents) -> R) -> R {
        let result = {
            let mut agents = self.agents();
            let result = work(&mut agents);
            settle_times(&mut agents, self.clock.now_ms());
            result
        };
        self.changes
            .send_modify(|version| *version = version.wrapping_add(1));
        result
    }

    /// 变化通知的订阅（列表的查询方据此重取）。
    pub(crate) fn subscribe(&self) -> tokio::sync::watch::Receiver<u64> {
        self.changes.subscribe()
    }

    /// 顶层会话 `root` 里条目的快照与查询时刻（列表用）。先过滤再复制（修复线 F6，审查 R5-11：以前每次列表查询都复制
    /// 全部会话的全部条目）。
    pub(crate) fn snapshot_in(&self, root: &SessionId) -> (Vec<LiveAgent>, u64) {
        let agents: Vec<LiveAgent> = self
            .agents()
            .values()
            .filter(|agent| &agent.root == root)
            .cloned()
            .collect();
        (agents, self.clock.now_ms())
    }

    /// 在一把锁里读改（通知的路由要一次看清并改动几个条目，agents.md §5.1）。
    pub(crate) fn with<R>(&self, work: impl FnOnce(&mut Agents) -> R) -> R {
        self.change(work)
    }

    pub(crate) fn insert(&self, mut agent: LiveAgent) {
        agent.shown.order = self.order.fetch_add(1, Ordering::Relaxed);
        agent.shown.started_ms = self.clock.now_ms();
        self.change(|agents| agents.insert(agent.agent.clone(), agent));
    }

    /// 子代理经父会话询问时记一次等待（`waiting` 为假时结束一次）。
    pub(crate) fn waiting(&self, agent: &AgentId, waiting: bool) {
        self.change(|agents| {
            if let Some(entry) = agents.get_mut(agent) {
                entry.shown.waiting = if waiting {
                    entry.shown.waiting + 1
                } else {
                    entry.shown.waiting.saturating_sub(1)
                };
            }
        });
    }

    /// 子会话提交了一条消息（只读查看据此重取）；这条消息是一个完成的子代理请求的回复时同时记下它的速度（F112）。
    /// 速度与版本在同一次改动里：看到新版本去取快照时，速度一定已经是这个请求的。
    pub(crate) fn committed(
        &self,
        agent: &AgentId,
        speed: Option<rust_cli_kernel_api::metering::OutputSpeed>,
    ) {
        self.change(|agents| {
            if let Some(entry) = agents.get_mut(agent) {
                entry.shown.version += 1;
                if speed.is_some() {
                    entry.shown.speed = speed;
                }
            }
        });
    }

    /// 子循环的事件改变了当前活动（L35）。活动没变时不通知；变了只发变化通知、不加 `version`
    /// （只读查看不因此重取子会话）。
    pub(crate) fn doing(&self, agent: &AgentId, doing: Option<Doing>) {
        let changed = {
            let mut agents = self.agents();
            let Some(entry) = agents.get_mut(agent) else {
                return;
            };
            if entry.shown.doing.as_ref().map(|(now, _)| now) == doing.as_ref() {
                false
            } else {
                entry.shown.doing = doing.map(|doing| (doing, self.clock.now_ms()));
                true
            }
        };
        if changed {
            self.changes
                .send_modify(|version| *version = version.wrapping_add(1));
        }
    }

    pub(crate) fn get(&self, agent: &AgentId) -> Option<LiveAgent> {
        self.agents().get(agent).cloned()
    }

    /// 顶层会话：`session` 是某个子代理的子会话时取它的顶层会话，否则就是它自己。
    pub(crate) fn root_of(&self, session: &SessionId) -> SessionId {
        self.agents()
            .values()
            .find(|agent| &agent.child == session)
            .map_or_else(|| session.clone(), |agent| agent.root.clone())
    }

    /// 由 `session` 启动的子代理，按 ID 排序。
    pub(crate) fn spawned_by(&self, session: &SessionId) -> Vec<LiveAgent> {
        self.agents()
            .values()
            .filter(|agent| &agent.parent == session)
            .cloned()
            .collect()
    }

    /// 顶层会话 `root` 里还在跑或停放的后台子代理（含子代理里再派生的），按 ID 排序。
    pub(crate) fn background_in(&self, root: &SessionId) -> Vec<LiveAgent> {
        self.agents()
            .values()
            .filter(|agent| &agent.root == root && agent.background && agent.is_live())
            .cloned()
            .collect()
    }

    /// SendMessage 给 `from` 这个会话启动的子代理（agents.md §6）：在一把锁里按它此刻的状态定去向。不是这个会话启动的（或没有）时为 None。
    /// 续跑时这次运行属于 `tool_use` 这次 SendMessage 调用，在状态转移的同一把锁里换，运行与它属于的调用不会一新一旧（R-RESUME-NOTICE）。
    ///
    /// 修 bug（EXP-DEFECTS-4，agents.md §6）：以前先排邮箱、不在跑再续跑、续跑说在跑再排邮箱，各自一把锁；用户停过的子代理照常续跑，
    /// 用户刚停、还在收尾的照常排进邮箱（运行结束时丢掉，结果却说已排队）。现在去向只在这一处、一次锁里定，用户停过的不续跑。
    pub(crate) fn deliver(
        &self,
        agent: &AgentId,
        from: &SessionId,
        message: String,
        tool_use: ToolUseId,
    ) -> Option<Delivery> {
        self.change(|agents| {
            let entry = agents
                .get_mut(agent)
                .filter(|entry| &entry.parent == from)?;
            let previous = entry.status();
            Some(match &mut entry.state {
                // 修 bug（EXP-DEFECTS-5，agents.md §6）：以前只拦用户停的，TaskStop 停了、还在收尾的照常排进邮箱，运行结束时丢掉，结果却说已排队。
                // 停了、还没退出就是停止句柄已中止而条目还是 Running（收尾在 `routing::settle` 的锁里换成 Finished），不另记「正在停」。
                AgentState::Running { stop, .. } if stop.is_stopped() => match stop.by() {
                    StoppedBy::User => Delivery::StoppedByUser,
                    StoppedBy::Model | StoppedBy::CallCancelled => Delivery::StillStopping,
                },
                AgentState::Running { mailbox, .. } => {
                    mailbox.push(Mail::Message(message));
                    Delivery::Queued
                }
                AgentState::Finished(ending) if ending.stopped_by_user() => Delivery::StoppedByUser,
                AgentState::Parked { .. } | AgentState::Finished(_) => {
                    entry.background = true;
                    entry.tool_use = Some(tool_use);
                    entry.state = AgentState::running(Vec::new());
                    Delivery::Restarted {
                        previous,
                        agent: Box::new(entry.clone()),
                    }
                }
            })
        })
    }

    pub(crate) fn finish(&self, agent: &AgentId, ending: Ending) {
        self.change(|agents| {
            if let Some(entry) = agents.get_mut(agent) {
                entry.state = AgentState::Finished(ending);
            }
        });
    }

    /// 工具边界取走邮箱里的一类（按到达顺序）；已中止时不取，留到运行结束时处理。
    fn take(&self, agent: &AgentId, notices: bool) -> Vec<Mail> {
        match self.agents().get_mut(agent).map(|entry| &mut entry.state) {
            Some(AgentState::Running { stop, mailbox }) if !stop.is_stopped() => {
                let (taken, kept) = std::mem::take(mailbox)
                    .into_iter()
                    .partition(|mail| matches!(mail, Mail::Notice(_)) == notices);
                *mailbox = kept;
                taken
            }
            _ => Vec::new(),
        }
    }

    /// SendMessage 排进来的消息。
    pub(crate) fn take_mailbox(&self, agent: &AgentId) -> Vec<String> {
        self.take(agent, false)
            .into_iter()
            .filter_map(|mail| match mail {
                Mail::Message(text) => Some(text),
                Mail::Notice(_) => None,
            })
            .collect()
    }

    /// 发给它的通知（信封）。
    pub(crate) fn take_notices(&self, agent: &AgentId) -> Vec<Notice> {
        self.take(agent, true)
            .into_iter()
            .filter_map(|mail| match mail {
                Mail::Notice(notice) => Some(notice),
                Mail::Message(_) => None,
            })
            .collect()
    }

    /// 中止正在跑的子代理，记下停止者（`by`，由停止命令进来的地方给）；不在跑时返回 false（停放的由 `routing::stop_parked` 处理）。
    pub(crate) fn stop(&self, agent: &AgentId, by: StoppedBy) -> bool {
        match self.agents().get(agent).map(|entry| &entry.state) {
            Some(AgentState::Running { stop, .. }) => {
                stop.stop(by);
                true
            }
            _ => false,
        }
    }
}

/// 状态转移之后统一记时刻：进入运行时重新开始计时，离开运行（停放、结束）时记结束时刻。状态改在很多处
/// （routing、TaskStop、SendMessage），时刻只在这里记，不在每个转移点各写一遍。
fn settle_times(agents: &mut Agents, now: u64) {
    for agent in agents.values_mut() {
        let running = matches!(agent.state, AgentState::Running { .. });
        match (running, agent.shown.ended_ms) {
            (true, Some(_)) => {
                agent.shown.started_ms = now;
                agent.shown.ended_ms = None;
            }
            (false, None) => {
                agent.shown.ended_ms = Some(now);
                agent.shown.doing = None;
            }
            _ => {}
        }
    }
}

#[cfg(test)]
#[path = "live_tests.rs"]
mod tests;
