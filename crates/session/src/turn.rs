//! 回合内的逻辑：开始回合、处理主循环的提交请求、待回答的交互、回合收尾。
//!
//! 每条消息先提交到存储，成功后才进入内存历史并发出事件；提交失败时中止本回合并以失败收尾。

use rust_cli_kernel_api::abort::AbortReason;
use rust_cli_kernel_api::command_output::ToolOutputHead;
use rust_cli_kernel_api::compaction::LoopCompactionEnd;
use rust_cli_kernel_api::hooks::HookOutcome;
use rust_cli_kernel_api::ids::{CommandId, InteractionId};
use rust_cli_kernel_api::input_extras::{InputRecord, TurnConfig};
use rust_cli_kernel_api::message::{ContentBlock, Message, UserMessage, UserOrigin};
use rust_cli_kernel_api::metering::{CallStatus, Metered, ModelCall, OutputSpeed};
use rust_cli_kernel_api::model::QuerySource;
use rust_cli_kernel_api::orchestration::{
    Answer, InteractionAnswer, InteractionRequest, OrchestrationError, QueryError, QueryEvent,
    QueryStop, RunView,
};
use rust_cli_kernel_api::session_api::{
    Ack, DropReason, InputEnd, InputFate, KernelEvent, QueueHold, TurnOrigin, TurnOutcome,
};
use rust_cli_kernel_api::store::{Fact, StoreError, TurnEnded, TurnRecord};
use tokio::sync::oneshot;

use crate::actor::{Actor, Pending, accepted, rejected};
use crate::lane::Waiting;
use crate::sink::SinkRequest;

/// 工具请求的改动没能落库：原因是存储的原文。
fn failed(error: StoreError) -> OrchestrationError {
    OrchestrationError::Failed {
        message: error.to_string(),
    }
}

/// footer 的 TPS（修复线 F105，docs/specs/rust-cli/tui-display.md §2.6）：只有完成的主回合请求才有；子代理、旁路与压缩
/// 请求、中断与失败的请求不更新。时刻都相对请求开始。
pub(crate) fn output_speed(call: &ModelCall) -> Option<OutputSpeed> {
    // 写法与子代理共用（kernel-api `OutputSpeed::of`，F112），这里只筛主回合。
    (call.source == QuerySource::MainThread)
        .then(|| OutputSpeed::of(call))
        .flatten()
}

/// 成批进同一个回合的一条后续输入（修复线 L-A，query.md §4.3）。
#[derive(Debug)]
pub(crate) struct MorePrompt {
    /// 它的消息 id：开回合时就定下，提交那条消息的批次带它的命令 id（效果记录，session-lifecycle.md §10.3）。
    pub(crate) id: rust_cli_kernel_api::ids::MessageId,
    pub(crate) command: CommandId,
    pub(crate) text: String,
    /// 在工具边界已跑过的 UserPromptSubmit 结果。
    pub(crate) screened: Option<Box<HookOutcome>>,
}

/// 开启一个回合的输入。
pub(crate) enum TurnInput {
    Human {
        command: CommandId,
        text: String,
        /// 插话在并入点已跑过的 UserPromptSubmit 结果（query.md §4.1）；有值时不再跑 hook。
        screened: Option<Box<HookOutcome>>,
        /// 与它成批进这个回合的后续排队输入（修复线 L-A，query.md §4.3），按队列顺序。
        more: Vec<MorePrompt>,
        /// 正文之外的事实（session-lifecycle.md §10.14）：不为空时 `more` 一定为空（这样的输入自己一轮）。装箱：它比别的开回合的
        /// 输入大得多（附件、上传、引用、页面状态各是一个列表）。
        extras: Box<rust_cli_kernel_api::input_extras::InputExtras>,
    },
    /// 回合结束后还没并入的后台通知（命令与子代理，到达顺序）：连续的几条合成一个回合，每条一条
    /// meta user 消息、只调一次模型（docs/specs/rust-cli/query.md §5，通知合并）。
    Notices(Vec<Waiting>),
    /// 到期的定时任务：prompt 原文作为 meta user 消息（docs/specs/rust-cli/tools-more.md §10.4）。
    Cron(rust_cli_kernel_api::cron::CronFire),
    /// 排队的 bash 模式命令（tui-input.md §2.2）。
    Bash { command: CommandId, line: String },
    /// 带内容块的人类输入（粘贴的图片，tui-input.md §3.2）。
    Content {
        command: CommandId,
        content: Vec<ContentBlock>,
    },
    /// v5 的压缩（`session.compact`，session-lifecycle.md §10.23）：开压缩的回合。`config` 只有这一次提交的模型（§10.30）。
    Compact {
        command: CommandId,
        config: TurnConfig,
    },
}

