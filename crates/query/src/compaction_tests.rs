//! 主循环里的自动与反应式压缩（compact.md §4.1）：用假压缩器，检查提交的消息与下一次请求。
use rust_cli_base::sync::LockUnpoisoned;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rust_cli_kernel_api::abort::Abort;
use rust_cli_kernel_api::compaction::{CompactMeter, CompactProgress};
use rust_cli_kernel_api::ids::MessageId;
use rust_cli_kernel_api::message::{
    AttachmentMessage, CompactBoundary, CompactTrigger, ContentBlock, Message, SystemMessage,
    UserMessage, UserOrigin,
};
use rust_cli_kernel_api::metering::{CallStatus, Metered, ModelCall};
use rust_cli_kernel_api::model::{ApiRole, ModelError, ModelSelection, QuerySource};
use rust_cli_kernel_api::orchestration::{
    AutoCompactOutcome, CompactRequest, CompactResult, CompactTracking, Compactor, Interactions,
    OrchestrationError, QueryError, QueryStop, RunSink,
};
use rust_cli_kernel_api::testing::{RecordingSink, ScriptedModel, text_response};
use tokio_util::sync::CancellationToken;

use crate::run::tests::{parts, request, scope};

/// 自动压缩按脚本回答，像真的压缩器一样说进度（压缩了说开始，失败了说开始与失败）、交出一次摘要请求的计量
/// （[`summary_call`]）；反应式压缩同样交出计量，在 `reactive_fails` 为假时成功。记下收到的触发与初始 token 差。`blocks` 为真时每一步都到了阻塞线，记下阻塞线收到的压缩失败说明。
struct FakeCompactor {
    auto: Mutex<Vec<AutoCompactOutcome>>,
    /// 自动压缩的请求带的顶层会话（21 §14）。
    roots: Mutex<Vec<Option<rust_cli_kernel_api::ids::SessionId>>>,
    calls: Mutex<Vec<CompactTrigger>>,
    gaps: Mutex<Vec<Option<u64>>>,
    blocks: bool,
    failures: Mutex<Vec<Option<String>>>,
    reactive_fails: bool,
}

impl FakeCompactor {
    fn new(auto: Vec<AutoCompactOutcome>) -> Arc<Self> {
        Arc::new(Self {
            auto: Mutex::new(auto.into_iter().rev().collect()),
            roots: Mutex::default(),
            calls: Mutex::default(),
            gaps: Mutex::default(),
            blocks: false,
            failures: Mutex::default(),
            reactive_fails: false,
        })
    }

    fn reactive_failing() -> Arc<Self> {
        Arc::new(Self {
            auto: Mutex::default(),
            roots: Mutex::default(),
            calls: Mutex::default(),
            gaps: Mutex::default(),
            blocks: false,
            failures: Mutex::default(),
            reactive_fails: true,
        })
    }

    fn blocking(auto: Vec<AutoCompactOutcome>) -> Arc<Self> {
        Arc::new(Self {
            auto: Mutex::new(auto.into_iter().rev().collect()),
            roots: Mutex::default(),
            calls: Mutex::default(),
            gaps: Mutex::default(),
            blocks: true,
            failures: Mutex::default(),
            reactive_fails: false,
        })
    }
}

/// 假压缩器交出的摘要请求的计量：消息 id 带触发（`msg_summary_auto` / `msg_summary_reactive`）。
fn summary_call(trigger: CompactTrigger) -> ModelCall {
    ModelCall::new(
        MessageId::new(format!("msg_summary_{trigger:?}").to_lowercase()),
        QuerySource::Compact,
        ModelSelection::new("anthropic", "scripted"),
        1,
        2,
        CallStatus::Completed,
    )
}

