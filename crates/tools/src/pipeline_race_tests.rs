//! PermissionRequest hooks 与询问竞速（docs/specs/rust-cli/hooks.md §6.1）：先到者胜，交互的 owner 裁决，后到的得到“已回答”。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use rust_cli_base::sync::LockUnpoisoned;
use rust_cli_kernel_api::env::WorkspaceRef;
use rust_cli_kernel_api::hooks::{HookEvent, HookOutcome};
use rust_cli_kernel_api::ids::{InteractionId, ToolUseId};
use rust_cli_kernel_api::orchestration::{InteractionAnswer, Interactions, OrchestrationError};
use rust_cli_kernel_api::testing::ScriptedInteractions;
use rust_cli_kernel_api::tool::PromptContext;
use serde_json::json;
use tokio::sync::{Notify, oneshot};
use tokio_util::sync::CancellationToken;

use super::*;

/// 等测试里的一步发生；旧的顺序（先跑完 hook 再问）在这里卡住，超时让用例失败而不是挂住。
const STEP: Duration = Duration::from_secs(10);

/// 回显 `command` 的工具。
struct Echo;

#[async_trait]
impl Tool for Echo {
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
        json!({"type": "object", "properties": {"command": {"type": "string"}}, "required": ["command"]})
    }
    fn description(&self, _context: &PromptContext) -> String {
        String::new()
    }
    async fn call(
        &self,
        input: Value,
        _context: ToolUseContext,
        _progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput {
            content: vec![ToolResultContent::Text {
                text: format!("ran {}", input["command"].as_str().unwrap_or_default()),
            }],
            is_error: false,
            display: Value::Null,
        })
    }
}

/// 判决链替身：总是询问；`rules` 是 `check_rules` 的固定值（只在 hook 改写之后用到）；记下读回 LocalSettings 层的次数。
#[derive(Default)]
struct Gate {
    rules: Option<PermissionDecision>,
    reloads: Mutex<u32>,
}

#[async_trait]
impl PermissionGate for Gate {
    async fn decide(&self, _t: &dyn Tool, _i: &Value, _c: &ToolUseContext) -> PermissionDecision {
        PermissionDecision::ask("chain asks", Vec::new())
    }
    async fn local_rules_committed(&self, _w: &WorkspaceRef) {
        *self.reloads.lock_unpoisoned() += 1;
    }
    async fn session_rules_committed(
        &self,
        _: &rust_cli_kernel_api::ids::SessionId,
        workspace: &WorkspaceRef,
    ) {
        self.local_rules_committed(workspace).await;
    }
    async fn check_rules(
        &self,
        _t: &dyn Tool,
        _i: &Value,
        _c: &ToolUseContext,
    ) -> Option<PermissionDecision> {
        self.rules.clone()
    }
}

type Reply = oneshot::Sender<Result<InteractionAnswer, OrchestrationError>>;

/// 交互 owner 的替身：登记询问，按到达先后裁决。用户的回答由测试经 [`Owner::user`] 给出。
#[derive(Default)]
struct Owner {
    pending: Mutex<Option<(ToolUseId, Reply)>>,
    asked: Mutex<Vec<InteractionRequest>>,
    changed: Notify,
    /// 有值时，hook 的替答到达之前这个回答先到（交互的 owner 先处理了它）。
    ahead: Mutex<Option<PermissionAnswer>>,
    /// hook 的替答（不论胜负）：回答里带的规则由 owner 与解决同一批落库（hooks.md §6.2）。
    hook_answers: Mutex<Vec<Answer>>,
    /// 没有人能回答时管线另请 owner 落的更新（permission-updates.md §5）。
    applied: Mutex<Vec<Vec<PermissionUpdate>>>,
}