impl Actor {
    pub(crate) async fn start_turn(&mut self, input: TurnInput) -> Result<(), StoreError> {
        let meta = |source: &str| UserOrigin::Meta {
            source: source.to_owned(),
        };
        // 定时任务触发：界面行与 `/loop` 的折叠说明（排在 prompt 之前，tools-more.md §18.3）。
        let mut fired = None;
        let (texts, notices) = match input {
            // 人类输入先跑 UserPromptSubmit（hooks.rs），输入在回合任务里提交。
            TurnInput::Human {
                command,
                text,
                screened,
                more,
                extras,
            } => {
                // 这条输入的执行配置先写成会话的配置，这一轮按它跑（§10.25，照 v4 开跑时套用）；与开回合同一步，期间插不进别的命令。
                self.adopt(&extras.config).await?;
                // 只有给模型的材料、没有正文的输入：用户消息没有内容块（API 不收空的文字块，§10.14）。
                let content = if extras.gives_the_model_something()
                    && rust_cli_base::js::string::trim(&text).is_empty()
                {
                    Vec::new()
                } else {
                    vec![ContentBlock::Text { text }]
                };
                self.start_human_turn(command, content, screened, more, *extras);
                return Ok(());
            }
            TurnInput::Content { command, content } => {
                self.start_human_turn(command, content, None, Vec::new(), Default::default());
                return Ok(());
            }
            TurnInput::Bash { command, line } => {
                self.start_bash_turn(command, line);
                return Ok(());
            }
            TurnInput::Compact { command, config } => {
                // 压缩用这一次提交的模型（§10.30，照 v4）：先写成会话的模型，摘要请求按它发；与开回合同一步
                self.adopt(&config).await?;
                self.start_compaction(command, None);
                return Ok(());
            }
            TurnInput::Cron(fire) => {
                fired = Some(self.fire_prelude(fire.kind));
                // `/loop` 的哨兵在这里展开（tools-more.md §17.3）。
                let prompt = self.resolve_loop_prompt(fire.prompt).await;
                (vec![(prompt, meta(crate::cron::CRON_SOURCE))], Vec::new())
            }
            TurnInput::Notices(notices) => {
                // 同伴消息开回合时用空闲的写法与自己的来源（cross-session.md §1.7）。
                let texts = notices
                    .iter()
                    .map(|notice| (notice.turn_text(), meta(notice.source())))
                    .collect();
                (texts, notices)
            }
        };
        // 回合的来源（§10.8）：定时任务；全是同伴消息时是同伴，否则是后台结果。
        let origin = if fired.is_some() {
            TurnOrigin::Scheduled
        } else if notices
            .iter()
            .all(|notice| matches!(notice, Waiting::Peer(_)))
        {
            TurnOrigin::Peer
        } else {
            TurnOrigin::Notice
        };
        let before = fired
            .as_ref()
            .map(|fired| fired.before.clone())
            .unwrap_or_default();
        let users: Vec<Message> = before
            .into_iter()
            .chain(texts.into_iter().map(|(text, origin)| {
                Message::User(UserMessage {
                    id: self.deps.ids.message(),
                    content: vec![ContentBlock::Text { text }],
                    origin,
                })
            }))
            .collect();
        // 一个事务：几条通知一起成立；折叠说明与定时任务的 prompt 一起成立；同伴项“已并入”的事实与它们的 user 消息
        // 一起成立（cross-session.md §2.4，F139）。checkpoint 挂在这次运行最后一条 user 消息上（drive 的同一规则）。
        let prompt = users.last().map(|message| message.id().clone());
        let merged = notices.iter().flat_map(Waiting::merged_facts).collect();
        // 回合记录与这些消息同一批（session-lifecycle.md §10.1）；`TurnStarted` 照旧在提交之后发。提交失败时没有这个回合，
        // 记录也不能留给之后别的提交。
        let turn = self.deps.ids.turn();
        // 开这个回合的后台工作（§10.50）：前端据此说明这一轮是哪项后台工作的结果，不读通知的文字
        let works = notices
            .iter()
            .filter_map(|notice| notice.work(&self.works))
            .collect();
        let started = self.open_record(turn.clone(), None, origin, works);
        let appended = match self.append(None, users, merged).await {
            Ok(appended) => appended,
            Err(error) => {
                self.unrecorded = None;
                return Err(error);
            }
        };
        if let (Some(fired), Some(prompt)) = (fired, &prompt) {
            self.fired(fired, prompt.clone());
        }
        // 通知已经在对话里：现在才 ack（至少一次）。
        for notice in &notices {
            self.ack_notice(notice).await;
        }
        // 先开始回合，再发这些消息（与人类输入的回合同一个顺序）。
        self.log.append(started);
        self.attach_session_hooks(&turn, origin);
        self.after_commit(appended).await;
        let (opening, request) = self.open_main_turn(turn);
        let runner = self.deps.runner.clone();
        let sink = opening.sink.clone();
        let signal = opening.abort.clone();
        let task = self.deps.spawner.spawn("query-run", async move {
            runner.run(request, sink, signal).await
        });
        self.launch(opening, task, prompt, Vec::new());
        Ok(())
    }

