//! 工具执行管线（校验、hooks、权限、调用与结果预算）。
//!
//! 顺序：schema 校验 → 工具自己的 `validate_input` → PreToolUse hooks → 权限判决（需要时先跑
//! PermissionRequest hooks，再询问用户）→ 执行 → PostToolUse / PostToolUseFailure hooks。任何一步拒绝
//! 都返回 `is_error` 的结果，不进入下一步。hook 的决定与判决链的先后、hook 消息的位置见
//! docs/specs/rust-cli/hooks.md §6。

use std::sync::Arc;

use async_trait::async_trait;
use rust_cli_base::js::string::utf16_prefix;
use rust_cli_kernel_api::hooks::HookOutcome;
use rust_cli_kernel_api::message::ToolResultContent;
use rust_cli_kernel_api::orchestration::{
    Answer, HookEventName, HookRunner, InteractionRequest, PermissionAnswer, PermissionGate,
    ToolExecution, ToolExecutor,
};
use rust_cli_kernel_api::permission::{PermissionDecision, PermissionRule};
use rust_cli_kernel_api::permission_updates::PermissionUpdate;
use rust_cli_kernel_api::tool::{
    Tool, ToolCallOutput, ToolError, ToolOutput, ToolProgress, ToolUseContext, Validation,
};
use rust_cli_kernel_api::unparsed_input;
use serde_json::Value;
use tokio::sync::mpsc;

use crate::asking::Cancelled;
use crate::budget;
use crate::pipeline_hooks::{self as hooks, HookMessages};
use crate::pipeline_machine::{Machines, Routed, tag};
use crate::texts;
use crate::validate::{strip_unknown, validate};

pub(crate) const REJECT_MESSAGE: &str = rust_cli_kernel_api::tool::REJECT_MESSAGE;
pub(crate) const REJECT_WITH_REASON: &str = "The user doesn't want to proceed with this tool use. The tool use was rejected (eg. if it was a file edit, the new_string was NOT written to the file). To tell you how to proceed, the user said:\n";
/// 工具使用被中断时的文字（kernel-api 的定义，F55）。
const INTERRUPT_FOR_TOOL_USE: &str = rust_cli_kernel_api::user_input::INTERRUPT_TEXTS[1];
const NO_OUTPUT: &str = "Command failed with no output";
/// `formatError` 超过这个长度（UTF-16 单元）时保留首尾各一半。
const ERROR_LIMIT: usize = 10_000;

/// 执行管线；权限判决链、hooks 与执行环境的注册表由 runtime 注入。
pub struct Pipeline {
    gate: Arc<dyn PermissionGate>,
    hooks: Arc<dyn HookRunner>,
    /// 按调用的 `machine` 选机器（R3，remote-exec.md §20.6）；没有时不路由。
    machines: Machines,
}

/// 子代理里的询问被拒时的结果文字（主线程是 [`REJECT_MESSAGE`]，docs/specs/rust-cli/agents.md §4.1.1）。
const SUBAGENT_REJECT: &str = "Permission for this tool use was denied. The tool use was rejected (eg. if it was a file edit, the new_string was NOT written to the file). Try a different approach or report the limitation to complete your task.";
const SUBAGENT_REJECT_WITH_REASON: &str = "Permission for this tool use was denied. The tool use was rejected (eg. if it was a file edit, the new_string was NOT written to the file). The user said:\n";

/// 用户拒绝（或撤回询问）时给模型的文字：子代理里的调用用子代理的那一段。
///
/// 修复线 F67：以前子代理与主线程同一段（`The user doesn't want to proceed …`）；子代理被拒时应该换个做法或报告限制，
/// 而不是停下来等用户。
fn rejection(context: &ToolUseContext, feedback: Option<&str>) -> String {
    match (context.agent.is_some(), feedback) {
        (false, None) => REJECT_MESSAGE.to_owned(),
        (false, Some(feedback)) => format!("{REJECT_WITH_REASON}{feedback}"),
        (true, None) => SUBAGENT_REJECT.to_owned(),
        (true, Some(feedback)) => format!("{SUBAGENT_REJECT_WITH_REASON}{feedback}"),
    }
}

