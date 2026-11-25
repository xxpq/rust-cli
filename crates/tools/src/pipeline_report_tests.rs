//! 管线交出 hook 的运行事实（hooks.md §14.3）：四个工具事件各交一次、带这次调用与工具名；PermissionRequest 与询问竞速、用户先答时交的是
//! 被取消的那一次；没有跑 hook 时不交。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rust_cli_kernel_api::env::WorkspaceRef;
use rust_cli_kernel_api::hook_runs::{HookInvocation, HookOrigin, HookRun, HookRunOutcome};
use rust_cli_kernel_api::hooks::{HookEvent, HookEventName, HookOutcome, HookRunner};
use rust_cli_kernel_api::message::ToolResultContent;
use rust_cli_kernel_api::orchestration::{PermissionGate, ToolExecutor};
use rust_cli_kernel_api::permission::PermissionDecision;
use rust_cli_kernel_api::testing::{OpsOverride, ScriptedInteractions, tool_use_context};
use rust_cli_kernel_api::tool::{
    PromptContext, Tool, ToolError, ToolOutput, ToolProgress, ToolUseContext,
};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::pipeline::Pipeline;

fn run(outcome: HookRunOutcome) -> HookRun {
    HookRun {
        index: 0,
        origin: HookOrigin::User,
        command: "hook.sh".to_owned(),
        status_message: None,
        outcome,
        reason: None,
        stderr: None,
        started_at: 1,
        ended_at: 2,
    }
}

/// 每个事件都跑了一个 hook；PermissionRequest 的那个被取消时如实交 `Cancelled`。
struct Hooks;

#[async_trait]
impl HookRunner for Hooks {
    async fn run(&self, event: HookEvent, cancel: CancellationToken) -> HookOutcome {
        let outcome = if event.name == HookEventName::PermissionRequest {
            // 用户先答（询问总在 hook 之前送出）：hook 被取消
            cancel.cancelled().await;
            HookRunOutcome::Cancelled
        } else {
            HookRunOutcome::Success
        };
        HookOutcome {
            runs: vec![run(outcome)],
            ..HookOutcome::default()
        }
    }
}

/// 记下交来的 hook 运行（别的方法照 `NoSession`）。
#[derive(Default)]
struct Recorder(Mutex<Vec<HookInvocation>>);

#[async_trait]
impl OpsOverride for Recorder {
    async fn hooks_ran(&self, invocation: HookInvocation) {
        self.0.lock().unwrap().push(invocation);
    }
}

/// 判决链一律询问。
struct AskGate;

#[async_trait]
impl PermissionGate for AskGate {
    async fn decide(&self, _t: &dyn Tool, _i: &Value, _c: &ToolUseContext) -> PermissionDecision {
        PermissionDecision::ask("asks", Vec::new())
    }
    async fn local_rules_committed(&self, _w: &WorkspaceRef) {}
    async fn session_rules_committed(
        &self,
        _: &rust_cli_kernel_api::ids::SessionId,
        _: &WorkspaceRef,
    ) {
    }
    async fn check_rules(
        &self,
        _t: &dyn Tool,
        _i: &Value,
        _c: &ToolUseContext,
    ) -> Option<PermissionDecision> {
        None
    }
}

/// `fail` 为真时失败的 Bash。
struct Bash {
    fail: bool,
}

#[async_trait]
impl Tool for Bash {
    fn classifier_input(
        &self,
        _input: &serde_json::Value,
    ) -> rust_cli_kernel_api::classifier::ClassifierInput {
        rust_cli_kernel_api::classifier::ClassifierInput::Skip
    }

    fn name(&self) -> &str {
        "Bash"
    }
    fn input_schema(&self) -> Value {
        json!({"type": "object", "properties": {"command": {"type": "string"}}})
    }
    fn description(&self, _context: &PromptContext) -> String {
        String::new()
    }
    async fn call(
        &self,
        _input: Value,
        _context: ToolUseContext,
        _progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        if self.fail {
            return Err(ToolError::Failed {
                message: "boom".to_owned(),
            });
        }
        Ok(ToolOutput {
            content: vec![ToolResultContent::Text {
                text: "ran".to_owned(),
            }],
            is_error: false,
            display: Value::Null,
        })
    }
}

async fn reported(hooks: Arc<dyn HookRunner>, fail: bool) -> Vec<(HookEventName, HookRunOutcome)> {
    let recorder = Arc::new(Recorder::default());
    let context = ToolUseContext {
        interactions: Arc::new(ScriptedInteractions::allowing()),
        session_ops: recorder.clone(),
        ..tool_use_context()
    };
    let (progress, _updates) = mpsc::channel(4);
    Pipeline::new(Arc::new(AskGate), hooks)
        .execute(
            Arc::new(Bash { fail }),
            json!({"command": "ls"}),
            context,
            progress,
        )
        .await;
    let invocations = recorder.0.lock().unwrap().clone();
    for invocation in &invocations {
        let tool = invocation.tool.as_ref().unwrap();
        assert_eq!(
            (tool.tool_use.as_str(), tool.name.as_str()),
            ("toolu_1", "Bash")
        );
    }
    invocations
        .into_iter()
        .map(|invocation| (invocation.event, invocation.runs[0].outcome))
        .collect()
}

/// 修 bug（19 §47.3 R-HOOK-ROWS）：以前工具事件的 hook 只交合并后的结果，界面没有 hook 行。
#[tokio::test]
async fn each_tool_event_reports_the_hooks_it_ran() {
    use HookEventName as E;
    assert_eq!(
        reported(Arc::new(Hooks), false).await,
        [
            (E::PreToolUse, HookRunOutcome::Success),
            (E::PermissionRequest, HookRunOutcome::Cancelled),
            (E::PostToolUse, HookRunOutcome::Success),
        ]
    );
    assert_eq!(
        reported(Arc::new(Hooks), true).await,
        [
            (E::PreToolUse, HookRunOutcome::Success),
            (E::PermissionRequest, HookRunOutcome::Cancelled),
            (E::PostToolUseFailure, HookRunOutcome::Success),
        ]
    );
}

/// 没有跑 hook 时不交。
#[tokio::test]
async fn nothing_is_reported_when_no_hook_ran() {
    assert!(
        reported(Arc::new(rust_cli_kernel_api::testing::NoHooks), false)
            .await
            .is_empty()
    );
}
