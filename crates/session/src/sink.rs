//! 会话 actor 给主循环的输出端（[`RunSink`]）：每个请求经通道交给 actor，在 actor 里提交后
//! 才回复，所以提交顺序与 actor 内的状态变更顺序一致（会话状态只有 actor 一个写者）。

use std::sync::Arc;

use async_trait::async_trait;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::message::{CompactTrigger, Message};
use rust_cli_kernel_api::orchestration::{
    Absorbed, Answer, InteractionAnswer, InteractionRequest, Interactions, OrchestrationError,
    QueryEvent, RunSink, RunView, SessionOps,
};
use rust_cli_kernel_api::permission::auto_mode::{AutoVerdict, DenialLimit};
use rust_cli_kernel_api::store::StoreError;
use tokio::sync::{mpsc, oneshot};

pub(crate) enum SinkRequest {
    Message {
        message: Message,
        commands: Vec<rust_cli_kernel_api::ids::CommandId>,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    Event(QueryEvent),
    Ask {
        request: InteractionRequest,
        reply: oneshot::Sender<Result<InteractionAnswer, OrchestrationError>>,
    },
    /// 工具被取消：撤回它还没回答的询问（与 `Ask` 走同一个通道，所以总排在它的 `Ask` 之后）。
    CancelAsk {
        tool_use: rust_cli_kernel_api::ids::ToolUseId,
    },
    /// 替用户回答（PermissionRequest hook 先有结论，hooks.md §6.1；同一个通道，总排在它的 `Ask` 之后）：回复是不是先到。
    AnswerAsk {
        tool_use: rust_cli_kernel_api::ids::ToolUseId,
        answer: Answer,
        reply: oneshot::Sender<bool>,
    },
    /// 没有人能回答、用了 hook 的结论时落它的权限更新（permission-updates.md §5）。
    ApplyUpdates {
        updates: Vec<rust_cli_kernel_api::permission_updates::PermissionUpdate>,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    /// 工具边界领取插话与通知（不出队，query.md §4.1、§5）。
    ClaimSteering {
        reply: oneshot::Sender<Option<crate::steering::Claimed>>,
    },
    /// 插话的结局（出队或留下）与并入的通知（按顺序）：回复出了队、可以并入的插话，与每条通知的 attachment 用的
    /// 消息 ID。
    SettleSteering {
        fates: Vec<(rust_cli_kernel_api::ids::CommandId, crate::steering::Fate)>,
        notices: Vec<crate::inbox::NoticeKey>,
        reply: oneshot::Sender<crate::steering::Settled>,
    },
    /// 一条消息（可以没有）与它的计量一个事务（docs/specs/rust-cli/usage.md §2.5）。
    Metered {
        message: Option<Message>,
        metered: Box<rust_cli_kernel_api::metering::Metered>,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    /// 一组消息一个事务（压缩的边界、摘要与附件）。
    Messages {
        messages: Vec<Message>,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    /// 主循环开始压缩。
    CompactStarted(CompactTrigger),
    /// 开始了的压缩失败了（compact.md §4.1）。
    CompactFailed(CompactTrigger, String),
    /// 工具请求打开或关掉 plan 开关（EnterPlanMode / ExitPlanMode，修复线 F20）；`by` 是请求它的工具调用（session-lifecycle.md §10.25）。
    SetPlan {
        enabled: bool,
        by: rust_cli_kernel_api::ids::ToolUseId,
        reply: oneshot::Sender<Result<(), OrchestrationError>>,
    },
    /// 主循环与工具读会话现在的状态（判决用的模式、计划模式的会话标志、工作目录、主模型、目标，一次往返，工作线
    /// F134：以前分五个请求：模式、模式与计划标志、工作目录、模型选择、目标）。
    View {
        reply: oneshot::Sender<RunView>,
    },
    /// 判决链记一次自动模式的结论（auto-classifier.md §16.5）：actor 改计数，回到了的上限。
    AutoVerdict {
        verdict: AutoVerdict,
        reply: oneshot::Sender<Option<DenialLimit>>,
    },
    /// hook 给出的会话标题（UserPromptSubmit / SessionStart 的 `sessionTitle`）。
    Title(String),
    /// 工具请求修改工作目录（docs/specs/rust-cli/tools-more.md §11.4）。
    SetWorkspace {
        root: rust_cli_kernel_api::env::EnvPath,
        reply: oneshot::Sender<Result<(), OrchestrationError>>,
    },
    /// fork 子代理读已提交的对话（修复线 L-A，agents.md §8.1.2）。
    Conversation {
        reply: oneshot::Sender<Vec<Message>>,
    },
    /// Monitor 工具起好的 monitor 交给 actor（工作线 F83，tools-more.md §19.6）。
    ArmMonitor {
        monitor: rust_cli_kernel_api::monitor::MonitorArmed,
        events: rust_cli_kernel_api::machine::CommandEvents,
        reply: oneshot::Sender<Result<(), OrchestrationError>>,
    },
    /// 回合任务在主循环之后要这个回合的 checkpoint 清单（回合的文件改动，session-lifecycle.md §10.42）。
    Checkpoints {
        reply: oneshot::Sender<Vec<rust_cli_kernel_api::store::WorkspaceCheckpoint>>,
    },
    /// 一次 hook 事件里跑了的 hook（session-lifecycle.md §10.51）：`blocked` 是 UserPromptSubmit 拦下开回合的输入时那条输入的原文。
    HooksRan {
        invocation: rust_cli_kernel_api::hook_runs::HookInvocation,
        blocked: Option<String>,
    },
    /// Agent 工具派生了一个子代理（session-lifecycle.md §10.56）：记下之后回复，子代理在那之后才跑。
    SubagentStarted {
        start: rust_cli_kernel_api::subagent_runs::SubagentStart,
        reply: oneshot::Sender<()>,
    },
    /// 主循环报告目标的判定：actor 提交反馈与 `goal_status`，回复已提交的消息（goal.md §3）。
    GoalChecked {
        condition: String,
        verdict: rust_cli_kernel_api::hooks::StopVerdict,
        reply: oneshot::Sender<Result<Vec<Message>, StoreError>>,
    },
}

pub(crate) struct ActorSink {
    pub(crate) sender: mpsc::Sender<SinkRequest>,
    /// 会话的请求通道（弱引用）：会话级的询问入口经它（`Interactions::lasting`），不让 actor 因此不退出。
    pub(crate) asks: mpsc::WeakSender<crate::actor::Request>,
    /// 会话自己的 ID 与会话关闭之后的询问入口（agents.md §4.1.2，工作线 F72）：会话级的询问入口带上它们。
    pub(crate) session: rust_cli_kernel_api::ids::SessionId,
    pub(crate) detached: Option<Arc<crate::detached::DetachedAsks>>,
}

fn closed() -> StoreError {
    StoreError::Storage {
        message: "the session is closed".to_owned(),
    }
}

impl ActorSink {
    /// 改标题（不等待提交结果：失败只记日志，不影响回合）。
    pub(crate) async fn title(&self, title: String) {
        let _ = self.sender.send(SinkRequest::Title(title)).await;
    }

    /// 开回合的输入的 UserPromptSubmit 跑了的 hook（§10.51）；拦下了这条输入时带上它的原文。不等回复。
    pub(crate) async fn prompt_hooks_ran(
        &self,
        invocation: rust_cli_kernel_api::hook_runs::HookInvocation,
        blocked: Option<String>,
    ) {
        let _ = self
            .sender
            .send(SinkRequest::HooksRan {
                invocation,
                blocked,
            })
            .await;
    }

    /// 这个回合的 checkpoint 清单（§10.42）：回合任务拿着输出端时问，actor 总在。
    pub(crate) async fn checkpoints(&self) -> Vec<rust_cli_kernel_api::store::WorkspaceCheckpoint> {
        let (reply, answer) = oneshot::channel();
        let _ = self.sender.send(SinkRequest::Checkpoints { reply }).await;
        answer.await.unwrap_or_default()
    }

    async fn call<T>(
        &self,
        request: impl FnOnce(oneshot::Sender<Result<T, StoreError>>) -> SinkRequest,
    ) -> Result<T, StoreError> {
        let (reply, answer) = oneshot::channel();
        self.sender
            .send(request(reply))
            .await
            .map_err(|_| closed())?;
        answer.await.map_err(|_| closed())?
    }
}

#[async_trait]
impl Interactions for ActorSink {
    async fn ask(
        &self,
        request: InteractionRequest,
    ) -> Result<InteractionAnswer, OrchestrationError> {
        let gone = || OrchestrationError::Failed {
            message: "the interaction ended without an answer".to_owned(),
        };
        let (reply, answer) = oneshot::channel();
        self.sender
            .send(SinkRequest::Ask { request, reply })
            .await
            .map_err(|_| gone())?;
        answer.await.map_err(|_| gone())?
    }

    async fn cancel_ask(&self, tool_use: &rust_cli_kernel_api::ids::ToolUseId) {
        let request = SinkRequest::CancelAsk {
            tool_use: tool_use.clone(),
        };
        // 回合已经结束（通道关了）时，回合收尾已经取消了全部待回答的交互。
        let _ = self.sender.send(request).await;
    }

    /// 回合已经结束（通道关了）时询问已随回合取消：已回答。
    async fn answer_ask(
        &self,
        tool_use: &rust_cli_kernel_api::ids::ToolUseId,
        answer: Answer,
    ) -> bool {
        let (reply, settled) = oneshot::channel();
        let request = SinkRequest::AnswerAsk {
            tool_use: tool_use.clone(),
            answer,
            reply,
        };
        self.sender.send(request).await.is_ok() && settled.await.unwrap_or(false)
    }

    async fn apply_permission_updates(
        &self,
        updates: Vec<rust_cli_kernel_api::permission_updates::PermissionUpdate>,
    ) -> Result<(), OrchestrationError> {
        self.call(|reply| SinkRequest::ApplyUpdates { updates, reply })
            .await
            .map_err(|error| OrchestrationError::Failed {
                message: error.to_string(),
            })
    }

    /// 会话级的询问入口（docs/specs/rust-cli/agents.md §4.1.1）：回合通道在回合结束时关闭，后台子代理在那之后仍要问。
    fn lasting(&self) -> Option<Arc<dyn Interactions>> {
        Some(Arc::new(SessionAsks {
            requests: self.asks.clone(),
            session: self.session.clone(),
            detached: self.detached.clone(),
        }))
    }
}

impl ActorSink {
    /// 一个要回复的会话命令：回合已经结束（通道关了）时是 `Closed`。
    async fn command<T>(
        &self,
        request: impl FnOnce(oneshot::Sender<Result<T, OrchestrationError>>) -> SinkRequest,
    ) -> Result<T, OrchestrationError> {
        let (reply, answer) = oneshot::channel();
        self.sender
            .send(request(reply))
            .await
            .map_err(|_| OrchestrationError::Closed)?;
        answer.await.map_err(|_| OrchestrationError::Closed)?
    }
}

/// 会话状态与对话的 owner 是会话 actor：按请求时的值回答，不另存副本。
#[async_trait]
impl SessionOps for ActorSink {
    async fn view(&self) -> Option<RunView> {
        let (reply, answer) = oneshot::channel();
        self.sender.send(SinkRequest::View { reply }).await.ok()?;
        answer.await.ok()
    }

    /// 顶层会话就是自己。
    async fn owner_session(&self, _session: &SessionId) -> SessionId {
        self.session.clone()
    }

    /// 按请求时已提交的消息回答（fork 子代理继承它，修复线 L-A）。
    async fn conversation(&self) -> Option<Vec<Message>> {
        let (reply, answer) = oneshot::channel();
        self.sender
            .send(SinkRequest::Conversation { reply })
            .await
            .ok()?;
        answer.await.ok()
    }

    async fn set_plan(
        &self,
        enabled: bool,
        by: &rust_cli_kernel_api::ids::ToolUseId,
    ) -> Result<(), OrchestrationError> {
        let by = by.clone();
        self.command(|reply| SinkRequest::SetPlan { enabled, by, reply })
            .await
    }

    async fn set_workspace(
        &self,
        root: rust_cli_kernel_api::env::EnvPath,
    ) -> Result<(), OrchestrationError> {
        self.command(|reply| SinkRequest::SetWorkspace { root, reply })
            .await
    }

    /// monitor 的 owner 是会话 actor（工作线 F83）。
    async fn arm_monitor(
        &self,
        monitor: rust_cli_kernel_api::monitor::MonitorArmed,
        events: rust_cli_kernel_api::machine::CommandEvents,
    ) -> Result<(), OrchestrationError> {
        self.command(|reply| SinkRequest::ArmMonitor {
            monitor,
            events,
            reply,
        })
        .await
    }

    /// hook 行的事实（§10.51）：经回合的提交通道，总在这一轮结束之前到 actor。
    async fn hooks_ran(&self, invocation: rust_cli_kernel_api::hook_runs::HookInvocation) {
        self.prompt_hooks_ran(invocation, None).await;
    }

    /// 子代理表的 owner 是 actor（§10.56）：经回合的提交通道，排在这次调用的 `ToolStarted` 之后、`ToolFinished` 之前；等它记下再返回，后台子代理的
    /// 终态通知就不会先于开始到 actor。回合已经结束（通道关了）时不会有：派生它的调用还在跑。
    async fn subagent_started(&self, start: rust_cli_kernel_api::subagent_runs::SubagentStart) {
        let (reply, recorded) = oneshot::channel();
        if self
            .sender
            .send(SinkRequest::SubagentStarted { start, reply })
            .await
            .is_ok()
        {
            let _ = recorded.await;
        }
    }

    /// 计数的 owner 是 actor：经同一个通道，按到达先后改。actor 已经退出时不计（回 None）。
    async fn note_auto_verdict(&self, verdict: AutoVerdict) -> Option<DenialLimit> {
        let (reply, answer) = oneshot::channel();
        self.sender
            .send(SinkRequest::AutoVerdict { verdict, reply })
            .await
            .ok()?;
        answer.await.ok().flatten()
    }

    /// 会话级的模式入口（agents.md §4.1.3）：经会话的请求通道（弱引用），回合结束之后照样能问。
    fn live_mode(&self) -> Option<Arc<dyn rust_cli_kernel_api::session_ops::LiveMode>> {
        Some(Arc::new(SessionMode {
            requests: self.asks.clone(),
        }))
    }
}

/// 会话此刻判决用的模式（agents.md §4.1.3，session-lifecycle.md 的会话级的读）：子代理派生时拿走它，每次取视图都现问 actor。
///
/// 修 bug（2026-11-12，用户报告“中途改成了 yolo 还一直弹权限申请”）：以前子代理的模式在派生时定下，回合中途切换之后正在跑的
/// 子代理照旧询问。模式每次从父会话现在的模式算；回合通道在回合结束时关闭，后台子代理还要问，所以经会话的请求通道。
struct SessionMode {
    requests: mpsc::WeakSender<crate::actor::Request>,
}

#[async_trait]
impl rust_cli_kernel_api::session_ops::LiveMode for SessionMode {
    /// actor 已经退出（通道没了、回复被丢下）时没有回答。只在发送时持有强引用，不让 actor 因此不退出。
    async fn mode(&self) -> Option<rust_cli_kernel_api::permission::PermissionMode> {
        let (reply, answer) = oneshot::channel();
        let requests = self.requests.upgrade()?;
        requests
            .send(crate::actor::Request::Mode { reply })
            .await
            .ok()?;
        drop(requests);
        answer.await.ok()
    }
}

/// 会话级的询问入口：询问经会话的请求通道交给 actor，登记为不属于回合的交互，只随会话关闭取消。actor 收不下时
/// （已经退出、正在关闭，或关闭时取消了这条询问）改走进程级的入口（agents.md §4.1.2）。
pub(crate) struct SessionAsks {
    requests: mpsc::WeakSender<crate::actor::Request>,
    session: SessionId,
    detached: Option<Arc<crate::detached::DetachedAsks>>,
}

impl SessionAsks {
    /// 交给会话 actor；actor 收不下（发不出去或回复被丢下）时为 None。
    async fn to_actor(
        &self,
        request: InteractionRequest,
    ) -> Option<Result<InteractionAnswer, OrchestrationError>> {
        let (reply, answer) = oneshot::channel();
        // 只在发送时持有强引用：等回答的这段时间不拖住会话的请求通道。
        let requests = self.requests.upgrade()?;
        requests
            .send(crate::actor::Request::Ask { request, reply })
            .await
            .ok()?;
        drop(requests);
        answer.await.ok()
    }
}

#[async_trait]
impl Interactions for SessionAsks {
    async fn ask(
        &self,
        request: InteractionRequest,
    ) -> Result<InteractionAnswer, OrchestrationError> {
        if let Some(answered) = self.to_actor(request.clone()).await {
            return answered;
        }
        // 修复（工作线 F72）：以前父会话关闭之后这里直接按会话已关闭失败，后台子代理按拒绝处理。
        match &self.detached {
            Some(detached) => detached.ask(&self.session, request).await,
            None => Err(OrchestrationError::Closed),
        }
    }

    async fn cancel_ask(&self, tool_use: &rust_cli_kernel_api::ids::ToolUseId) {
        // 会话已关闭时，关闭已经取消了全部待回答的交互；询问可能已转到进程级的入口。
        if let Some(requests) = self.requests.upgrade() {
            let request = crate::actor::Request::CancelAsk {
                tool_use: tool_use.clone(),
            };
            let _ = requests.send(request).await;
        }
        if let Some(detached) = &self.detached {
            detached.cancel(&self.session, tool_use);
        }
    }

    /// 先问会话 actor；不在（已退出，或询问已转到进程级的入口，agents.md §4.1.2）时问进程级的入口。正好在转的那一刻两边都没有：
    /// 已回答，询问方等用户的回答，不会有两个结论（hooks.md §6.1 的竞态表）。
    async fn answer_ask(
        &self,
        tool_use: &rust_cli_kernel_api::ids::ToolUseId,
        answer: Answer,
    ) -> bool {
        if let Some(requests) = self.requests.upgrade() {
            let (reply, settled) = oneshot::channel();
            let request = crate::actor::Request::AnswerAsk {
                tool_use: tool_use.clone(),
                answer: answer.clone(),
                reply,
            };
            let sent = requests.send(request).await.is_ok();
            drop(requests);
            if sent && settled.await.unwrap_or(false) {
                return true;
            }
        }
        match &self.detached {
            Some(detached) => detached.answer(&self.session, tool_use, answer),
            None => false,
        }
    }

    /// 交给会话 actor；会话已关闭（询问在进程级的入口）时不落（permission-updates.md D8），回错误。
    async fn apply_permission_updates(
        &self,
        updates: Vec<rust_cli_kernel_api::permission_updates::PermissionUpdate>,
    ) -> Result<(), OrchestrationError> {
        let requests = self.requests.upgrade().ok_or(OrchestrationError::Closed)?;
        let (reply, applied) = oneshot::channel();
        requests
            .send(crate::actor::Request::ApplyUpdates { updates, reply })
            .await
            .map_err(|_| OrchestrationError::Closed)?;
        drop(requests);
        applied
            .await
            .map_err(|_| OrchestrationError::Closed)?
            .map_err(|error| OrchestrationError::Failed {
                message: error.to_string(),
            })
    }

    fn lasting(&self) -> Option<Arc<dyn Interactions>> {
        Some(Arc::new(Self {
            requests: self.requests.clone(),
            session: self.session.clone(),
            detached: self.detached.clone(),
        }))
    }
}

/// 手动 `/compact` 的摘要请求的计量（usage.md §2.1.3）：同主循环没有消息的请求，一批只有计量；会话 actor 在压缩回合里把它记在
/// 压缩回合上。写不进去只记 warn，压缩照常（同自动压缩，§2.1.2）。
#[async_trait]
impl rust_cli_kernel_api::compaction::CompactMeter for ActorSink {
    async fn metered(&self, call: rust_cli_kernel_api::metering::ModelCall) {
        let metered = rust_cli_kernel_api::metering::Metered::Model(call);
        if let Err(reason) = self.commit_metered(None, metered).await {
            tracing::warn!(%reason, "the compaction usage was not stored");
        }
    }
}

#[async_trait]
impl RunSink for ActorSink {
    async fn commit_input(
        &self,
        message: Message,
        commands: Vec<rust_cli_kernel_api::ids::CommandId>,
    ) -> Result<(), StoreError> {
        self.call(|reply| SinkRequest::Message {
            message,
            commands,
            reply,
        })
        .await
    }

    async fn commit_message(&self, message: Message) -> Result<(), StoreError> {
        self.call(|reply| SinkRequest::Message {
            message,
            commands: Vec::new(),
            reply,
        })
        .await
    }

    async fn commit_messages(&self, messages: Vec<Message>) -> Result<(), StoreError> {
        self.call(|reply| SinkRequest::Messages { messages, reply })
            .await
    }

    async fn commit_metered(
        &self,
        message: Option<Message>,
        metered: rust_cli_kernel_api::metering::Metered,
    ) -> Result<(), StoreError> {
        self.call(|reply| SinkRequest::Metered {
            message,
            metered: Box::new(metered),
            reply,
        })
        .await
    }

    async fn compact_started(&self, trigger: CompactTrigger) {
        let _ = self.sender.send(SinkRequest::CompactStarted(trigger)).await;
    }

    async fn compact_failed(&self, trigger: CompactTrigger, message: String) {
        let _ = self
            .sender
            .send(SinkRequest::CompactFailed(trigger, message))
            .await;
    }

    async fn emit(&self, event: QueryEvent) -> bool {
        self.sender.send(SinkRequest::Event(event)).await.is_ok()
    }

    /// 插话（并入前先跑 UserPromptSubmit）与后台通知，按到达顺序（steering.rs，query.md §5）。
    async fn take_absorbed(&self) -> Vec<Absorbed> {
        self.absorbed().await
    }

    async fn goal_checked(
        &self,
        condition: String,
        verdict: rust_cli_kernel_api::hooks::StopVerdict,
    ) -> Result<Vec<Message>, StoreError> {
        self.call(|reply| SinkRequest::GoalChecked {
            condition,
            verdict,
            reply,
        })
        .await
    }
}

#[cfg(test)]
#[path = "sink_tests.rs"]
mod tests;