/// 拒绝的来源：PreToolUse hook 拒绝时，它的停止说明随结果交出（只在这类拒绝后交出）。
enum Rejection {
    ByHook(ToolOutput),
    Other(ToolOutput),
    /// PermissionRequest hook 的拒绝带 `interrupt: true`（hooks.md §6.2）：执行器中止整个回合（同用户的停止）。
    Interrupt(ToolOutput),
}

fn error(text: String) -> ToolOutput {
    ToolOutput {
        content: vec![ToolResultContent::Text { text }],
        is_error: true,
        display: Value::Null,
    }
}

/// 用户拒绝时的结果（拒绝文本、`is_error`）；提问与计划审批的工具在 `call` 里用它。
pub(crate) fn rejected(text: String) -> ToolOutput {
    error(text)
}

/// 一次权限询问：文字、建议规则与“只批准这一次”（判决链或 PreToolUse hook 的 `Ask`）。
struct Asking {
    message: String,
    suggestions: Vec<PermissionRule>,
    once: bool,
}

/// PermissionRequest hook 的结论（hooks.md §6.1）：放行（可能改写了输入、带权限更新），或拒绝（可能要求中止整个回合）。
enum Verdict {
    Allow {
        updated: Option<Value>,
        remember: Vec<PermissionUpdate>,
    },
    Deny {
        message: String,
        interrupt: bool,
    },
}

impl Verdict {
    /// 只有 allow / deny 是结论；没有决定的 hook 退出竞速。
    fn of(outcome: HookOutcome) -> Option<Self> {
        match outcome.decision {
            Some(PermissionDecision::Allow { .. }) => Some(Self::Allow {
                updated: outcome.updated_input,
                remember: outcome.remember,
            }),
            Some(PermissionDecision::Deny { message }) => Some(Self::Deny {
                message,
                interrupt: outcome.interrupt,
            }),
            Some(PermissionDecision::Ask { .. } | PermissionDecision::Passthrough) | None => None,
        }
    }
}

/// hook 的结论对这次调用的效果（hooks.md §6.1、§6.2）：在回答询问之前算出，改写的输入已过 schema 与不可绕过的关卡。
enum Effect {
    /// 照这份输入执行；`remember` 是 hook 的权限更新，只在 hook 的结论被用上时由交互的 owner 落（permission-updates.md §5）。
    Run {
        input: Value,
        remember: Vec<PermissionUpdate>,
    },
    /// 拒绝（可能要求中止整个回合）。
    Refuse(Rejection),
    /// 改写之后规则要询问：用新输入再问一次，这一次不跑 hook。
    Reask { input: Value, asking: Asking },
}

impl Effect {
    /// 交给交互 owner 的回答：解决询问（关掉对话框）；放行时带上要记住的规则，owner 与解决同一批落库（同“总是允许”）。
    fn answer(&self) -> Answer {
        Answer::Permission(match self {
            Self::Run { remember, .. } => PermissionAnswer::Allow {
                remember: remember.clone(),
            },
            Self::Reask { .. } => PermissionAnswer::Allow {
                remember: Vec::new(),
            },
            Self::Refuse(_) => PermissionAnswer::Deny { feedback: None },
        })
    }
}

/// 竞速里先结束的一方：用户的回答，或 hook（有没有结论）。
enum First {
    User(Result<Option<PermissionAnswer>, Cancelled>),
    Hook(Option<Verdict>),
}

/// 询问的结局：用户的回答，或先到的 hook 的结论的效果。
enum Settled {
    User(Result<Option<PermissionAnswer>, Cancelled>),
    Hook(Effect),
}

/// 人的决定（允许、拒绝）或工具被取消：hook 不再有用。没有人能回答与询问失败不是决定，等 hook（hooks.md §6.1）。
fn decided(answer: &Result<Option<PermissionAnswer>, Cancelled>) -> bool {
    matches!(
        answer,
        Err(Cancelled)
            | Ok(Some(
                PermissionAnswer::Allow { .. } | PermissionAnswer::Deny { .. }
            ))
    )
}

