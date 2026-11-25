//! ExitPlanMode 拿给用户批准的计划（session-lifecycle.md §10.43）：与 `ToolDone` 同一次处理进视图，与结果同批落成 `rust-cli/plan`，冷读读回同一份。

use std::sync::Arc;

use rust_cli_kernel_api::ids::{SessionId, ToolUseId};
use rust_cli_kernel_api::plan::ShownPlan;
use rust_cli_kernel_api::session_api::{KernelEvent, SessionHandle};
use rust_cli_kernel_api::store::SessionStore;
use rust_cli_kernel_api::testing::{ScriptedModel, text_response, tool_use_response};
use rust_cli_kernel_api::tool::{
    PromptContext, Tool, ToolError, ToolOutput, ToolProgress, ToolUseContext, text,
};
use serde_json::{Value, json};

use crate::test_support::{Setup, finished, submit, until};

const PLAN: &str = "# Plan\n1. do it";
const PATH: &str = "/work/.zcode/plans/plan-sess_1.md";

/// tools 的 ExitPlanMode 的最小替身：结果的 display 带计划。
struct Exiting;

#[async_trait::async_trait]
impl Tool for Exiting {
    fn classifier_input(
        &self,
        _input: &serde_json::Value,
    ) -> rust_cli_kernel_api::classifier::ClassifierInput {
        rust_cli_kernel_api::classifier::ClassifierInput::Skip
    }

    fn name(&self) -> &str {
        "ExitPlanMode"
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
        Ok(ShownPlan::attach(
            PLAN.to_owned(),
            PATH.to_owned(),
            text("approved".to_owned()),
        ))
    }
}

/// 以前计划只在询问与批准的结果文字里：视图与库里都没有，冷读的计划行没有计划文字（拒绝的那一行连结果文字里也没有）。
#[tokio::test]
async fn a_shown_plan_is_stored_with_its_result_and_kept_in_the_view() {
    let model = Arc::new(ScriptedModel::new(vec![
        tool_use_response("toolu_1", "ExitPlanMode", &json!({})),
        text_response(&["done"]),
    ]));
    let mut setup = Setup::new(model);
    setup.tools = vec![Arc::new(Exiting)];
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "plan it")).await;
    let seen = until(&mut events, finished).await;
    let expected = vec![ShownPlan {
        tool_use: ToolUseId::new("toolu_1"),
        plan: PLAN.to_owned(),
        path: PATH.to_owned(),
    }];
    let view = built.session.view().await.unwrap();
    assert_eq!(view.plans, expected);
    let loaded = built.store.load(&SessionId::new("sess_1")).await.unwrap();
    assert_eq!(loaded.plans, expected);
    // 活会话的 `ToolDone` 带同一份
    let done = seen.iter().find_map(|event| match event {
        KernelEvent::ToolDone {
            tool_use, output, ..
        } => ShownPlan::of(tool_use, output),
        _ => None,
    });
    assert_eq!(done.as_ref(), expected.first());
}
