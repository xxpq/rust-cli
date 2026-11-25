//! 询问与 hook 的替答（`Interactions::answer_ask`，docs/specs/rust-cli/hooks.md §6.1）：会话 actor 按到达先后裁决，后到的得到“已回答”。

use std::sync::Arc;

use rust_cli_kernel_api::admission::Refusal;
use rust_cli_kernel_api::interaction::{Answer, PermissionAnswer};
use rust_cli_kernel_api::orchestration::InteractionRequest;
use rust_cli_kernel_api::session_api::{Ack, Command, KernelEvent, SessionHandle};
use rust_cli_kernel_api::testing::{ScriptedModel, text_response, tool_use_response};
use rust_cli_kernel_api::tool::{
    PromptContext, Tool, ToolError, ToolOutput, ToolProgress, ToolUseContext,
};
use serde_json::{Value, json};
use tokio::sync::Notify;

use crate::test_support::{Setup, envelope, finished, submit, until};

/// 问一次权限，同时扮演 PermissionRequest hook：`release` 放行后替用户回答“允许”。结果是询问的回答与替答有没有先到。
struct Racing {
    release: Arc<Notify>,
}

#[async_trait::async_trait]
impl Tool for Racing {
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
                remember: Vec::new(),
            });
            context
                .interactions
                .answer_ask(&context.tool_use, allow)
                .await
        };
        let (answer, first) = tokio::join!(context.interactions.ask(request), hook);
        let answer = answer.map_err(|error| ToolError::Failed {
            message: error.to_string(),
        })?;
        Ok(ToolOutput {
            content: vec![rust_cli_kernel_api::message::ToolResultContent::Text {
                text: json!({"answer": answer.answer.decision(), "hook_first": first}).to_string(),
            }],
            is_error: false,
            display: Value::Null,
        })
    }
}

fn deny(interaction: &rust_cli_kernel_api::ids::InteractionId) -> Command {
    Command::ResolveInteraction {
        interaction: interaction.clone(),
        answer: Answer::Permission(PermissionAnswer::Deny { feedback: None }),
    }
}

/// 开一轮、等到询问登记；返回会话、模型、事件流、交互 id 与 hook 的放行。
async fn asked() -> (
    crate::test_support::Built,
    Arc<ScriptedModel>,
    futures_util::stream::BoxStream<'static, rust_cli_kernel_api::session_api::SequencedEvent>,
    rust_cli_kernel_api::ids::InteractionId,
    Arc<Notify>,
) {
    let model = Arc::new(ScriptedModel::new(vec![
        tool_use_response("toolu_1", "Write", &json!({})),
        text_response(&["done"]),
    ]));
    let release = Arc::new(Notify::new());
    let mut setup = Setup::new(model.clone());
    setup.tools = vec![Arc::new(Racing {
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
    let interaction = interaction.clone();
    (built, model, events, interaction, release)
}

/// hook 先到：询问解决（前端收到 `InteractionResolved` 关框），用户迟到的回答被拒绝（v5 投成 `alreadyResolved`）。
#[tokio::test]
async fn the_hook_answers_first_and_the_late_user_answer_is_refused() {
    let (built, model, mut events, interaction, release) = asked().await;
    release.notify_one();
    until(&mut events, |event| {
        matches!(event, KernelEvent::InteractionResolved { interaction: resolved } if *resolved == interaction)
    })
    .await;
    let ack = built.session.send(envelope("d1", deny(&interaction))).await;
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
    let result = serde_json::to_string(&model.requests()[1].messages).unwrap();
    assert!(
        result.contains(r#"{\"answer\":\"allow\",\"hook_first\":true}"#),
        "{result}"
    );
}

/// 用户先到：询问按用户的回答解决，hook 迟到的替答是“已回答”，什么都不改。
#[tokio::test]
async fn the_user_answers_first_and_the_late_hook_changes_nothing() {
    let (built, model, mut events, interaction, release) = asked().await;
    let ack = built.session.send(envelope("d1", deny(&interaction))).await;
    assert!(matches!(ack, Ack::Accepted { .. }), "{ack:?}");
    release.notify_one();
    until(&mut events, finished).await;
    let result = serde_json::to_string(&model.requests()[1].messages).unwrap();
    assert!(
        result.contains(r#"{\"answer\":\"deny\",\"hook_first\":false}"#),
        "{result}"
    );
}
