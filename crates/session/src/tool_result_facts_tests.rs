//! 视图在工具结束的那一次处理里就有只给界面的结果事实（session-lifecycle.md §10.38）：大输出的头部与停止时的取消。以前头部在结果提交之后才进视图、
//! 取消在回合结束时才进回合记录：视图取在 `ToolDone` 与它们之间时，由这个视图建的投影少了它们。
//!
//! 两个时机由测试定住：工具在模型的流还开着时结束（流式执行，结果暂存到流结束才提交，query.md §1.6），流由测试放行；停止之后，同一批里另一个
//! 不看取消的调用（`toolu_first`）由测试放行，回合在那之前不会结束。

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use futures_util::StreamExt;
use futures_util::stream::{self, BoxStream};
use rust_cli_kernel_api::command_output::{CommandOutputHead, ToolOutputHead};
use rust_cli_kernel_api::ids::ToolUseId;
use rust_cli_kernel_api::message::{ContentBlock, StopReason, Usage};
use rust_cli_kernel_api::model::{BlockDelta, ModelError, ModelEvent, ModelPort, ModelRequest};
use rust_cli_kernel_api::session_api::{Command, KernelEvent, SessionHandle};
use rust_cli_kernel_api::testing::{ScriptedModel, text_response};
use rust_cli_kernel_api::tool::{
    PromptContext, Tool, ToolError, ToolOutput, ToolProgress, ToolUseContext, text,
};
use serde_json::{Value, json};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::test_support::{Setup, envelope, finished, submit, until};

const FIRST: &str = "toolu_first";
const SECOND: &str = "toolu_second";

/// 排在前面的调用：等测试放行才结束，不看取消。
struct Gated {
    gate: Arc<Notify>,
}

/// 排在后面的调用：`large` 时立即给出大输出（结果里带只给界面的头部，tools 的 Bash 的最小替身）；否则等到被取消。
struct Second {
    large: bool,
}

fn head() -> CommandOutputHead {
    CommandOutputHead {
        head: "line-1\nline-2".to_owned(),
        output_file: "/tmp/zcode-1000/sess_1/tasks/b1.output".to_owned(),
    }
}

#[async_trait::async_trait]
impl Tool for Gated {
    fn classifier_input(
        &self,
        _input: &serde_json::Value,
    ) -> rust_cli_kernel_api::classifier::ClassifierInput {
        rust_cli_kernel_api::classifier::ClassifierInput::Skip
    }

    fn name(&self) -> &str {
        "Gate"
    }

    fn input_schema(&self) -> Value {
        json!({"type": "object"})
    }

    fn description(&self, _context: &PromptContext) -> String {
        String::new()
    }

    fn is_concurrency_safe(&self, _input: &Value) -> bool {
        true
    }

    async fn call(
        &self,
        _input: Value,
        _context: ToolUseContext,
        _progress: tokio::sync::mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        self.gate.notified().await;
        Ok(text("opened".to_owned()))
    }
}

#[async_trait::async_trait]
impl Tool for Second {
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
        json!({"type": "object"})
    }

    fn description(&self, _context: &PromptContext) -> String {
        String::new()
    }

    fn is_concurrency_safe(&self, _input: &Value) -> bool {
        true
    }

    async fn call(
        &self,
        _input: Value,
        context: ToolUseContext,
        _progress: tokio::sync::mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        if self.large {
            return Ok(head().attach(text("<persisted-output>".to_owned())));
        }
        context.cancel.cancelled().await;
        Ok(text("stopped".to_owned()))
    }
}

/// 一次响应里的 tool_use：`calls` 是（调用 id，工具名），按顺序；`end` 为假时不写结尾（停止原因与 `message_stop`）。
fn tool_calls(calls: &[(&str, &str)], end: bool) -> Vec<Result<ModelEvent, ModelError>> {
    let mut events = vec![Ok(ModelEvent::MessageStart {
        id: "msg_api".to_owned(),
        model: "scripted".to_owned(),
        usage: Usage::default(),
    })];
    for (index, (id, name)) in calls.iter().enumerate() {
        events.extend([
            Ok(ModelEvent::BlockStart {
                index,
                block: ContentBlock::ToolUse {
                    id: ToolUseId::new(*id),
                    name: (*name).to_owned(),
                    input: Value::Object(serde_json::Map::new()),
                },
            }),
            Ok(ModelEvent::BlockDelta {
                index,
                delta: BlockDelta::InputJson("{}".to_owned()),
            }),
            Ok(ModelEvent::BlockStop { index }),
        ]);
    }
    if end {
        events.extend(ending());
    }
    events
}

