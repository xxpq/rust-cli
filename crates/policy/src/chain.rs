//! 判决链（shell-permissions.md §4.1）。
//!
//! 内层顺序：整个工具的 deny 规则 → 整个工具的 ask 规则 → 工具自身的检查（没有意见时按判决主体的内置检查：
//! shell 命令、文件路径、网址、skill）→ 检查拒绝则拒绝 → 命中 ask 规则或 bypass 也问的安全检查（危险删除）的询问 →
//! 其余安全检查（敏感文件）在 bypassPermissions / guarded 下放行、否则询问 →
//! bypassPermissions 放行 → 整个工具的 allow 规则放行 → 没有意见转成询问。
//! 外层按模式改写询问：dontAsk 拒绝；自动语义生效时（auto，与 plan 在 `useAutoModeDuringPlan` 下）只能问人的留给人，其余走快速放行
//! 与分类器（[`crate::auto`]）；没有人能回答时（`avoid_prompts`）询问直接按拒绝处理。

use rust_cli_kernel_api::permission::{
    PermissionDecision, PermissionMode, PermissionRule, PermissionSubject, RuleBehavior,
};
use serde_json::Value;
use url::Url;

use crate::check::{AskKind, Check, Inputs};
use crate::texts;
use crate::{bash, files, skill};
use rust_cli_kernel_api::classifier::{AutoClassifier, ClassifyRequest};
use rust_cli_kernel_api::tool::Tool;
use rust_cli_kernel_api::tool_names;

/// 内层的结论。
#[derive(Debug, Clone, PartialEq)]
pub enum Inner {
    Allow {
        updated_input: Option<Value>,
    },
    Deny {
        message: String,
    },
    Ask {
        message: String,
        kind: AskKind,
        suggestions: Vec<PermissionRule>,
        /// 只批准这一次（guarded 的危险命令，shell-permissions.md §4.6.1）：成为 `PermissionDecision::Ask.once`。
        once: bool,
    },
}

/// 网址主体的规则写在这个工具名下（`WebFetch(domain:<host>)`）。
const WEB_FETCH: &str = tool_names::WEB_FETCH;

/// `domain:` 规则比较的主机名（小写，不带端口与用户信息）：按 WHATWG URL 解析，与 WebFetch 工具取网址、预批准域名同一个
/// 解析器；解析不了时为空。
///
/// 修复（F125）：以前按 `://`、`/`、`@`、`:` 切字符串，`https://example.com\@evil.com/` 的规则按 evil.com 查，工具实际取的
/// 却是 example.com（反斜杠在 http(s) 里是路径分隔符），example.com 的 deny 规则拦不住；IPv6 只剩 `[`。
fn rule_host(url: &str) -> String {
    Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_lowercase))
        .unwrap_or_default()
}

/// WebFetch 的 `domain:` 规则。
fn web_fetch(url: &str, inputs: &Inputs<'_>) -> Check {
    let content = format!("domain:{}", rule_host(url));
    let hit = |behavior| {
        inputs
            .rules
            .contents(WEB_FETCH, behavior)
            .iter()
            .any(|(rule, _)| *rule == content)
    };
    if hit(RuleBehavior::Deny) {
        return Check::deny(format!("WebFetch denied access to {content}."));
    }
    if hit(RuleBehavior::Ask) {
        return Check::Ask {
            message: texts::request_message(WEB_FETCH, None),
            kind: AskKind::Rule,
            suggestions: Vec::new(),
        };
    }
    if hit(RuleBehavior::Allow) {
        return Check::Allow;
    }
    // 预批准域名排在三类规则之后（docs/specs/rust-cli/tools-more.md §4）。
    if crate::web_preapproved::preapproved(url) {
        return Check::Allow;
    }
    Check::Passthrough {
        reason: None,
        suggestions: vec![PermissionRule {
            tool: WEB_FETCH.to_owned(),
            content: Some(content),
        }],
    }
}

