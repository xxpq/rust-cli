//! 每条输入的结局（session-lifecycle.md §10.7，K-Q3）：排队、开回合 / 成批 / 并入、结束、取回、被拦下，各有一条事实。

use std::sync::Arc;

use rust_cli_kernel_api::hooks::{HookEventName, HookOutcome};
use rust_cli_kernel_api::ids::CommandId;
use rust_cli_kernel_api::session_api::{
    Ack, Command, CommandResult, DropReason, InputEnd, InputFate, Joined, KernelEvent,
    SessionHandle,
};
use rust_cli_kernel_api::testing::{ScriptedModel, text_response};
use tokio::sync::Notify;

use crate::test_support::{ScriptedHooks, Setup, envelope, finished, submit, until};

/// 事件里的结局，按到达顺序：(命令 id, 结局)。
fn fates(events: &[KernelEvent]) -> Vec<(String, InputFate)> {
    events
        .iter()
        .filter_map(|event| match event {
            KernelEvent::InputFate { input, fate, .. } => {
                Some((input.as_str().to_owned(), fate.clone()))
            }
            _ => None,
        })
        .collect()
}

fn started(fate: &InputFate) -> Option<Joined> {
    match fate {
        InputFate::Started { joined, .. } => Some(*joined),
        _ => None,
    }
}

/// 以前成批并进回合的输入没有任何事实连到回合；排队与结束也没有。
#[tokio::test]
async fn queued_inputs_start_batched_and_end_with_their_turn() {
    let gate = Arc::new(Notify::new());
    let model = Arc::new(
        ScriptedModel::new(vec![text_response(&["ok"]), text_response(&["both"])])
            .gated_at(0, gate.clone()),
    );
    let built = Setup::new(model).build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "hello")).await;
    let mut seen = until(&mut events, |event| {
        matches!(event, KernelEvent::TurnStarted { .. })
    })
    .await;
    built.session.send(submit("c2", "one")).await;
    built.session.send(submit("c3", "two")).await;
    gate.notify_one();
    seen.extend(until(&mut events, finished).await);
    seen.extend(until(&mut events, finished).await);
    let fates = fates(&seen);
    let of = |id: &str| -> Vec<InputFate> {
        fates
            .iter()
            .filter(|(input, _)| input == id)
            .map(|(_, fate)| fate.clone())
            .collect()
    };
    assert_eq!(started(&of("c1")[0]), Some(Joined::Opened));
    assert!(matches!(
        of("c1")[1],
        InputFate::Ended {
            end: InputEnd::Completed,
            ..
        }
    ));
    assert_eq!(of("c2")[0], InputFate::Queued);
    assert_eq!(started(&of("c2")[1]), Some(Joined::Opened));
    assert_eq!(of("c3")[0], InputFate::Queued);
    assert_eq!(started(&of("c3")[1]), Some(Joined::Batched));
    assert!(matches!(of("c3")[2], InputFate::Ended { .. }));
}

#[tokio::test]
async fn a_taken_back_input_is_dropped() {
    let gate = Arc::new(Notify::new());
    let model =
        Arc::new(ScriptedModel::new(vec![text_response(&["ok"])]).gated_at(0, gate.clone()));
    let built = Setup::new(model).build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "hello")).await;
    until(&mut events, |event| {
        matches!(event, KernelEvent::TurnStarted { .. })
    })
    .await;
    built.session.send(submit("c2", "later")).await;
    built
        .session
        .send(envelope(
            "d1",
            Command::Dequeue {
                command: CommandId::new("c2"),
            },
        ))
        .await;
    let seen = until(&mut events, |event| {
        matches!(
            event,
            KernelEvent::InputFate {
                fate: InputFate::Dropped { .. },
                ..
            }
        )
    })
    .await;
    assert_eq!(
        fates(&seen).last().map(|(_, fate)| fate.clone()),
        Some(InputFate::Dropped {
            why: DropReason::TakenBack
        })
    );
    gate.notify_one();
}

/// 以前 UserPromptSubmit 拦下的输入只有一条只供显示的提示。
#[tokio::test]
async fn a_blocked_input_is_dropped_as_blocked() {
    let hooks = Arc::new(ScriptedHooks::with(
        HookEventName::UserPromptSubmit,
        HookOutcome {
            blocking_error: Some("no".to_owned()),
            notices: vec!["blocked by policy".to_owned()],
            ..HookOutcome::default()
        },
    ));
    let mut setup = Setup::new(Arc::new(ScriptedModel::new(Vec::new())));
    setup.hooks = hooks;
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "hello")).await;
    let seen = until(&mut events, finished).await;
    assert_eq!(
        fates(&seen).last().map(|(_, fate)| fate.clone()),
        Some(InputFate::Dropped {
            why: DropReason::Blocked
        })
    );
}

/// 停止的回执列出仍在队列的与随回合取消的输入（02 §4.2）。
#[tokio::test]
async fn the_stop_receipt_lists_queued_and_cancelled_inputs() {
    let gate = Arc::new(Notify::new());
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["ok"])]).gated_at(0, gate));
    let built = Setup::new(model).build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "hello")).await;
    until(&mut events, |event| {
        matches!(event, KernelEvent::TurnStarted { .. })
    })
    .await;
    built.session.send(submit("c2", "later")).await;
    assert_eq!(
        built
            .session
            .send(envelope("s1", Command::Interrupt { target: None }))
            .await,
        Ack::Accepted {
            result: CommandResult::Halted {
                queued: vec![CommandId::new("c2")],
                cancelled: vec![CommandId::new("c1")],
            }
        }
    );
}

/// 结局带会话 actor 的时钟（session-lifecycle.md §10.13；02 §1.5 的 `InputFate.at`）：时刻由内核给，前端不另读时钟。
#[tokio::test]
async fn a_fate_carries_the_actor_clock() {
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["ok"])]));
    let built = Setup::new(model).build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "hello")).await;
    let seen = until(&mut events, finished).await;
    let times: Vec<u64> = seen
        .iter()
        .filter_map(|event| match event {
            KernelEvent::InputFate { at, .. } => Some(*at),
            _ => None,
        })
        .collect();
    // 开回合与回合结束各一条，都是测试时钟的 1000 ms
    assert_eq!(times, [1_000, 1_000]);
}