fn wrapped(text: &str) -> ToolOutput {
    error(format!("<tool_use_error>{text}</tool_use_error>"))
}

/// 错误文本：空消息换成固定文本，过长时保留首尾各 5000 个 UTF-16 单元。
fn format_error(message: &str) -> String {
    let message = message.trim();
    if message.is_empty() {
        return NO_OUTPUT.to_owned();
    }
    let length = message.encode_utf16().count();
    if length <= ERROR_LIMIT {
        return message.to_owned();
    }
    let half = ERROR_LIMIT / 2;
    let start = utf16_prefix(message, half);
    let end_units = length - half;
    let end = &message[utf16_prefix(message, end_units).len()..];
    format!(
        "{start}\n\n... [{} characters truncated] ...\n\n{end}",
        length - ERROR_LIMIT
    )
}

impl Pipeline {
    pub fn new(gate: Arc<dyn PermissionGate>, hooks: Arc<dyn HookRunner>) -> Self {
        Self {
            gate,
            hooks,
            machines: Machines::default(),
        }
    }

    /// 接上执行环境的注册表：带 `machine` 参数的工具按调用选机器（remote-exec.md §20.6）。
    #[must_use]
    pub fn with_machines(
        mut self,
        registry: Arc<dyn rust_cli_kernel_api::env_registry::EnvironmentRegistry>,
    ) -> Self {
        self.machines = Machines::new(registry);
        self
    }

    /// 判决链或 hook 改过的输入要重新通过 schema。
    fn revalidate(tool: &dyn Tool, schema: &Value, input: Value) -> Result<Value, ToolOutput> {
        match validate(tool.name(), schema, &input) {
            Some(message) => Err(wrapped(&format!("InputValidationError: {message}"))),
            None => Ok(input),
        }
    }

    /// 询问用户：PermissionRequest hooks 与询问竞速，先到者胜（docs/specs/rust-cli/hooks.md §6.1）。返回最终的输入（hook 可能改写它）。
    /// 只批准这一次的询问不跑 hook：hook 不能替用户批准（修复线 F20，Node `approvalMode: "user-once"` 时 `runHooks: false`）。
    async fn ask_for(
        &self,
        tool: &dyn Tool,
        schema: &Value,
        input: Value,
        asking: Asking,
        call: &Routed,
    ) -> Result<Value, Rejection> {
        if asking.once {
            let answer = self.prompt(tool, &input, &asking, call).await;
            return self
                .answered(tool, answer, &asking.message, call)
                .await
                .map(|()| input)
                .map_err(Rejection::Other);
        }
        match self.race(tool, schema, &input, &asking, call).await {
            Settled::User(answer) => self
                .answered(tool, answer, &asking.message, call)
                .await
                .map(|()| input)
                .map_err(Rejection::Other),
            Settled::Hook(Effect::Run { input, .. }) => Ok(input),
            Settled::Hook(Effect::Refuse(rejection)) => Err(rejection),
            Settled::Hook(Effect::Reask { input, asking }) => {
                self.reask(tool, input, asking, call).await
            }
        }
    }

