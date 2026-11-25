use std::sync::Arc;

use rust_cli_kernel_api::abort::{Abort, AbortReason};
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{EnvironmentId, Generation, MessageId, RunId, SessionId, ToolUseId};
use rust_cli_kernel_api::message::{
    ContentBlock, Message, ToolResultContent, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::model::{
    ApiRole, BlockDelta, ModelError, ModelPort, QuerySource, SystemBlock,
};
use rust_cli_kernel_api::orchestration::{QueryError, QueryEvent, QueryRequest, QueryStop};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::testing::{
    DirectExecutor, FixedContext, ListTools, MemoryStore, NeverCompact, NoHooks, RecordingSink,
    ScriptedModel, SequentialIds, UnusedEnvironment, selection, text_response, tool_use_chunks,
    tool_use_response,
};
use rust_cli_kernel_api::tool::Tool;
use serde_json::json;

use crate::{QueryParts, RunScope};

pub(crate) fn parts(model: Arc<dyn ModelPort>, tools: Vec<Arc<dyn Tool>>) -> QueryParts {
    let env = Arc::new(UnusedEnvironment);
    let ids: Arc<dyn rust_cli_kernel_api::clock::Ids> = Arc::new(SequentialIds::default());
    QueryParts {
        model,
        context: Arc::new(FixedContext(vec![SystemBlock {
            text: "system".to_owned(),
            cache_control: None,
        }])),
        tools: Arc::new(ListTools(tools)),
        executor: Arc::new(DirectExecutor),
        compactor: Arc::new(NeverCompact),
        hooks: Arc::new(NoHooks),
        ids: ids.clone(),
        clock: Arc::new(rust_cli_kernel_api::testing::FixedClock(0)),
        machines: rust_cli_kernel_api::testing::environments(env),
        artifacts: Arc::new(MemoryStore::default()),
        operations: rust_cli_kernel_api::env::Operations::new(Generation::new("g1"), ids),
        non_interactive: false,
        limits: crate::RequestLimits::default(),
        prefixes: None,
    }
}

pub(crate) fn request(text: &str) -> QueryRequest {
    QueryRequest::new(
        SessionId::new("sess_1"),
        RunId::new("run_1"),
        WorkspaceRef {
            environment: EnvironmentId::new("local"),
            root: EnvPath::new("/work"),
        },
        vec![Message::User(UserMessage {
            id: MessageId::new("msg_user"),
            content: vec![ContentBlock::Text {
                text: text.to_owned(),
            }],
            origin: UserOrigin::Human,
        })],
        selection(),
        PermissionMode::Default,
        QuerySource::MainThread,
    )
}

pub(crate) fn scope(
    parts: QueryParts,
    request: &QueryRequest,
    abort: Abort,
) -> (Arc<RunScope>, Arc<RecordingSink>) {
    let sink = Arc::new(RecordingSink::default());
    let scope = Arc::new(RunScope::new(request, sink.clone(), parts, abort));
    (scope, sink)
}

async fn run_once(
    model: Arc<ScriptedModel>,
    tools: Vec<Arc<dyn Tool>>,
    request: QueryRequest,
) -> (Result<QueryStop, QueryError>, Arc<RecordingSink>) {
    let (scope, sink) = scope(parts(model, tools), &request, Abort::new());
    (super::run(scope, request).await, sink)
}

#[tokio::test]
async fn a_text_reply_ends_the_loop_after_one_request() {
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["hel", "lo"])]));
    let (stop, sink) = run_once(model.clone(), Vec::new(), request("hi")).await;
    assert_eq!(stop.unwrap(), QueryStop::Completed);
    let deltas = sink
        .events()
        .iter()
        .filter(|event| matches!(event, QueryEvent::Delta { .. }))
        .count();
    assert_eq!(deltas, 2);
    let messages = sink.messages();
    let Some(Message::Assistant(assistant)) = messages.last() else {
        panic!("last message is not the assistant reply: {messages:?}");
    };
    assert_eq!(
        assistant.content,
        vec![ContentBlock::Text {
            text: "hello".to_owned()
        }]
    );
    let requests = model.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].system[0].text, "system");
    assert_eq!(requests[0].messages[0].role, ApiRole::User);
}

#[tokio::test]
async fn an_unknown_tool_gets_the_no_such_tool_result_and_the_model_continues() {
    let model = Arc::new(ScriptedModel::new(vec![
        tool_use_response("toolu_1", "Read", &json!({"file_path": "a"})),
        text_response(&["done"]),
    ]));
    let (stop, sink) = run_once(model.clone(), Vec::new(), request("read a")).await;
    assert_eq!(stop.unwrap(), QueryStop::Completed);
    let requests = model.requests();
    assert_eq!(requests.len(), 2);
    let roles: Vec<_> = requests[1]
        .messages
        .iter()
        .map(|message| message.role)
        .collect();
    assert_eq!(
        roles,
        vec![ApiRole::User, ApiRole::Assistant, ApiRole::User]
    );
    let messages = sink.messages();
    let Message::User(result) = &messages[1] else {
        panic!("expected the tool result: {messages:?}");
    };
    assert!(matches!(
        &result.content[0],
        ContentBlock::ToolResult { is_error: true, content, .. }
            if format!("{content:?}").contains("No such tool available: Read")
    ));
}

