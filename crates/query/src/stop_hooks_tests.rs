//! Stop / SubagentStop hooks 在主循环里的效果（docs/specs/rust-cli/hooks.md §7、query.md §1）。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rust_cli_kernel_api::abort::Abort;
use rust_cli_kernel_api::hooks::{HookEvent, HookEventName, HookOutcome, HookRunner};
use rust_cli_kernel_api::ids::MessageId;
use rust_cli_kernel_api::message::{ContentBlock, Message, UserMessage, UserOrigin};
use rust_cli_kernel_api::orchestration::{QueryEvent, QueryStop};
use rust_cli_kernel_api::session_api::{NoticeKind, NoticeLevel};
use rust_cli_kernel_api::testing::{ScriptedModel, text_response};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::limits::StopHookCap;
use crate::run::tests::{parts, request, scope};

/// 按顺序返回给定的结果，并记下收到的事件。
#[derive(Default)]
struct ScriptedHooks {
    outcomes: Mutex<VecDeque<HookOutcome>>,
    events: Mutex<Vec<HookEvent>>,
}

#[async_trait]
impl HookRunner for ScriptedHooks {
    async fn run(&self, event: HookEvent, _cancel: CancellationToken) -> HookOutcome {
        self.events.lock().unwrap().push(event);
        self.outcomes
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_default()
    }
}

fn feedback(text: &str) -> Message {
    Message::User(UserMessage {
        id: MessageId::new("msg_hook"),
        content: vec![ContentBlock::Text {
            text: text.to_owned(),
        }],
        origin: UserOrigin::Meta {
            source: "hook_blocking_error".to_owned(),
        },
    })
}

fn blocking(text: &str) -> HookOutcome {
    HookOutcome {
        blocking_error: Some(text.to_owned()),
        messages: vec![feedback(text)],
        ..HookOutcome::default()
    }
}

async fn run(
    responses: usize,
    outcomes: Vec<HookOutcome>,
    max_turns: Option<u32>,
) -> (QueryStop, Vec<Message>, Vec<HookEvent>, usize) {
    let (stop, messages, events, requests, _emitted) =
        run_capped(responses, outcomes, max_turns, StopHookCap::default()).await;
    (stop, messages, events, requests)
}

/// 同 [`run`]，Stop hook 的上限由入口给；另交出主循环发给前端的事件。
async fn run_capped(
    responses: usize,
    outcomes: Vec<HookOutcome>,
    max_turns: Option<u32>,
    cap: StopHookCap,
) -> (
    QueryStop,
    Vec<Message>,
    Vec<HookEvent>,
    usize,
    Vec<QueryEvent>,
) {
    let model = Arc::new(ScriptedModel::new(
        (0..responses)
            .map(|index| text_response(&[&format!("reply {index}")]))
            .collect(),
    ));
    let hooks = Arc::new(ScriptedHooks {
        outcomes: Mutex::new(outcomes.into()),
        events: Mutex::default(),
    });
    let mut parts = parts(model.clone(), Vec::new());
    parts.hooks = hooks.clone();
    parts.limits.stop_hook_cap = cap;
    let mut request = request("go");
    request.max_turns = max_turns;
    let (scope, sink) = scope(parts, &request, Abort::new());
    let stop = crate::run::run(scope, request).await.unwrap();
    let events = hooks.events.lock().unwrap().clone();
    (
        stop,
        sink.messages(),
        events,
        model.requests().len(),
        sink.events(),
    )
}

/// 主循环发给前端的提示（只供显示）：级别、原文与种类。
fn notices(events: &[QueryEvent]) -> Vec<(NoticeLevel, String, Option<NoticeKind>)> {
    events
        .iter()
        .filter_map(|event| match event {
            QueryEvent::Notice { level, text, kind } => Some((*level, text.clone(), kind.clone())),
            _ => None,
        })
        .collect()
}

fn blocks(count: usize) -> Vec<HookOutcome> {
    (0..count)
        .map(|_| blocking("Stop hook feedback:\nagain"))
        .collect()
}