fn result(trigger: CompactTrigger) -> CompactResult {
    CompactResult {
        boundary: CompactBoundary {
            id: MessageId::new("msg_boundary"),
            trigger,
            pre_tokens: 10,
            summary: MessageId::new("msg_summary"),
            preserved: None,
            discovered_tools: Vec::new(),
        },
        summary: UserMessage {
            id: MessageId::new("msg_summary"),
            content: vec![ContentBlock::Text {
                text: "SUMMARY".to_owned(),
            }],
            origin: UserOrigin::CompactSummary,
        },
        restored: vec![Message::Attachment(AttachmentMessage {
            id: MessageId::new("msg_file"),
            source: "file".to_owned(),
            text: "<system-reminder>\nFILE\n</system-reminder>".to_owned(),
            data: serde_json::Value::Null,
        })],
    }
}

#[async_trait]
impl Compactor for FakeCompactor {
    fn should_auto_compact(&self, _history: &[Message], _selection: &ModelSelection) -> bool {
        !self.auto.lock_unpoisoned().is_empty()
    }

    async fn auto_compact(
        &self,
        request: CompactRequest,
        _tracking: &mut CompactTracking,
        _cancel: CancellationToken,
    ) -> Result<AutoCompactOutcome, OrchestrationError> {
        self.roots.lock_unpoisoned().push(request.root);
        let mut auto = self.auto.lock_unpoisoned();
        Ok(auto.pop().unwrap_or(AutoCompactOutcome::NotNeeded))
    }

    async fn auto_compact_with(
        &self,
        request: CompactRequest,
        tracking: &mut CompactTracking,
        cancel: CancellationToken,
        progress: &dyn CompactProgress,
    ) -> Result<AutoCompactOutcome, OrchestrationError> {
        let outcome = self.auto_compact(request, tracking, cancel).await?;
        match &outcome {
            AutoCompactOutcome::Compacted { .. } => {
                progress.started(CompactTrigger::Auto).await;
                progress.metered(summary_call(CompactTrigger::Auto)).await;
            }
            AutoCompactOutcome::Failed { message } => {
                progress.started(CompactTrigger::Auto).await;
                progress.failed(CompactTrigger::Auto, message.clone()).await;
            }
            AutoCompactOutcome::NotNeeded | AutoCompactOutcome::Thrashing { .. } => {}
        }
        Ok(outcome)
    }

    async fn compact_metered(
        &self,
        request: CompactRequest,
        _cancel: CancellationToken,
        meter: &dyn CompactMeter,
    ) -> Result<CompactResult, OrchestrationError> {
        meter.metered(summary_call(request.trigger)).await;
        self.calls.lock_unpoisoned().push(request.trigger);
        self.gaps.lock_unpoisoned().push(request.initial_token_gap);
        if self.reactive_fails {
            return Err(OrchestrationError::Failed {
                message: "Error during compaction: overloaded".to_owned(),
            });
        }
        Ok(result(request.trigger))
    }

    fn blocking_limit(
        &self,
        _history: &[Message],
        _selection: &ModelSelection,
        failure: Option<&str>,
    ) -> Option<String> {
        self.failures
            .lock_unpoisoned()
            .push(failure.map(str::to_owned));
        self.blocks.then(|| "Prompt is too long".to_owned())
    }
}

fn texts(request: &rust_cli_kernel_api::model::ModelRequest) -> Vec<String> {
    request
        .messages
        .iter()
        .flat_map(|message| message.content.iter())
        .filter_map(|item| match &item.block {
            ContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn auto_compaction_commits_together_and_the_request_sees_only_the_active_history() {
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["done"])]));
    let compactor = FakeCompactor::new(vec![AutoCompactOutcome::Compacted {
        result: Box::new(result(CompactTrigger::Auto)),
    }]);
    let mut parts = parts(model.clone(), Vec::new());
    parts.compactor = compactor.clone();
    let request = request("old question");
    let (scope, sink) = scope(parts, &request, Abort::new());
    let stop = crate::run::run(scope, request).await.unwrap();
    assert_eq!(stop, QueryStop::Completed);
    let ids: Vec<String> = sink
        .messages()
        .iter()
        .map(|m| m.id().as_str().to_owned())
        .collect();
    assert_eq!(&ids[..3], ["msg_boundary", "msg_summary", "msg_file"]);
    assert!(matches!(
        sink.messages()[0],
        Message::System(SystemMessage::CompactBoundary(_))
    ));
    let sent = model.requests();
    assert_eq!(sent.len(), 1);
    let seen = texts(&sent[0]);
    assert!(seen.iter().any(|text| text == "SUMMARY"), "{seen:?}");
    assert!(
        !seen.iter().any(|text| text.contains("old question")),
        "{seen:?}"
    );
}

