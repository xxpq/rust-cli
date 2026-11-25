//! 工具结果的输出头部（session-lifecycle.md §10.34）：与这次调用的结果同批落库，提交之后进视图；冷读读回同一份。

use std::sync::Arc;

use rust_cli_kernel_api::command_output::{CommandOutputHead, ToolOutputHead};
use rust_cli_kernel_api::ids::{SessionId, ToolUseId};
use rust_cli_kernel_api::session_api::{KernelEvent, SessionHandle};
use rust_cli_kernel_api::store::SessionStore;
use rust_cli_kernel_api::testing::{ScriptedModel, text_response, tool_use_response};
use rust_cli_kernel_api::tool::{
    PromptContext, Tool, ToolError, ToolOutput, ToolProgress, ToolUseContext, text,
};
use serde_json::{Value, json};

use crate::test_support::{Setup, finished, submit, until};

/// 给出大输出的命令：结果是给模型的预览，`display` 里带只给界面的头部（tools 的 Bash 的最小替身）。`failed` 时是带头部的失败。
struct LargeOutput {
    failed: bool,
}

fn head() -> CommandOutputHead {
    CommandOutputHead {
        head: "line-1\nline-2".to_owned(),
        output_file: "/tmp/zcode-1000/sess_1/tasks/b1.output".to_owned(),
    }
}

#[async_trait::async_trait]
impl Tool for LargeOutput {
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

    async fn call(
        &self,
        _input: Value,
        _context: ToolUseContext,
        _progress: tokio::sync::mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        let output = head().attach(text("<persisted-output>".to_owned()));
        if self.failed {
            // 执行管线把带 display 的失败换成同样带 display 的结果（tools 的 pipeline）；这里直接给出换算之后的样子。
            return Ok(ToolOutput {
                is_error: true,
                ..output
            });
        }
        Ok(output)
    }
}

/// 以前头部只在 `ToolDone` 里：视图与库里都没有，由视图建的投影与冷读拿不到它。成功与失败的结果都落库。
#[tokio::test]
async fn a_large_output_head_is_stored_with_its_result_and_kept_in_the_view() {
    for failed in [false, true] {
        let model = Arc::new(ScriptedModel::new(vec![
            tool_use_response("toolu_1", "Bash", &json!({"command": "seq 100000"})),
            text_response(&["done"]),
        ]));
        let mut setup = Setup::new(model);
        setup.tools = vec![Arc::new(LargeOutput { failed })];
        let built = setup.build().await;
        let mut events = built.session.subscribe(Some(0));
        built.session.send(submit("c1", "print a lot")).await;
        until(&mut events, finished).await;
        let expected = vec![ToolOutputHead {
            tool_use: ToolUseId::new("toolu_1"),
            head: head(),
        }];
        let view = built.session.view().await.unwrap();
        assert_eq!(view.output_heads, expected, "failed = {failed}");
        let loaded = built.store.load(&SessionId::new("sess_1")).await.unwrap();
        assert_eq!(loaded.output_heads, expected, "failed = {failed}");
        // 活会话的 `ToolDone` 带同一份（前端的活会话路径不变）
        let mut all = built.session.subscribe(Some(0));
        let seen = until(&mut all, |event| {
            matches!(event, KernelEvent::ToolDone { .. })
        })
        .await;
        match seen.last() {
            Some(KernelEvent::ToolDone { output, .. }) => {
                assert_eq!(CommandOutputHead::of(output), Some(head()));
            }
            other => panic!("expected ToolDone, saw {other:?}"),
        }
    }
}