impl Owner {
    /// 等到收到第 `count` 个询问。
    async fn asked(&self, count: usize) -> Vec<InteractionRequest> {
        let waiting = async {
            loop {
                let notified = self.changed.notified();
                let asked = self.asked.lock_unpoisoned().clone();
                if asked.len() >= count {
                    return asked;
                }
                notified.await;
            }
        };
        tokio::time::timeout(STEP, waiting)
            .await
            .expect("the question was never asked")
    }

    /// 用户回答；询问已不在时是“已回答”（false）。
    fn user(&self, answer: PermissionAnswer) -> bool {
        self.settle(Answer::Permission(answer))
    }

    fn settle(&self, answer: Answer) -> bool {
        let Some((_, reply)) = self.pending.lock_unpoisoned().take() else {
            return false;
        };
        let _ = reply.send(Ok(InteractionAnswer {
            interaction: InteractionId::new("interaction_owner"),
            answer,
        }));
        true
    }
}

#[async_trait]
impl Interactions for Owner {
    async fn ask(
        &self,
        request: InteractionRequest,
    ) -> Result<InteractionAnswer, OrchestrationError> {
        let (reply, answer) = oneshot::channel();
        *self.pending.lock_unpoisoned() = Some((request.tool_use().clone(), reply));
        self.asked.lock_unpoisoned().push(request);
        self.changed.notify_waiters();
        answer.await.map_err(|_| OrchestrationError::Closed)?
    }

    async fn cancel_ask(&self, _tool_use: &ToolUseId) {
        self.pending.lock_unpoisoned().take();
    }

    async fn answer_ask(&self, tool_use: &ToolUseId, answer: Answer) -> bool {
        self.hook_answers.lock_unpoisoned().push(answer.clone());
        if let Some(ahead) = self.ahead.lock_unpoisoned().take() {
            self.user(ahead);
        }
        let matches = self
            .pending
            .lock_unpoisoned()
            .as_ref()
            .is_some_and(|(asked, _)| asked == tool_use);
        matches && self.settle(answer)
    }

    async fn apply_permission_updates(
        &self,
        updates: Vec<PermissionUpdate>,
    ) -> Result<(), OrchestrationError> {
        self.applied.lock_unpoisoned().push(updates);
        Ok(())
    }

    fn lasting(&self) -> Option<Arc<dyn Interactions>> {
        None
    }
}

/// PermissionRequest hook 的替身：放行之前一直等（或直接给结论）；被取消时没有结论，记下取消。
#[derive(Default)]
struct Hook {
    outcome: HookOutcome,
    /// 有值时等它放行才给结论。
    gate: Option<Arc<Notify>>,
    runs: Mutex<u32>,
    cancelled: Mutex<bool>,
}

#[async_trait]
impl HookRunner for Hook {
    async fn run(&self, event: HookEvent, cancel: CancellationToken) -> HookOutcome {
        if event.name != HookEventName::PermissionRequest {
            return HookOutcome::default();
        }
        *self.runs.lock_unpoisoned() += 1;
        if let Some(gate) = &self.gate {
            tokio::select! {
                () = gate.notified() => {}
                () = cancel.cancelled() => {
                    *self.cancelled.lock_unpoisoned() = true;
                    return HookOutcome::default();
                }
            }
        }
        self.outcome.clone()
    }
}

fn allow(updated: Option<Value>) -> HookOutcome {
    HookOutcome {
        decision: Some(PermissionDecision::Allow {
            updated_input: None,
        }),
        updated_input: updated,
        ..HookOutcome::default()
    }
}

fn deny(message: &str) -> HookOutcome {
    HookOutcome {
        decision: Some(PermissionDecision::Deny {
            message: message.to_owned(),
        }),
        ..HookOutcome::default()
    }
}

fn context(interactions: Arc<dyn Interactions>) -> ToolUseContext {
    ToolUseContext {
        interactions,
        ..rust_cli_kernel_api::testing::tool_use_context()
    }
}

fn text(output: &ToolOutput) -> String {
    match &output.content[0] {
        ToolResultContent::Text { text } => text.clone(),
        other => panic!("{other:?}"),
    }
}