/// 工作线 F147（docs/specs/rust-cli/headless.md §3.7）：块一开始就转出 `BlockStart`（tool_use 带 ID 与名字），在它的
/// 增量之前；`message` 与增量的相同。
#[tokio::test]
async fn a_block_start_is_emitted_before_its_deltas() {
    let model = Arc::new(ScriptedModel::new(vec![
        tool_use_chunks("toolu_1", "Read", &["{\"file_path\":", "\"a\"}"]),
        text_response(&["done"]),
    ]));
    let (stop, sink) = run_once(model, Vec::new(), request("read a")).await;
    assert_eq!(stop.unwrap(), QueryStop::Completed);
    let streamed: Vec<QueryEvent> = sink
        .events()
        .into_iter()
        .filter(|event| {
            matches!(
                event,
                QueryEvent::BlockStart { .. } | QueryEvent::Delta { .. }
            )
        })
        .collect();
    let input = |text: &str| QueryEvent::Delta {
        message: 0,
        id: MessageId::new("msg_1"),
        index: 0,
        tool_use: Some(ToolUseId::new("toolu_1")),
        delta: BlockDelta::InputJson(text.to_owned()),
    };
    assert_eq!(
        streamed,
        vec![
            QueryEvent::BlockStart {
                message: 0,
                index: 0,
                block: ContentBlock::ToolUse {
                    id: ToolUseId::new("toolu_1"),
                    name: "Read".to_owned(),
                    input: json!({}),
                },
            },
            input("{\"file_path\":"),
            input("\"a\"}"),
            QueryEvent::BlockStart {
                message: 1,
                index: 0,
                block: ContentBlock::Text {
                    text: String::new()
                },
            },
            QueryEvent::Delta {
                message: 1,
                // 第二条 assistant：msg_2 是工具结果
                id: MessageId::new("msg_3"),
                index: 0,
                tool_use: None,
                delta: BlockDelta::Text("done".to_owned()),
            },
        ]
    );
}

#[tokio::test]
async fn max_turns_stops_before_another_request() {
    let model = Arc::new(ScriptedModel::new(vec![tool_use_response(
        "toolu_1",
        "Read",
        &json!({}),
    )]));
    let mut limited = request("go");
    limited.max_turns = Some(1);
    let (stop, _sink) = run_once(model.clone(), Vec::new(), limited).await;
    assert_eq!(stop.unwrap(), QueryStop::MaxTurns);
    assert_eq!(model.requests().len(), 1);
}

#[tokio::test]
async fn model_errors_propagate_and_a_cancelled_stream_appends_the_interruption() {
    let failing = Arc::new(ScriptedModel::new(vec![vec![Err(
        ModelError::Overloaded {
            message: "busy".to_owned(),
        },
    )]]));
    let (stop, _sink) = run_once(failing, Vec::new(), request("x")).await;
    assert!(matches!(
        stop.unwrap_err(),
        QueryError::Model(ModelError::Overloaded { .. })
    ));

    let abort = Abort::new();
    abort.abort(AbortReason::User);
    let cancelled = Arc::new(ScriptedModel::new(vec![vec![Err(ModelError::Cancelled)]]));
    let request = request("x");
    let (scope, sink) = scope(parts(cancelled.clone(), Vec::new()), &request, abort);
    // 已经中止：不发请求，但写中断标记（修复线 F37，query.md §1.2；以前什么都不写）。
    assert_eq!(
        super::run(scope, request).await.unwrap(),
        QueryStop::Aborted
    );
    assert!(cancelled.requests().is_empty());
    let messages = sink.messages();
    assert!(
        matches!(
            &messages[..],
            [Message::User(UserMessage { content, origin: UserOrigin::Human, .. })]
                if content == &[ContentBlock::Text { text: "[Request interrupted by user]".to_owned() }]
        ),
        "{messages:?}"
    );
}

