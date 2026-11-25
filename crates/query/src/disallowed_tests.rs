//! 这一次运行不许用的工具（session-lifecycle.md §10.15）：清单里没有它们，模型仍调用时是找不到这个工具。

use std::sync::Arc;

use rust_cli_kernel_api::abort::Abort;
use rust_cli_kernel_api::message::{ContentBlock, Message};
use rust_cli_kernel_api::orchestration::QueryStop;
use rust_cli_kernel_api::testing::{FakeTool, ScriptedModel, text_response, tool_use_response};
use rust_cli_kernel_api::tool::Tool;
use serde_json::json;

use crate::run::tests::{parts, request, scope};

#[tokio::test]
async fn a_disallowed_tool_is_left_out_and_its_call_is_refused() {
    let model = Arc::new(ScriptedModel::new(vec![
        tool_use_response("toolu_1", "Read", &json!({"file_path": "a"})),
        text_response(&["done"]),
    ]));
    let tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(FakeTool::new("Read", true, "content")),
        Arc::new(FakeTool::new("Grep", true, "found")),
    ];
    let mut limited = request("read a");
    limited.disallowed_tools = vec!["Read".to_owned()];
    let (run_scope, sink) = scope(parts(model.clone(), tools), &limited, Abort::new());
    assert_eq!(
        crate::run::run(run_scope, limited).await.unwrap(),
        QueryStop::Completed
    );
    let requests = model.requests();
    let names: Vec<&str> = requests[0]
        .tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect();
    assert_eq!(names, ["Grep"]);
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