/// 在后台跑一次调用：测试同时扮演用户与 hook 的放行。
fn execute(
    rules: Option<PermissionDecision>,
    hook: Arc<Hook>,
    interactions: Arc<dyn Interactions>,
) -> tokio::task::JoinHandle<ToolExecution> {
    execute_with(
        Arc::new(Gate {
            rules,
            ..Gate::default()
        }),
        hook,
        interactions,
    )
}

/// 同 [`execute`]，判决链由测试给出（要看读回的次数时）。
fn execute_with(
    gate: Arc<Gate>,
    hook: Arc<Hook>,
    interactions: Arc<dyn Interactions>,
) -> tokio::task::JoinHandle<ToolExecution> {
    let pipeline = Pipeline::new(gate, hook);
    let (progress, _updates) = mpsc::channel(4);
    tokio::spawn(async move {
        pipeline
            .execute(
                Arc::new(Echo),
                json!({"command": "ls"}),
                context(interactions),
                progress,
            )
            .await
    })
}

async fn finished(call: tokio::task::JoinHandle<ToolExecution>) -> ToolExecution {
    tokio::time::timeout(STEP, call)
        .await
        .expect("the call did not finish")
        .expect("the call panicked")
}

/// 修前（hooks.md D10 的旧写法）：hook 阻塞时询问没有发出，用户不能答（19-accept.md R-PERMREQ-HOOK）。
#[tokio::test]
async fn the_user_answers_while_the_hook_still_runs() {
    let hook = Arc::new(Hook {
        outcome: deny("the hook would deny"),
        gate: Some(Arc::new(Notify::new())),
        ..Hook::default()
    });
    let owner = Arc::new(Owner::default());
    let call = execute(None, hook.clone(), owner.clone());
    owner.asked(1).await;
    assert!(owner.user(PermissionAnswer::Allow {
        remember: Vec::new()
    }));
    let execution = finished(call).await;
    assert_eq!(text(&execution.output), "ran ls");
    // 败者被结束，不比询问活得久。
    assert!(*hook.cancelled.lock_unpoisoned());
}

#[tokio::test]
async fn a_hook_that_answers_first_resolves_the_question_and_the_user_comes_too_late() {
    let gate = Arc::new(Notify::new());
    let hook = Arc::new(Hook {
        outcome: allow(None),
        gate: Some(gate.clone()),
        ..Hook::default()
    });
    let owner = Arc::new(Owner::default());
    let call = execute(None, hook.clone(), owner.clone());
    // 询问照常登记（前端照常弹框），hook 之后才给结论。
    let asked = owner.asked(1).await;
    assert!(
        matches!(&asked[0], InteractionRequest::Permission { message, .. } if message == "chain asks")
    );
    gate.notify_one();
    let execution = finished(call).await;
    assert_eq!(text(&execution.output), "ran ls");
    // hook 已经解决了询问：用户迟到的回答是“已回答”。
    assert!(!owner.user(PermissionAnswer::Deny { feedback: None }));
}

#[tokio::test]
async fn a_hook_deny_that_comes_first_rejects_the_call() {
    let gate = Arc::new(Notify::new());
    let hook = Arc::new(Hook {
        outcome: deny("Permission denied by hook"),
        gate: Some(gate.clone()),
        ..Hook::default()
    });
    let owner = Arc::new(Owner::default());
    let call = execute(None, hook, owner.clone());
    owner.asked(1).await;
    gate.notify_one();
    let execution = finished(call).await;
    assert!(execution.output.is_error);
    assert_eq!(text(&execution.output), "Permission denied by hook");
}

#[tokio::test]
async fn a_hook_without_a_decision_leaves_the_question_to_the_user() {
    let hook = Arc::new(Hook::default());
    let owner = Arc::new(Owner::default());
    let call = execute(None, hook, owner.clone());
    owner.asked(1).await;
    assert!(owner.user(PermissionAnswer::Deny { feedback: None }));
    let execution = finished(call).await;
    assert_eq!(text(&execution.output), REJECT_MESSAGE);
}

