//! 一次提问的事件循环（docs/specs/rust-cli/headless.md §3.1）：提交输入，按内核事件写 stream-json 的事件，
//! 每个回合结束写一个 result；`-p` 等后台命令与通知开的回合处理完才结束（query.md §5）。

use std::sync::Arc;

use anyhow::{Result, bail};
use futures_util::StreamExt;
use rust_cli_kernel_api::catalog::CatalogQuery;
use rust_cli_kernel_api::clock::Ids;
use rust_cli_kernel_api::message::{ContentBlock, Message, StopReason, Usage, UserMessage};
use rust_cli_kernel_api::orchestration::{Answer, PermissionAnswer};
use rust_cli_kernel_api::panels::SubagentStatus;
use rust_cli_kernel_api::permission::ExecutionState;
use rust_cli_kernel_api::session_api::{
    Ack, Command, CommandEnvelope, Delivery, Kernel, KernelEvent, SessionHandle, TurnOutcome,
};
use rust_cli_kernel_api::session_follow::follow;
use rust_cli_kernel_api::user_input::is_interrupt;
use tokio::io::AsyncWrite;
use tokio_util::sync::CancellationToken;

use crate::events::{self, Finish, Init, ResultKind, TurnResult};
use crate::partial::{Partial, wrap};
use crate::printer::Printer;

/// 提问怎样结束。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Asked {
    /// 回合（与它等的后台工作）结束，result 已写。
    Done,
    /// 收到退出信号：在跑的回合结束后返回（json / stream-json 已写这个回合的 result）。
    Stopped,
    /// 回合以错误结束（result 已写，`is_error` 为真）；带着 text 输出写到 stderr 的那句。
    Failed(String),
}

/// 一个回合里累计的事实（result 的 `num_turns`、`usage`、`result` 等，headless.md §3.6）。
struct Turn {
    started_ms: u64,
    first_output_ms: Option<u64>,
    /// 开回合的那条 user 消息已经过去（不回显它）。
    input_seen: bool,
    /// 这个回合已有模型的回复：之前提交的 user 消息是输入的展开（skill 正文、本地命令的输出等），不回显。
    answered: bool,
    user_events: u64,
    usage: Usage,
    stop_reason: Option<StopReason>,
    /// 最后一条 assistant 的最后一个块是文字时是它；之后有 user 消息时清空。
    result_text: String,
    /// `max_turns_reached` 的 `turnCount` 与 `maxTurns`。
    max_turns: Option<(u64, u64)>,
    /// 中断标记是工具使用中的那一句：中止在工具执行之后（result 的 `terminal_reason` 是 `aborted_tools`）。
    interrupted_after_tools: bool,
}

impl Turn {
    fn new(now_ms: u64) -> Self {
        Self {
            started_ms: now_ms,
            first_output_ms: None,
            input_seen: false,
            answered: false,
            user_events: 0,
            usage: Usage::default(),
            stop_reason: None,
            result_text: String::new(),
            max_turns: None,
            interrupted_after_tools: false,
        }
    }
}

/// 不是内置命令的本地结果在 result 的 `local_command` 里的名字（headless.md §3.6）。
const CUSTOM_COMMAND: &str = "custom";

/// 本地命令的输出（`<local-command-stdout>…</local-command-stdout>`）。
fn local_output(user: &UserMessage) -> Option<&str> {
    let [ContentBlock::Text { text }] = user.content.as_slice() else {
        return None;
    };
    text.strip_prefix("<local-command-stdout>")?
        .strip_suffix("</local-command-stdout>")
}

/// 一个会话的提问：会话、内核与输出。
pub(crate) struct Driver<'p, 'w, W> {
    pub(crate) kernel: Arc<dyn Kernel>,
    pub(crate) session: Arc<dyn SessionHandle>,
    pub(crate) printer: &'p mut Printer<'w, W>,
    pub(crate) stop: CancellationToken,
    /// 退出信号是用户中断（SIGINT）时先于 `stop` 取消（修复线 F38，[`crate::PrintOptions::cancel`]）。
    pub(crate) cancel: CancellationToken,
    /// 会话命令的 id（[`crate::PrintOptions::ids`]）。
    pub(crate) ids: Arc<dyn Ids>,
    /// 输入已经关了（单次 `-p`；stream-json 输入读到结束之前为假）：这时还有在跑的后台子代理，回合的 result 推迟写。
    pub(crate) input_closed: bool,
}