#[tokio::test]
async fn prompt_too_long_compacts_once_and_resends() {
    let model = Arc::new(ScriptedModel::new(vec![
        vec![Err(ModelError::PromptTooLong {
            message: "prompt is too long: 300 tokens > 200 maximum".to_owned(),
        })],
        text_response(&["after compaction"]),
    ]));
    let compactor = FakeCompactor::new(Vec::new());
    let mut parts = parts(model.clone(), Vec::new());
    parts.compactor = compactor.clone();
    let request = request("question");
    let (scope, sink) = scope(parts, &request, Abort::new());
    let stop = crate::run::run(scope, request).await.unwrap();
    assert_eq!(stop, QueryStop::Completed);
    assert_eq!(
        *compactor.calls.lock().unwrap(),
        vec![CompactTrigger::Reactive]
    );
    // 触发它的错误给出的差交给压缩核心（compact.md §3.1）。
    assert_eq!(*compactor.gaps.lock().unwrap(), vec![Some(100)]);
    let sent = model.requests();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[1].messages[0].role, ApiRole::User);
    assert!(texts(&sent[1]).iter().any(|text| text == "SUMMARY"));
    assert!(
        sink.messages()
            .iter()
            .any(|m| m.id().as_str() == "msg_boundary")
    );
}

/// 没有输出的窗口用满（provider-errors.md §3.7，照 Node 的 `createEmptyContextExceededError`）：当成上下文超限，这次请求的
/// 计量按失败提交（带已报告的用量与超窗），反应式压缩后重发，不发续写的提示。以前照输出上限续写，续写请求还是超窗。
#[tokio::test]
async fn a_blockless_reply_with_the_window_full_compacts_and_resends() {
    use rust_cli_kernel_api::message::{StopReason, Usage};
    use rust_cli_kernel_api::metering::{CallStatus, Metered};
    use rust_cli_kernel_api::model::ModelEvent;
    let reported = Usage {
        input_tokens: 80,
        output_tokens: 5,
        ..Usage::default()
    };
    let window_full = vec![
        Ok(ModelEvent::MessageStart {
            id: "msg_full".to_owned(),
            model: "scripted".to_owned(),
            usage: Usage {
                input_tokens: 80,
                ..Usage::default()
            },
        }),
        Ok(ModelEvent::MessageDelta {
            stop_reason: Some(StopReason::ModelContextWindowExceeded),
            usage: reported,
        }),
        Ok(ModelEvent::MessageStop),
    ];
    let model = Arc::new(ScriptedModel::new(vec![
        window_full,
        text_response(&["after compaction"]),
    ]));
    let compactor = FakeCompactor::new(Vec::new());
    let mut parts = parts(model.clone(), Vec::new());
    parts.compactor = compactor.clone();
    let request = request("question");
    let (scope, sink) = scope(parts, &request, Abort::new());
    let stop = crate::run::run(scope, request).await.unwrap();
    assert_eq!(stop, QueryStop::Completed);
    assert_eq!(
        *compactor.calls.lock().unwrap(),
        vec![CompactTrigger::Reactive]
    );
    let sent = model.requests();
    assert_eq!(sent.len(), 2);
    assert!(
        !texts(&sent[1])
            .iter()
            .any(|text| text.contains("Output token limit hit")),
        "{:?}",
        texts(&sent[1])
    );
    let Some((None, Metered::Model(call))) = sink.metered().into_iter().next() else {
        panic!("{:?}", sink.metered());
    };
    assert_eq!(call.status, CallStatus::Error);
    assert_eq!(call.usage, Some(reported));
    assert!(matches!(call.error, Some(ModelError::PromptTooLong { .. })));
}

