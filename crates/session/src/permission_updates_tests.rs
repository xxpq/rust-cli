//! 一组权限更新的 owner 是会话 actor（docs/specs/rust-cli/permission-updates.md §3、§4）：一批提交、成功后改会话层、模式与
//! 目录；失败时一项都不改；hook 先答时更新随回答在 `settle` 里落，用户先答时不落。

use std::sync::Arc;

use rust_cli_kernel_api::admission::Refusal;
use rust_cli_kernel_api::env::EnvPath;
use rust_cli_kernel_api::interaction::{Answer, PermissionAnswer};
use rust_cli_kernel_api::message::{ContentBlock, Message, ToolResultContent};
use rust_cli_kernel_api::orchestration::InteractionRequest;
use rust_cli_kernel_api::permission::{
    ExecutionState, PermissionMode, PermissionRule, RuleBehavior, RuleSource, SourcedRule,
};
use rust_cli_kernel_api::permission_updates::{PermissionUpdate, UpdatePlace};
use rust_cli_kernel_api::session_api::{Ack, Command, KernelEvent, SessionHandle};
use rust_cli_kernel_api::store::{Fact, LocalSettings};
use rust_cli_kernel_api::testing::{ScriptedModel, text_response, tool_use_response};
use rust_cli_kernel_api::tool::{
    PromptContext, Tool, ToolError, ToolOutput, ToolProgress, ToolUseContext,
};
use serde_json::{Value, json};
use tokio::sync::{Notify, mpsc};

use crate::test_support::{Setup, envelope, finished, submit, until};

fn rule(tool: &str, content: Option<&str>) -> PermissionRule {
    PermissionRule {
        tool: tool.to_owned(),
        content: content.map(str::to_owned),
    }
}

fn session_rule(behavior: RuleBehavior, rule: PermissionRule) -> SourcedRule {
    SourcedRule {
        source: RuleSource::Session,
        behavior,
        rule,
    }
}

fn update(updates: Vec<PermissionUpdate>) -> Command {
    Command::UpdatePermissions { updates }
}

/// 报告自己的工具上下文里的会话层（规则串，逗号分开）。
struct Layer;

#[async_trait::async_trait]
impl Tool for Layer {
    fn classifier_input(
        &self,
        _input: &serde_json::Value,
    ) -> rust_cli_kernel_api::classifier::ClassifierInput {
        rust_cli_kernel_api::classifier::ClassifierInput::Skip
    }

    fn name(&self) -> &str {
        "Layer"
    }

    fn input_schema(&self) -> Value {
        json!({"type": "object"})
    }

    fn description(&self, _context: &PromptContext) -> String {
        String::new()
    }

    async fn call(
        &self,
        _input: Value,
        context: ToolUseContext,
        _progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        let text = context
            .session_rules
            .iter()
            .map(|sourced| rust_cli_kernel_api::permission::syntax::format_rule(&sourced.rule))
            .collect::<Vec<_>>()
            .join(",");
        Ok(ToolOutput {
            content: vec![ToolResultContent::Text { text }],
            is_error: false,
            display: Value::Null,
        })
    }
}

