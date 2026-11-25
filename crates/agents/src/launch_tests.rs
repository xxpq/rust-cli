//! 子代理缺省的启动方式（重做线 SUBAGENT-MODE，agents.md §8.2）：模型没给 `run_in_background` 时按它；Agent 的描述与
//! schema 随它换写法。

use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::tool::PromptContext;
use serde_json::json;

use crate::SubagentLaunch;
use crate::test_support::{harness, launching, text};

const LAUNCHED: &str = "Async agent launched successfully.";
const HANDED_BACK: &str = "[Subagent hand-back]";
const BACKGROUND_NOTE: &str = "\n- Subagents run in the background by default;";
const FOREGROUND_NOTE: &str =
    "\n- A subagent runs in the background only if you pass `run_in_background: true`.";
/// 父运行里那次 Agent 调用的 id（测试的调用上下文固定给它）。
const PARENT_CALL: &str = "toolu_1";
const FOREGROUND_PARAMETER: &str =
    "Set to true to run this agent in the background. You will be notified when it completes.";

fn prompt() -> PromptContext {
    PromptContext {
        tool_names: Vec::new(),
        mode: PermissionMode::Default,
    }
}

fn look(extra: serde_json::Value) -> serde_json::Value {
    let mut input = json!({"description": "Look", "prompt": "Look around.", "subagent_type": "general-purpose"});
    input
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    input
}

#[tokio::test]
async fn by_default_an_agent_without_the_flag_runs_in_the_background() {
    let harness = harness(None);
    let output = harness.call("sess_parent", "Agent", look(json!({}))).await;
    assert!(text(&output).starts_with(LAUNCHED), "{}", text(&output));
}

#[tokio::test]
async fn a_foreground_default_runs_an_agent_without_the_flag_in_the_foreground() {
    let harness = launching(SubagentLaunch::Foreground);
    let output = harness.call("sess_parent", "Agent", look(json!({}))).await;
    let text = text(&output);
    assert!(text.starts_with(HANDED_BACK), "{text}");
    assert!(text.contains("Hello from the subagent."), "{text}");
}

#[tokio::test]
async fn a_foreground_default_still_runs_an_agent_that_asks_for_it_in_the_background() {
    let harness = launching(SubagentLaunch::Foreground);
    let output = harness
        .call(
            "sess_parent",
            "Agent",
            look(json!({"run_in_background": true})),
        )
        .await;
    assert!(text(&output).starts_with(LAUNCHED), "{}", text(&output));
}

#[tokio::test]
async fn the_agent_text_follows_the_default_launch() {
    let background = harness(None);
    let tool = background.agents.registry().find("Agent").unwrap();
    let description = tool.description(&prompt());
    assert!(description.contains(BACKGROUND_NOTE), "{description}");
    assert!(!description.contains(FOREGROUND_NOTE), "{description}");
    assert_eq!(
        tool.input_schema(),
        rust_cli_prompts::definition("Agent").schema
    );

    let foreground = launching(SubagentLaunch::Foreground);
    let tool = foreground.agents.registry().find("Agent").unwrap();
    let description = tool.description(&prompt());
    assert!(description.contains(FOREGROUND_NOTE), "{description}");
    assert!(!description.contains(BACKGROUND_NOTE), "{description}");
    assert_eq!(
        tool.input_schema()["properties"]["run_in_background"]["description"],
        FOREGROUND_PARAMETER
    );
}

/// Agent 派生之后把开始交给会话（session-lifecycle.md §10.56）：调用、agentId、子会话、类型、说明与前台 / 后台；前台的在交回结果之前、
/// 后台的在回「已启动」之前各报一次。
#[tokio::test]
async fn an_agent_reports_its_subagent_to_the_session() {
    let harness = harness(None);
    let output = harness.call("sess_parent", "Agent", look(json!({}))).await;
    assert!(text(&output).starts_with(LAUNCHED), "{}", text(&output));
    let output = harness
        .call(
            "sess_parent",
            "Agent",
            look(json!({"run_in_background": false})),
        )
        .await;
    assert!(text(&output).starts_with(HANDED_BACK), "{}", text(&output));
    let reported = harness.ops.subagents();
    assert_eq!(
        reported
            .iter()
            .map(|start| (
                start.background,
                start.agent_type.as_str(),
                start.description.as_str()
            ))
            .collect::<Vec<_>>(),
        [
            (true, "general-purpose", "Look"),
            (false, "general-purpose", "Look")
        ]
    );
    for start in &reported {
        // 子会话是 `sess_subagent_<agentId>`，调用是父运行里这次 Agent 的
        assert_eq!(
            start.child.as_str(),
            format!("sess_subagent_{}", start.agent)
        );
        assert_eq!(start.tool_use.as_str(), PARENT_CALL);
    }
}