#[tokio::test]
async fn the_stop_hook_sees_the_last_reply_and_lets_the_turn_end() {
    let (stop, _messages, events, requests) = run(1, Vec::new(), None).await;
    assert_eq!(stop, QueryStop::Completed);
    assert_eq!(requests, 1);
    let [event] = events.as_slice() else {
        panic!("expected one Stop event: {events:?}");
    };
    assert_eq!(event.name, HookEventName::Stop);
    assert_eq!(
        event.payload,
        json!({
            "stop_hook_active": false,
            "last_assistant_message": "reply 0",
            "background_tasks": [],
            "session_crons": [],
        })
    );
    assert!(event.context.mode.is_some());
}

#[tokio::test]
async fn a_blocking_stop_hook_continues_the_turn_with_its_feedback() {
    let (stop, messages, events, requests) = run(
        2,
        vec![blocking("Stop hook feedback:\nrun the tests")],
        None,
    )
    .await;
    assert_eq!(stop, QueryStop::Completed);
    assert_eq!(requests, 2);
    assert!(messages.contains(&feedback("Stop hook feedback:\nrun the tests")));
    // 第二次 Stop 时 stop_hook_active 为真。
    assert_eq!(events.len(), 2);
    assert_eq!(events[1].payload["stop_hook_active"], json!(true));
}

#[tokio::test]
async fn a_prevented_turn_ends_without_another_request() {
    let prevented = HookOutcome {
        prevent_continuation: true,
        stop_reason: Some("done for today".to_owned()),
        messages: vec![feedback("done for today")],
        ..HookOutcome::default()
    };
    let (stop, messages, _events, requests) = run(2, vec![prevented], None).await;
    // 工作线 F134：以 `stop_hook_prevented` 结束（以前是 `Completed`）。
    assert_eq!(stop, QueryStop::StopHookPrevented);
    assert_eq!(requests, 1);
    assert!(messages.contains(&feedback("done for today")));
}

#[tokio::test]
async fn blocking_stops_at_max_turns_and_at_the_block_cap() {
    // maxTurns = 1：阻塞后 turnCount 变 2，超过上限。
    let (stop, messages, _events, requests) =
        run(2, vec![blocking("Stop hook feedback:\nagain")], Some(1)).await;
    assert_eq!(stop, QueryStop::MaxTurns);
    assert_eq!(requests, 1);
    assert!(messages.iter().any(|message| matches!(
        message,
        Message::Attachment(attachment) if attachment.source == "max_turns_reached"
    )));
    // 连续阻塞 9 次：第 9 次超过上限 8，按 completed 结束，不再请求。
    let outcomes = (0..9)
        .map(|_| blocking("Stop hook feedback:\nagain"))
        .collect();
    let (stop, _messages, events, requests) = run(10, outcomes, None).await;
    assert_eq!(stop, QueryStop::Completed);
    assert_eq!(requests, 9);
    assert_eq!(events.len(), 9);
}

/// Q21：到了上限显示一条只给前端的警告，再按 completed 结束。警告不进记录，也不进之后的请求；这一次的阻塞反馈照常
/// 提交。修前只记日志。
#[tokio::test]
async fn the_block_cap_shows_a_warning_and_ends_the_turn() {
    let (stop, messages, _events, requests, emitted) =
        run_capped(10, blocks(9), None, StopHookCap::default()).await;
    assert_eq!((stop, requests), (QueryStop::Completed, 9));
    let warning = "A hook blocked the turn from ending 9 consecutive times \u{2014} overriding and ending turn. For Stop/SubagentStop hooks, check stop_hook_active in the input and return success while it's true. Set ZCODE_STOP_HOOK_BLOCK_CAP to raise this limit.";
    // SMALL-3（query.md §3）：警告带种类与这一次的连续次数，要按语言显示整句的前端据它取文案；原文照旧
    assert_eq!(
        notices(&emitted),
        [(
            NoticeLevel::Warning,
            warning.to_owned(),
            Some(NoticeKind::StopHookLimit { blocked: 9 })
        )]
    );
    assert_eq!(
        messages
            .iter()
            .filter(|message| **message == feedback("Stop hook feedback:\nagain"))
            .count(),
        9
    );
    let shown = serde_json::to_string(&messages).unwrap();
    assert!(!shown.contains("consecutive times"), "{shown}");
}