    pub(crate) async fn on_sink(&mut self, request: SinkRequest) {
        match request {
            SinkRequest::Message {
                message,
                commands,
                reply,
            } => {
                // 输入的消息落库时，同一批里带上账本要的事实（持久账本：这些命令已提升，与消息同一个事务，
                // pg-store.md §4；不记账的账本：没有）。给什么由账本定，这里不判断。
                let facts = self
                    .deps
                    .journal
                    .promotion(self.journaled.retain(commands), message.id());
                let _ = reply.send(self.record(vec![message], facts).await);
            }
            SinkRequest::Event(event) => self.forward(event),
            SinkRequest::ArmMonitor {
                monitor,
                events,
                reply,
            } => {
                let _ = reply.send(self.arm_monitor(monitor, events));
            }
            SinkRequest::Ask { request, reply } => {
                self.ask(request, reply, crate::actor::AskScope::Turn);
            }
            SinkRequest::CancelAsk { tool_use } => self.cancel_ask(&tool_use),
            SinkRequest::AnswerAsk {
                tool_use,
                answer,
                reply,
            } => {
                let _ = reply.send(self.answer_ask(&tool_use, answer).await);
            }
            SinkRequest::ApplyUpdates { updates, reply } => {
                let _ = reply.send(self.apply_updates(None, &updates).await);
            }
            SinkRequest::ClaimSteering { reply } => {
                let _ = reply.send(self.claim_steering());
            }
            SinkRequest::SettleSteering {
                fates,
                notices,
                reply,
            } => {
                let _ = reply.send(self.settle_steering(fates, notices).await);
            }
            SinkRequest::Metered {
                message,
                metered,
                reply,
            } => {
                self.note_request(&metered);
                let call = match metered.as_ref() {
                    Metered::Model(call) => Some(call.clone()),
                    Metered::Tool(_) => None,
                };
                // 没有消息的计量（没有内容的失败请求、丢弃的响应）单独一批。
                let messages = message.into_iter().collect();
                let fact = self.usage_fact(*metered);
                let result = self.record(messages, vec![fact]).await;
                // 先落库，再记下并发出（只在内存，F105；模型请求的计量 §10.44）。
                if let (Ok(()), Some(call)) = (&result, call) {
                    if let Some(speed) = output_speed(&call) {
                        self.output_speed = Some(speed);
                        self.log.append(KernelEvent::OutputSpeed { speed });
                    }
                    self.log.append(KernelEvent::ModelCalled { call });
                }
                let _ = reply.send(result);
            }
            SinkRequest::Messages { messages, reply } => {
                let _ = reply.send(self.record(messages, Vec::new()).await);
            }
            SinkRequest::CompactStarted(trigger) => crate::compact::compact_started(self, trigger),
            SinkRequest::CompactFailed(trigger, message) => {
                self.log
                    .append(KernelEvent::CompactFailed { trigger, message });
                if let Some(turn) = self.running.as_ref().map(|running| running.turn.clone()) {
                    self.loop_compaction_ended(&turn, LoopCompactionEnd::Failed);
                }
            }
            SinkRequest::SetPlan { enabled, by, reply } => {
                let changed = self.change_plan(enabled, by).await;
                let _ = reply.send(changed.map_err(failed));
            }
            SinkRequest::View { reply } => {
                let _ = reply.send(self.run_view());
            }
            SinkRequest::AutoVerdict { verdict, reply } => {
                let (counts, limit) = self.auto_denials.record(verdict);
                self.auto_denials = counts;
                let _ = reply.send(limit);
            }
            SinkRequest::Title(title) => self.retitle(title).await,
            SinkRequest::SetWorkspace { root, reply } => {
                let _ = reply.send(self.change_workspace(root).await);
            }
            SinkRequest::Conversation { reply } => {
                let _ = reply.send(self.messages.clone());
            }
            SinkRequest::Checkpoints { reply } => {
                let _ = reply.send(self.turn_checkpoints());
            }
            SinkRequest::HooksRan {
                invocation,
                blocked,
            } => self.hooks_ran(invocation, blocked),
            SinkRequest::SubagentStarted { start, reply } => {
                self.subagent_started(start);
                let _ = reply.send(());
            }
            SinkRequest::GoalChecked {
                condition,
                verdict,
                reply,
            } => {
                let _ = reply.send(self.goal_checked(condition, verdict).await);
            }
        }
    }