    /// 询问与 PermissionRequest hooks 竞速（hooks.md §6.1）。询问先发：第一次轮询就把它送进通道，hook 的回答只可能排在它后面
    /// （`answer_ask` 与 `ask` 走同一条通道）。谁先到由交互的 owner 裁决，这里不用计时器。
    async fn race(
        &self,
        tool: &dyn Tool,
        schema: &Value,
        input: &Value,
        asking: &Asking,
        call: &Routed,
    ) -> Settled {
        let context = &call.context;
        let payload = tag(
            hooks::permission_request(tool.name(), input, &asking.suggestions),
            &call.machine,
        );
        let event = hooks::event(HookEventName::PermissionRequest, &call.hooks, payload).await;
        let asked = self.prompt(tool, input, asking, call);
        let stop_hook = context.cancel.child_token();
        let hook = hooks::run(self.hooks.as_ref(), event, context, stop_hook.clone());
        tokio::pin!(asked, hook);
        let first = tokio::select! {
            biased;
            answer = &mut asked => First::User(answer),
            outcome = &mut hook => First::Hook(Verdict::of(outcome)),
        };
        match first {
            // 用户先答（或工具被取消）：hook 是败者，结束它并等它收尾，hook 进程不比这次询问活得久（同 Node；结束有执行环境的
            // TERM → KILL 上限）。
            First::User(answer) if decided(&answer) => {
                stop_hook.cancel();
                hook.await;
                Settled::User(answer)
            }
            // 没有人能回答（`-p`、`-p` 的后台子代理）或询问失败：等 hook 的结论（不能询问时先等 hook 的结论）。
            First::User(nobody) => match Verdict::of(hook.await) {
                Some(verdict) => {
                    let effect = self.effect(tool, schema, input, verdict, call).await;
                    self.apply_unanswered(&effect, call).await;
                    Settled::Hook(effect)
                }
                None => Settled::User(nobody),
            },
            First::Hook(verdict) => match verdict {
                // hook 没有结论是退赛：只等用户。
                None => Settled::User(asked.await),
                Some(verdict) => {
                    // 先算效果再回答（工作线 HOOKS-2）：改写的输入被拒绝或要再问时，回答不带 hook 要记住的规则。
                    let effect = self.effect(tool, schema, input, verdict, call).await;
                    let first = context
                        .interactions
                        .answer_ask(&context.tool_use, effect.answer())
                        .await;
                    if first {
                        // hook 解决了询问：它的更新已与解决同一次落，判决链读回 LocalSettings 层（同“总是允许”，下面的 `answered`）。
                        if let Effect::Run { remember, .. } = &effect {
                            self.local_rules_changed(remember, call).await;
                        }
                        return Settled::Hook(effect);
                    }
                    // 询问已经先有了回答：人的决定先到就用它，hook 的结论丢掉。修 bug：“没有人能回答”（`-p` 当场答的
                    // `Unavailable`）比 hook 的回答先到 owner、却晚于 hook 被这里看到时，以前用了它，hook 的结论丢了；它不是决定，
                    // 照“没有人能回答时等 hook”用 hook 的结论（规则没有落库）。
                    let answer = asked.await;
                    if decided(&answer) {
                        Settled::User(answer)
                    } else {
                        self.apply_unanswered(&effect, call).await;
                        Settled::Hook(effect)
                    }
                }
            },
        }
    }

    /// hook 的结论对这次调用的效果（hooks.md §6.1）：放行并改写了输入时，新输入先过 schema，再过不可绕过的关卡（deny / ask 规则、
    /// 安全路径）。
    ///
    /// 修 bug（工作线 HOOKS）：以前只做 schema 校验，hook 改写出来的命令绕过了 deny 与 ask 规则。
    /// 工作线 HOOKS-2：在回答询问之前算，回答才知道带不带 hook 要记住的规则（规则只在这道关卡之后写，hooks.md §6.2）。
    async fn effect(
        &self,
        tool: &dyn Tool,
        schema: &Value,
        input: &Value,
        verdict: Verdict,
        call: &Routed,
    ) -> Effect {
        let (updated, remember) = match verdict {
            Verdict::Deny { message, interrupt } => {
                let output = error(message);
                return Effect::Refuse(if interrupt {
                    Rejection::Interrupt(output)
                } else {
                    Rejection::Other(output)
                });
            }
            Verdict::Allow {
                updated: None,
                remember,
            } => {
                return Effect::Run {
                    input: input.clone(),
                    remember,
                };
            }
            Verdict::Allow {
                updated: Some(updated),
                remember,
            } => (updated, remember),
        };
        let updated = match Self::revalidate(tool, schema, updated) {
            Ok(updated) => updated,
            Err(output) => return Effect::Refuse(Rejection::Other(output)),
        };
        match self.gate.check_rules(tool, &updated, &call.context).await {
            Some(PermissionDecision::Deny { message }) => {
                Effect::Refuse(Rejection::Other(error(message)))
            }
            Some(PermissionDecision::Ask {
                message,
                suggestions,
                once,
            }) => Effect::Reask {
                input: updated,
                asking: Asking {
                    message,
                    suggestions,
                    once,
                },
            },
            Some(PermissionDecision::Allow { .. } | PermissionDecision::Passthrough) | None => {
                Effect::Run {
                    input: updated,
                    remember,
                }
            }
        }
    }

