//! 工具调用的上下文带这一步的对话摘录（docs/specs/rust-cli/query.md §1.6.1，auto-classifier.md §15.5）：历史是这一步发给模型的
//! 消息，同一个回复里第二个调用的 `earlier` 是第一个。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rust_cli_kernel_api::abort::Abort;
use rust_cli_kernel_api::classifier::{EarlierCall, RunTranscript};
use rust_cli_kernel_api::message::{ContentBlock, Message, UserOrigin};
use rust_cli_kernel_api::orchestration::{ToolExecution, ToolExecutor};
use rust_cli_kernel_api::testing::{DirectExecutor, FakeTool, ScriptedModel, text_response};
use rust_cli_kernel_api::tool::{Tool, ToolProgress, ToolUseContext};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::tests::tool_uses;
use crate::run::tests::{parts, request, scope};

/// 记下每次调用的摘录，再照常执行。
#[derive(Default)]
struct Transcripts(Mutex<Vec<Option<Arc<RunTranscript>>>>);

#[async_trait]
impl ToolExecutor for Transcripts {
    async fn execute(
        &self,
        tool: Arc<dyn Tool>,
        input: Value,
        context: ToolUseContext,
        progress: mpsc::Sender<ToolProgress>,
    ) -> ToolExecution {
        self.0.lock().unwrap().push(context.transcript.clone());
        DirectExecutor.execute(tool, input, context, progress).await
    }
}

fn user_texts(history: &[Message]) -> Vec<String> {
    history
        .iter()
        .filter_map(|message| match message {
            Message::User(user) if user.origin == UserOrigin::Human => Some(&user.content),
            _ => None,
        })
        .flatten()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn each_call_sees_this_step_and_the_calls_before_it_in_the_reply() {
    let model = Arc::new(ScriptedModel::new(vec![
        tool_uses(&[
            ("t1", "Read", json!({"file_path": "/w/a"})),
            ("t2", "Read", json!({"file_path": "/w/b"})),
        ]),
        text_response(&["ok"]),
    ]));
    let seen = Arc::new(Transcripts::default());
    let mut parts = parts(model, vec![Arc::new(FakeTool::new("Read", true, "text"))]);
    parts.executor = seen.clone();
    let request = request("go");
    let (scope, _sink) = scope(parts, &request, Abort::new());
    crate::run::run(scope, request).await.unwrap();
    let seen = seen.0.lock().unwrap().clone();
    let [Some(first), Some(second)] = seen.as_slice() else {
        panic!("{seen:?}");
    };
    // 同一个回复的两个调用共用这一步的历史快照：有用户的输入，还没有这次回复（assistant 在流结束之后才提交）。
    assert!(Arc::ptr_eq(&first.history, &second.history));
    assert_eq!(user_texts(&first.history), ["go"]);
    assert!(
        !first
            .history
            .iter()
            .any(|message| matches!(message, Message::Assistant(_)))
    );
    assert!(first.earlier.is_empty());
    assert_eq!(
        second.earlier,
        [EarlierCall {
            id: rust_cli_kernel_api::ids::ToolUseId::new("t1"),
            name: "Read".to_owned(),
            input: json!({"file_path": "/w/a"}),
        }]
    );
}
