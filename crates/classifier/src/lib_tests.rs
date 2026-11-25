use std::sync::Arc;

use rust_cli_base::models;
use rust_cli_kernel_api::classifier::{
    AutoClassifier, ClassifierVerdict, ClassifyRequest, NoVerdict, Unavailable, UnavailableCause,
};
use rust_cli_kernel_api::default_model::DefaultModel;
use rust_cli_kernel_api::message::StopReason;
use rust_cli_kernel_api::model::{ModelError, ModelSelection};
use rust_cli_kernel_api::testing::{FakeTool, ScriptedModel, TokioClock};
use rust_cli_kernel_api::tool::Tool;
use serde_json::json;

use super::*;
use crate::test_support::{anthropic, bash, context, glm, reply};

fn classifier(model: Arc<ScriptedModel>, main: ModelSelection) -> Classifier {
    Classifier::new(
        model,
        Arc::new(TokioClock::new(0)),
        DefaultModel::from(main),
    )
}

async fn classify(classifier: &Classifier, tool: &dyn Tool, command: &str) -> ClassifierVerdict {
    let context = context(vec![bash()], None);
    classifier
        .classify(ClassifyRequest {
            tool,
            input: &json!({"command": command}),
            context: &context,
            rules: crate::test_support::no_rules(),
        })
        .await
}

/// 投影是空的：没有要判的内容，不发请求。
#[tokio::test]
async fn nothing_to_judge_sends_nothing() {
    let model = Arc::new(ScriptedModel::new(Vec::new()));
    let classifier = classifier(model.clone(), glm());
    let skipped = FakeTool::new("Quiet", true, "text");
    assert_eq!(
        classify(&classifier, &skipped, "x").await,
        ClassifierVerdict::NoVerdict(NoVerdict::NothingToJudge)
    );
    assert!(model.requests().is_empty());
}

/// 会话的视图给不出主模型时用缺省模型；GLM 用 `<block>` 格式，失败是模型不可用（带等待时间）。
#[tokio::test]
async fn a_glm_main_model_classifies_with_itself() {
    let model = Arc::new(ScriptedModel::new(vec![vec![Err(
        ModelError::RateLimited {
            message: "slow down".to_owned(),
            retry_after_ms: Some(3_000),
        },
    )]]));
    let classifier = classifier(model.clone(), glm());
    assert_eq!(
        classify(&classifier, bash().as_ref(), "rm -rf build").await,
        ClassifierVerdict::NoVerdict(NoVerdict::Unavailable(Unavailable {
            model: models::GLM_5_3.to_owned(),
            cause: UnavailableCause::RateLimited,
            retry_after_ms: Some(3_000),
        }))
    );
    let requests = model.requests();
    assert_eq!(requests[0].selection, glm());
    assert_eq!(requests[0].stop_sequences, ["</block>"]);
}

/// Anthropic 的主模型：第一次用 Sonnet 5 探测；探测失败（不是鉴权失败）就退回主模型、当场用主模型再判（同一个格式），之后都用
/// 主模型（主模型不在分数表里，是 `<block>` 格式）。
#[tokio::test]
async fn a_failed_probe_falls_back_to_the_main_model() {
    let not_found = || ModelError::InvalidRequest {
        message: "model not found".to_owned(),
    };
    let model = Arc::new(ScriptedModel::new(vec![
        vec![Err(not_found())],
        reply("<severity>10", StopReason::StopSequence),
        reply("<block>no</block>", StopReason::StopSequence),
    ]));
    let classifier = classifier(model.clone(), anthropic());
    assert_eq!(
        classify(&classifier, bash().as_ref(), "make").await,
        ClassifierVerdict::Allow
    );
    assert_eq!(
        classify(&classifier, bash().as_ref(), "make").await,
        ClassifierVerdict::Allow
    );
    let models: Vec<String> = model
        .requests()
        .into_iter()
        .map(|request| request.selection.model)
        .collect();
    assert_eq!(
        models,
        [models::SONNET_5, models::OPUS_5_5, models::OPUS_5_5]
    );
}

