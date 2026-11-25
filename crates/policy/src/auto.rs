//! 自动语义下的判决（docs/specs/rust-cli/auto-classifier.md §16.2、§16.3、§16.5）：内层要询问且自动语义生效时，先把只能问人的
//! 留给人，再走快速放行（acceptEdits 的模拟、安全名单）与分类器；判完重读会话，结论落成判决并交 owner 计数。
//!
//! 会话的状态（执行状态、主模型、拒绝计数）只在模式可能自动（auto、plan）时经 `SessionOps::view` 读，owner 是会话 actor 与子代理
//! 的输出端；这里不存副本。

use rust_cli_kernel_api::classifier::{
    AutoClassifier, ClassifierVerdict, ClassifyRequest, NoVerdict,
};
use rust_cli_kernel_api::permission::auto_mode::{
    AutoModeSettings, AutoVerdict, DenialCounts, DenialLimit, auto_active,
};
use rust_cli_kernel_api::permission::{ExecutionState, PermissionDecision, PermissionMode};
use rust_cli_kernel_api::tool::ToolUseContext;
use rust_cli_kernel_api::tool_names;

use crate::chain::Inner;
use crate::check::AskKind;
use crate::rules::Stripped;
use crate::texts;

/// 安全名单：自动语义下它们的询问直接放行，不交分类器。
const SAFE_ALLOWLIST: [&str; 17] = [
    tool_names::READ,
    tool_names::GREP,
    tool_names::GLOB,
    tool_names::TOOL_SEARCH,
    tool_names::LIST_MCP_RESOURCES,
    tool_names::READ_MCP_RESOURCE,
    tool_names::READ_MCP_RESOURCE_DIR,
    tool_names::WAIT_FOR_MCP_SERVERS,
    tool_names::REPORT_FINDINGS,
    tool_names::TODO_WRITE,
    tool_names::TASK_CREATE,
    tool_names::TASK_UPDATE,
    tool_names::TASK_GET,
    tool_names::TASK_LIST,
    tool_names::TASK_STOP,
    tool_names::ENTER_PLAN_MODE,
    tool_names::EXIT_PLAN_MODE,
];

/// 拦下时不提示加规则的工具（它们的检查在 acceptEdits 下就放行，自动语义里只交分类器）。模拟用的是工具开工时按真实模式做的自检
/// （`own` 只算一次），这几个工具 auto 下的 passthrough 不会被模拟成放行，所以模拟不另排除它们（auto-classifier.md §16.2）。
const NO_RULE_HINT: [&str; 4] = [
    tool_names::AGENT,
    tool_names::CRON_CREATE,
    tool_names::SCHEDULE_WAKEUP,
    tool_names::SEND_MESSAGE,
];

/// 一次读到的会话状态。
#[derive(Debug, Clone, Copy)]
pub(crate) struct Auto {
    /// 判决用的模式（会话这一刻的）。
    pub(crate) mode: PermissionMode,
    /// 自动语义生效：会话这一刻的模式与工具开工时的相同，且 [`auto_active`]。
    pub(crate) active: bool,
    pub(crate) counts: DenialCounts,
}

impl Auto {
    /// 模式不可能自动（不是 auto、plan）：不读会话。
    fn off(mode: PermissionMode) -> Self {
        Self {
            mode,
            active: false,
            counts: DenialCounts::default(),
        }
    }

    /// 读一次会话。工具开工之后换了模式（`ctx.mode` 与这一刻的不同）不按自动语义：这次判决退回原来的询问，同判完重读的规则。
    /// 没有视图的输出端（斜杠命令、`@` 提及、bash 模式、压缩后的恢复）按 `ctx.mode` 的旧写法读，计数为 0。
    pub(crate) async fn read(context: &ToolUseContext, settings: &AutoModeSettings) -> Self {
        if !matches!(context.mode, PermissionMode::Auto | PermissionMode::Plan) {
            return Self::off(context.mode);
        }
        let view = context.session_ops.view().await;
        let execution = view
            .as_ref()
            .map_or_else(|| ExecutionState::from(context.mode), |view| view.execution);
        let model = view
            .as_ref()
            .and_then(|view| view.selection.as_ref())
            .map(|selection| selection.model.as_str());
        let mode = execution.effective();
        Self {
            mode,
            active: mode == context.mode && auto_active(execution, settings, model),
            counts: view.map(|view| view.auto_denials).unwrap_or_default(),
        }
    }

    /// 这一次判决读规则时要去掉的 allow 规则。
    pub(crate) fn stripped(&self, settings: &AutoModeSettings) -> Option<Stripped> {
        self.active.then_some(Stripped {
            all_shell: settings.classify_all_shell,
        })
    }
}

/// 内层放行时：模式是 auto 且连续数大于 0 才清零（plan 里的规则放行不清）。
pub(crate) async fn allowed(context: &ToolUseContext, auto: &Auto) {
    if context.mode == PermissionMode::Auto {
        cleared(context, auto.counts).await;
    }
}

/// 快速放行与分类器放行：连续数大于 0 时发清零的命令（不给每个放行加一次往返）。
async fn cleared(context: &ToolUseContext, counts: DenialCounts) {
    if counts.consecutive > 0 {
        context
            .session_ops
            .note_auto_verdict(AutoVerdict::Allowed)
            .await;
    }
}

/// 内层要询问时的那一问（原样交给用户，没有人能回答时按它的文字拒绝）。
pub(crate) struct Asked {
    pub(crate) message: String,
    pub(crate) kind: AskKind,
    pub(crate) suggestions: Vec<rust_cli_kernel_api::permission::PermissionRule>,
    pub(crate) once: bool,
    pub(crate) avoid_prompts: bool,
}

