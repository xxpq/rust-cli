//! 「完全访问」的回答（session-lifecycle.md §10.22，v5 的 `fullAccess`）：切模式与放行在会话 actor 的一次处理里。

use std::sync::{Arc, Mutex};

use futures_util::stream::BoxStream;
use rust_cli_base::sync::LockUnpoisoned;
use rust_cli_kernel_api::admission::Refusal;
use rust_cli_kernel_api::ids::InteractionId;
use rust_cli_kernel_api::interaction::{Answer, PermissionAnswer};
use rust_cli_kernel_api::orchestration::InteractionRequest;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::{Ack, Command, KernelEvent, SequencedEvent, SessionHandle};
use rust_cli_kernel_api::store::{Fact, StoreError};
use rust_cli_kernel_api::testing::{
    ScriptedModel, StoreCall, StoreHooks, text_response, tool_use_response,
};
use rust_cli_kernel_api::tool::{
    PromptContext, Tool, ToolError, ToolOutput, ToolProgress, ToolUseContext,
};
use serde_json::{Value, json};

use crate::test_support::{Built, Setup, envelope, finished, submit, until};

/// 问一次（权限或提问），把回答原样作为结果。
struct Asking {
    question: bool,
}

#[async_trait::async_trait]
impl Tool for Asking {
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
        _progress: tokio::sync::mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        let request = if self.question {
            InteractionRequest::Question {
                tool_use: context.tool_use.clone(),
                questions: json!([]),
                asked_by: None,
            }
        } else {
            InteractionRequest::Permission {
                tool_use: context.tool_use.clone(),
                tool: "Write".to_owned(),
                input: json!({}),
                message: "may I?".to_owned(),
                suggestions: Vec::new(),
                rule_seed: None,
                asked_by: None,
                machine: Default::default(),
                once: false,
            }
        };
        let answer =
            context
                .interactions
                .ask(request)
                .await
                .map_err(|error| ToolError::Failed {
                    message: error.to_string(),
                })?;
        Ok(ToolOutput {
            content: vec![rust_cli_kernel_api::message::ToolResultContent::Text {
                text: serde_json::to_string(&answer.answer).unwrap(),
            }],
            is_error: false,
            display: Value::Null,
        })
    }
}

/// 让带 `ExecutionChanged` 的那一批落库失败。
#[derive(Default)]
struct RefuseModeWrites {
    refused: Mutex<usize>,
}

#[async_trait::async_trait]
impl StoreHooks for RefuseModeWrites {
    async fn before(&self, call: StoreCall<'_>) -> Result<(), StoreError> {
        if let StoreCall::Commit(_, batch) = call
            && batch
                .facts
                .iter()
                .any(|fact| matches!(fact, Fact::ExecutionChanged { .. }))
        {
            *self.refused.lock_unpoisoned() += 1;
            return Err(StoreError::Storage {
                message: "refused by the test".to_owned(),
            });
        }
        Ok(())
    }
}

/// 一轮里问一次；返回问到时的交互 id。
async fn asked(
    question: bool,
    hooks: Option<Arc<dyn StoreHooks>>,
) -> (
    Built,
    Arc<ScriptedModel>,
    BoxStream<'static, SequencedEvent>,
    InteractionId,
) {
    let model = Arc::new(ScriptedModel::new(vec![
        tool_use_response("toolu_1", "Write", &json!({})),
        text_response(&["done"]),
    ]));
    let mut setup = Setup::new(model.clone());
    setup.tools = vec![Arc::new(Asking { question })];
    setup.store_hooks = hooks;
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
    let interaction = interaction.clone();
    (built, model, events, interaction)
}

fn full_access(
    id: &str,
    interaction: &InteractionId,
) -> rust_cli_kernel_api::session_api::CommandEnvelope {
    envelope(
        id,
        Command::ResolveSwitchingMode {
            interaction: interaction.clone(),
            mode: rust_cli_kernel_api::permission::PermissionMode::BypassPermissions,
        },
    )
}