    /// 改写之后规则要询问：用新输入再问一次（新的询问），这一次不跑 hook。
    async fn reask(
        &self,
        tool: &dyn Tool,
        input: Value,
        asking: Asking,
        call: &Routed,
    ) -> Result<Value, Rejection> {
        // 没有人能回答时拒绝的文字是这次 ask 的文字，`-p` 的后台子代理也是（不能询问时直接按 ask 的文字拒绝）。
        let answer = self
            .prompt(tool, &input, &asking, call)
            .await
            .map(|answer| {
                answer.map(|answer| match answer {
                    PermissionAnswer::NoPrompts => PermissionAnswer::Unavailable,
                    other => other,
                })
            });
        self.answered(tool, answer, &asking.message, call)
            .await
            .map_err(Rejection::Other)?;
        Ok(input)
    }

    /// 发出询问，等用户的回答（不跑 hook）。
    async fn prompt(
        &self,
        tool: &dyn Tool,
        input: &Value,
        asking: &Asking,
        call: &Routed,
    ) -> Result<Option<PermissionAnswer>, Cancelled> {
        // “不再询问”一行的种子与建议规则出自同一套命令分析（shell-permissions.md §4.10）；前端不再自己算（F133）。
        let rule_seed = rust_cli_policy::ask_rule_seed(tool.name(), input, &asking.suggestions);
        let request = InteractionRequest::Permission {
            tool_use: call.context.tool_use.clone(),
            tool: tool.name().to_owned(),
            input: input.clone(),
            message: asking.message.clone(),
            suggestions: asking.suggestions.clone(),
            rule_seed,
            asked_by: None,
            // 额外机器上的调用，询问带机器名（R3，remote-exec.md §20.8；主 env 不写出）。
            machine: call.machine.clone(),
            once: asking.once,
        };
        crate::asking::ask::<PermissionAnswer>(&call.context, request).await
    }

    /// 用户的回答：放行，或给模型的拒绝文字。取消时按拒绝处理（中断的结果由执行器统一替换）。
    async fn answered(
        &self,
        tool: &dyn Tool,
        answer: Result<Option<PermissionAnswer>, Cancelled>,
        message: &str,
        call: &Routed,
    ) -> Result<(), ToolOutput> {
        let context = &call.context;
        let Ok(answer) = answer else {
            return Err(error(rejection(context, None)));
        };
        match answer {
            Some(PermissionAnswer::Allow { remember }) => {
                // “总是允许”：会话 actor 已把规则与交互的解决同一批落库；判决链按 `/permissions` 的路径读回库值、换上
                // LocalSettings 层（shell-permissions.md §5）。修复（R1-2）：以前这里交给判决链另存一份按会话的规则，
                // 子代理与 `/clear` 之后的新会话看不到，又问一次。
                self.local_rules_changed(&remember, call).await;
                Ok(())
            }
            Some(PermissionAnswer::Deny {
                feedback: Some(feedback),
            }) if !feedback.trim().is_empty() => Err(error(rejection(context, Some(&feedback)))),
            // 没有人能回答（-p）：模型收到询问本身的文本。
            Some(PermissionAnswer::Unavailable) => Err(error(message.to_owned())),
            // 修 bug（工作线 F69）：`-p` 的后台子代理以前也给询问本身的文本；它所在的上下文不能询问，PermissionRequest
            // hooks 没有结论时给“不能询问”的拒绝文字（agents.md §4.1.1）。
            Some(PermissionAnswer::NoPrompts) => Err(error(
                rust_cli_policy::texts::prompts_unavailable(tool.name()),
            )),
            Some(PermissionAnswer::Deny { .. }) | None => Err(error(rejection(context, None))),
        }
    }