fn ending() -> [Result<ModelEvent, ModelError>; 2] {
    [
        Ok(ModelEvent::MessageDelta {
            stop_reason: Some(StopReason::ToolUse),
            usage: Usage::default(),
        }),
        Ok(ModelEvent::MessageStop),
    ]
}

/// 第一次请求：流出调用 Bash 的 tool_use 块之后停住，等测试放行才结尾；之后的请求回一句 `done`。
struct HeldStream {
    gate: Arc<Notify>,
    calls: AtomicUsize,
}

impl ModelPort for HeldStream {
    fn stream(
        &self,
        _request: ModelRequest,
        _cancel: CancellationToken,
    ) -> BoxStream<'static, Result<ModelEvent, ModelError>> {
        if self.calls.fetch_add(1, Ordering::SeqCst) > 0 {
            return stream::iter(text_response(&["done"])).boxed();
        }
        let gate = self.gate.clone();
        let held = stream::once(async move { gate.notified().await })
            .filter_map(|()| async { None::<Result<ModelEvent, ModelError>> });
        stream::iter(tool_calls(&[(SECOND, "Bash")], false))
            .chain(held)
            .chain(stream::iter(ending()))
            .boxed()
    }
}

fn done(id: &'static str) -> impl Fn(&KernelEvent) -> bool {
    move |event| matches!(event, KernelEvent::ToolDone { tool_use, .. } if tool_use.as_str() == id)
}

/// 修 bug（§10.38）：以前头部在结果提交之后才进视图；工具在流还开着时结束（`ToolDone` 已发）、结果暂存到流结束时取的视图没有它，
/// 由这个视图建的投影里结果行没有头部。现在与 `ToolDone` 同一次处理进视图。
#[tokio::test]
async fn the_view_has_the_output_head_once_the_call_is_done() {
    let gate = Arc::new(Notify::new());
    let model = Arc::new(HeldStream {
        gate: gate.clone(),
        calls: AtomicUsize::new(0),
    });
    let mut setup = Setup::new(model);
    setup.tools = vec![Arc::new(Second { large: true })];
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "print a lot")).await;
    until(&mut events, done(SECOND)).await;
    let expected = vec![ToolOutputHead {
        tool_use: ToolUseId::new(SECOND),
        head: head(),
    }];
    let view = built.session.view().await.unwrap();
    assert_eq!(view.output_heads, expected);
    // 结果还没提交（流还开着）：视图的消息里只有这条输入
    assert_eq!(view.messages.len(), 1, "{:?}", view.messages);
    gate.notify_one();
    until(&mut events, finished).await;
    // 提交之后不重复
    assert_eq!(built.session.view().await.unwrap().output_heads, expected);
}

/// 修 bug（§10.38）：以前停止时取消的调用只在回合结束时进回合记录；在那之前取的视图里没有，由这个视图建的投影把这一行读成「出错」。
/// 现在视图带在跑回合里取消的调用（与 `ToolDone{cancelled}` 同一次处理），回合结束之后在回合记录里。
#[tokio::test]
async fn the_view_has_the_cancelled_call_until_the_turn_record_takes_it() {
    let gate = Arc::new(Notify::new());
    let model = Arc::new(ScriptedModel::new(vec![tool_calls(
        &[(FIRST, "Gate"), (SECOND, "Bash")],
        true,
    )]));
    let mut setup = Setup::new(model);
    setup.tools = vec![
        Arc::new(Gated { gate: gate.clone() }),
        Arc::new(Second { large: false }),
    ];
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "wait")).await;
    until(&mut events, |event| {
        matches!(event, KernelEvent::ToolStarted { tool_use, .. } if tool_use.as_str() == SECOND)
    })
    .await;
    built
        .session
        .send(envelope("c2", Command::Interrupt { target: None }))
        .await;
    until(&mut events, done(SECOND)).await;
    let view = built.session.view().await.unwrap();
    assert_eq!(view.cancelled, [ToolUseId::new(SECOND)]);
    gate.notify_one();
    until(&mut events, finished).await;
    let view = built.session.view().await.unwrap();
    assert!(view.cancelled.is_empty(), "{:?}", view.cancelled);
    let ended = view
        .turns
        .last()
        .and_then(|record| record.ended.clone())
        .expect("the turn record has its end");
    assert!(
        ended.cancelled.contains(&ToolUseId::new(SECOND)),
        "{:?}",
        ended.cancelled
    );
}
