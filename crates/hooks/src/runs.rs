//! 每个 hook 的运行事实（docs/specs/rust-cli/hooks.md §14.2）：结局、拦下的原因与 stderr 照 v4 的写法（出处在 spec §14.1）。原始结果
//! 在解释之前看一次（[`Observed`]），解释拿走原始结果之后再拼成 [`HookRun`]。

use rust_cli_kernel_api::hook_runs::{HookRun, HookRunOutcome};
use rust_cli_kernel_api::hooks::HookEventName;
use serde_json::{Map, Value};

use crate::consts::BLOCKING_EXIT;
use crate::display;
use crate::matcher::Selected;
use crate::output::{self, Parsed};
use crate::result::{Behavior, Executed, HookResult, Outcome, PermissionRequestDecision, Raw};
use crate::run::Ran;

/// 拦下却没有给原因时的说明（v4 的 `Hook blocked execution`）。
const NO_REASON: &str = "Hook blocked execution";

/// 原始结果里运行事实要的那几样（解释之前取；stderr 与 stdout 是还没脱敏的预览）。
pub(crate) struct Observed {
    /// command hook 以退出码 2 结束：原因照 v4 取 stderr，再取 stdout（`createExitCodeBlockOutput`）。
    exit_two: bool,
    timed_out: bool,
    stderr: Option<String>,
    stdout: Option<String>,
    /// 输出是 JSON 时它的原因（照 v4 的 `blockReason`，只在拦下时用）。
    json_reason: Option<String>,
}

impl Observed {
    pub(crate) fn of(executed: &Executed, event: HookEventName) -> Self {
        let json = |parsed: Parsed| match parsed {
            Parsed::Json(json) => json_reason(&json, event),
            Parsed::Async | Parsed::Plain | Parsed::Invalid(_) => None,
        };
        match executed {
            Executed::Raw(Raw::Command {
                stdout,
                stderr,
                exit,
            }) => Self {
                exit_two: *exit == BLOCKING_EXIT,
                timed_out: false,
                stderr: display::cut(stderr),
                stdout: display::cut(stdout),
                json_reason: json(output::parse(stdout)),
            },
            Executed::Raw(Raw::Http { body, .. }) => Self {
                json_reason: json(output::parse_http(body)),
                ..Self::nothing()
            },
            Executed::Raw(Raw::TimedOut) => Self {
                timed_out: true,
                ..Self::nothing()
            },
            Executed::Raw(Raw::Cancelled | Raw::Backgrounded | Raw::Failed(_))
            | Executed::Result(_) => Self::nothing(),
        }
    }

    fn nothing() -> Self {
        Self {
            exit_two: false,
            timed_out: false,
            stderr: None,
            stdout: None,
            json_reason: None,
        }
    }
}

fn text(json: &Map<String, Value>, key: &str) -> Option<String> {
    json.get(key).and_then(Value::as_str).map(str::to_owned)
}

/// v4 的 `blockReason`：`continue: false`（Stop 除外）给 `stopReason ?? reason`，`decision: block` 给 `stopReason ?? reason ?? systemMessage`，
/// 之后是 PreToolUse 的 `permissionDecisionReason`、PermissionRequest 的 deny `message`。都没有时为 None（调用方用缺省的说明）。
fn json_reason(json: &Map<String, Value>, event: HookEventName) -> Option<String> {
    let stop = || text(json, "stopReason").or_else(|| text(json, "reason"));
    let mut reason = None;
    if json.get("continue") == Some(&Value::Bool(false)) && event != HookEventName::Stop {
        reason = stop();
    }
    if json.get("decision").and_then(Value::as_str) == Some("block") {
        reason = stop().or(reason).or_else(|| text(json, "systemMessage"));
    }
    let specific = json.get("hookSpecificOutput").and_then(Value::as_object);
    reason
        .or_else(|| {
            specific
                .filter(|specific| specific.contains_key("permissionDecision"))
                .and_then(|specific| text(specific, "permissionDecisionReason"))
        })
        .or_else(|| {
            specific
                .and_then(|specific| specific.get("decision"))
                .and_then(Value::as_object)
                .filter(|decision| decision.get("behavior").and_then(Value::as_str) == Some("deny"))
                .and_then(|decision| text(decision, "message"))
        })
}

/// 这个 hook 拦下了没有（照 v4 的 `blocked`：拦下的结果、拒绝的权限行为、PermissionRequest 的 deny、`continue: false`（Stop 除外））。
fn blocked(result: &HookResult, event: HookEventName) -> bool {
    result.outcome == Outcome::Blocking
        || result.behavior == Some(Behavior::Deny)
        || matches!(
            result.permission_request,
            Some(PermissionRequestDecision::Deny { .. })
        )
        || (result.prevent && event != HookEventName::Stop)
}

/// 拦下的原因：退出码 2 取 stderr → stdout；JSON 照 v4；别的拦下（`onError: "deny"`、prompt hook）是阻塞文字去掉 `[<命令>]: ` 的前缀。
fn reason(observed: &Observed, result: &HookResult) -> String {
    let given = if observed.exit_two {
        observed.stderr.clone().or_else(|| observed.stdout.clone())
    } else {
        observed.json_reason.clone().or_else(|| {
            result.blocking.as_ref().map(|blocking| {
                let prefix = format!("[{}]: ", blocking.command);
                blocking
                    .error
                    .strip_prefix(&prefix)
                    .unwrap_or(&blocking.error)
                    .to_owned()
            })
        })
    };
    display::shown(given.as_deref().unwrap_or(NO_REASON))
}

/// 一个跑完的 hook 的运行事实。`result` 是它解释之后的结果（`onError: "deny"` 已换成拦下）。
pub(crate) fn run(
    selected: &Selected,
    ran: &Ran,
    observed: &Observed,
    result: &HookResult,
    event: HookEventName,
) -> HookRun {
    let (outcome, reason, stderr) = if blocked(result, event) {
        let reason = reason(observed, result);
        let stderr = observed.stderr.as_deref().map(display::shown);
        (HookRunOutcome::Blocked, Some(reason), stderr)
    } else {
        let outcome = match result.outcome {
            Outcome::Cancelled if observed.timed_out => HookRunOutcome::TimedOut,
            Outcome::Cancelled => HookRunOutcome::Cancelled,
            Outcome::NonBlockingError => HookRunOutcome::Failed,
            Outcome::Success | Outcome::Blocking => HookRunOutcome::Success,
        };
        (outcome, None, None)
    };
    let status_message = match &selected.hook {
        crate::config::HookCommand::Command(command) => command.status_message.clone(),
        _ => None,
    };
    HookRun {
        index: ran.index,
        origin: selected.origin.clone(),
        command: display::command(&selected.hook),
        status_message,
        outcome,
        reason,
        stderr,
        started_at: ran.started_at,
        ended_at: ran.ended_at,
    }
}

#[cfg(test)]
#[path = "runs_tests.rs"]
mod tests;