impl<W: AsyncWrite + Unpin> Driver<'_, '_, W> {
    /// 写 system/init（每个回合开始前一次，headless.md §3.1）。
    async fn init(&mut self) -> Result<()> {
        if !self.printer.format().streams() && !self.printer.format().collects() {
            return Ok(());
        }
        let view = self.session.view().await?;
        let session_id = self.session.id().as_str().to_owned();
        let catalog = self
            .kernel
            .session_catalog(CatalogQuery {
                session: self.session.id().clone(),
                workspace: view.meta.workspace.clone(),
                selection: view.selection.clone(),
            })
            .await
            .unwrap_or_default();
        let servers = self.kernel.mcp_servers().await;
        // 权限模式的名字；plan 开时是 `plan`（F20 的独立开关）。
        let mode = ExecutionState::from_parts(view.mode, view.plan).effective();
        let uuid = self.printer.uuid();
        let output = &self.printer.output;
        let event = events::init(
            &Init {
                cwd: view.meta.workspace.root.as_str(),
                session_id: &session_id,
                catalog: &catalog,
                mcp_servers: &servers,
                model: &view.selection.model,
                permission_mode: mode.cli_name(),
                api_key_source: &output.api_key_source,
                version: &output.version,
            },
            &uuid,
        );
        self.printer.emit(event).await?;
        Ok(())
    }

