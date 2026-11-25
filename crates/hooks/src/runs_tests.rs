//! 每个 hook 的运行事实（hooks.md §14.2）：来源、结局与原因的每一行（照 v4）、stderr 只在拦下时带、时刻、脱敏、只在 REPL 路径给。

use std::sync::Arc;

use rust_cli_kernel_api::hook_runs::{HookOrigin, HookRun, HookRunOutcome};
use rust_cli_kernel_api::hooks::{HookEventName, HookRunner};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::config::{HookSource, HooksConfig, HooksSettings};
use crate::test_support::{HOOK_CLOCK_BASE_MS, Script, ScriptedEnv, event, ports, script};
use crate::{HookEngine, PluginHooks};

fn settings(event: &str, commands: &[&str]) -> HooksSettings {
    let hooks: Vec<Value> = commands
        .iter()
        .map(|command| json!({"type": "command", "command": command}))
        .collect();
    let (parsed, warnings) =
        HooksSettings::from_json(&json!({"hooks": {event: [{"hooks": hooks}]}}));
    assert!(warnings.is_empty(), "{warnings:?}");
    parsed
}

fn engine(scripts: &[(&str, Script)], sources: Vec<(HookSource, HooksSettings)>) -> HookEngine {
    HookEngine::with_config(
        ports(Arc::new(ScriptedEnv::with(scripts))),
        HooksConfig {
            version: 1,
            sources,
            ..HooksConfig::default()
        },
    )
}

/// 一个用户层的 hook 跑一次这个事件，给出它的运行事实。
async fn one(name: HookEventName, payload: Value, script: Script) -> HookRun {
    let engine = engine(
        &[("hook.sh", script)],
        vec![(HookSource::User, settings(name.as_str(), &["hook.sh"]))],
    );
    let mut runs = engine
        .run(event(name, payload), CancellationToken::new())
        .await
        .runs;
    assert_eq!(runs.len(), 1, "{runs:?}");
    runs.remove(0)
}

async fn prompt(stdout: &str, stderr: &str, exit: i32) -> HookRun {
    one(
        HookEventName::UserPromptSubmit,
        json!({"prompt": "hi"}),
        script(stdout, stderr, exit),
    )
    .await
}

/// 修 bug（19 §47.3 R-HOOK-ROWS）：以前引擎只交合并后的结果，来源在合并设置时就丢了，界面画不出 hook 行。每个跑了的 hook 一项，
/// 带它来自的层；去重时来源跟着留下的定义（后出现的）。
#[tokio::test]
async fn every_hook_that_ran_carries_its_source() {
    let engine = engine(
        &[],
        vec![
            (
                HookSource::User,
                settings("UserPromptSubmit", &["user.sh", "both.sh"]),
            ),
            (
                HookSource::Project,
                settings("UserPromptSubmit", &["both.sh", "project.sh"]),
            ),
        ],
    );
    let (plugin, warnings) = PluginHooks::from_json(
        crate::test_support::plugin_context("policy", "/Users/dev/plugins/policy"),
        &[
            json!({"hooks": {"UserPromptSubmit": [{"hooks": [{"type": "command", "command": "plugin.sh"}]}]}}),
        ],
    );
    assert!(warnings.is_empty(), "{warnings:?}");
    let engine = engine.with_plugins(vec![plugin]);
    let runs = engine
        .run(
            event(HookEventName::UserPromptSubmit, json!({"prompt": "hi"})),
            CancellationToken::new(),
        )
        .await
        .runs;
    let shown: Vec<(usize, &str, &HookOrigin)> = runs
        .iter()
        .map(|run| (run.index, run.command.as_str(), &run.origin))
        .collect();
    let plugin = HookOrigin::Plugin {
        name: "policy".to_owned(),
    };
    assert_eq!(
        shown,
        [
            (0, "user.sh", &HookOrigin::User),
            (1, "both.sh", &HookOrigin::Project),
            (2, "project.sh", &HookOrigin::Project),
            (3, "plugin.sh", &plugin),
        ]
    );
    assert!(runs.iter().all(|run| run.outcome == HookRunOutcome::Success
        && run.reason.is_none()
        && run.stderr.is_none()));
}