/// 没有人能回答（`-p`）：询问当场答 `Unavailable`，管线等 hook 的结论（不能询问时先等 hook）。
#[tokio::test]
async fn nobody_to_ask_waits_for_the_hook() {
    let hook = Arc::new(Hook {
        outcome: allow(Some(json!({"command": "pwd"}))),
        ..Hook::default()
    });
    let nobody = Arc::new(ScriptedInteractions::always(PermissionAnswer::Unavailable));
    let execution = finished(execute(None, hook, nobody.clone())).await;
    assert_eq!(text(&execution.output), "ran pwd");
    // 询问已经当场答了，hook 不再替答。
    assert!(nobody.settled().is_empty());
    // hook 也没有结论时：询问本身的文字。
    let nobody = Arc::new(ScriptedInteractions::always(PermissionAnswer::Unavailable));
    let execution = finished(execute(None, Arc::new(Hook::default()), nobody)).await;
    assert_eq!(text(&execution.output), "chain asks");
}

/// 修前：hook 改写的输入只过 schema，deny 规则拦不住它。
#[tokio::test]
async fn a_rewritten_input_goes_through_the_rules_again() {
    let hook = Arc::new(Hook {
        outcome: allow(Some(json!({"command": "rm -rf build"}))),
        ..Hook::default()
    });
    let rules = Some(PermissionDecision::Deny {
        message: "Permission to use Bash with command rm -rf build has been denied.".to_owned(),
    });
    let nobody = Arc::new(ScriptedInteractions::always(PermissionAnswer::Unavailable));
    let execution = finished(execute(rules, hook, nobody)).await;
    assert!(execution.output.is_error);
    assert_eq!(
        text(&execution.output),
        "Permission to use Bash with command rm -rf build has been denied."
    );
}

/// 改写之后规则要询问：用新输入再问一次，这一次不跑 hook；旧的询问已由 hook 解决。
#[tokio::test]
async fn a_rewritten_input_that_needs_asking_is_asked_again_without_hooks() {
    let gate = Arc::new(Notify::new());
    let hook = Arc::new(Hook {
        outcome: allow(Some(json!({"command": "git push"}))),
        gate: Some(gate.clone()),
        ..Hook::default()
    });
    let rules = Some(PermissionDecision::ask("rule asks", Vec::new()));
    let owner = Arc::new(Owner::default());
    let call = execute(rules, hook.clone(), owner.clone());
    owner.asked(1).await;
    gate.notify_one();
    let asked = owner.asked(2).await;
    assert!(matches!(
        &asked[1],
        InteractionRequest::Permission { message, input, .. }
            if message == "rule asks" && input["command"] == "git push"
    ));
    assert!(owner.user(PermissionAnswer::Allow {
        remember: Vec::new()
    }));
    let execution = finished(call).await;
    assert_eq!(text(&execution.output), "ran git push");
    assert_eq!(*hook.runs.lock_unpoisoned(), 1);
}

/// 改写之后要询问、却没有人能回答（`-p` 的后台子代理）：拒绝的文字是这次 ask 的文字，不是“不能询问”的那一段。
#[tokio::test]
async fn a_rewritten_input_that_nobody_can_approve_gets_the_ask_text() {
    let hook = Arc::new(Hook {
        outcome: allow(Some(json!({"command": "git push"}))),
        ..Hook::default()
    });
    let rules = Some(PermissionDecision::ask("rule asks", Vec::new()));
    let nobody = Arc::new(ScriptedInteractions::always(PermissionAnswer::NoPrompts));
    let execution = finished(execute(rules, hook, nobody)).await;
    assert_eq!(text(&execution.output), "rule asks");
}