    /// 没有人能回答、用了 hook 的放行（permission-updates.md §5）：另请交互的 owner 落 hook 的权限更新（不能询问时也写）。
    ///
    /// 修 bug（工作线 PERM-UPDATES，hooks.md §13 第 3 项）：以前更新只随 hook 对还开着的询问的回答落；`-p` 里 headless 当场答的
    /// `Unavailable` 先到 owner 时 hook 的规则不写、hook 先到时写，结局取决于谁先到。现在两种先后都落。落不了只记日志：这次调用照 hook
    /// 的结论走（规则没落下只影响以后的询问）。
    async fn apply_unanswered(&self, effect: &Effect, call: &Routed) {
        let Effect::Run { remember, .. } = effect else {
            return;
        };
        if remember.is_empty() {
            return;
        }
        match call
            .context
            .interactions
            .apply_permission_updates(remember.clone())
            .await
        {
            Ok(()) => self.local_rules_changed(remember, call).await,
            Err(error) => tracing::warn!(
                %error,
                tool_use = %call.context.tool_use,
                "the permission updates of a PermissionRequest hook were not applied"
            ),
        }
    }

    /// 落了本机项目规则集里的规则时，判决链读回 LocalSettings 层（只读一次）。规则只有一套，存在本机的项目里：用会话的工作区
    /// （远端调用的上下文换了工作区，R3）。会话有自己的规则来源时（云端的共享规则集）按会话读回，没有时同本机的读回
    /// （`PermissionGate::session_rules_committed`）。
    async fn local_rules_changed(&self, updates: &[PermissionUpdate], call: &Routed) {
        if updates.iter().any(PermissionUpdate::changes_local_rules) {
            self.gate
                .session_rules_committed(&call.hooks.session, &call.hooks.workspace)
                .await;
        }
    }

    /// 完整的权限判决链。
    async fn decide(
        &self,
        tool: &dyn Tool,
        schema: &Value,
        input: Value,
        call: &Routed,
    ) -> Result<Value, Rejection> {
        match self.gate.decide(tool, &input, &call.context).await {
            PermissionDecision::Allow {
                updated_input: Some(updated),
            } => Self::revalidate(tool, schema, updated).map_err(Rejection::Other),
            PermissionDecision::Allow {
                updated_input: None,
            } => Ok(input),
            PermissionDecision::Deny { message } => Err(Rejection::Other(error(message))),
            PermissionDecision::Ask {
                message,
                suggestions,
                once,
            } => {
                let asking = Asking {
                    message,
                    suggestions,
                    once,
                };
                self.ask_for(tool, schema, input, asking, call).await
            }
            // 没有理由的询问文字只在 policy 的 `texts`（F132 的 T15：以前这里内联了同一句）。
            PermissionDecision::Passthrough => {
                let asking = Asking {
                    message: rust_cli_policy::texts::request_message(tool.name(), None),
                    suggestions: Vec::new(),
                    once: false,
                };
                self.ask_for(tool, schema, input, asking, call).await
            }
        }
    }