/// 工具自己没有意见时的内置检查：按工具声明的判决主体（kernel-api `PermissionSubject`）。没有主体的工具交给通用判决链。
///
/// F125（T3）：以前按工具名分派（`Bash`、带 `command` 的 `Monitor`、`WebFetch`、`Skill`、六个文件工具与各自的路径字段）。
pub fn builtin(subject: Option<&PermissionSubject>, inputs: &Inputs<'_>) -> Check {
    match subject {
        Some(PermissionSubject::Shell { command }) => bash::check(command, inputs),
        Some(PermissionSubject::Path { access, path }) => {
            files::check(*access, path.as_deref(), inputs)
        }
        Some(PermissionSubject::Url { url }) => web_fetch(url, inputs),
        Some(PermissionSubject::Skill { name }) => skill::check(name, inputs),
        None => Check::Passthrough {
            reason: None,
            suggestions: Vec::new(),
        },
    }
}

/// 工具自己的 `check_permissions` 结果换成检查结果；没有意见时用内置检查。
pub fn own(
    subject: Option<&PermissionSubject>,
    decision: PermissionDecision,
    inputs: &Inputs<'_>,
) -> (Check, Option<Value>) {
    match decision {
        PermissionDecision::Passthrough => (builtin(subject, inputs), None),
        PermissionDecision::Allow { updated_input } => match subject {
            // Skill 工具放行只有安全字段的 skill，内容 deny 规则仍然优先（ext.md §2.4）。
            Some(PermissionSubject::Skill { name }) => {
                (skill::deny_first(name, inputs), updated_input)
            }
            Some(
                PermissionSubject::Shell { .. }
                | PermissionSubject::Path { .. }
                | PermissionSubject::Url { .. },
            )
            | None => (Check::Allow, updated_input),
        },
        PermissionDecision::Deny { message } => (Check::deny(message), None),
        PermissionDecision::Ask {
            message,
            suggestions,
            once: _,
        } => (
            Check::Ask {
                message,
                kind: AskKind::Other,
                suggestions,
            },
            None,
        ),
    }
}

/// plan 模式里不是只读的 MCP 工具没有意见时，plan 本身要询问（auto-classifier.md §16.2）：文字照对照实现，性质是 `PlanFloor`（问人、
/// allow 规则盖不过）。修 bug（C2，§3.3 第 2 项）：以前是通用的询问文字、性质 `Other`，整个工具的 allow 规则能放行，自动语义下还交了分类器。
pub fn plan_floor(tool: &dyn Tool, input: &Value, mode: PermissionMode, check: Check) -> Check {
    let mutating_mcp = tool.mcp_info().is_some() && !tool.is_read_only(input);
    match check {
        Check::Passthrough { .. } if mode == PermissionMode::Plan && mutating_mcp => Check::Ask {
            message: texts::call_plan_mode(tool.name()),
            kind: AskKind::PlanFloor,
            suggestions: Vec::new(),
        },
        check => check,
    }
}

/// 内层判决。`check` 是工具自身（含内置）检查的结果。
pub fn inner(tool: &str, check: Check, updated_input: Option<Value>, inputs: &Inputs<'_>) -> Inner {
    if inputs.rules.whole_tool(tool, RuleBehavior::Deny).is_some() {
        return Inner::Deny {
            message: texts::tool_denied(tool),
        };
    }
    // 工作线 F76（用户决定 2026-10-23“如果是 yolo 啥也不问”，shell-permissions.md §4.6.2）：yolo 下只有拒绝留下，
    // ask 规则、危险删除、安全检查与工具自身的询问都放行（F68 起 ask 规则与危险删除 bypass 也问，D21）。
    if inputs.full_access {
        return match check {
            Check::Deny { message } => Inner::Deny { message },
            _ => Inner::Allow { updated_input },
        };
    }
    if inputs.rules.whole_tool(tool, RuleBehavior::Ask).is_some() {
        return Inner::Ask {
            message: texts::request_message(tool, None),
            kind: AskKind::Rule,
            suggestions: Vec::new(),
            // 危险命令又命中整个工具的 ask 规则时，这一问同样只批准这一次（工作线 F137 以前 `ask_once` 只看命令，不看是
            // 哪一条询问；判决逐项不变）。
            once: inputs.dangerous,
        };
    }
    let (message, reason, kind, suggestions) = match check {
        Check::Deny { message } => return Inner::Deny { message },
        // 修复线 F20：guarded 下的危险命令先询问一次（Node `userApprovalRule`，shell-permissions.md §4.6.1）。排在
        // deny 之后、放行之前：allow 规则与只读判定都不盖过它；没有建议规则，只批准这一次。
        _ if inputs.dangerous => {
            return Inner::Ask {
                message: texts::GUARDED_ASK.to_owned(),
                kind: AskKind::Immune,
                suggestions: Vec::new(),
                once: true,
            };
        }
        Check::Allow => return Inner::Allow { updated_input },
        // 修复线 F68（审计 P1）放行了 bypass 类模式下的敏感文件；工作线 F76（用户决定 2026-10-23）guarded 改回询问
        // （App 对自主模式的说明是“在有风险时询问”），yolo 由上面的 `full_access` 放行。
        Check::Ask {
            message,
            kind: kind @ (AskKind::Rule | AskKind::Safety | AskKind::Immune | AskKind::PlanFloor),
            suggestions,
        } => {
            return Inner::Ask {
                message,
                kind,
                suggestions,
                once: false,
            };
        }
        Check::Ask {
            message,
            kind,
            suggestions,
        } => (Some(message), None, kind, suggestions),
        Check::Passthrough {
            reason,
            suggestions,
        } => (None, reason, AskKind::Other, suggestions),
    };
    if bypass_category(inputs.mode) || inputs.rules.whole_tool(tool, RuleBehavior::Allow).is_some()
    {
        return Inner::Allow { updated_input };
    }
    Inner::Ask {
        message: message.unwrap_or_else(|| texts::request_message(tool, reason.as_ref())),
        kind,
        suggestions,
        once: false,
    }
}