impl Asked {
    pub(crate) fn decision(self) -> PermissionDecision {
        if self.avoid_prompts {
            return PermissionDecision::Deny {
                message: self.message,
            };
        }
        PermissionDecision::Ask {
            message: self.message,
            suggestions: self.suggestions,
            once: self.once,
        }
    }

    /// 只能问人的几类：ask 规则、危险删除（与 guarded 的危险命令）、plan 本身的询问。
    pub(crate) fn for_the_user(&self) -> bool {
        matches!(
            self.kind,
            AskKind::Rule | AskKind::Immune | AskKind::PlanFloor
        )
    }
}

/// 自动语义生效、不是只能问人的询问（§16.2 第 6–10 步）。
pub(crate) async fn decide(
    tool: &str,
    asked: Asked,
    accept_edits: impl FnOnce() -> Inner,
    (classifier, request): (&dyn AutoClassifier, ClassifyRequest<'_>),
    (auto, settings): (Auto, &AutoModeSettings),
) -> PermissionDecision {
    let context = request.context;
    if asked.kind == AskKind::Other
        && auto.mode == PermissionMode::Auto
        && let Inner::Allow { updated_input } = accept_edits()
    {
        tracing::debug!(
            event = "policy.auto.fast_path",
            tool,
            path = "accept_edits",
            "allowed without the classifier"
        );
        cleared(context, auto.counts).await;
        return PermissionDecision::Allow { updated_input };
    }
    if SAFE_ALLOWLIST.contains(&tool) {
        tracing::debug!(
            event = "policy.auto.fast_path",
            tool,
            path = "allowlist",
            "allowed without the classifier"
        );
        cleared(context, auto.counts).await;
        return PermissionDecision::Allow {
            updated_input: None,
        };
    }
    let verdict = classifier.classify(request).await;
    if verdict == ClassifierVerdict::NoVerdict(NoVerdict::Cancelled) {
        return deny(texts::CLASSIFIER_ABORTED.to_owned());
    }
    // 判的过程中换了模式或关了 plan：结论按旧状态，丢掉，退回原来的询问（新模式是 dontAsk 时拒绝），不计数。
    let now = Auto::read(context, settings).await;
    if !now.active {
        if now.mode == PermissionMode::DontAsk {
            return deny(texts::dont_ask_rejected(tool));
        }
        return asked.decision();
    }
    match verdict {
        ClassifierVerdict::Allow | ClassifierVerdict::NoVerdict(NoVerdict::NothingToJudge) => {
            cleared(context, now.counts).await;
            PermissionDecision::Allow {
                updated_input: None,
            }
        }
        ClassifierVerdict::Block { reason } => {
            let hint = rule_hint(tool, &asked.kind, settings);
            blocked(context, asked, texts::classifier_blocked(&reason, hint)).await
        }
        ClassifierVerdict::NoVerdict(NoVerdict::Unparseable) => {
            blocked(context, asked, texts::CLASSIFIER_UNAVAILABLE.to_owned()).await
        }
        ClassifierVerdict::NoVerdict(NoVerdict::Refused) => {
            deny(texts::CLASSIFIER_REFUSED.to_owned())
        }
        ClassifierVerdict::NoVerdict(NoVerdict::Unavailable(unavailable)) => {
            deny(texts::classifier_unavailable(tool, &unavailable))
        }
        // 对话太长：先问人（压缩之后再判在 C4，auto-classifier.md D-C2）。
        ClassifierVerdict::NoVerdict(NoVerdict::TranscriptTooLong) => asked.decision(),
        ClassifierVerdict::NoVerdict(NoVerdict::Cancelled) => {
            deny(texts::CLASSIFIER_ABORTED.to_owned())
        }
    }
}

/// 拦下：交 owner 计数；到了上限改为原来的询问，否则按 `message` 拒绝。
async fn blocked(context: &ToolUseContext, asked: Asked, message: String) -> PermissionDecision {
    match context
        .session_ops
        .note_auto_verdict(AutoVerdict::Blocked)
        .await
    {
        Some(limit) => {
            let (limit, count) = match limit {
                DenialLimit::Consecutive(count) => ("consecutive", count),
                DenialLimit::Total(count) => ("total", count),
            };
            tracing::info!(
                event = "policy.auto.denial_limit",
                limit,
                count,
                "too many actions were blocked by the auto mode classifier; asking the user"
            );
            asked.decision()
        }
        None => deny(message),
    }
}

/// 拦下时“可以加一条权限规则”提示的工具名：安全检查与 [`NO_RULE_HINT`] 不提示；Agent、Monitor 的规则在自动语义下不生效，不提示；
/// classifyAllShell 时 Bash 不提示；Write、NotebookEdit 的规则写在 Edit 下。
fn rule_hint<'a>(tool: &'a str, kind: &AskKind, settings: &AutoModeSettings) -> Option<&'a str> {
    if *kind != AskKind::Other || NO_RULE_HINT.contains(&tool) {
        return None;
    }
    match tool {
        tool_names::BASH if settings.classify_all_shell => None,
        tool_names::WRITE | tool_names::NOTEBOOK_EDIT => Some(tool_names::EDIT),
        tool_names::MONITOR => None,
        _ => Some(tool),
    }
}

fn deny(message: String) -> PermissionDecision {
    PermissionDecision::Deny { message }
}