    /// 会话现在的状态（[`SinkRequest::View`]）：执行状态（判决用的模式由它算）、计划模式的会话标志、工作目录、主模型、目标与
    /// 自动模式的拒绝计数，同一时刻的值。
    fn run_view(&self) -> RunView {
        RunView {
            plan_flags: self.plan_flags,
            selection: Some(self.selection.clone()),
            goal: self.goal.condition(),
            additional_directories: self.additional_directories.clone(),
            session_rules: self.session_rules.clone(),
            auto_denials: self.auto_denials,
            prompt: self.prompt.current(),
            ..RunView::new(self.execution, self.meta.workspace.clone())
        }
    }

    fn failure(&self) -> Option<StoreError> {
        self.running
            .as_ref()
            .and_then(|running| running.failure.clone())
    }

    /// 提交失败：记下原因并中止本回合，之后的提交一律拒绝。主循环拿到的是存储原样的错误。
    ///
    /// 工作线 F77：记一条 warn。以前原因只进回合的结局（界面上的 `storage error: …`），日志里没有一行。
    pub(crate) fn fail(&mut self, error: StoreError) -> StoreError {
        if let Some(running) = &mut self.running {
            running.abort.abort(AbortReason::User);
            running.failure.get_or_insert_with(|| error.clone());
        }
        tracing::warn!(event = "session.write.failed", session = %self.meta.id, reason = %error, "a turn write could not be stored; the turn stops");
        error
    }

    /// 主循环交来的消息（一条、一组或没有）与计量：一个事务（[`Self::append`]），成功后进历史（[`Self::after_commit`]）；
    /// 失败时中止本回合（[`Self::fail`]），之后的提交一律拒绝。批次带上本回合开头那条输入的命令（`command_of`）。
    ///
    /// 工作线 F130：以前一条消息、一组消息（压缩的结果、目标的判定）与只有计量各走一条路径，一组消息那条内联了失败处理、
    /// 少了 warn 日志。
    pub(crate) async fn record(
        &mut self,
        messages: Vec<Message>,
        usage: Vec<Fact>,
    ) -> Result<(), StoreError> {
        if let Some(error) = self.failure() {
            return Err(error);
        }
        let command = messages
            .iter()
            .find_map(|message| self.command_of(message.id()));
        // 输入的显示事实与它的用户消息同一批（session-lifecycle.md §10.14、§10.59）：有记录就一定有消息。
        let inputs: Vec<InputRecord> = self
            .running
            .as_mut()
            .map(|running| {
                let (taken, kept) = std::mem::take(&mut running.records)
                    .into_iter()
                    .partition(|record| messages.iter().any(|m| *m.id() == record.message));
                running.records = kept;
                taken
            })
            .unwrap_or_default();
        let mut tail = usage;
        tail.extend(inputs.iter().map(InputRecord::fact));
        match self.append(command, messages, tail).await {
            Ok(mut appended) => {
                appended.inputs = inputs;
                self.after_commit(appended).await;
                Ok(())
            }
            Err(error) => Err(self.fail(error)),
        }
    }

