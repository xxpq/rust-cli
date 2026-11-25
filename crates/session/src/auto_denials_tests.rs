//! 自动模式的拒绝计数（docs/specs/rust-cli/auto-classifier.md §16.5）：owner 是会话 actor，判决链经 `SessionOps::note_auto_verdict`
//! 改它，`view()` 读出同一时刻的执行状态与计数。

use std::sync::Arc;

use rust_cli_kernel_api::permission::auto_mode::{AutoVerdict, DenialCounts, DenialLimit};
use rust_cli_kernel_api::session_api::SessionHandle;
use rust_cli_kernel_api::testing::{ScriptedModel, text_response, tool_use_response};
use rust_cli_kernel_api::tool::{
    PromptContext, Tool, ToolError, ToolOutput, ToolProgress, ToolUseContext, text,
};
use serde_json::{Value, json};

use crate::test_support::{Setup, finished, submit, until};

/// 按输入的顺序记结论，回每一次到了的上限与最后的计数（从视图读）。
struct Note;

#[async_trait::async_trait]
impl Tool for Note {
    fn classifier_input(&self, _input: &Value) -> rust_cli_kernel_api::classifier::ClassifierInput {
        rust_cli_kernel_api::classifier::ClassifierInput::Skip
    }

    fn name(&self) -> &str {
        "Note"
    }

    fn input_schema(&self) -> Value {
        json!({"type": "object"})
    }

    fn description(&self, _context: &PromptContext) -> String {
        String::new()
    }

    async fn call(
        &self,
        input: Value,
        context: ToolUseContext,
        _progress: tokio::sync::mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        let mut limits = Vec::new();
        for verdict in input["verdicts"].as_array().into_iter().flatten() {
            let verdict = if verdict == "blocked" {
                AutoVerdict::Blocked
            } else {
                AutoVerdict::Allowed
            };
            limits.push(context.session_ops.note_auto_verdict(verdict).await);
        }
        let view = context.session_ops.view().await;
        let counts = view.map(|view| view.auto_denials);
        Ok(text(format!("{limits:?} {counts:?}")))
    }
}

#[tokio::test]
async fn the_actor_owns_the_denial_counts() {
    let model = Arc::new(ScriptedModel::new(vec![
        tool_use_response(
            "toolu_1",
            "Note",
            &json!({"verdicts": ["blocked", "blocked", "blocked", "allowed", "blocked"]}),
        ),
        text_response(&["done"]),
    ]));
    let mut setup = Setup::new(model.clone());
    setup.tools = vec![Arc::new(Note)];
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "count")).await;
    until(&mut events, finished).await;
    let limits = [None, None, Some(DenialLimit::Consecutive(3)), None, None];
    let counts = Some(DenialCounts {
        consecutive: 1,
        total: 4,
    });
    let requests = model.requests();
    let last = format!("{:?}", requests.last().unwrap().messages.last().unwrap());
    assert!(
        last.contains(&format!("{limits:?} {counts:?}").replace('"', "\\\"")),
        "{last}"
    );
}