/// hook 的替答之前，别的回答先到了 owner（替答回 false）：人的决定照样胜出；“没有人能回答”（`-p` 当场答的 `Unavailable`）不是决定，
/// 用 hook 的结论。修前后一种也用了 `Unavailable`，hook 的放行丢了。
#[tokio::test]
async fn an_answer_that_reaches_the_owner_just_before_the_hook_wins_only_if_it_is_a_decision() {
    for (ahead, expected) in [
        (PermissionAnswer::Unavailable, "ran ls"),
        (PermissionAnswer::Deny { feedback: None }, REJECT_MESSAGE),
    ] {
        let hook = Arc::new(Hook {
            outcome: allow(None),
            ..Hook::default()
        });
        let owner = Arc::new(Owner {
            ahead: Mutex::new(Some(ahead)),
            ..Owner::default()
        });
        let execution = finished(execute(None, hook, owner)).await;
        assert_eq!(text(&execution.output), expected);
    }
}

fn npm_test() -> PermissionRule {
    PermissionRule {
        tool: "Bash".to_owned(),
        content: Some("npm test".to_owned()),
    }
}

/// hook 放行并带要记住的规则（`updatedPermissions` 写得进去的部分，hooks.md §6.2）。
fn granting(updated: Option<Value>) -> HookOutcome {
    HookOutcome {
        remember: PermissionUpdate::always_allow(vec![npm_test()]),
        ..allow(updated)
    }
}

fn allow_remembering(rules: Vec<PermissionRule>) -> Answer {
    Answer::Permission(PermissionAnswer::Allow {
        remember: PermissionUpdate::always_allow(rules),
    })
}

/// 修前（D10）：hook 的 `updatedPermissions` 不生效。现在 hook 先到时规则随它的回答交给 owner（与解决同一批落库），判决链随后读回。
#[tokio::test]
async fn a_hook_that_answers_first_hands_its_rules_to_the_owner() {
    let gate = Arc::new(Gate::default());
    let owner = Arc::new(Owner::default());
    let hook = Arc::new(Hook {
        outcome: granting(None),
        ..Hook::default()
    });
    let execution = finished(execute_with(gate.clone(), hook, owner.clone())).await;
    assert_eq!(text(&execution.output), "ran ls");
    assert_eq!(
        *owner.hook_answers.lock_unpoisoned(),
        [allow_remembering(vec![npm_test()])]
    );
    assert_eq!(*gate.reloads.lock_unpoisoned(), 1);
}

/// 别的回答先到 owner：人的决定胜出，hook 的更新不落、不读回。“没有人能回答”时照 hook 的结论执行，管线另请 owner 落它的更新
/// （permission-updates.md §5）：修前（HOOKS-2）这时不写，`-p` 的结局取决于 hook 与 headless 谁先到 owner。
#[tokio::test]
async fn updates_are_applied_whenever_the_hook_conclusion_is_used() {
    for (ahead, expected, applied) in [
        (PermissionAnswer::Deny { feedback: None }, REJECT_MESSAGE, 0),
        (PermissionAnswer::Unavailable, "ran ls", 1),
    ] {
        let gate = Arc::new(Gate::default());
        let owner = Arc::new(Owner {
            ahead: Mutex::new(Some(ahead)),
            ..Owner::default()
        });
        let hook = Arc::new(Hook {
            outcome: granting(None),
            ..Hook::default()
        });
        let execution = finished(execute_with(gate.clone(), hook, owner.clone())).await;
        assert_eq!(text(&execution.output), expected);
        assert_eq!(owner.applied.lock_unpoisoned().len(), applied);
        assert_eq!(*gate.reloads.lock_unpoisoned(), applied as u32);
    }
    // 询问当场答了 `Unavailable`（`-p`）：hook 不再替答，等它的结论，另请 owner 落。
    let gate = Arc::new(Gate::default());
    let nobody = Arc::new(ScriptedInteractions::always(PermissionAnswer::Unavailable));
    let hook = Arc::new(Hook {
        outcome: granting(None),
        ..Hook::default()
    });
    let execution = finished(execute_with(gate.clone(), hook, nobody.clone())).await;
    assert_eq!(text(&execution.output), "ran ls");
    assert!(nobody.settled().is_empty());
    assert_eq!(
        nobody.applied(),
        PermissionUpdate::always_allow(vec![npm_test()])
    );
    assert_eq!(*gate.reloads.lock_unpoisoned(), 1);
}