/// 结局与原因照 v4（`blocked` 与 `blockReason`、退出码 2 的写法，hooks.md §14.1）：拦下时带原因与 stderr，别的结局都不带。
#[tokio::test]
async fn outcomes_and_reasons_follow_v4() {
    // 退出码 2：原因是 stderr，再是 stdout，再是缺省
    let run = prompt("", "  no secrets here  \n", 2).await;
    assert_eq!(run.outcome, HookRunOutcome::Blocked);
    assert_eq!(run.reason.as_deref(), Some("no secrets here"));
    assert_eq!(run.stderr.as_deref(), Some("no secrets here"));
    assert_eq!(
        prompt("only stdout", "", 2).await.reason.as_deref(),
        Some("only stdout")
    );
    let silent = prompt("", "", 2).await;
    assert_eq!(silent.reason.as_deref(), Some("Hook blocked execution"));
    assert_eq!(silent.stderr, None);
    // `continue: false` 加 `reason`，stderr 另写一行（E2E HK-UP-01 的 hook）
    let stopped = prompt(
        r#"{"continue": false, "reason": "hooks_prompt_block"}"#,
        "python3: can't open file '/Users/dev/test/a.py'\n",
        0,
    )
    .await;
    assert_eq!(stopped.outcome, HookRunOutcome::Blocked);
    assert_eq!(stopped.reason.as_deref(), Some("hooks_prompt_block"));
    assert_eq!(
        stopped.stderr.as_deref(),
        Some("python3: can't open file '/Users/dev/test/a.py'")
    );
    // `stopReason` 先于 `reason`；`decision: block` 没有原因时退到 `systemMessage`
    assert_eq!(
        prompt(
            r#"{"continue": false, "stopReason": "stop", "reason": "why"}"#,
            "",
            0
        )
        .await
        .reason
        .as_deref(),
        Some("stop")
    );
    assert_eq!(
        prompt(r#"{"decision": "block", "systemMessage": "said"}"#, "", 0)
            .await
            .reason
            .as_deref(),
        Some("said")
    );
    // 别的结局不带原因与 stderr
    let failed = prompt("", "boom", 1).await;
    assert_eq!(
        (failed.outcome, failed.reason, failed.stderr),
        (HookRunOutcome::Failed, None, None)
    );
    assert_eq!(
        prompt(r#"{"continue": "no"}"#, "", 0).await.outcome,
        HookRunOutcome::Failed
    );
    let fine = prompt("plain text", "warning only", 0).await;
    assert_eq!(
        (fine.outcome, fine.reason, fine.stderr),
        (HookRunOutcome::Success, None, None)
    );
}

#[tokio::test]
async fn tool_events_use_the_permission_reasons() {
    let pre = |stdout: &str| {
        one(
            HookEventName::PreToolUse,
            json!({"tool_name": "Bash", "tool_input": {}, "tool_use_id": "toolu_1"}),
            script(stdout, "", 0),
        )
    };
    let denied = pre(r#"{"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "deny", "permissionDecisionReason": "no rm"}}"#).await;
    assert_eq!(denied.outcome, HookRunOutcome::Blocked);
    assert_eq!(denied.reason.as_deref(), Some("no rm"));
    let allowed = pre(
        r#"{"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "allow"}}"#,
    )
    .await;
    assert_eq!(allowed.outcome, HookRunOutcome::Success);
    let request = one(
        HookEventName::PermissionRequest,
        json!({"tool_name": "Bash", "tool_input": {}}),
        script(
            r#"{"hookSpecificOutput": {"hookEventName": "PermissionRequest", "decision": {"behavior": "deny", "message": "not today"}}}"#,
            "",
            0,
        ),
    )
    .await;
    assert_eq!(request.outcome, HookRunOutcome::Blocked);
    assert_eq!(request.reason.as_deref(), Some("not today"));
}

/// Stop 的 `continue: false` 不算拦下（同 v4 的 `processHookOutput`：Stop 不进 `blockRequested`）；`decision: block` 算。
#[tokio::test]
async fn a_stop_hook_that_ends_the_turn_is_not_a_block() {
    let stop = |stdout: &str| {
        one(
            HookEventName::Stop,
            json!({"stop_hook_active": false}),
            script(stdout, "", 0),
        )
    };
    assert_eq!(
        stop(r#"{"continue": false, "stopReason": "done"}"#)
            .await
            .outcome,
        HookRunOutcome::Success
    );
    let blocked = stop(r#"{"decision": "block", "reason": "keep going"}"#).await;
    assert_eq!(blocked.outcome, HookRunOutcome::Blocked);
    assert_eq!(blocked.reason.as_deref(), Some("keep going"));
}

/// 超时与取消分开记（v4 的 `timed_out`、`cancelled`）；时刻是引擎的时钟，并发的 hook 各有各的。
#[tokio::test(start_paused = true)]
async fn timeouts_cancellation_and_times() {
    let slow = Script {
        delay_ms: 30,
        ..script("", "", 0)
    };
    let run = prompt_with(slow).await;
    assert_eq!(run.started_at, HOOK_CLOCK_BASE_MS);
    assert_eq!(run.ended_at, HOOK_CLOCK_BASE_MS + 30);
    let stuck = Script {
        delay_ms: 60_000,
        ..script("", "", 0)
    };
    let timed_out = {
        let (parsed, _) = HooksSettings::from_json(
            &json!({"hooks": {"UserPromptSubmit": [{"hooks": [{"type": "command", "command": "hook.sh", "timeout": 1}]}]}}),
        );
        engine(
            &[("hook.sh", stuck.clone())],
            vec![(HookSource::User, parsed)],
        )
        .run(
            event(HookEventName::UserPromptSubmit, json!({"prompt": "hi"})),
            CancellationToken::new(),
        )
        .await
        .runs
    };
    assert_eq!(timed_out[0].outcome, HookRunOutcome::TimedOut);
    let cancel = CancellationToken::new();
    let running = engine(
        &[("hook.sh", stuck)],
        vec![(HookSource::User, settings("UserPromptSubmit", &["hook.sh"]))],
    );
    let run = running.run(
        event(HookEventName::UserPromptSubmit, json!({"prompt": "hi"})),
        cancel.clone(),
    );
    tokio::pin!(run);
    tokio::select! {
        biased;
        _ = &mut run => panic!("the hook ended before it was cancelled"),
        () = tokio::time::sleep(std::time::Duration::from_millis(5)) => cancel.cancel(),
    }
    assert_eq!(run.await.runs[0].outcome, HookRunOutcome::Cancelled);
}

async fn prompt_with(script: Script) -> HookRun {
    one(
        HookEventName::UserPromptSubmit,
        json!({"prompt": "hi"}),
        script,
    )
    .await
}

/// stderr 与原因离开引擎之前脱敏、截到 4000（照 v4 的 `sanitizeHookDiagnostics`）。
#[tokio::test]
async fn blocked_output_is_masked_and_cut() {
    let run = prompt("", &format!("password=hunter2 {}", "e".repeat(5000)), 2).await;
    let stderr = run.stderr.unwrap();
    assert!(stderr.starts_with("password=•••• eee"), "{stderr}");
    assert_eq!(stderr.encode_utf16().count(), 4000);
    assert_eq!(run.reason.as_deref(), Some(stderr.as_str()));
}

/// “外部”路径的事件不给运行事实（v4 没有它们的行，hooks.md §14.2）。
#[tokio::test]
async fn outside_events_give_no_runs() {
    let engine = engine(
        &[],
        vec![(HookSource::User, settings("SessionEnd", &["hook.sh"]))],
    );
    let outcome = engine
        .run(
            event(HookEventName::SessionEnd, json!({"reason": "other"})),
            CancellationToken::new(),
        )
        .await;
    assert!(outcome.runs.is_empty());
}
