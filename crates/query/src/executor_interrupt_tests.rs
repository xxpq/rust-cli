//! PermissionRequest hook 的拒绝带 `interrupt: true`（docs/specs/rust-cli/hooks.md §6.2、query.md §3）：执行器中止整个回合，
//! 同用户的停止。

use std::sync::Arc;

use async_trait::async_trait;
use rust_cli_kernel_api::abort::{Abort, AbortReason};
use rust_cli_kernel_api::message::{ContentBlock, Message, ToolResultContent, UserMessage};
use rust_cli_kernel_api::orchestration::{QueryStop, ToolExecution, ToolExecutor};
use rust_cli_kernel_api::testing::{DirectExecutor, FakeTool, ScriptedModel, text_response};
use rust_cli_kernel_api::tool::{Tool, ToolOutput, ToolProgress, ToolUseContext};
use serde_json::{Value, json};
use tokio::sync::{Notify, mpsc};

use super::tests::tool_uses;
use crate::run::tests::{parts, request, scope};
use crate::texts;

/// 等这次运行结束的上限：旧的执行器不中止，在跑的 `t1` 一直等，超时让用例失败而不是挂住。
const STEP: std::time::Duration = std::time::Duration::from_secs(10);

/// `t2` 的 PermissionRequest hook 拒绝并要求中止回合；其余调用直接执行。
struct InterruptsOnSecond;

#[async_trait]
impl ToolExecutor for InterruptsOnSecond {
    async fn execute(
        &self,
        tool: Arc<dyn Tool>,
        input: Value,
        context: ToolUseContext,
        progress: mpsc::Sender<ToolProgress>,
    ) -> ToolExecution {
        if context.tool_use.as_str() != "t2" {
            return DirectExecutor.execute(tool, input, context, progress).await;
        }
        ToolExecution {
            interrupt: true,
            ..ToolExecution::output(ToolOutput {
                content: vec![ToolResultContent::Text {
                    text: "stop the turn".to_owned(),
                }],
                is_error: true,
                display: Value::Null,
            })
        }
    }
}

/// 每个 tool_result 的（调用，文字）。
fn results(messages: &[Message]) -> Vec<(String, String)> {
    messages
        .iter()
        .filter_map(|message| match message {
            Message::User(UserMessage { content, .. }) => match &content[0] {
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    ..
                } => match &content[0] {
                    ToolResultContent::Text { text } => {
                        Some((tool_use_id.to_string(), text.clone()))
                    }
                    other => panic!("{other:?}"),
                },
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// 修前（D10）：`interrupt` 不生效，回合照常请求模型。现在同用户的停止：这次调用自己也按被中止交付（拒绝的文字，hook 的 message
/// 不进模型，hooks.md §6.2）；同批在跑的 `t1` 经取消结束、没开工的 `t3` 直接是拒绝；写中断标记，以 `Aborted` 结束，不再请求模型。
#[tokio::test]
async fn an_interrupting_hook_deny_aborts_the_turn_after_its_result() {
    let model = Arc::new(ScriptedModel::new(vec![
        tool_uses(&[
            ("t1", "Glob", json!({})),
            ("t2", "Read", json!({})),
            ("t3", "Bash", json!({})),
        ]),
        text_response(&["not requested"]),
    ]));
    let tools: Vec<Arc<dyn Tool>> = vec![
        // 并发安全、一直等到被取消：中止时还在跑。
        Arc::new(FakeTool::new("Glob", true, "globbed").gated(Arc::new(Notify::new()))),
        Arc::new(FakeTool::new("Read", true, "read")),
        // 不能并发：排在前两个后面，中止时还没开工。
        Arc::new(FakeTool::new("Bash", false, "ran")),
    ];
    let mut parts = parts(model.clone(), tools);
    parts.executor = Arc::new(InterruptsOnSecond);
    let request = request("go");
    let abort = Abort::new();
    let (scope, sink) = scope(parts, &request, abort.clone());
    let stop = tokio::time::timeout(STEP, crate::run::run(scope, request))
        .await
        .expect("the turn was not aborted");
    assert_eq!(stop.unwrap(), QueryStop::Aborted);
    assert_eq!(model.requests().len(), 1);
    assert_eq!(abort.reason(), Some(AbortReason::User));
    let messages = sink.messages();
    let mut got = results(&messages);
    got.sort();
    assert_eq!(
        got,
        [
            ("t1".to_owned(), texts::REJECT_MESSAGE.to_owned()),
            ("t2".to_owned(), texts::REJECT_MESSAGE.to_owned()),
            ("t3".to_owned(), texts::REJECT_MESSAGE.to_owned()),
        ]
    );
    // 最后是中断标记。替身的流与 `t2` 的结束谁先被读到不固定（读流的 select 不分先后）：流还开着时中止是不带 “for tool use”
    // 的那一句，之后中止是带的那一句（既有的规则，query.md §1.2）。这里不管是哪一句。
    assert!(matches!(
        messages.last(),
        Some(Message::User(UserMessage { content, .. }))
            if matches!(&content[0], ContentBlock::Text { text }
                if [texts::INTERRUPT_MESSAGE, texts::INTERRUPT_MESSAGE_FOR_TOOL_USE].contains(&text.as_str()))
    ));
}