    /// 一次计量的事实。手动 `/compact` 的回合里（`Running.compaction`）摘要请求记在压缩回合上（usage.md §2.1.3，照 Node：手动压缩是
    /// 自己的一轮）；主循环里的自动、反应式压缩仍属于这一轮（§2.1.2）。
    ///
    /// 修 bug（工作线 HOOKS）：以前手动 `/compact` 的摘要请求不写用量行，用量统计漏掉它的 token（usage.md D3）。
    fn usage_fact(&self, metered: Metered) -> Fact {
        match (metered, self.running.as_ref()) {
            (Metered::Model(call), Some(running)) if running.compaction => Fact::CompactionCalled {
                call,
                turn: running.turn.clone(),
            },
            (metered, _) => Fact::from(metered),
        }
    }

    fn forward(&mut self, event: QueryEvent) {
        let Some(turn) = self.running.as_ref().map(|running| running.turn.clone()) else {
            return;
        };
        // 工具结果说明的后台工作的变化（session-lifecycle.md §10.28）与它派生的前台子代理的结局（§10.56）：排在这次的 ToolDone 之后
        let mut finished = None;
        let event = match event {
            QueryEvent::BlockStart {
                message,
                index,
                block,
            } => KernelEvent::StreamBlockStart {
                turn,
                response: u32::try_from(message).unwrap_or(u32::MAX),
                index,
                block,
            },
            QueryEvent::Delta {
                message,
                id,
                index,
                tool_use,
                delta,
            } => KernelEvent::StreamDelta {
                turn,
                response: u32::try_from(message).unwrap_or(u32::MAX),
                message: id,
                index,
                tool_use,
                delta,
            },
            QueryEvent::ToolStarted {
                tool_use,
                name,
                input,
                started_at,
            } => KernelEvent::ToolStarted {
                tool_use,
                name,
                input,
                started_at,
            },
            QueryEvent::ToolProgress { tool_use, progress } => {
                KernelEvent::ToolProgress { tool_use, progress }
            }
            QueryEvent::ToolFinished {
                tool_use,
                output,
                cancelled,
            } => {
                self.ledger.tool_finished(&tool_use, &output);
                finished = Some((
                    tool_use.clone(),
                    rust_cli_kernel_api::background::BackgroundChange::of(&output),
                    crate::subagent_rows::finished_outcome(&output, cancelled),
                ));
                // 修 bug（§10.38）：以前头部在结果提交之后才进视图，取消在回合结束时才进回合记录；视图取在 `ToolDone` 与它们之间时，
                // 由这个视图建的投影里结果行没有头部、取消的行读成「出错」。两样都在发 `ToolDone` 的这一次处理里进视图，交界不漏不重。
                self.output_heads
                    .extend(ToolOutputHead::of(&tool_use, &output));
                // ExitPlanMode 拿给用户批准的计划（§10.43）：同上，与 `ToolDone` 同一次处理进视图，由视图建的投影与之后的事件交界不漏不重。
                self.plans
                    .extend(rust_cli_kernel_api::plan::ShownPlan::of(&tool_use, &output));
                // 因停止而取消的调用（§10.17）：视图带着它（`SessionView.cancelled`）；回合结束时写进回合记录，冷读与活会话同样读成「已取消」。
                if cancelled && let Some(running) = self.running.as_mut() {
                    running.cancelled.push(tool_use.clone());
                }
                KernelEvent::ToolDone {
                    tool_use,
                    output,
                    cancelled,
                }
            }
            // 模型通知只供显示，不落库（stall-visibility.md §1）。
            QueryEvent::ModelNotice { notice } => KernelEvent::ModelNotice {
                notice,
                at: self.deps.clock.now_ms(),
            },
            // 主循环的提示（Stop hook 到了上限，query.md §3）：只供显示，不落库。
            QueryEvent::Notice { level, text, kind } => {
                KernelEvent::SystemNotice { level, text, kind }
            }
            // 执行覆盖退回了（§10.48）：先记进回合记录，再发事件
            QueryEvent::ExecutionFellBack { reason } => self.fell_back(turn, reason),
        };
        self.log.append(event);
        if let Some((tool_use, change, outcome)) = finished {
            if let Some(change) = change {
                self.tool_changed_work(&tool_use, change);
            }
            // 停放的子代理已经随它的 Launched 转了后台（works.rs），这里只结束还在前台的
            self.subagent_call_finished(&tool_use, outcome);
        }
    }