#[tokio::test]
async fn a_second_prompt_too_long_in_the_same_round_ends_with_the_error() {
    let ptl = || {
        vec![Err(ModelError::PromptTooLong {
            message: "prompt is too long".to_owned(),
        })]
    };
    let model = Arc::new(ScriptedModel::new(vec![ptl(), ptl()]));
    let mut parts = parts(model, Vec::new());
    parts.compactor = FakeCompactor::new(Vec::new());
    let request = request("question");
    let (scope, _sink) = scope(parts, &request, Abort::new());
    let error = crate::run::run(scope, request).await.unwrap_err();
    assert!(matches!(
        error,
        QueryError::Model(ModelError::PromptTooLong { .. })
    ));
}

#[tokio::test]
async fn thrashing_ends_the_run_with_the_compactor_message() {
    let model = Arc::new(ScriptedModel::new(Vec::new()));
    let mut parts = parts(model.clone(), Vec::new());
    parts.compactor = FakeCompactor::new(vec![AutoCompactOutcome::Thrashing {
        message: "Autocompact is thrashing".to_owned(),
    }]);
    let request = request("question");
    let (scope, _sink) = scope(parts, &request, Abort::new());
    let stop = crate::run::run(scope, request).await.unwrap();
    assert_eq!(
        stop,
        QueryStop::ApiError {
            message: "Autocompact is thrashing".to_owned()
        }
    );
    assert!(model.requests().is_empty());
}

/// 到了阻塞线（compact.md §2.2）：不发请求，以 API 错误结束；这一步自动压缩失败的说明交给阻塞线。
#[tokio::test]
async fn the_blocking_line_ends_the_run_without_a_request() {
    let model = Arc::new(ScriptedModel::new(Vec::new()));
    let compactor = FakeCompactor::blocking(vec![AutoCompactOutcome::Failed {
        message: "Error during compaction: boom".to_owned(),
    }]);
    let mut parts = parts(model.clone(), Vec::new());
    parts.compactor = compactor.clone();
    let request = request("question");
    let (scope, _sink) = scope(parts, &request, Abort::new());
    let stop = crate::run::run(scope, request).await.unwrap();
    assert_eq!(
        stop,
        QueryStop::ApiError {
            message: "Prompt is too long".to_owned()
        }
    );
    assert!(model.requests().is_empty());
    assert_eq!(
        *compactor.failures.lock().unwrap(),
        vec![Some("Error during compaction: boom".to_owned())]
    );
}

/// 刚压缩过的那一步不看阻塞线；没到阻塞线时每一步照常查（没有失败说明）。
#[tokio::test]
async fn a_step_that_just_compacted_is_not_blocked() {
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["done"])]));
    let compactor = FakeCompactor::blocking(vec![AutoCompactOutcome::Compacted {
        result: Box::new(result(CompactTrigger::Auto)),
    }]);
    let mut parts = parts(model.clone(), Vec::new());
    parts.compactor = compactor.clone();
    let request = request("question");
    let (scope, _sink) = scope(parts, &request, Abort::new());
    let stop = crate::run::run(scope, request).await.unwrap();
    assert_eq!(stop, QueryStop::Completed);
    assert_eq!(model.requests().len(), 1);
    assert!(compactor.failures.lock().unwrap().is_empty());
}

#[tokio::test]
async fn each_step_checks_the_blocking_line() {
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["done"])]));
    let compactor = FakeCompactor::new(Vec::new());
    let mut parts = parts(model.clone(), Vec::new());
    parts.compactor = compactor.clone();
    let request = request("question");
    let (scope, _sink) = scope(parts, &request, Abort::new());
    assert_eq!(
        crate::run::run(scope, request).await.unwrap(),
        QueryStop::Completed
    );
    assert_eq!(*compactor.failures.lock().unwrap(), vec![None]);
}