/// 改写的输入在回答之前过关：被规则拒绝时，hook 的回答是拒绝、不带规则（规则只在这道关卡之后写，hooks.md §6.2）。修前先回答放行、
/// 带上规则，再查改写的输入。
#[tokio::test]
async fn a_rewrite_that_the_rules_deny_brings_no_rules() {
    let gate = Arc::new(Gate {
        rules: Some(PermissionDecision::Deny {
            message: "Permission to use Bash with command rm -rf build has been denied.".to_owned(),
        }),
        ..Gate::default()
    });
    let owner = Arc::new(Owner::default());
    let hook = Arc::new(Hook {
        outcome: granting(Some(json!({"command": "rm -rf build"}))),
        ..Hook::default()
    });
    let execution = finished(execute_with(gate.clone(), hook, owner.clone())).await;
    assert_eq!(
        text(&execution.output),
        "Permission to use Bash with command rm -rf build has been denied."
    );
    assert_eq!(
        *owner.hook_answers.lock_unpoisoned(),
        [Answer::Permission(PermissionAnswer::Deny {
            feedback: None
        })]
    );
    assert_eq!(*gate.reloads.lock_unpoisoned(), 0);
}

/// 改写的输入通过：执行新输入，规则随回答写。
#[tokio::test]
async fn a_rewrite_that_passes_keeps_the_rules() {
    let gate = Arc::new(Gate::default());
    let owner = Arc::new(Owner::default());
    let hook = Arc::new(Hook {
        outcome: granting(Some(json!({"command": "npm test"}))),
        ..Hook::default()
    });
    let execution = finished(execute_with(gate.clone(), hook, owner.clone())).await;
    assert_eq!(text(&execution.output), "ran npm test");
    assert_eq!(
        *owner.hook_answers.lock_unpoisoned(),
        [allow_remembering(vec![npm_test()])]
    );
    assert_eq!(*gate.reloads.lock_unpoisoned(), 1);
}

fn interrupting() -> HookOutcome {
    HookOutcome {
        interrupt: true,
        ..deny("stop the turn")
    }
}

/// 修前（D10）：`interrupt` 不生效。hook 的拒绝被用上时（hook 先到，或没有人能回答）执行结果要求中止整个回合；用户先答时丢掉。
#[tokio::test]
async fn an_interrupting_deny_asks_to_abort_the_turn_only_when_it_is_used() {
    let owner = Arc::new(Owner::default());
    let hook = Arc::new(Hook {
        outcome: interrupting(),
        ..Hook::default()
    });
    let execution = finished(execute(None, hook, owner)).await;
    assert_eq!(text(&execution.output), "stop the turn");
    assert!(execution.interrupt);

    let nobody = Arc::new(ScriptedInteractions::always(PermissionAnswer::Unavailable));
    let hook = Arc::new(Hook {
        outcome: interrupting(),
        ..Hook::default()
    });
    assert!(finished(execute(None, hook, nobody)).await.interrupt);

    let gate = Arc::new(Notify::new());
    let hook = Arc::new(Hook {
        outcome: interrupting(),
        gate: Some(gate),
        ..Hook::default()
    });
    let owner = Arc::new(Owner::default());
    let call = execute(None, hook, owner.clone());
    owner.asked(1).await;
    assert!(owner.user(PermissionAnswer::Allow {
        remember: Vec::new()
    }));
    let execution = finished(call).await;
    assert_eq!(text(&execution.output), "ran ls");
    assert!(!execution.interrupt);
}