fn results(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .flat_map(|message| match message {
            Message::User(user) => user.content.clone(),
            _ => Vec::new(),
        })
        .filter_map(|block| match block {
            ContentBlock::ToolResult { content, .. } => match content.as_slice() {
                [ToolResultContent::Text { text }] => Some(text.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

fn permission_facts(facts: &[Fact]) -> Vec<Fact> {
    facts
        .iter()
        .filter(|fact| {
            matches!(
                fact,
                Fact::PermissionRulesAdded { .. }
                    | Fact::PermissionRulesReplaced { .. }
                    | Fact::PermissionRulesRemoved { .. }
                    | Fact::DirectoriesRemembered { .. }
                    | Fact::DirectoriesForgotten { .. }
                    | Fact::ExecutionChanged { .. }
                    | Fact::ProjectModeSet { .. }
            )
        })
        .cloned()
        .collect()
}

/// 会话层只在内存：不提交事实；工具的上下文与视图都读到它。
#[tokio::test]
async fn the_session_layer_reaches_the_tools_and_the_view() {
    let model = Arc::new(ScriptedModel::new(vec![
        tool_use_response("toolu_1", "Layer", &json!({})),
        text_response(&["done"]),
    ]));
    let mut setup = Setup::new(model);
    setup.tools = vec![Arc::new(Layer)];
    let built = setup.build().await;
    let ack = built
        .session
        .send(envelope(
            "u1",
            update(vec![
                PermissionUpdate::AddRules {
                    place: UpdatePlace::Session,
                    behavior: RuleBehavior::Deny,
                    rules: vec![rule("Write", None)],
                },
                PermissionUpdate::AddRules {
                    place: UpdatePlace::Session,
                    behavior: RuleBehavior::Allow,
                    rules: vec![rule("Bash", Some("ls *"))],
                },
            ]),
        ))
        .await;
    assert!(matches!(ack, Ack::Accepted { .. }), "{ack:?}");
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "go")).await;
    until(&mut events, finished).await;
    let view = built.session.view().await.unwrap();
    assert_eq!(results(&view.messages), ["Write,Bash(ls *)"]);
    assert_eq!(
        view.session_rules,
        [
            session_rule(RuleBehavior::Deny, rule("Write", None)),
            session_rule(RuleBehavior::Allow, rule("Bash", Some("ls *"))),
        ]
    );
    let session = &view.meta.id;
    assert!(permission_facts(&built.store.facts(session)).is_empty());
}

/// 落在本机的规则与目录、模式在一批里提交；之后才改内存、发事件（先去掉的目录，再加的，再模式）。相对路径的目录不收。
#[tokio::test]
async fn local_updates_are_one_batch_before_memory_changes() {
    let model = Arc::new(ScriptedModel::new(Vec::new()));
    let mut setup = Setup::new(model);
    setup.additional_directories = vec![EnvPath::new("/other")];
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    let ack = built
        .session
        .send(envelope(
            "u1",
            update(vec![
                PermissionUpdate::AddRules {
                    place: UpdatePlace::Local,
                    behavior: RuleBehavior::Deny,
                    rules: vec![rule("Bash", Some("rm *"))],
                },
                PermissionUpdate::SetMode {
                    place: UpdatePlace::Local,
                    mode: PermissionMode::AcceptEdits,
                },
                PermissionUpdate::AddDirectories {
                    place: UpdatePlace::Local,
                    directories: vec![EnvPath::new("/extra"), EnvPath::new("relative")],
                },
                PermissionUpdate::RemoveDirectories {
                    place: UpdatePlace::Session,
                    directories: vec![EnvPath::new("/other")],
                },
            ]),
        ))
        .await;
    assert!(matches!(ack, Ack::Accepted { .. }), "{ack:?}");
    let seen = until(&mut events, |event| {
        matches!(event, KernelEvent::ModeChanged { .. })
    })
    .await;
    let order: Vec<&KernelEvent> = seen
        .iter()
        .filter(|event| {
            matches!(
                event,
                KernelEvent::DirectoryAdded { .. }
                    | KernelEvent::DirectoryRemoved { .. }
                    | KernelEvent::ModeChanged { .. }
            )
        })
        .collect();
    assert_eq!(
        order,
        [
            &KernelEvent::DirectoryRemoved {
                directory: EnvPath::new("/other")
            },
            &KernelEvent::DirectoryAdded {
                directory: EnvPath::new("/extra")
            },
            &KernelEvent::ModeChanged {
                mode: PermissionMode::AcceptEdits
            },
        ]
    );
    let view = built.session.view().await.unwrap();
    assert_eq!(view.additional_directories, [EnvPath::new("/extra")]);
    assert_eq!(view.mode, PermissionMode::AcceptEdits);
    assert!(view.session_rules.is_empty());
    let next = ExecutionState::default().with_mode(PermissionMode::AcceptEdits);
    assert_eq!(
        permission_facts(&built.store.facts(&view.meta.id)),
        [
            Fact::PermissionRulesAdded {
                behavior: RuleBehavior::Deny,
                rules: vec![rule("Bash", Some("rm *"))],
            },
            Fact::DirectoriesRemembered {
                directories: vec![EnvPath::new("/extra")],
            },
            Fact::ExecutionChanged { state: next },
            Fact::ProjectModeSet {
                mode: PermissionMode::AcceptEdits,
            },
        ]
    );
    let project = built
        .store
        .project_rules(&view.meta.workspace)
        .await
        .unwrap();
    assert_eq!(project.deny, [rule("Bash", Some("rm *"))]);
    assert_eq!(project.additional_directories, [EnvPath::new("/extra")]);
}

/// 提交失败：回执是拒绝，会话层、模式与目录一项都不改。
#[tokio::test]
async fn a_failed_commit_changes_nothing() {
    let model = Arc::new(ScriptedModel::new(Vec::new()));
    let mut setup = Setup::new(model);
    setup.stored = false;
    let built = setup.build().await;
    let ack = built
        .session
        .send(envelope(
            "u1",
            update(vec![
                PermissionUpdate::AddRules {
                    place: UpdatePlace::Session,
                    behavior: RuleBehavior::Allow,
                    rules: vec![rule("Read", None)],
                },
                PermissionUpdate::AddRules {
                    place: UpdatePlace::Local,
                    behavior: RuleBehavior::Allow,
                    rules: vec![rule("Glob", None)],
                },
                PermissionUpdate::SetMode {
                    place: UpdatePlace::Session,
                    mode: PermissionMode::AcceptEdits,
                },
            ]),
        ))
        .await;
    assert!(
        matches!(
            ack,
            Ack::Rejected {
                refusal: Refusal::Failed,
                ..
            }
        ),
        "{ack:?}"
    );
    let view = built.session.view().await.unwrap();
    assert!(view.session_rules.is_empty());
    assert_eq!(view.mode, PermissionMode::Default);
}

/// 问一次权限，同时扮演 PermissionRequest hook：`release` 放行后替用户回答“允许”，回答里带一条会话层的规则。
struct Granting {
    release: Arc<Notify>,
}

#[async_trait::async_trait]
impl Tool for Granting {
    fn classifier_input(
        &self,
        _input: &serde_json::Value,
    ) -> rust_cli_kernel_api::classifier::ClassifierInput {
        rust_cli_kernel_api::classifier::ClassifierInput::Skip
    }

    fn name(&self) -> &str {
        "Write"
    }

    fn input_schema(&self) -> Value {
        json!({"type": "object"})
    }

    fn description(&self, _context: &PromptContext) -> String {
        String::new()
    }

    async fn call(
        &self,
        _input: Value,
        context: ToolUseContext,
        _progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        let request = InteractionRequest::Permission {
            tool_use: context.tool_use.clone(),
            tool: "Write".to_owned(),
            input: json!({}),
            message: "may I?".to_owned(),
            suggestions: Vec::new(),
            rule_seed: None,
            asked_by: None,
            machine: Default::default(),
            once: false,
        };
        let hook = async {
            self.release.notified().await;
            let allow = Answer::Permission(PermissionAnswer::Allow {
                remember: vec![PermissionUpdate::AddRules {
                    place: UpdatePlace::Session,
                    behavior: RuleBehavior::Allow,
                    rules: vec![rule("Write", None)],
                }],
            });
            context
                .interactions
                .answer_ask(&context.tool_use, allow)
                .await
        };
        let (answer, _first) = tokio::join!(context.interactions.ask(request), hook);
        answer.map_err(|error| ToolError::Failed {
            message: error.to_string(),
        })?;
        Ok(ToolOutput {
            content: vec![ToolResultContent::Text {
                text: "ok".to_owned(),
            }],
            is_error: false,
            display: Value::Null,
        })
    }
}

/// hook 先答：更新随回答在 `settle` 里落；用户先答：hook 迟到的回答什么都不改。
#[tokio::test]
async fn only_the_hook_that_resolves_the_ask_brings_its_updates() {
    for hook_first in [true, false] {
        let model = Arc::new(ScriptedModel::new(vec![
            tool_use_response("toolu_1", "Write", &json!({})),
            text_response(&["done"]),
        ]));
        let release = Arc::new(Notify::new());
        let mut setup = Setup::new(model);
        setup.tools = vec![Arc::new(Granting {
            release: release.clone(),
        })];
        let built = setup.build().await;
        let mut events = built.session.subscribe(Some(0));
        built.session.send(submit("c1", "write it")).await;
        let seen = until(&mut events, |event| {
            matches!(event, KernelEvent::InteractionAsked { .. })
        })
        .await;
        let Some(KernelEvent::InteractionAsked { interaction, .. }) = seen.last() else {
            unreachable!("until stops at InteractionAsked");
        };
        if !hook_first {
            let answer = Answer::Permission(PermissionAnswer::Allow {
                remember: Vec::new(),
            });
            let resolve = Command::ResolveInteraction {
                interaction: interaction.clone(),
                answer,
            };
            built.session.send(envelope("r1", resolve)).await;
        }
        release.notify_one();
        until(&mut events, finished).await;
        let view = built.session.view().await.unwrap();
        let expected = if hook_first {
            vec![session_rule(RuleBehavior::Allow, rule("Write", None))]
        } else {
            Vec::new()
        };
        assert_eq!(view.session_rules, expected, "hook first: {hook_first}");
    }
}