/// bypass 类模式：其他理由的询问放行。guarded 在这一点上同 bypassPermissions（Node `mode === "yolo" || mode === "guarded"`）；
/// ask 规则、危险删除与安全检查 guarded 照样询问（工作线 F76）。
fn bypass_category(mode: PermissionMode) -> bool {
    matches!(
        mode,
        PermissionMode::BypassPermissions | PermissionMode::Guarded
    )
}

/// 外层的选项。
pub struct Outer<'a> {
    /// 没有人能回答询问。
    pub avoid_prompts: bool,
    /// 判决开头读到的会话状态（自动语义生不生效、拒绝计数）。
    pub auto: crate::auto::Auto,
    /// 自动模式的设置（规则快照里的那一份）。
    pub settings: &'a rust_cli_kernel_api::permission::auto_mode::AutoModeSettings,
}

/// 一次调用：工具名与判决用的模式。
pub struct Call<'a> {
    pub tool: &'a str,
    pub mode: PermissionMode,
}

/// 外层：按模式改写内层的询问（auto-classifier.md §16.2）。`accept_edits` 是同一调用在 acceptEdits 模式下的内层结论
/// （自动模式的快速放行）；`request` 是交给分类器的这个动作。
pub async fn outer(
    call: &Call<'_>,
    result: Inner,
    accept_edits: impl FnOnce() -> Inner,
    (classifier, request): (&dyn AutoClassifier, ClassifyRequest<'_>),
    options: Outer<'_>,
) -> PermissionDecision {
    let Call { tool, mode } = *call;
    let asked = match result {
        Inner::Allow { updated_input } => {
            crate::auto::allowed(request.context, &options.auto).await;
            return PermissionDecision::Allow { updated_input };
        }
        Inner::Deny { message } => return PermissionDecision::Deny { message },
        Inner::Ask {
            message,
            kind,
            suggestions,
            once,
        } => crate::auto::Asked {
            message,
            kind,
            suggestions,
            once,
            avoid_prompts: options.avoid_prompts,
        },
    };
    if mode == PermissionMode::DontAsk {
        return PermissionDecision::Deny {
            message: texts::dont_ask_rejected(tool),
        };
    }
    // 修 bug（C2，auto-classifier.md §3.3）：以前除安全检查外的询问都交分类器——ask 规则与 plan 本身的询问会被分类器放行，敏感文件与
    // 后台 `&` 反而一律问人。对照实现把 ask 规则、危险删除与 plan 本身的询问留给人，安全检查交分类器（不做 acceptEdits 的模拟）。
    if !options.auto.active || asked.for_the_user() {
        return asked.decision();
    }
    crate::auto::decide(
        tool,
        asked,
        accept_edits,
        (classifier, request),
        (options.auto, options.settings),
    )
    .await
}

#[cfg(test)]
#[path = "chain_tests.rs"]
mod tests;