#[tokio::test]
async fn steering_input_joins_the_turn_at_the_tool_boundary() {
    let model = Arc::new(ScriptedModel::new(vec![
        tool_use_response("toolu_1", "Read", &json!({})),
        text_response(&["done"]),
    ]));
    let first = request("go");
    let (run_scope, sink) = scope(parts(model.clone(), Vec::new()), &first, Abort::new());
    sink.steer("also check b");
    assert_eq!(
        super::run(run_scope, first).await.unwrap(),
        QueryStop::Completed
    );
    let messages = sink.messages();
    let Some(Message::Attachment(queued)) = messages.get(2) else {
        panic!("expected the queued_command attachment after the tool result: {messages:?}");
    };
    assert_eq!(queued.source, "queued_command");
    assert_eq!(
        queued.data,
        json!({"prompt": "also check b", "commandMode": "prompt"})
    );
    // 测试模型不支持 system turn：插话按 user 角色带包装，并进前面的工具结果
    //（context.md §3 第 9 条）。
    let requests = model.requests();
    let last = requests[1].messages.last().unwrap();
    let ContentBlock::ToolResult { content, .. } = &last.content.last().unwrap().block else {
        panic!("expected the tool result: {last:?}");
    };
    let [ToolResultContent::Text { text }] = content.as_slice() else {
        panic!("expected one text in the tool result: {content:?}");
    };
    assert!(
        text.contains(
            "</tool_use_error>\n\n<system-reminder>\nThe user sent a new message while you were working:\nalso check b\n\n"
        ),
        "{text}"
    );

    // 没有工具边界：插话留在队列里，由会话在回合结束后处理。
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["hi"])]));
    let second = request("go");
    let (run_scope, sink) = scope(parts(model, Vec::new()), &second, Abort::new());
    sink.steer("later");
    super::run(run_scope, second).await.unwrap();
    assert!(
        !sink
            .messages()
            .iter()
            .any(|message| matches!(message, Message::Attachment(_)))
    );
}

/// 先给一条重试通知、再给响应的模型（传输层等待后重发时的形状）。
struct RetriesOnce(ScriptedModel);

impl ModelPort for RetriesOnce {
    fn stream(
        &self,
        request: rust_cli_kernel_api::model::ModelRequest,
        cancel: tokio_util::sync::CancellationToken,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<rust_cli_kernel_api::model::ModelEvent, ModelError>,
    > {
        self.0.stream(request, cancel)
    }

    fn stream_with_notices(
        &self,
        request: rust_cli_kernel_api::model::ModelRequest,
        cancel: tokio_util::sync::CancellationToken,
    ) -> futures_util::stream::BoxStream<
        'static,
        Result<rust_cli_kernel_api::model::ModelStreamItem, ModelError>,
    > {
        use futures_util::StreamExt;
        use rust_cli_kernel_api::model::{ModelNotice, ModelStreamItem};
        let notice = ModelStreamItem::Notice(ModelNotice::Retrying {
            attempt: 1,
            max_retries: 10,
            delay_ms: 500,
            error: ModelError::RateLimited {
                message: "slow down".to_owned(),
                retry_after_ms: None,
            },
        });
        futures_util::stream::once(async move { Ok(notice) })
            .chain(
                self.0
                    .stream(request, cancel)
                    .map(|item| item.map(ModelStreamItem::Event)),
            )
            .boxed()
    }
}

#[tokio::test]
async fn a_retry_notice_is_not_part_of_the_reply() {
    // 主循环读带通知的流（重试记日志，logging.md §4）：通知不改变响应。
    let model = Arc::new(RetriesOnce(ScriptedModel::new(vec![text_response(&[
        "ok",
    ])])));
    let request = request("hi");
    let (scope, sink) = scope(parts(model.clone(), Vec::new()), &request, Abort::new());
    assert_eq!(
        super::run(scope, request).await.unwrap(),
        QueryStop::Completed
    );
    let messages = sink.messages();
    let Some(Message::Assistant(assistant)) = messages.last() else {
        panic!("{messages:?}");
    };
    assert_eq!(
        assistant.content,
        vec![ContentBlock::Text {
            text: "ok".to_owned()
        }]
    );
    assert_eq!(model.0.requests().len(), 1);
    // 同一份通知转给界面（L35，stall-visibility.md §1），排在响应的增量之前。
    let events = sink.events();
    assert!(
        matches!(
            events.first(),
            Some(QueryEvent::ModelNotice {
                notice: rust_cli_kernel_api::model::ModelNotice::Retrying {
                    attempt: 1,
                    delay_ms: 500,
                    ..
                }
            })
        ),
        "{events:?}"
    );
}

/// 增量带它所属的 assistant 消息与工具调用（session-lifecycle.md §10.13，K-I3）：前端按它们把增量对到最后落成的行。
/// 以前只有本回合内的响应序号与块序号，序号会漂（服务端块被去掉、冷读把 tool_use 挪到末尾）。
#[tokio::test]
async fn deltas_carry_the_assistant_message_and_the_tool_call() {
    let model = Arc::new(ScriptedModel::new(vec![
        tool_use_response("toolu_1", "Read", &json!({"file_path": "a"})),
        text_response(&["done"]),
    ]));
    let (stop, sink) = run_once(model, Vec::new(), request("read a")).await;
    assert_eq!(stop.unwrap(), QueryStop::Completed);
    let assistants: Vec<_> = sink
        .messages()
        .iter()
        .filter_map(|message| match message {
            Message::Assistant(assistant) => Some(assistant.id.clone()),
            _ => None,
        })
        .collect();
    let deltas: Vec<_> = sink
        .events()
        .iter()
        .filter_map(|event| match event {
            QueryEvent::Delta { id, tool_use, .. } => Some((id.clone(), tool_use.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        deltas,
        vec![
            (assistants[0].clone(), Some(ToolUseId::new("toolu_1"))),
            (assistants[1].clone(), None),
        ]
    );
}