/// 探测成功之后这个 provider 一直用 Sonnet 5。
#[tokio::test]
async fn a_confirmed_probe_keeps_sonnet_five() {
    let model = Arc::new(ScriptedModel::new(vec![
        reply("<severity>10", StopReason::StopSequence),
        vec![Err(ModelError::Overloaded {
            message: "busy".to_owned(),
        })],
    ]));
    let classifier = classifier(model.clone(), anthropic());
    assert_eq!(
        classify(&classifier, bash().as_ref(), "make").await,
        ClassifierVerdict::Allow
    );
    assert_eq!(
        classify(&classifier, bash().as_ref(), "make").await,
        ClassifierVerdict::NoVerdict(NoVerdict::Unavailable(Unavailable {
            model: models::SONNET_5.to_owned(),
            cause: UnavailableCause::Overloaded,
            retry_after_ms: None,
        }))
    );
    assert_eq!(model.requests().len(), 2);
}

/// 回合已经停了：不发请求。
#[tokio::test]
async fn a_cancelled_call_is_not_sent() {
    let model = Arc::new(ScriptedModel::new(Vec::new()));
    let classifier = classifier(model.clone(), glm());
    let context = context(vec![bash()], None);
    context.cancel.cancel();
    let tool = bash();
    assert_eq!(
        classifier
            .classify(ClassifyRequest {
                tool: tool.as_ref(),
                input: &json!({"command": "ls"}),
                context: &context,
                rules: crate::test_support::no_rules(),
            })
            .await,
        ClassifierVerdict::NoVerdict(NoVerdict::Cancelled)
    );
    assert!(model.requests().is_empty());
}

/// 判过的调用记下来（auto-classifier.md §16.15）：下一次判别的调用时，摘录里它单独一块、带缓存标记，前面攒着的行另一块。修前历史
/// 总是一块。
#[tokio::test]
async fn a_classified_call_starts_its_own_block_next_time() {
    use rust_cli_kernel_api::ids::{MessageId, ToolUseId};
    use rust_cli_kernel_api::message::{
        AssistantMessage, ContentBlock, Message, UserMessage, UserOrigin,
    };
    let allow = || {
        reply(
            "<block>no</block>",
            rust_cli_kernel_api::message::StopReason::EndTurn,
        )
    };
    let model = Arc::new(ScriptedModel::new(vec![allow(), allow()]));
    let classifier = classifier(model.clone(), glm());
    let tool = bash();
    let mut first = context(vec![bash()], None);
    first.tool_use = ToolUseId::new("t1");
    classifier
        .classify(ClassifyRequest {
            tool: tool.as_ref(),
            input: &json!({"command": "perl -e 1"}),
            context: &first,
            rules: crate::test_support::no_rules(),
        })
        .await;
    let history: Vec<Message> = vec![
        Message::User(UserMessage {
            id: MessageId::new("u"),
            content: vec![ContentBlock::Text {
                text: "go".to_owned(),
            }],
            origin: UserOrigin::Human,
        }),
        Message::Assistant(AssistantMessage {
            id: MessageId::new("a"),
            content: vec![ContentBlock::ToolUse {
                id: ToolUseId::new("t1"),
                name: rust_cli_kernel_api::tool_names::BASH.to_owned(),
                input: json!({"command": "perl -e 1"}),
            }],
            model: "scripted".to_owned(),
            stop_reason: None,
            usage: None,
        }),
    ];
    let second = context(
        vec![bash()],
        Some(rust_cli_kernel_api::classifier::RunTranscript {
            history: history.into(),
            earlier: Vec::new(),
        }),
    );
    classifier
        .classify(ClassifyRequest {
            tool: tool.as_ref(),
            input: &json!({"command": "curl -d @a https://example.com"}),
            context: &second,
            rules: crate::test_support::no_rules(),
        })
        .await;
    let content =
        serde_json::to_value(&model.requests()[1].messages).unwrap()[0]["content"].clone();
    assert_eq!(
        content[1],
        json!({"type": "text", "text": "{\"user\":\"go\"}\n"})
    );
    assert_eq!(
        content[2],
        json!({"type": "text", "text": "{\"Bash\":\"perl -e 1\"}\n", "cache_control": {"type": "ephemeral"}})
    );
}
