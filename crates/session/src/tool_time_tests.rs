//! 工具调用的开工与结束（session-lifecycle.md §10.37）：`ToolStarted` 带开工的时刻；结果提交之后、它的 `MessageDone` 之前发
//! `ToolTimeRecorded`，与库里 tool part 的 `time`（同批计量的 `ToolCall::time`）、视图的 `tool_times` 同值。

use std::sync::Arc;

use rust_cli_kernel_api::ids::{SessionId, ToolUseId};
use rust_cli_kernel_api::message::{Message, UserOrigin};
use rust_cli_kernel_api::metering::ToolTime;
use rust_cli_kernel_api::session_api::{KernelEvent, SessionHandle};
use rust_cli_kernel_api::store::SessionStore;
use rust_cli_kernel_api::testing::{
    FakeTool, ScriptedModel, StepClock, text_response, tool_use_response,
};
use serde_json::json;

use crate::test_support::{Setup, finished, submit, until};

/// 跑一个回合（第一次回复调 `tool`，第二次回答），返回这个回合的事件、视图的 `tool_times` 与冷读的 `tool_times`。执行器的时钟每读一次前进 10 ms。
async fn run(
    tool: &str,
    tools: Vec<Arc<dyn rust_cli_kernel_api::tool::Tool>>,
) -> (Vec<KernelEvent>, Vec<ToolTime>, Vec<ToolTime>) {
    let model = Arc::new(ScriptedModel::new(vec![
        tool_use_response("toolu_1", tool, &json!({})),
        text_response(&["done"]),
    ]));
    let mut setup = Setup::new(model);
    setup.tools = tools;
    setup.query_clock = Arc::new(StepClock::new(1_000, 10));
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "go")).await;
    let seen = until(&mut events, finished).await;
    let view = built.session.view().await.unwrap();
    let loaded = built.store.load(&SessionId::new("sess_1")).await.unwrap();
    (seen, view.tool_times, loaded.tool_times)
}

/// 结果消息的 `MessageDone` 在事件里的位置。
fn result_done(events: &[KernelEvent]) -> usize {
    events
        .iter()
        .position(|event| {
            matches!(event, KernelEvent::MessageDone { message: Message::User(user) }
                if matches!(user.origin, UserOrigin::ToolResult { .. }))
        })
        .unwrap()
}

fn recorded(events: &[KernelEvent]) -> Vec<(usize, ToolTime)> {
    events
        .iter()
        .enumerate()
        .filter_map(|(at, event)| match event {
            KernelEvent::ToolTimeRecorded { time } => Some((at, time.clone())),
            _ => None,
        })
        .collect()
}

/// 以前 `ToolStarted` 不带时刻，起止只在库里，事件与视图都没有，前端的工具行给不出时刻。
#[tokio::test]
async fn a_tool_run_announces_its_start_and_the_stored_start_and_end() {
    let tool: Arc<dyn rust_cli_kernel_api::tool::Tool> =
        Arc::new(FakeTool::new("Echo", true, "ok"));
    let (events, view, loaded) = run("Echo", vec![tool]).await;
    let started: Vec<u64> = events
        .iter()
        .filter_map(|event| match event {
            KernelEvent::ToolStarted { started_at, .. } => Some(*started_at),
            _ => None,
        })
        .collect();
    let recorded = recorded(&events);
    let [(at, time)] = recorded.as_slice() else {
        panic!("one ToolTimeRecorded expected: {recorded:?}");
    };
    assert_eq!(time.tool_use, ToolUseId::new("toolu_1"));
    assert_eq!(started, vec![time.started_at]);
    assert!(time.ended_at > time.started_at, "{time:?}");
    // 随提交给：在结果消息的 MessageDone 之前
    assert!(*at < result_done(&events));
    // 库里（同批计量）与视图同值
    assert_eq!(loaded, vec![time.clone()]);
    assert_eq!(view, vec![time.clone()]);
}

/// 找不到的工具没有开工：没有 `ToolStarted` / `ToolDone`，起止仍随结果落库（两个时刻相同），同样发出。
#[tokio::test]
async fn a_call_that_never_started_still_announces_its_stored_time() {
    let (events, view, loaded) = run("Missing", Vec::new()).await;
    assert!(
        !events.iter().any(|event| matches!(
            event,
            KernelEvent::ToolStarted { .. } | KernelEvent::ToolDone { .. }
        )),
        "{events:?}"
    );
    let recorded = recorded(&events);
    let [(at, time)] = recorded.as_slice() else {
        panic!("one ToolTimeRecorded expected: {recorded:?}");
    };
    assert_eq!(time.started_at, time.ended_at);
    assert!(*at < result_done(&events));
    assert_eq!(loaded, vec![time.clone()]);
    assert_eq!(view, vec![time.clone()]);
}