    /// 校验、机器路由、PreToolUse hooks 与权限；通过时返回最终输入与路由后的上下文。
    async fn admit(
        &self,
        tool: &dyn Tool,
        mut input: Value,
        context: ToolUseContext,
        messages: &mut HookMessages,
    ) -> Result<(Value, Routed), Rejection> {
        // R5-1：输入在组装时不是合法 JSON（标记对象，kernel-api `unparsed_input`）：先于 schema 校验
        // 给出 JSON 解析失败的说明，不跑 hooks 与权限。
        if let Some(unparsed) = unparsed_input::parse(&input) {
            let message = texts::unparsed_input(tool.name(), unparsed.raw, unparsed.len);
            return Err(Rejection::Other(wrapped(&format!(
                "InputValidationError: {message}"
            ))));
        }
        // F98（remote-exec.md §25.2.2）：点名清单里有、没连上的机器时回“没有执行”，先于 schema 校验与去键。
        if let Some(message) = self.machines.not_connected(tool, &input) {
            return Err(Rejection::Other(wrapped(&message)));
        }
        let schema = tool.input_schema();
        // R3（remote-exec.md §20.6）：有远端机器时，去键与校验用加了 `machine` 的 schema（发给模型的同一组取值）。
        let plan = self.machines.plan(tool, &context);
        let checked = Machines::schema(schema.clone(), &plan);
        // 修复线 F51（agents.md §2）：宽松对象的工具（Agent）丢掉 schema 外的键、不报错，之后的 hook、
        // `validate_input` 与 `call` 都用丢掉之后的输入。以前一律按严格对象，
        // 模型给 Agent 传了 schema 外的键（例如 F51 删掉的 `model`）就报 InputValidationError。
        if tool.strips_unknown_keys() {
            input = strip_unknown(&checked, input);
        }
        if let Some(message) = validate(tool.name(), &checked, &input) {
            return Err(Rejection::Other(wrapped(&format!(
                "InputValidationError: {message}"
            ))));
        }
        let call = self
            .machines
            .route(tool, &mut input, context, &plan)
            .map_err(|message| Rejection::Other(wrapped(&message)))?;
        let context = &call.context;
        if let Validation::Invalid { message, .. } = tool.validate_input(&input, context).await {
            return Err(Rejection::Other(wrapped(&message)));
        }
        let payload = tag(
            hooks::pre_tool_use(tool.name(), &input, context),
            &call.machine,
        );
        let event = hooks::event(HookEventName::PreToolUse, &call.hooks, payload).await;
        let mut pre = hooks::run(self.hooks.as_ref(), event, context, context.cancel.clone()).await;
        hooks::take_pre(messages, &mut pre);
        if let Some(updated) = pre.updated_input.take() {
            if let Some(message) = validate(tool.name(), &schema, &updated) {
                return Err(Rejection::ByHook(error(format!(
                    "PreToolUse hook for {} returned updatedInput that failed schema validation: {message}",
                    tool.name()
                ))));
            }
            input = updated;
        }
        let other = Rejection::Other;
        let input = match pre.decision {
            Some(PermissionDecision::Deny { message }) => Err(Rejection::ByHook(error(message))),
            // hook 放行：deny / ask 规则与安全路径仍然生效（不可绕过的权限关卡）。
            Some(PermissionDecision::Allow { .. }) => {
                match self.gate.check_rules(tool, &input, context).await {
                    Some(PermissionDecision::Deny { message }) => Err(other(error(message))),
                    Some(PermissionDecision::Ask { .. }) => {
                        self.decide(tool, &schema, input, &call).await
                    }
                    Some(PermissionDecision::Allow { .. } | PermissionDecision::Passthrough)
                    | None => Ok(input),
                }
            }
            // hook 要求询问：用 hook 的原因询问。hook 的询问不是“只批准这一次”（危险命令时 `check_rules` 一定拒绝或询问，
            // 走上面两支）。
            Some(PermissionDecision::Ask {
                message,
                suggestions,
                once,
            }) => match self.gate.check_rules(tool, &input, context).await {
                Some(PermissionDecision::Deny { message }) => Err(other(error(message))),
                Some(PermissionDecision::Ask { .. }) => {
                    self.decide(tool, &schema, input, &call).await
                }
                Some(PermissionDecision::Allow { .. } | PermissionDecision::Passthrough) | None => {
                    let asking = Asking {
                        message,
                        suggestions,
                        once,
                    };
                    self.ask_for(tool, &schema, input, asking, &call).await
                }
            },
            Some(PermissionDecision::Passthrough) | None => {
                self.decide(tool, &schema, input, &call).await
            }
        }?;
        Ok((input, call))
    }