/// 询问还在：先切到完全访问，再放行这一次；模式的变化排在询问解决之前。只改这个会话：落库的是执行状态，不写项目的持久模式
/// （主会话审定，照 v4：这个项目之后新建的会话仍是原来的模式）。
#[tokio::test]
async fn full_access_switches_the_mode_then_allows_the_call() {
    let (built, model, mut events, interaction) = asked(false, None).await;
    let ack = built.session.send(full_access("f1", &interaction)).await;
    assert!(matches!(ack, Ack::Accepted { .. }), "{ack:?}");
    let seen = until(&mut events, finished).await;
    let mode = seen.iter().position(|event| {
        matches!(
            event,
            KernelEvent::ModeChanged {
                mode: PermissionMode::BypassPermissions
            }
        )
    });
    let resolved = seen
        .iter()
        .position(|event| matches!(event, KernelEvent::InteractionResolved { .. }));
    assert!(mode.is_some() && mode < resolved, "{seen:?}");
    let view = built.session.view().await.unwrap();
    assert_eq!(view.mode, PermissionMode::BypassPermissions);
    let facts = built.store.facts(built.session.id());
    assert!(
        facts.iter().any(|fact| matches!(
            fact,
            Fact::ExecutionChanged { state } if state.mode == PermissionMode::BypassPermissions
        )),
        "{facts:?}"
    );
    assert!(
        !facts
            .iter()
            .any(|fact| matches!(fact, Fact::ProjectModeSet { .. })),
        "{facts:?}"
    );
    // 工具拿到的是「允许」（它把回答原样作为结果交给模型）
    let result = serde_json::to_string(&model.requests()[1].messages).unwrap();
    assert!(result.contains("allow"), "{result}");
}

/// 别的应答者先回答了：「完全访问」找不到询问，模式不变（输了的一方什么都不改）。
#[tokio::test]
async fn a_late_full_access_changes_nothing() {
    let (built, _model, mut events, interaction) = asked(false, None).await;
    let deny = envelope(
        "d1",
        Command::ResolveInteraction {
            interaction: interaction.clone(),
            answer: Answer::Permission(PermissionAnswer::Deny { feedback: None }),
        },
    );
    assert!(matches!(
        built.session.send(deny).await,
        Ack::Accepted { .. }
    ));
    let ack = built.session.send(full_access("f1", &interaction)).await;
    assert!(
        matches!(
            ack,
            Ack::Rejected {
                refusal: Refusal::NotFound,
                ..
            }
        ),
        "{ack:?}"
    );
    until(&mut events, finished).await;
    assert_eq!(
        built.session.view().await.unwrap().mode,
        PermissionMode::Default
    );
}

/// 切模式落库失败：询问留着、模式不变，可以再答。
#[tokio::test]
async fn a_failed_mode_write_keeps_the_ask() {
    let hooks = Arc::new(RefuseModeWrites::default());
    let (built, _model, mut events, interaction) = asked(false, Some(hooks.clone())).await;
    let ack = built.session.send(full_access("f1", &interaction)).await;
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
    assert_eq!(*hooks.refused.lock_unpoisoned(), 1);
    let view = built.session.view().await.unwrap();
    assert_eq!(view.mode, PermissionMode::Default);
    assert_eq!(view.pending.len(), 1);
    let allow = envelope(
        "a1",
        Command::ResolveInteraction {
            interaction,
            answer: Answer::Permission(PermissionAnswer::Allow {
                remember: Vec::new(),
            }),
        },
    );
    assert!(matches!(
        built.session.send(allow).await,
        Ack::Accepted { .. }
    ));
    until(&mut events, finished).await;
}

/// 「完全访问」只回答权限询问。
#[tokio::test]
async fn full_access_does_not_answer_a_question() {
    let (built, _model, _events, interaction) = asked(true, None).await;
    let ack = built.session.send(full_access("f1", &interaction)).await;
    assert!(
        matches!(
            ack,
            Ack::Rejected {
                refusal: Refusal::Unavailable,
                ..
            }
        ),
        "{ack:?}"
    );
    assert_eq!(built.session.view().await.unwrap().pending.len(), 1);
}

/// 放行并切到自主模式（session-lifecycle.md §10.59，v5 的 `allowAndSwitchGuarded`）：同一次处理里切到 guarded、放行这一次、发 `ModeGranted`，
/// 只改这个会话（不写项目的持久模式）。
#[tokio::test]
async fn switching_to_guarded_allows_the_call_in_the_same_step() {
    let (built, model, mut events, interaction) = asked(false, None).await;
    let guarded = envelope(
        "g1",
        Command::ResolveSwitchingMode {
            interaction: interaction.clone(),
            mode: PermissionMode::Guarded,
        },
    );
    let ack = built.session.send(guarded).await;
    assert!(matches!(ack, Ack::Accepted { .. }), "{ack:?}");
    let seen = until(&mut events, finished).await;
    assert!(seen.iter().any(|event| matches!(
        event,
        KernelEvent::ModeChanged {
            mode: PermissionMode::Guarded
        }
    )));
    assert!(
        seen.iter()
            .any(|event| matches!(event, KernelEvent::ModeGranted { .. }))
    );
    let view = built.session.view().await.unwrap();
    assert_eq!(view.mode, PermissionMode::Guarded);
    assert!(view.mode_grant.is_some());
    let facts = built.store.facts(built.session.id());
    assert!(
        !facts
            .iter()
            .any(|fact| matches!(fact, Fact::ProjectModeSet { .. })),
        "{facts:?}"
    );
    let result = serde_json::to_string(&model.requests()[1].messages).unwrap();
    assert!(result.contains("allow"), "{result}");
}