/// 记下输出端收到的压缩进度，其余转给 [`RecordingSink`]。
#[derive(Default)]
struct ProgressSink {
    inner: RecordingSink,
    said: Mutex<Vec<String>>,
}

impl rust_cli_kernel_api::testing::OpsOverride for ProgressSink {}

/// 询问交给内层的记录输出端。
impl rust_cli_kernel_api::testing::AskOverride for ProgressSink {
    fn asks(&self) -> &dyn Interactions {
        &self.inner
    }
}

#[async_trait]
impl rust_cli_kernel_api::testing::SinkOverride for ProgressSink {
    fn sink(&self) -> &dyn RunSink {
        &self.inner
    }

    async fn compact_started(&self, trigger: CompactTrigger) {
        let committed = self.inner.messages().len();
        self.said
            .lock_unpoisoned()
            .push(format!("started {trigger:?} with {committed} committed"));
    }

    async fn compact_failed(&self, trigger: CompactTrigger, message: String) {
        self.said
            .lock_unpoisoned()
            .push(format!("failed {trigger:?}: {message}"));
    }
}

async fn said_by(
    model: Arc<ScriptedModel>,
    compactor: Arc<FakeCompactor>,
) -> (Result<QueryStop, QueryError>, Vec<String>) {
    let mut parts = parts(model, Vec::new());
    parts.compactor = compactor;
    let request = request("question");
    let (scope, _) = scope(parts, &request, Abort::new());
    let sink = Arc::new(ProgressSink::default());
    let scope = Arc::new(crate::RunScope {
        sink: sink.clone(),
        ..Arc::into_inner(scope).expect("only the test holds the scope")
    });
    let stop = crate::run::run(scope, request).await;
    let said = sink.said.lock_unpoisoned().clone();
    (stop, said)
}

/// 缺陷修复（compact.md §4.1）：主循环把压缩器说的进度转给输出端，不再在提交摘要的同一刻自己补一个开始事件。
#[tokio::test]
async fn auto_compaction_progress_reaches_the_sink_once_and_before_the_commit() {
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["done"])]));
    let compactor = FakeCompactor::new(vec![AutoCompactOutcome::Compacted {
        result: Box::new(result(CompactTrigger::Auto)),
    }]);
    let (stop, said) = said_by(model, compactor).await;
    assert_eq!(stop, Ok(QueryStop::Completed));
    assert_eq!(said, ["started Auto with 0 committed"]);
}

/// 自动压缩失败时输出端收到失败，回合照常继续。
#[tokio::test]
async fn a_failed_auto_compaction_is_said_and_the_turn_goes_on() {
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["done"])]));
    let compactor = FakeCompactor::new(vec![AutoCompactOutcome::Failed {
        message: "Error during compaction: overloaded".to_owned(),
    }]);
    let (stop, said) = said_by(model.clone(), compactor).await;
    assert_eq!(stop, Ok(QueryStop::Completed));
    assert_eq!(
        said,
        [
            "started Auto with 0 committed",
            "failed Auto: Error during compaction: overloaded"
        ]
    );
    assert_eq!(model.requests().len(), 1);
}

/// 反应式压缩失败也说失败，回合随后以上下文超限结束。
#[tokio::test]
async fn a_failed_reactive_compaction_is_said() {
    let model = Arc::new(ScriptedModel::new(vec![vec![Err(
        ModelError::PromptTooLong {
            message: "prompt is too long".to_owned(),
        },
    )]]));
    let (stop, said) = said_by(model, FakeCompactor::reactive_failing()).await;
    assert!(matches!(
        stop,
        Err(QueryError::Model(ModelError::PromptTooLong { .. }))
    ));
    assert_eq!(
        said,
        [
            "started Reactive with 0 committed",
            "failed Reactive: Error during compaction: overloaded"
        ]
    );
}