    /// 执行并跑 PostToolUse / PostToolUseFailure hooks。
    async fn call(
        &self,
        tool: &dyn Tool,
        input: Value,
        call: Routed,
        progress: mpsc::Sender<ToolProgress>,
        messages: &mut HookMessages,
    ) -> ToolOutput {
        let Routed {
            context,
            hooks: session,
            machine,
        } = call;
        let machine = &machine;
        match tool
            .call_with_messages(input.clone(), context.clone(), progress)
            .await
        {
            Ok(ToolCallOutput {
                mut output,
                mut new_messages,
            }) => {
                let payload = tag(
                    hooks::post_tool_use(tool.name(), &input, &output, &context),
                    machine,
                );
                let event = hooks::event(HookEventName::PostToolUse, &session, payload).await;
                let mut post =
                    hooks::run(self.hooks.as_ref(), event, &context, context.cancel.clone()).await;
                messages.after.append(&mut post.messages);
                // 工具自己的后续消息（Skill 的正文）排在 PostToolUse 的消息之后
                // （docs/specs/rust-cli/ext.md §2.4）。
                messages.after.append(&mut new_messages);
                if let Some(replacement) = post.updated_tool_output.take() {
                    hooks::replace_output(&mut output, replacement);
                }
                // hook 看到的是原始结果；给模型的结果再按预算落盘（在 hook 之后持久化）。
                budget::apply(
                    output,
                    tool.name(),
                    tool.max_result_size_chars(),
                    &context.session,
                    context.artifacts.as_ref(),
                )
                .await
            }
            Err(failure) => {
                let interrupted = matches!(failure, ToolError::Cancelled);
                let text = if interrupted {
                    INTERRUPT_FOR_TOOL_USE.to_owned()
                } else {
                    format_error(&failure.to_string())
                };
                let payload = tag(
                    hooks::post_tool_use_failure(tool.name(), &input, &text, interrupted, &context),
                    machine,
                );
                let event =
                    hooks::event(HookEventName::PostToolUseFailure, &session, payload).await;
                let mut post =
                    hooks::run(self.hooks.as_ref(), event, &context, context.cancel.clone()).await;
                messages.after.append(&mut post.messages);
                let mut output = error(text);
                // 修 bug（PROJ-GAPS，shell-permissions.md §1.3.1）：失败另带的只给界面的数据（Bash 失败的大输出的头部）以前到这里就丢了，
                // 界面只拿到给模型的错误文字。原样放进结果的 `display`；给模型的文字不变。
                if let ToolError::FailedWithDisplay { display, .. } = failure {
                    output.display = display;
                }
                output
            }
        }
    }
}

#[async_trait]
impl ToolExecutor for Pipeline {
    async fn execute(
        &self,
        tool: Arc<dyn Tool>,
        input: Value,
        context: ToolUseContext,
        progress: mpsc::Sender<ToolProgress>,
    ) -> ToolExecution {
        let mut messages = HookMessages::default();
        let (input, call) = match self
            .admit(tool.as_ref(), input, context, &mut messages)
            .await
        {
            Ok(admitted) => admitted,
            Err(Rejection::ByHook(output)) => return messages.finish(output, true),
            Err(Rejection::Other(output)) => return messages.finish(output, false),
            Err(Rejection::Interrupt(output)) => {
                return ToolExecution {
                    interrupt: true,
                    ..messages.finish(output, false)
                };
            }
        };
        let output = self
            .call(tool.as_ref(), input, call, progress, &mut messages)
            .await;
        messages.finish(output, true)
    }
}

#[cfg(test)]
#[path = "pipeline_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "pipeline_hook_tests.rs"]
mod hook_tests;

#[cfg(test)]
#[path = "pipeline_message_tests.rs"]
mod message_tests;

#[cfg(test)]
#[path = "pipeline_machine_tests.rs"]
mod machine_tests;

#[cfg(test)]
#[path = "pipeline_race_tests.rs"]
mod race_tests;
