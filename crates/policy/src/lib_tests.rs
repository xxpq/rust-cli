//! 判决链的每个分支：整个工具的规则、工具自身的检查、六种模式、分类器、没有人回答。

use std::sync::Arc;

use async_trait::async_trait;
use rust_cli_kernel_api::env::{EnvPath, OperationContext, Origin, WorkspaceRef};
use rust_cli_kernel_api::ids::{EnvironmentId, Generation, OperationId, RunId, SessionId};
use rust_cli_kernel_api::permission::RuleBehavior;
use rust_cli_kernel_api::testing::UnusedEnvironment;
use rust_cli_kernel_api::tool::{ToolError, ToolOutput, ToolProgress};
use serde_json::json;
use tokio::sync::mpsc;

use super::*;
use crate::test_support::rules;
use crate::texts;
use rust_cli_kernel_api::classifier::ClassifierVerdict;

/// 一个名字可选、自身检查可选的工具。
struct Named(&'static str, PermissionDecision);

#[async_trait]
impl Tool for Named {
    fn classifier_input(
        &self,
        _input: &serde_json::Value,
    ) -> rust_cli_kernel_api::classifier::ClassifierInput {
        rust_cli_kernel_api::classifier::ClassifierInput::Skip
    }

    fn name(&self) -> &str {
        self.0
    }
    fn input_schema(&self) -> Value {
        json!({"type": "object"})
    }
    fn description(&self, _context: &rust_cli_kernel_api::tool::PromptContext) -> String {
        String::new()
    }
    fn permission_subject(
        &self,
        input: &Value,
    ) -> Option<rust_cli_kernel_api::permission::PermissionSubject> {
        crate::test_support::subject_of(self.0, input)
    }
    async fn check_permissions(
        &self,
        _input: &Value,
        _context: &ToolUseContext,
    ) -> PermissionDecision {
        self.1.clone()
    }
    async fn call(
        &self,
        _input: Value,
        _context: ToolUseContext,
        _progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        Err(ToolError::Unsupported {
            what: "test".to_owned(),
        })
    }
}

fn context(mode: PermissionMode) -> ToolUseContext {
    ToolUseContext {
        session: SessionId::new("s1"),
        run: RunId::new("r1"),
        workspace: WorkspaceRef {
            environment: EnvironmentId::new("local"),
            root: EnvPath::new("/w"),
        },
        mode,
        interactions: Arc::new(rust_cli_kernel_api::testing::NoOneToAsk),
        session_ops: Arc::new(rust_cli_kernel_api::testing::NoSession),
        operation: OperationContext::new(
            EnvironmentId::new("local"),
            Generation::new("g"),
            OperationId::new("op"),
            Origin::Agent {
                session: SessionId::new("s1"),
                run: RunId::new("r1"),
            },
        ),
        ..rust_cli_kernel_api::testing::tool_use_context()
    }
}

/// 会话的替身（auto-classifier.md §16.2）：视图里的执行状态、主模型与拒绝计数由测试给，计数照 owner 的规则改。
pub(super) struct Live(std::sync::Mutex<rust_cli_kernel_api::session_ops::RunView>);

impl Live {
    pub(super) fn new(state: rust_cli_kernel_api::permission::ExecutionState) -> Arc<Self> {
        Arc::new(Self(std::sync::Mutex::new(
            rust_cli_kernel_api::session_ops::RunView::new(
                state,
                context(PermissionMode::Default).workspace,
            ),
        )))
    }

    fn with<T>(&self, f: impl FnOnce(&mut rust_cli_kernel_api::session_ops::RunView) -> T) -> T {
        use rust_cli_base::sync::LockUnpoisoned;
        f(&mut self.0.lock_unpoisoned())
    }

    pub(super) fn set(&self, state: rust_cli_kernel_api::permission::ExecutionState) {
        self.with(|view| view.execution = state);
    }

    pub(super) fn set_model(&self, model: &str) {
        self.with(|view| {
            view.selection = Some(rust_cli_kernel_api::model::ModelSelection::new(
                "provider", model,
            ));
        });
    }

    pub(super) fn counts(&self) -> rust_cli_kernel_api::permission::auto_mode::DenialCounts {
        self.with(|view| view.auto_denials)
    }

    pub(super) fn set_counts(
        &self,
        counts: rust_cli_kernel_api::permission::auto_mode::DenialCounts,
    ) {
        self.with(|view| view.auto_denials = counts);
    }

    /// 工具开工时读到的上下文：模式取视图这一刻的。
    pub(super) fn context(self: &Arc<Self>) -> ToolUseContext {
        ToolUseContext {
            mode: self.with(|view| view.mode()),
            session_ops: self.clone(),
            ..context(PermissionMode::Default)
        }
    }
}

#[async_trait]
impl rust_cli_kernel_api::testing::OpsOverride for Live {
    async fn view(&self) -> Option<rust_cli_kernel_api::session_ops::RunView> {
        Some(self.with(|view| view.clone()))
    }

    async fn note_auto_verdict(
        &self,
        verdict: rust_cli_kernel_api::permission::auto_mode::AutoVerdict,
    ) -> Option<rust_cli_kernel_api::permission::auto_mode::DenialLimit> {
        self.with(|view| {
            let (counts, limit) = view.auto_denials.record(verdict);
            view.auto_denials = counts;
            limit
        })
    }
}

async fn decide(
    gate: &PolicyGate,
    mode: PermissionMode,
    tool: &'static str,
    input: Value,
) -> PermissionDecision {
    gate.decide(
        &Named(tool, PermissionDecision::Passthrough),
        &input,
        &context(mode),
    )
    .await
}

async fn bash(gate: &PolicyGate, mode: PermissionMode, command: &str) -> PermissionDecision {
    decide(gate, mode, "Bash", json!({"command": command})).await
}

fn allow() -> PermissionDecision {
    PermissionDecision::Allow {
        updated_input: None,
    }
}

fn deny(message: impl Into<String>) -> PermissionDecision {
    PermissionDecision::Deny {
        message: message.into(),
    }
}

fn ask_message(decision: &PermissionDecision) -> &str {
    match decision {
        PermissionDecision::Ask { message, .. } => message,
        other => panic!("not ask: {other:?}"),
    }
}

const TOUCH_INSIDE: &str = "touch in '/w/x' needs approval. The path is inside the working directories for this session ('/w'), and ZCode asks before a shell command creates, changes or removes files there.";

#[tokio::test]
async fn default_mode_allows_read_only_and_asks_for_the_rest() {
    let gate = PolicyGate::default();
    assert_eq!(
        bash(&gate, PermissionMode::Default, "echo hi").await,
        allow()
    );
    let asked = bash(&gate, PermissionMode::Default, "touch x").await;
    assert_eq!(ask_message(&asked), TOUCH_INSIDE);
    let PermissionDecision::Ask { suggestions, .. } =
        bash(&gate, PermissionMode::Default, "npm --version").await
    else {
        panic!("ask");
    };
    // 第二个词不像子命令：精确规则（§4.10；以前是 `npm:*`）。
    assert_eq!(suggestions[0].content.as_deref(), Some("npm --version"));
}

#[tokio::test]
async fn dont_ask_turns_every_question_into_a_denial() {
    let gate = PolicyGate::default();
    assert_eq!(bash(&gate, PermissionMode::DontAsk, "ls").await, allow());
    assert_eq!(
        bash(&gate, PermissionMode::DontAsk, "touch x").await,
        deny(texts::dont_ask_rejected("Bash"))
    );
}

#[tokio::test]
async fn whole_tool_rules_come_first_and_last() {
    let denied = PolicyGate::new(rules(&[(RuleBehavior::Deny, "Bash")]));
    assert_eq!(
        bash(&denied, PermissionMode::BypassPermissions, "echo hi").await,
        deny("Permission to use Bash has been denied.")
    );
    let asked = PolicyGate::new(rules(&[(RuleBehavior::Ask, "Bash")]));
    assert_eq!(
        ask_message(&bash(&asked, PermissionMode::Guarded, "echo hi").await),
        "ZCode requested permissions to use Bash, but you haven't granted it yet."
    );
    // 工作线 F76（用户决定）：yolo 下 ask 规则也放行（以前询问）。
    assert_eq!(
        bash(&asked, PermissionMode::BypassPermissions, "echo hi").await,
        allow()
    );
    let allowed = PolicyGate::new(rules(&[(RuleBehavior::Allow, "Bash")]));
    assert_eq!(
        bash(&allowed, PermissionMode::Default, "touch x").await,
        allow()
    );
    assert_eq!(
        bash(&allowed, PermissionMode::Default, "mkdir -p a && cd a").await,
        allow()
    );
}

/// 工作线 F76（用户决定 2026-10-23“如果是 yolo 啥也不问”）：yolo 下只有 deny 规则生效，ask 规则放行（以前询问）。
#[tokio::test]
async fn yolo_keeps_only_deny_rules() {
    let gate = PolicyGate::new(rules(&[
        (RuleBehavior::Deny, "Bash(rm:*)"),
        (RuleBehavior::Ask, "Bash(git push:*)"),
    ]));
    let mode = PermissionMode::BypassPermissions;
    assert_eq!(bash(&gate, mode, "touch /etc/x").await, allow());
    assert_eq!(
        bash(&gate, mode, "rm -f a").await,
        deny(texts::bash_command_denied("rm -f a"))
    );
    assert_eq!(bash(&gate, mode, "git push").await, allow());
    assert!(matches!(
        bash(&gate, PermissionMode::Guarded, "git push").await,
        PermissionDecision::Ask { .. }
    ));
}

#[tokio::test]
async fn accept_edits_only_covers_the_working_directories() {
    let gate = PolicyGate::default();
    let mode = PermissionMode::AcceptEdits;
    assert_eq!(
        decide(&gate, mode, "Edit", json!({"file_path": "/w/src/a.rs"})).await,
        allow()
    );
    assert!(matches!(
        decide(&gate, mode, "Edit", json!({"file_path": "/tmp/a.rs"})).await,
        PermissionDecision::Ask { .. }
    ));
    assert_eq!(bash(&gate, mode, "mkdir a && touch a/b").await, allow());
    // 读工作目录以外要询问（任何模式）。
    assert_eq!(
        ask_message(
            &decide(
                &gate,
                PermissionMode::Default,
                "Read",
                json!({"file_path": "/etc/hosts"})
            )
            .await
        ),
        "ZCode requested permissions to read from /etc/hosts, but you haven't granted it yet."
    );
    assert_eq!(
        decide(
            &gate,
            PermissionMode::Default,
            "Glob",
            json!({"pattern": "*.rs"})
        )
        .await,
        allow()
    );
}

/// plan 里要询问的动作按自动语义交分类器（C2 起，auto-classifier.md §16.6；D6 时期询问用户）：这里的分类器是缺省的“判不了”，
/// 拦下；设置关掉 `useAutoModeDuringPlan` 时询问用户。
#[tokio::test]
async fn plan_mode_goes_to_the_classifier_unless_the_setting_is_off() {
    let gate = PolicyGate::default();
    assert_eq!(
        bash(&gate, PermissionMode::Plan, "echo planning").await,
        allow()
    );
    assert_eq!(
        bash(&gate, PermissionMode::Plan, "touch plan.txt").await,
        deny(texts::CLASSIFIER_UNAVAILABLE)
    );
    let mut off = rules(&[]);
    off.auto_mode.use_auto_mode_during_plan = false;
    assert!(matches!(
        bash(
            &PolicyGate::new(off),
            PermissionMode::Plan,
            "touch plan.txt"
        )
        .await,
        PermissionDecision::Ask { .. }
    ));
}

#[path = "lib_tests_more.rs"]
mod more;

#[path = "lib_tests_guarded.rs"]
mod guarded_chain;

#[path = "lib_tests_find.rs"]
mod find_paths;

#[path = "lib_tests_safety.rs"]
mod safety;

#[path = "lib_tests_deny_forms.rs"]
mod deny_forms;

#[path = "lib_tests_monitor.rs"]
mod monitor_chain;

#[path = "lib_tests_machine.rs"]
mod machine;

#[path = "lib_tests_allowed.rs"]
mod allowed;

#[path = "lib_tests_session_layer.rs"]
mod session_layer;

#[path = "lib_tests_auto.rs"]
mod auto_chain;