/// 上限由入口给（`ZCODE_STOP_HOOK_BLOCK_CAP`）；不封顶时一直续到 hook 放行，没有警告。
#[tokio::test]
async fn the_block_cap_comes_from_the_entry_and_can_be_lifted() {
    let (stop, _messages, _events, requests, emitted) =
        run_capped(5, blocks(3), None, StopHookCap::Capped(2)).await;
    assert_eq!((stop, requests), (QueryStop::Completed, 3));
    let shown = notices(&emitted);
    let [(NoticeLevel::Warning, text, Some(NoticeKind::StopHookLimit { blocked: 3 }))] =
        shown.as_slice()
    else {
        panic!("{emitted:?}");
    };
    assert!(
        text.starts_with("A hook blocked the turn from ending 3 consecutive times"),
        "{text}"
    );
    let (stop, _messages, events, requests, emitted) =
        run_capped(13, blocks(12), None, StopHookCap::Unlimited).await;
    assert_eq!(
        (stop, requests, events.len()),
        (QueryStop::Completed, 13, 13)
    );
    assert!(notices(&emitted).is_empty());
}

/// 子代理里的 SubagentStop 的上下文带顶层会话（21 §14）：hook 发的旁路请求按它经宿主取凭据；顶层会话自己的 Stop 为 None。
#[tokio::test]
async fn the_hook_context_in_a_subagent_names_the_root_session() {
    for root in [
        Some(rust_cli_kernel_api::ids::SessionId::new("sess_root")),
        None,
    ] {
        let model = Arc::new(ScriptedModel::new(vec![text_response(&["done"])]));
        let hooks = Arc::new(ScriptedHooks::default());
        let mut parts = parts(model, Vec::new());
        parts.hooks = hooks.clone();
        let mut request = request("go");
        if root.is_some() {
            request.agent = Some(rust_cli_kernel_api::ids::AgentId::new("agent_1"));
            request.source =
                rust_cli_kernel_api::model::QuerySource::Subagent("general-purpose".to_owned());
        }
        request.root = root.clone();
        let (scope, _sink) = scope(parts, &request, Abort::new());
        crate::run::run(scope, request).await.unwrap();
        let events = hooks.events.lock().unwrap().clone();
        let [event] = events.as_slice() else {
            panic!("{events:?}");
        };
        assert_eq!(event.context.root, root);
    }
}

/// 修 bug（19 §47.3 R-HOOK-ROWS）：Stop 跑了的 hook 交给会话（hook 行的事实，hooks.md §14.3）；以前只交合并后的结果。
#[tokio::test]
async fn the_stop_hooks_that_ran_are_reported() {
    use rust_cli_kernel_api::hook_runs::{HookOrigin, HookRun, HookRunOutcome};
    let ran = HookRun {
        index: 0,
        origin: HookOrigin::Project,
        command: "stop.sh".to_owned(),
        status_message: None,
        outcome: HookRunOutcome::Success,
        reason: None,
        stderr: None,
        started_at: 1,
        ended_at: 2,
    };
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["done"])]));
    let mut parts = parts(model, Vec::new());
    parts.hooks = Arc::new(ScriptedHooks {
        outcomes: Mutex::new(
            vec![HookOutcome {
                runs: vec![ran.clone()],
                ..HookOutcome::default()
            }]
            .into(),
        ),
        events: Mutex::default(),
    });
    let request = request("go");
    let (scope, sink) = scope(parts, &request, Abort::new());
    crate::run::run(scope, request).await.unwrap();
    let hooks = sink.hooks();
    assert_eq!(hooks.len(), 1);
    assert_eq!(hooks[0].event, HookEventName::Stop);
    assert_eq!(hooks[0].tool, None);
    assert_eq!(hooks[0].runs, [ran]);
}
