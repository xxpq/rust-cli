//! 人类输入开的回合（docs/specs/rust-cli/session-lifecycle.md §3.1 的时序）：斜杠命令展开 → `@` 提及 →
//! UserPromptSubmit → 提交输入 → 主循环，都在回合任务里，actor 照常处理中断与 `view`（工作线 F130 从 hooks.rs 移来：
//! hooks.rs 只留 hook 事件的组装与会话侧的 hook）。

use rust_cli_kernel_api::abort::AbortReason;
use rust_cli_kernel_api::frontend_prompt::FrontendPrompt;
use rust_cli_kernel_api::hook_runs::HookInvocation;
use rust_cli_kernel_api::hooks::{HookEventName, HookOutcome};
use rust_cli_kernel_api::ids::{CommandId, MessageId};
use rust_cli_kernel_api::input_extras::{InputExtras, InputRecord, TurnConfig};
use rust_cli_kernel_api::input_extras::{page_state_attachment, quote_attachment};
use rust_cli_kernel_api::message::{
    AttachmentMessage, ContentBlock, Message, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::orchestration::{QueryStop, RunSink};
use rust_cli_kernel_api::session_api::{InputFate, Joined, TurnOrigin};
use rust_cli_kernel_api::user_input::InputAttachment;
use rust_cli_prompts::reminder;

use crate::actor::Actor;

impl Actor {
    /// 人类输入开的回合：UserPromptSubmit → 提交输入 → 主循环（当步 attachment 之后提交 hook 消息），都在回合
    /// 任务里（spec §3.1 的时序图）。`screened` 是插话在并入点已跑过的结果，有值时不再跑 hook。`content` 是输入的
    /// 内容块（文字，加粘贴的图片，tui-input.md §3.2）；UPS 与 `@` 提及只看文字块。
    ///
    /// `more` 是成批进这个回合的后续输入（修复线 L-A，query.md §4.3）：第一条之后按顺序各提交一条人类消息，它们在
    /// 工具边界筛查过的 hook 消息随第一条的一起交给主循环。
    ///
    /// `extras` 是这条输入正文之外的事实（session-lifecycle.md §10.14）：附件在提及之后读成 attachment；显示的那一部分是
    /// 输入记录，与这条用户消息同一批提交（`Running.record`）。
    pub(crate) fn start_human_turn(
        &mut self,
        command: CommandId,
        content: Vec<ContentBlock>,
        screened: Option<Box<HookOutcome>>,
        more: Vec<crate::turn::MorePrompt>,
        extras: InputExtras,
    ) {
        let prompt = self.deps.ids.message();
        // 这一轮用的配置（§10.59）：开回合时已经套用了这条输入的配置（turn.rs 的 `adopt`），记下会话此刻的值。成批的后续输入与这一条的配置相同
        // （成批的条件，inbox.rs 的 `batches_with`），各记一份：界面编辑任何一条都能载入它提交时的配置。
        let ran = self.ran_config();
        let batched = InputExtras {
            config: extras.config.clone(),
            ..InputExtras::default()
        };
        let records: Vec<InputRecord> = extras
            .record(prompt.clone(), &ran)
            .into_iter()
            .chain(
                more.iter()
                    .filter_map(|extra| batched.record(extra.id.clone(), &ran)),
            )
            .collect();
        // 这个回合里定下消息的输入（效果记录，session-lifecycle.md §10.3）：开回合的这一条与成批的后续输入。
        let inputs = std::iter::once((prompt.clone(), command.clone()))
            .chain(
                more.iter()
                    .map(|extra| (extra.id.clone(), extra.command.clone())),
            )
            .collect();
        let text = content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        let mut input = UserMessage {
            id: prompt.clone(),
            content,
            origin: UserOrigin::Human,
        };
        let turn = self.deps.ids.turn();
        self.turn_started(turn.clone(), Some(command.clone()), TurnOrigin::Input);
        // 这个回合里的输入：开回合的这一条与成批的后续输入（§10.7）。
        let joined = std::iter::once((command.clone(), Joined::Opened)).chain(
            more.iter()
                .map(|extra| (extra.command.clone(), Joined::Batched)),
        );
        for (input, joined) in joined {
            let turn = turn.clone();
            self.input_fate(input, InputFate::Started { turn, joined });
        }
        let hook = self.user_prompt_submit(&text);
        // `/goal` 是内置的本地命令：消息按 actor 现在的目标算好，不经 skill 展开（goal.md §2.1）。
        let goal = crate::goal::command(&text).map(|args| self.goal_inputs(args, prompt.clone()));
        let (opening, mut request) = self.open_main_turn(turn);
        // 这条输入开的这一轮不许用的工具（§10.15）：主循环与这一轮里启动的子代理都看它。
        request
            .disallowed_tools
            .clone_from(&extras.disallowed_tools);
        // 这一轮的执行覆盖（§10.39）：请求按它的选择与引用发，会话的模型（`request.selection`，开回合时 `adopt` 写的）是退回目标
        request.execution = extras
            .execution
            .as_deref()
            .cloned()
            .map(rust_cli_kernel_api::turn_execution::RunExecution::new);
        let hooks = self.deps.hooks.clone();
        let runner = self.deps.runner.clone();
        let commands = self.deps.commands.clone();
        let ids = self.deps.ids.clone();
        let sink = opening.sink.clone();
        let context = self.command_context(&request.run, &opening.abort, sink.clone());
        let mention_context = self.command_context(&request.run, &opening.abort, sink.clone());
        let attach_context = self.command_context(&request.run, &opening.abort, sink.clone());
        let media_context = self.command_context(&request.run, &opening.abort, sink.clone());
        let user_inputs = self.deps.inputs.clone();
        let frontend_prompt = self.deps.frontend_prompt.clone();
        let signal = opening.abort.clone();
        let input_command = command;
        let task = self.deps.spawner.spawn("query-run", async move {
            // 持久输入账本：第一条消息带上开启回合的命令，落库时同批标为已提升；账本里没有的命令（SessionStart 的首条输入）
            // 由会话 actor 滤掉（durable_inputs.rs 的 `Journaled`）。
            let promoted = |index: usize| {
                if index == 0 {
                    vec![input_command.clone()]
                } else {
                    Vec::new()
                }
            };
            // 附上的媒体与上传的文件是这条输入自己的内容（session-lifecycle.md §10.40、§10.41）：在斜杠展开之前放进用户消息，
            // 正文之后。读文件、读存储、缩放图片要等 IO 与 CPU，所以在回合任务里做，不在 actor 里。
            let media = user_inputs
                .media(&extras.files, &extras.uploads, media_context)
                .await;
            input.content.extend(media.content);
            let user = Message::User(input);
            // 斜杠命令先展开（UPS 在展开之后，ext.md §2.7）；不是命令时就是这条输入本身。
            let mut inputs = match goal {
                Some(inputs) => inputs,
                None => crate::slash::expand(commands.as_ref(), ids.as_ref(), user, context).await,
            };
            // 普通输入的 `@` 提及（在 UPS 之前）：attachment 排在输入之后提交
            // （docs/specs/rust-cli/tui-input.md §1.1）。
            let as_message = |attachment: InputAttachment| {
                Message::Attachment(AttachmentMessage {
                    id: ids.message(),
                    source: attachment.source,
                    text: attachment.text,
                    data: attachment.data,
                })
            };
            // 这条输入给模型的材料（§10.14）只在要调模型时加（本地命令的结果不加），排在 UPS 之前：引用紧跟输入，
            // 之后是提及、附件与上传的文字（§10.41），页面状态排在最后。引用与页面状态的写法由前端层给（context.md §15.8）。
            if inputs.query
                && !extras.quotes.is_empty()
                && let Some(frontend_prompt) = &frontend_prompt
            {
                inputs.messages.push(as_message(quote_attachment(
                    frontend_prompt.quotes(&extras.quotes),
                )));
            }
            if inputs.plain && inputs.query {
                let mentioned = user_inputs.mentions(&text, mention_context).await;
                inputs
                    .messages
                    .extend(mentioned.into_iter().map(as_message));
            }
            if inputs.query && !extras.files.is_empty() {
                let attached = user_inputs.attached(&extras.files, attach_context).await;
                inputs.messages.extend(attached.into_iter().map(as_message));
            }
            // 上传的文字（§10.41）：排在附件之后，同样只在要调模型时加
            if inputs.query {
                inputs
                    .messages
                    .extend(media.attachments.into_iter().map(as_message));
            }
            if inputs.query
                && !extras.pages.is_empty()
                && let Some(frontend_prompt) = &frontend_prompt
            {
                inputs.messages.push(as_message(pages_material(
                    frontend_prompt.as_ref(),
                    &extras.pages,
                )));
            }
            // 修 bug（F37）：以前展开之后、hook 之后各看一次中止，已中止就返回 `Aborted`、什么都不提交，空档里按 esc、
            // 收到退出信号或关闭时用户刚发的输入没了。处理输入之后不看中止（session-lifecycle.md §3.1）：输入照常
            // 提交，主循环在发请求之前看到中止、写中断标记（query.md §1.2）。阻塞与停止仍按 hook 的结果。
            // 退出（`Shutdown`，修复线 F38）不同：收到退出信号不中止、直接退出，这条输入还没进对话，所以什么都不写。
            let shutdown = || signal.reason() == Some(AbortReason::Shutdown);
            if shutdown() {
                return Ok(QueryStop::Aborted);
            }
            // 不调模型的本地结果（`user-invocable: false`）：只提交，不跑 UPS。
            if !inputs.query {
                for (index, message) in inputs.messages.into_iter().enumerate() {
                    sink.commit_input(message, promoted(index)).await?;
                }
                return Ok(QueryStop::Completed);
            }
            // 已中止时 hook 收到已取消的令牌，按取消收尾（结果里只有已完成的 hook 给出的）。
            let outcome = match screened {
                Some(outcome) => *outcome,
                None => {
                    let outcome = hooks.run(hook, signal.token().clone()).await;
                    // hook 行的事实（§10.51）：拦下了这条输入时带上它的原文（它不落库，v5 照 v4 给它一行）。插话在并入点已经交过
                    if !outcome.runs.is_empty() {
                        let invocation = HookInvocation {
                            event: HookEventName::UserPromptSubmit,
                            tool: None,
                            runs: outcome.runs.clone(),
                        };
                        let blocked = outcome.blocking_error.as_ref().map(|_| text.clone());
                        sink.prompt_hooks_ran(invocation, blocked).await;
                    }
                    outcome
                }
            };
            if shutdown() {
                return Ok(QueryStop::Aborted);
            }
            if outcome.blocking_error.is_some() {
                let message = outcome.notices.first().cloned().unwrap_or_default();
                return Ok(QueryStop::Blocked { message });
            }
            for (index, message) in inputs.messages.iter().enumerate() {
                sink.commit_input(message.clone(), promoted(index)).await?;
            }
            let mut hook_messages = outcome.messages.clone();
            for extra in more {
                let message = Message::User(UserMessage {
                    id: extra.id,
                    content: vec![ContentBlock::Text { text: extra.text }],
                    origin: UserOrigin::Human,
                });
                sink.commit_input(message.clone(), vec![extra.command])
                    .await?;
                inputs.messages.push(message);
                if let Some(screened) = extra.screened {
                    hook_messages.extend(screened.messages);
                }
            }
            if let Some(title) = outcome.session_title {
                sink.title(title).await;
            }
            if outcome.prevent_continuation {
                if !outcome.messages.is_empty() {
                    sink.commit_messages(outcome.messages.clone()).await?;
                }
                return Ok(QueryStop::Completed);
            }
            // 修 bug（F10）：以前 hook 消息在这里提交，排在当步 attachment（环境段等）之前；应先给输入
            // 产生当步 attachment，再追加 UPS 的结果。交给主循环在当步 attachment
            // 之后提交（query.md §1.1）。
            let mut request = request;
            request.messages.extend(inputs.messages);
            request.prompt_hook_messages = hook_messages;
            runner.run(request, sink, signal).await
        });
        self.launch(opening, task, Some(prompt), inputs);
        if let Some(running) = &mut self.running {
            running.records = records;
        }
    }

    /// 会话此刻的执行配置：模型、权限模式与 plan（输入记录里的「这一轮用的配置」，§10.59）。
    fn ran_config(&self) -> TurnConfig {
        TurnConfig {
            selection: Some(Box::new(self.selection.clone())),
            mode: Some(self.execution.mode),
            plan: Some(self.execution.plan),
        }
    }

    /// 回合里一条输入的消息：提交它的批次带这条输入的命令 ID（Node `anchor.sourceCommandId`，效果记录，§10.3）。
    ///
    /// 修 bug（K-A2）：以前只认本回合开头那一条，成批的后续输入与工具边界并入的插话落库时不带命令 ID，跨进程的命令账本
    /// 认不出它们执行过，重发会再执行一次。
    pub(crate) fn command_of(&self, message: &MessageId) -> Option<CommandId> {
        self.running.as_ref().and_then(|running| {
            running
                .inputs
                .iter()
                .find(|(id, _)| id == message)
                .map(|(_, command)| command.clone())
        })
    }
}

/// 页面状态给模型的材料（context.md §15.8）：网页给的不可信数据，前端层写正文，这里包 `<system-reminder>`，user 角色。
fn pages_material(
    frontend_prompt: &dyn FrontendPrompt,
    pages: &[rust_cli_kernel_api::input_extras::PageState],
) -> InputAttachment {
    page_state_attachment(reminder::wrap(&frontend_prompt.page_states(pages)))
}