/// 计量的来源与消息 id，按提交顺序（`None` 是没有同批消息的计量）。
fn metered_calls(sink: &RecordingSink) -> Vec<(Option<String>, String, String)> {
    sink.metered()
        .into_iter()
        .filter_map(|(message, metered)| match metered {
            Metered::Model(call) => Some((
                message.map(|id| id.as_str().to_owned()),
                call.source.node_name().to_owned(),
                call.message.as_str().to_owned(),
            )),
            Metered::Tool(_) => None,
        })
        .collect()
}

/// 修 bug（usage.md §2.1.2）：自动压缩的摘要请求经本次运行的输出端写用量行（没有同批消息、排在主请求之前）。以前压缩器的
/// 计量被丢掉，`model_usage` 里没有压缩的行。
#[tokio::test]
async fn auto_compaction_meters_the_summary_request_through_the_run_sink() {
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["done"])]));
    let mut parts = parts(model, Vec::new());
    parts.compactor = FakeCompactor::new(vec![AutoCompactOutcome::Compacted {
        result: Box::new(result(CompactTrigger::Auto)),
    }]);
    let request = request("question");
    let (scope, sink) = scope(parts, &request, Abort::new());
    assert_eq!(
        crate::run::run(scope, request).await,
        Ok(QueryStop::Completed)
    );
    let calls = metered_calls(&sink);
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert_eq!(
        calls[0],
        (None, "compact".to_owned(), "msg_summary_auto".to_owned())
    );
    assert_eq!(calls[1].1, "main_turn");
}

/// 修 bug（usage.md §2.1.2）：反应式压缩带上计量端口（`compact_metered`），摘要请求的行排在超窗的那次请求与重发之间。以前主循环调
/// 不计量的 `compact`。
#[tokio::test]
async fn reactive_compaction_meters_the_summary_request_through_the_run_sink() {
    let model = Arc::new(ScriptedModel::new(vec![
        vec![Err(ModelError::PromptTooLong {
            message: "prompt is too long: 300 tokens > 200 maximum".to_owned(),
        })],
        text_response(&["after compaction"]),
    ]));
    let mut parts = parts(model, Vec::new());
    parts.compactor = FakeCompactor::new(Vec::new());
    let request = request("question");
    let (scope, sink) = scope(parts, &request, Abort::new());
    assert_eq!(
        crate::run::run(scope, request).await,
        Ok(QueryStop::Completed)
    );
    let sources: Vec<String> = metered_calls(&sink)
        .into_iter()
        .map(|(_, source, _)| source)
        .collect();
    assert_eq!(sources, ["main_turn", "compact", "main_turn"]);
    assert!(
        metered_calls(&sink).contains(&(
            None,
            "compact".to_owned(),
            "msg_summary_reactive".to_owned()
        )),
        "{:?}",
        metered_calls(&sink)
    );
}

/// 子代理里的压缩请求带顶层会话（21 §14）：摘要请求与压缩的 hook 按它经宿主取凭据；顶层会话自己的压缩为 None。
#[tokio::test]
async fn a_compaction_in_a_subagent_names_the_root_session() {
    for root in [
        Some(rust_cli_kernel_api::ids::SessionId::new("sess_root")),
        None,
    ] {
        let model = Arc::new(ScriptedModel::new(vec![text_response(&["done"])]));
        let compactor = FakeCompactor::new(vec![AutoCompactOutcome::Compacted {
            result: Box::new(result(CompactTrigger::Auto)),
        }]);
        let mut parts = parts(model, Vec::new());
        parts.compactor = compactor.clone();
        let mut request = request("old question");
        if root.is_some() {
            request.agent = Some(rust_cli_kernel_api::ids::AgentId::new("agent_1"));
        }
        request.root = root.clone();
        let (scope, _sink) = scope(parts, &request, Abort::new());
        crate::run::run(scope, request).await.unwrap();
        assert_eq!(compactor.roots.lock_unpoisoned().first(), Some(&root));
    }
}