    pub(crate) async fn finish_turn(&mut self) {
        let Some(running) = self.running.take() else {
            return;
        };
        self.works.turn_ended();
        // 没有消息落库就结束的回合没有记录（session-lifecycle.md §10.1）。
        self.unrecorded = None;
        self.cancel_turn_asks();
        // 回合任务的结局与这个回合的文件改动（§10.42：在回合任务里读 artifact 算好，这里只取）。
        let (result, files) = match running.task.await {
            Ok(run) => (run.stop, run.files),
            Err(error) => (Err(error), Vec::new()),
        };
        // 回合结束的时刻与用时只在这里算一次（session-lifecycle.md §10.36）：回合记录、`TurnFinished` 与回合结束那一行用同一份，
        // 冷读与活会话是同一个值。以前事件与记录都不带它，回合头的结束与耗时在冷读里给不出来。
        let time = running.timing.ended(self.deps.clock.now_ms());
        // UserPromptSubmit 拦下了开回合的输入：它不会变成消息，账本里记取消（pg-store.md §4）。开回合的输入是这个回合的
        // 第一条输入（`running.inputs` 的第一项）。
        if matches!(&result, Ok(Ok(QueryStop::Blocked { .. })))
            && let Some((_, command)) = running.inputs.first()
            && let Err(error) = self.cancel_durable_input(command).await
        {
            let reason = self.store_failed(error);
            tracing::warn!(%reason, "blocked input cancellation was not confirmed");
        }
        let outcome = match (running.failure, result) {
            (Some(error), _) => TurnOutcome::Failed {
                message: error.to_string(),
                cause: None,
            },
            (None, Ok(Ok(QueryStop::Completed))) => TurnOutcome::Success,
            (None, Ok(Ok(QueryStop::Aborted))) => TurnOutcome::Interrupted,
            (None, Ok(Ok(QueryStop::HookStopped))) => TurnOutcome::HookStopped,
            (None, Ok(Ok(QueryStop::StopHookPrevented))) => TurnOutcome::StopHookPrevented,
            // 修改（SMALL-7，§10.51）：UserPromptSubmit 拦下了开回合的输入（这条输入没有落库，§10.7）。以前记成 `Failed`，v5 的投影再按
            // `HooksRan.blocked` 读成成功，同一件事两个事实；现在这里是唯一的写者，各前端只读这个结局。
            (None, Ok(Ok(QueryStop::Blocked { message }))) => {
                TurnOutcome::PromptBlocked { message }
            }
            (None, Ok(Ok(QueryStop::MaxTurns))) => TurnOutcome::Failed {
                message: "reached the maximum number of turns".to_owned(),
                cause: None,
            },
            (None, Ok(Ok(QueryStop::ApiError { message }))) => TurnOutcome::Failed {
                message,
                cause: None,
            },
            // 模型请求失败（§10.9，K-S6）：带上中性的模型错误，前端按类别分支（以前只有文字）。
            (None, Ok(Err(QueryError::Model(error)))) => TurnOutcome::Failed {
                message: error.to_string(),
                cause: Some(error),
            },
            (None, Ok(Err(error))) => TurnOutcome::Failed {
                message: error.to_string(),
                cause: None,
            },
            (None, Err(error)) => TurnOutcome::Failed {
                message: error.to_string(),
                cause: None,
            },
        };
        // 这个回合里每条输入的结局，也是回合记录里的结局（§10.7、§10.17）。
        // 被拦下的输入是一次成功结束的回合（照 v4）；这一轮没有消息落库，没有回合记录与用量行，这个值不写出（§10.51）。
        let end = match &outcome {
            TurnOutcome::Success
            | TurnOutcome::HookStopped
            | TurnOutcome::StopHookPrevented
            | TurnOutcome::PromptBlocked { .. } => InputEnd::Completed,
            TurnOutcome::Interrupted => InputEnd::Cancelled,
            TurnOutcome::Failed { .. } => InputEnd::Failed,
        };
        // Node `turn_usage`（usage.md §2.4）：只在这次运行的输入已经落库时写；压缩运行没有输入。用量行是观测，
        // 写不进去只记日志，不改回合的结果。
        let prompted = running
            .prompt
            .as_ref()
            .is_some_and(|prompt| self.messages.iter().rev().any(|m| m.id() == prompt));
        let mut facts = Vec::new();
        let status = match end {
            InputEnd::Completed => CallStatus::Completed,
            InputEnd::Cancelled => CallStatus::Cancelled,
            InputEnd::Failed => CallStatus::Error,
        };
        if prompted {
            facts.push(Fact::TurnEnded { status });
        } else if running.compaction {
            // 手动 `/compact` 是自己的一轮（usage.md §2.1.3，照 Node）：压缩回合一行 `turn_usage`。
            facts.push(Fact::CompactionEnded {
                turn: running.turn.clone(),
                started_at_ms: running.timing.started_ms(),
                status,
            });
        }
        // 回合结束时主循环的压缩还在跑（§10.52）：被中断写取消，别的结局写失败；在改写回合记录与 TurnFinished 之前，记录与事件都带上它。
        let unfinished = match end {
            InputEnd::Cancelled => LoopCompactionEnd::Cancelled,
            InputEnd::Completed | InputEnd::Failed => LoopCompactionEnd::Failed,
        };
        self.loop_compaction_ended(&running.turn, unfinished);
        // 修 bug（RUST-SESSION 记的，§10.17）：以前回合记录只在第一条消息时写，没有结局，冷读的回合头一律「完成」、停止时
        // 取消的工具读成「出错」。回合结束时同一条记录再写一次，带上结局与取消的工具（与用量行同一批）；没有记录的回合
        // （没有消息落库）不写。
        let ended = self
            .turns
            .iter()
            .position(|record| record.turn == running.turn)
            .map(|at| {
                let record = TurnRecord {
                    ended: Some(TurnEnded {
                        outcome: end,
                        cancelled: running.cancelled.clone(),
                        time: Some(time),
                        files: files.clone(),
                    }),
                    ..self.turns[at].clone()
                };
                facts.push(record.fact());
                (at, record)
            });
        if !facts.is_empty() {
            match self.commit(None, facts).await {
                Ok(()) => {
                    if let Some((at, record)) = ended {
                        self.turns[at] = record;
                    }
                }
                Err(reason) => tracing::warn!(%reason, "the turn usage could not be stored"),
            }
        }
        let usage = self
            .messages
            .iter()
            .rev()
            .find_map(|message| match message {
                Message::Assistant(assistant) => assistant.usage,
                _ => None,
            });
        // 停止或失败之后扣住排队的输入（session-lifecycle.md §10.6，开关开时）：与回合结束在同一步里，之后到达的命令看到的
        // 已经是扣住之后的状态。
        // 被「现在发」抢占的回合（`AbortReason::Submit`，§10.27）不是用户停止：照 Node 的 preserveQueueAutoDrainOnCancel，队列不扣住，
        // 抢占的那一条开完之后排着的输入接着开。只有 `SubmitNow` 用这个原因，TUI 与 `-p` 的停止照旧扣住。
        let preempted =
            running.abort.reason() == Some(rust_cli_kernel_api::abort::AbortReason::Submit);
        let hold = match &outcome {
            TurnOutcome::Interrupted if preempted => None,
            TurnOutcome::Interrupted => Some(QueueHold::Stopped),
            TurnOutcome::Failed { .. } => Some(QueueHold::TurnFailed),
            // UserPromptSubmit 拦下开回合的输入不是失败：照 v4（成功结束的回合）不扣住排队的输入（§10.51）
            TurnOutcome::Success
            | TurnOutcome::HookStopped
            | TurnOutcome::StopHookPrevented
            | TurnOutcome::PromptBlocked { .. } => None,
        }
        .filter(|_| self.deps.hold_queue && self.queue_hold.is_none() && self.inbox.has_inputs());
        // 这个回合里每条输入的结局（§10.7）：消息落了库的随回合结束，没落库的是被拦下（或回合在提交它之前就被中断）。
        for (message, input) in &running.inputs {
            let fate = if self.messages.iter().any(|m| m.id() == message) {
                InputFate::Ended {
                    turn: running.turn.clone(),
                    end,
                }
            } else if matches!(outcome, TurnOutcome::PromptBlocked { .. }) {
                InputFate::Dropped {
                    why: DropReason::Blocked,
                }
            } else {
                InputFate::Ended {
                    turn: running.turn.clone(),
                    end: InputEnd::Cancelled,
                }
            };
            self.input_fate(input.clone(), fate);
        }
        // 回合结束那一行（修复线 F118，tui-display.md §2.7）：排在 TurnFinished 之后、下一个回合之前。
        let interrupted = matches!(outcome, TurnOutcome::Interrupted);
        let turn = running.turn.clone();
        self.log.append(KernelEvent::TurnFinished {
            turn: running.turn,
            outcome,
            usage,
            time,
            files,
        });
        self.turn_duration(turn, running.timing, time.ended_at, interrupted)
            .await;
        if hold.is_some() {
            self.hold_queue(hold);
        }
        self.start_next().await;
    }