    /// 提交 `prompt` 并跟到结束。
    pub(crate) async fn ask(&mut self, prompt: String) -> Result<Asked> {
        // 提问之前就收到退出信号：不再开回合。
        if self.stop.is_cancelled() {
            return Ok(Asked::Stopped);
        }
        let session = self.session.clone();
        let session_id = session.id().clone();
        // 续写时会话里已有消息与事件：只订阅这之后的事件（缺陷修复：原来从 0 重放，会话已打开时会读到上一次提问的
        // TurnFinished，提前收尾）。
        let view = session.view().await?;
        let mut model = view.selection.model.clone();
        // 缺陷修复（D2）：原来流结束（读端慢、订阅方落后）就当会话关了、报错退出；现在经 `follow` 按最后的 seq 重新订阅，
        // 与 TUI、App 协议相同（session-lifecycle.md §2.1）。
        let mut events = follow(session.clone(), view.last_seq);
        let ack = session
            .send(CommandEnvelope {
                id: self.ids.command(),
                command: Command::Submit {
                    text: prompt,
                    extras: Default::default(),
                    delivery: Delivery::Queue,
                    target: None,
                },
            })
            .await;
        if let Ack::Rejected { reason, .. } | Ack::OutcomeUnknown { reason } = ack {
            bail!("the prompt was rejected: {reason}");
        }
        let mut turn: Option<Turn> = None;
        let mut partial = Partial::default();
        let mut stopping = false;
        loop {
            let event = tokio::select! {
                event = events.next() => event,
                // 缺陷修复（R5-2）：原来没有退出信号的处理，进程按信号的默认动作结束，Bash 的子进程留成孤儿。
                // SIGINT 先中止在跑的 query，等它结束后收尾（session-lifecycle.md §3.5）。
                // 是否在跑问 actor（唯一事实源）：它在处理完中断之后回复，回复是 running 时这个回合的
                // TurnFinished 一定在之后发出，订阅已经开着，不会漏。
                // 修复线 F38：只有 SIGINT 以用户中断中止；SIGTERM / SIGHUP 时不中止 query、直接退出，
                // 这里以 `Shutdown` 中止（只为回收工具），之后这个回合不再记录、也不写 result。
                () = self.stop.cancelled(), if !stopping => {
                    stopping = true;
                    let id = self.ids.command();
                    let command = if self.cancel.is_cancelled() {
                        Command::Interrupt { target: None }
                    } else {
                        Command::Shutdown
                    };
                    let _ = session.send(CommandEnvelope { id, command }).await;
                    if session.view().await?.running.is_none() {
                        return Ok(Asked::Stopped);
                    }
                    continue;
                }
            };
            let Some(event) = event else { break };
            match event.event {
                KernelEvent::TurnStarted { .. } => {
                    self.init().await?;
                    turn = Some(Turn::new(self.printer.now_ms()));
                }
                KernelEvent::ModelChanged { selection } => model = selection.model,
                // tool_use 块一开始就写 `content_block_start`（工作线 F147，headless.md §3.7）。
                KernelEvent::StreamBlockStart { index, block, .. }
                    if self.printer.format().partial =>
                {
                    for item in partial.start(&model, index, &block) {
                        let uuid = self.printer.uuid();
                        self.printer
                            .emit(wrap(item, session_id.as_str(), &uuid))
                            .await?;
                    }
                }
                KernelEvent::StreamDelta { index, delta, .. } => {
                    if let Some(turn) = turn.as_mut() {
                        turn.first_output_ms.get_or_insert(self.printer.now_ms());
                    }
                    if self.printer.format().partial {
                        for item in partial.delta(&model, index, &delta) {
                            let uuid = self.printer.uuid();
                            self.printer
                                .emit(wrap(item, session_id.as_str(), &uuid))
                                .await?;
                        }
                    }
                }
                KernelEvent::MessageDone { message } => {
                    self.message(&mut turn, &mut partial, &model, message)
                        .await?;
                }
                // 没有人能回答（print 模式）：权限询问的工具结果是询问本身的文本
                // （docs/specs/rust-cli/shell-permissions.md §4）；提问按不回答，计划审批按继续计划（工作线 F134 以前
                // 一律答权限的 Unavailable，工具读不出，同样按没有回答处理）。
                KernelEvent::InteractionAsked {
                    interaction,
                    request,
                } => {
                    let answer = Answer::nobody(&request, PermissionAnswer::Unavailable);
                    let command = Command::ResolveInteraction {
                        interaction,
                        answer,
                    };
                    let ack = session
                        .send(CommandEnvelope {
                            id: self.ids.command(),
                            command,
                        })
                        .await;
                    // 修 bug（工作线 HOOKS-2，hooks.md §6.2）：PermissionRequest hook 与询问竞速（Q18），hook 先替答时这条自动回答
                    // 晚到，owner 回 `NotFound`（已回答）。以前当成错误，`-p` 直接退出；询问已经没有了，照常往下读。
                    match ack {
                        Ack::Rejected {
                            refusal: rust_cli_kernel_api::admission::Refusal::NotFound,
                            ..
                        } => {}
                        Ack::Rejected { reason, .. } | Ack::OutcomeUnknown { reason } => {
                            bail!("the interaction could not be answered: {reason}");
                        }
                        Ack::Accepted { .. } | Ack::Duplicate { .. } => {}
                    }
                }
                // SIGTERM / SIGHUP 之后（F38 的 `Shutdown`）：直接退出，不写这个回合的 result。
                KernelEvent::TurnFinished { .. } if stopping && !self.cancel.is_cancelled() => {
                    return Ok(Asked::Stopped);
                }
                KernelEvent::TurnFinished { outcome, .. } => {
                    let finished = turn
                        .take()
                        .unwrap_or_else(|| Turn::new(self.printer.now_ms()));
                    let failure = self.result(finished, &outcome, stopping).await?;
                    // 退出信号（SIGINT）之后：写完被中断的回合的 result 就返回，不等后台命令。
                    if stopping {
                        return Ok(Asked::Stopped);
                    }
                    if let Some(message) = failure {
                        return Ok(Asked::Failed(message));
                    }
                    // `-p` 等后台命令结束、通知处理完才收尾（docs/specs/rust-cli/query.md §5）。
                    // 三项都由会话 actor 在同一处给出；通知会开新回合，到时再看一次。
                    let view = session.view().await?;
                    if view.running.is_some()
                        || !view.queue.is_empty()
                        || !view.background.is_empty()
                    {
                        continue;
                    }
                    return Ok(Asked::Done);
                }
                _ => {}
            }
        }
        bail!("the session closed before the turn finished")
    }