    /// 扣住或放开排队的输入（§10.6）：唯一的写入处，同时发 `QueueHeld`。
    pub(crate) fn hold_queue(&mut self, hold: Option<QueueHold>) {
        self.queue_hold = hold;
        self.log.append(KernelEvent::QueueHeld { hold });
    }

    /// `SetAutoDrain`（§10.6）：`true` 放开扣住的队列，空闲就开下一条；`false` 扣住（已经扣住时保留原来的原因）。
    pub(crate) async fn set_auto_drain(&mut self, enabled: bool) {
        match (enabled, self.queue_hold) {
            (true, Some(_)) => {
                self.hold_queue(None);
                if !self.busy() {
                    self.start_next().await;
                }
            }
            (false, None) => self.hold_queue(Some(QueueHold::Requested)),
            (true, None) | (false, Some(_)) => {}
        }
    }

    /// 空闲时开下一个回合：先 next 通道（插话与后台通知，到达顺序），再 `Queue` 输入（优先级 `later`），
    /// 最后是到期的定时任务（lane.rs）。
    pub(crate) async fn start_next(&mut self) {
        // 关闭中（R1-1）：排队的输入、通知与定时任务都不再开回合；通知没有 ack，会话再打开时送达。进程要退出
        // （`Shutdown`，修复线 F38）时同样。
        if self.closing.is_some()
            || self.shutting_down
            || self.resume_queue_paused
            || self.write_blocked.load(std::sync::atomic::Ordering::SeqCst)
        {
            return;
        }
        // 这次工作期间到达的本地命令记录先进对话，再开下一个回合（修复线 L-A，tui-manage.md §11.3）。
        self.flush_held_records().await;
        // 换掉一条输入（§10.53）：它中止的回合刚结束，先回退再开新输入的回合，扣住与否都不挡它（与提升的输入只有一条）。
        if let Some(replacement) = self.replacing.take() {
            self.replace_now(*replacement).await;
            return;
        }
        // 提升的输入（§10.13）先开：它中止的回合刚结束，扣住与否都不挡它。
        let input = if let Some(input) = self.promoted.take() {
            input
        } else if let Some(input) = self.next_turn_input() {
            input
        } else if let Some(fire) = self.next_cron().await {
            TurnInput::Cron(fire)
        } else {
            return;
        };
        // 提交失败时只记日志：排队输入已出队，前端从 QueueChanged 看到它消失；通知没有 ack，
        // 会再次送达。
        if let Err(reason) = self.start_turn(input).await {
            tracing::warn!(%reason, "queued input could not start a turn");
        }
    }
}

#[path = "turn_interact.rs"]
mod interact;

#[cfg(test)]
#[path = "write_failure_tests.rs"]
mod write_failure_tests;