    /// 一条已落库的消息：assistant 按块写事件，user（开回合的那条除外）写一个事件。
    async fn message(
        &mut self,
        turn: &mut Option<Turn>,
        partial: &mut Partial,
        model: &str,
        message: Message,
    ) -> Result<()> {
        let now = self.printer.now_ms();
        let session_id = self.session.id().as_str().to_owned();
        match message {
            Message::Assistant(assistant) => {
                if let Some(turn) = turn.as_mut() {
                    turn.answered = true;
                    turn.first_output_ms.get_or_insert(now);
                    if let Some(usage) = assistant.usage {
                        turn.usage += usage;
                    }
                    turn.stop_reason = assistant.stop_reason;
                    turn.result_text = match assistant.content.last() {
                        Some(ContentBlock::Text { text }) => text.clone(),
                        _ => String::new(),
                    };
                }
                // `modelUsage` 按请求的模型记（键是请求用的模型，不是响应里的 `model`）。
                if let Some(usage) = assistant.usage {
                    *self
                        .printer
                        .model_usage
                        .entry(model.to_owned())
                        .or_default() += usage;
                }
                let (before, after) = if self.printer.format().partial {
                    partial.finish(&assistant)
                } else {
                    Default::default()
                };
                for item in before {
                    let uuid = self.printer.uuid();
                    self.printer.emit(wrap(item, &session_id, &uuid)).await?;
                }
                let printer = &*self.printer;
                for event in events::assistant_events(&assistant, &session_id, || printer.stamp()) {
                    self.printer.emit(event).await?;
                }
                for item in after {
                    let uuid = self.printer.uuid();
                    self.printer.emit(wrap(item, &session_id, &uuid)).await?;
                }
            }
            Message::User(user) => {
                let Some(turn) = turn.as_mut() else {
                    return Ok(());
                };
                // 开回合的输入（人类输入、通知等）不回显（headless.md §3.5）。
                if !turn.input_seen {
                    turn.input_seen = true;
                    return Ok(());
                }
                // 模型回复之前提交的消息是输入的展开（skill 正文、`/goal` 的说明等），不回显；本地命令的
                // 输出（`<local-command-stdout>`）写成一条合成的 assistant。只有中断标记照写。
                // 修复：以前人类来源的都照写，`user-invocable: false` 的 skill 跟在 caveat 后面的命令输入也回显了。
                if !turn.answered && !is_interrupt(&user) {
                    if let Some(text) = local_output(&user) {
                        let event =
                            events::synthetic_assistant(text, &session_id, self.printer.stamp());
                        self.printer.emit(event).await?;
                    }
                    return Ok(());
                }
                turn.user_events += 1;
                turn.result_text.clear();
                // 修 bug（工作线 HOOKS-2，hooks.md §6.2）：以前被中断的回合一律按读流时被中断写 result；PermissionRequest hook 的
                // `interrupt` 在工具执行之后中止，result 应是 `aborted_tools`。主循环写哪一句中断标记就是哪一种。
                turn.interrupted_after_tools = matches!(
                    user.content.as_slice(),
                    [ContentBlock::Text { text }] if text == rust_cli_kernel_api::user_input::INTERRUPT_TEXTS[1]
                );
                let event = events::user_event(&user, &session_id, self.printer.stamp());
                self.printer.emit(event).await?;
            }
            Message::Attachment(attachment) if attachment.source == "max_turns_reached" => {
                if let Some(turn) = turn.as_mut() {
                    let field = |name: &str| {
                        attachment
                            .data
                            .get(name)
                            .and_then(serde_json::Value::as_u64)
                    };
                    if let (Some(count), Some(max)) = (field("turnCount"), field("maxTurns")) {
                        turn.max_turns = Some((count, max));
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// 回合结束：写 result（headless.md §3.6）。回合失败时返回失败的文字（text 输出写到 stderr 的那句）。
    async fn result(
        &mut self,
        turn: Turn,
        outcome: &TurnOutcome,
        stopping: bool,
    ) -> Result<Option<String>> {
        // 没调模型就结束的回合（用户调用 `user-invocable: false` 的 skill，只提交本地消息）照本地命令写 result。
        if matches!(outcome, TurnOutcome::Success) && !turn.answered {
            let session_id = self.session.id().as_str().to_owned();
            let value = events::local_result(events::LocalResult {
                session_id: &session_id,
                duration_api_ms: 0,
                model_usage: &self.printer.model_usage,
                result: "",
                command: CUSTOM_COMMAND,
                duration_ms: self.printer.now_ms().saturating_sub(turn.started_ms),
                uuid: self.printer.uuid(),
                result_index: self.printer.result_index(),
            });
            self.printer.result(value, "\n".to_owned(), false).await?;
            return Ok(None);
        }
        let (kind, failure) = match outcome {
            // 工具的 hook 要求停止、Stop hook 阻止继续也是 `success`，只是 `terminal_reason` 不同（query.md §3，工作线
            // F131、F134）。
            TurnOutcome::Success | TurnOutcome::HookStopped | TurnOutcome::StopHookPrevented => (
                ResultKind::Success {
                    is_error: false,
                    result: turn.result_text.clone(),
                    ttft_ms: turn
                        .first_output_ms
                        .map(|first| first.saturating_sub(turn.started_ms)),
                    finish: match outcome {
                        TurnOutcome::HookStopped => Finish::HookStopped,
                        TurnOutcome::StopHookPrevented => Finish::StopHookPrevented,
                        TurnOutcome::Success
                        | TurnOutcome::Interrupted
                        | TurnOutcome::PromptBlocked { .. }
                        | TurnOutcome::Failed { .. } => Finish::Completed,
                    },
                },
                None,
            ),
            TurnOutcome::Interrupted => (
                ResultKind::Interrupted {
                    after_tools: turn.interrupted_after_tools,
                },
                (!stopping).then(|| "the turn was interrupted".to_owned()),
            ),
            // UserPromptSubmit 拦下了开回合的输入：照回合失败写 result（headless.md §3.6）。
            TurnOutcome::Failed { message, .. } | TurnOutcome::PromptBlocked { message } => {
                match turn.max_turns {
                    Some((_, max)) => (ResultKind::MaxTurns { max }, Some(message.clone())),
                    // 阻塞线的错误文字只由发请求前的拦截产生（服务端的上下文超限带着服务端的原文）。
                    None if is_blocking_limit(message) => (
                        ResultKind::Blocked {
                            result: message.clone(),
                        },
                        Some(message.clone()),
                    ),
                    None => (
                        ResultKind::Success {
                            is_error: true,
                            result: message.clone(),
                            ttft_ms: None,
                            finish: Finish::Completed,
                        },
                        Some(message.clone()),
                    ),
                }
            }
        };
        let num_turns = match (&kind, turn.max_turns) {
            (ResultKind::MaxTurns { .. }, Some((count, _))) => count,
            _ => 1 + turn.user_events,
        };
        let text = match &kind {
            ResultKind::Success { result, .. } | ResultKind::Blocked { result }
                if result.ends_with('\n') =>
            {
                result.clone()
            }
            ResultKind::Success { result, .. } | ResultKind::Blocked { result } => {
                format!("{result}\n")
            }
            ResultKind::MaxTurns { max } => format!("Error: Reached max turns ({max})"),
            ResultKind::Interrupted { .. } => "Execution error".to_owned(),
        };
        let duration_api_ms = self
            .kernel
            .queries()
            .metering(self.session.id())
            .await
            .map_or(0, |metering| metering.api_duration_ms);
        let is_error = kind.is_error();
        let session_id = self.session.id().as_str().to_owned();
        // 输入已关、还有在跑的后台子代理：推迟写（headless.md §3.1）。
        let defer = self.input_closed
            && self.session.subagents().await.is_ok_and(|agents| {
                agents
                    .iter()
                    .any(|agent| agent.background && agent.status == SubagentStatus::Running)
            });
        // 阻塞线拦下的回合最后是一条合成的错误消息（停在 stop_sequence），不是上一条回复。
        let stop_reason = match kind {
            ResultKind::Blocked { .. } => {
                Some(rust_cli_kernel_api::message::StopReason::StopSequence)
            }
            _ => turn.stop_reason,
        };
        let value = events::turn_result(TurnResult {
            duration_api_ms,
            stop_reason,
            session_id: &session_id,
            usage: turn.usage,
            model_usage: &self.printer.model_usage,
            num_turns,
            kind,
            duration_ms: self.printer.now_ms().saturating_sub(turn.started_ms),
            uuid: self.printer.uuid(),
            result_index: self.printer.result_index(),
        });
        if defer {
            self.printer.defer(value, text, is_error);
        } else {
            self.printer.result(value, text, is_error).await?;
        }
        Ok(failure)
    }
}

/// 发请求前的阻塞线拦下的回合（compact.md §2.2）：`Prompt is too long`，或接着压缩失败原因的写法。
fn is_blocking_limit(message: &str) -> bool {
    use rust_cli_kernel_api::compaction::PROMPT_TOO_LONG;
    message == PROMPT_TOO_LONG || message.starts_with(&format!("{PROMPT_TOO_LONG} \u{b7} "))
}

#[cfg(test)]
#[path = "drive_tests.rs"]
mod tests;
