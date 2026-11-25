//! 会话的跟随方式与「现在发」（docs/specs/rust-cli/session-lifecycle.md §10.27，K-S4、13 Q17）。

use rust_cli_kernel_api::session_api::{InputFate, Joined, QueueHold};

use super::*;
use crate::test_support::{Setup, envelope, finished, until};

fn following(id: &str, text: &str) -> CommandEnvelope {
    envelope(
        id,
        Command::SubmitFollowUp {
            text: text.to_owned(),
            extras: Default::default(),
        },
    )
}

fn set_follow_up(id: &str, delivery: Delivery) -> CommandEnvelope {
    envelope(id, Command::SetFollowUp { delivery })
}

fn start_now(id: &str, text: &str) -> CommandEnvelope {
    envelope(
        id,
        Command::SubmitNow {
            text: text.to_owned(),
            extras: Default::default(),
        },
    )
}

/// 设成插话之后，没指定投递方式的输入并入在跑的这一轮（工具边界）；视图与事件都给出新的跟随方式。
#[tokio::test]
async fn a_follow_up_input_steers_once_the_session_is_set_to() {
    let scope = TaskScope::root("test", |_| {});
    let (_store, model, session) = asking_session(&scope).await;
    let mut events = session.subscribe(Some(0));
    assert!(matches!(
        session.send(set_follow_up("f1", Delivery::Steer)).await,
        Ack::Accepted { .. }
    ));
    let seen = until(&mut events, |event| {
        matches!(event, KernelEvent::FollowUpChanged { .. })
    })
    .await;
    assert!(matches!(
        seen.last(),
        Some(KernelEvent::FollowUpChanged {
            delivery: Delivery::Steer
        })
    ));
    assert_eq!(session.view().await.unwrap().follow_up, Delivery::Steer);
    session.send(submit("c1", "ask me")).await;
    let interaction = asked(&mut events).await;
    session.send(following("c2", "also b")).await;
    let view = session.view().await.unwrap();
    assert_eq!(view.queue.len(), 1);
    assert_eq!(view.queue[0].delivery, Delivery::Steer);
    session.send(answer("c3", interaction)).await;
    until_finished(&mut events, 1).await;
    // 插话进了本回合的第二次请求（同一轮）
    let requests = model.requests();
    assert_eq!(requests.len(), 2);
    assert!(format!("{:?}", requests[1].messages).contains("also b"));
}

/// 缺省（没设过）是排队：同一条输入留到回合结束后自己开一轮。
#[tokio::test]
async fn a_follow_up_input_queues_by_default() {
    let scope = TaskScope::root("test", |_| {});
    let (_store, model, session) = asking_session(&scope).await;
    let mut events = session.subscribe(Some(0));
    session.send(submit("c1", "ask me")).await;
    let interaction = asked(&mut events).await;
    session.send(following("c2", "next turn")).await;
    let view = session.view().await.unwrap();
    assert_eq!(view.follow_up, Delivery::Queue);
    assert_eq!(view.queue[0].delivery, Delivery::Queue);
    session.send(answer("c3", interaction)).await;
    until_finished(&mut events, 1).await;
    assert!(!format!("{:?}", model.requests()[1].messages).contains("next turn"));
}

/// 设成现在的值：不发事件（设值语义）。
#[tokio::test]
async fn setting_the_same_follow_up_mode_changes_nothing() {
    let built = Setup::new(Arc::new(ScriptedModel::new(vec![])))
        .build()
        .await;
    let mut events = built.session.subscribe(Some(0));
    assert!(matches!(
        built
            .session
            .send(set_follow_up("f1", Delivery::Queue))
            .await,
        Ack::Accepted { .. }
    ));
    built
        .session
        .send(set_follow_up("f2", Delivery::Steer))
        .await;
    // 第一条事件就是第二次设置的：设成现在的值什么都不发
    let seen = until(&mut events, |event| {
        matches!(event, KernelEvent::FollowUpChanged { .. })
    })
    .await;
    assert_eq!(
        seen.iter()
            .filter(|event| matches!(event, KernelEvent::FollowUpChanged { .. }))
            .count(),
        1
    );
}

/// 「现在发」：在跑的回合被中止（中断结束、不写中断标记），这一条下一个开；队列不因这次中止扣住，排着的输入在它之后接着开。
#[tokio::test]
async fn start_now_preempts_the_running_turn_and_keeps_the_queue_draining() {
    let gate = Arc::new(Notify::new());
    let model = Arc::new(
        ScriptedModel::new(vec![
            text_response(&["one"]),
            text_response(&["now"]),
            text_response(&["queued"]),
        ])
        .gated_at(0, gate.clone()),
    );
    let mut setup = Setup::new(model.clone());
    setup.hold_queue = true;
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "first")).await;
    until(&mut events, |event| {
        matches!(event, KernelEvent::TurnStarted { .. })
    })
    .await;
    built.session.send(submit("c2", "queued")).await;
    assert!(matches!(
        built.session.send(start_now("c3", "right now")).await,
        Ack::Accepted { .. }
    ));
    let mut seen = until(&mut events, finished).await;
    assert!(
        matches!(
            seen.last(),
            Some(KernelEvent::TurnFinished {
                outcome: TurnOutcome::Interrupted,
                ..
            })
        ),
        "{seen:?}"
    );
    seen.extend(until(&mut events, finished).await);
    seen.extend(until(&mut events, finished).await);
    let opened: Vec<&str> = seen
        .iter()
        .filter_map(|event| match event {
            KernelEvent::InputFate {
                input,
                fate:
                    InputFate::Started {
                        joined: Joined::Opened,
                        ..
                    },
                ..
            } => Some(input.as_str()),
            _ => None,
        })
        .collect();
    // c1 开的回合被抢占之后先开 c3，再开排着的 c2
    assert_eq!(opened, ["c1", "c3", "c2"], "{seen:?}");
    // c3 不进队列（它是提升的输入，不是排队的）
    assert!(
        !seen.iter().any(|event| matches!(
            event,
            KernelEvent::QueueChanged { items } if items.iter().any(|item| item.command.as_str() == "c3")
        )),
        "{seen:?}"
    );
    // 被抢占不是用户停止：队列没扣住，也没有中断标记
    assert!(
        !seen.iter().any(|event| matches!(
            event,
            KernelEvent::QueueHeld {
                hold: Some(QueueHold::Stopped)
            }
        )),
        "{seen:?}"
    );
    let view = built.session.view().await.unwrap();
    assert!(view.queue_hold.is_none());
    assert!(
        !format!("{:?}", view.messages).contains("[Request interrupted by user"),
        "{:?}",
        view.messages
    );
    // 抢占的请求里，被中止的那条输入在前，现在发的在后（按人的输入顺序）
    let requests = model.requests();
    let wire = format!("{:?}", requests[1].messages);
    assert!(wire.find("first") < wire.find("right now"), "{wire}");
}

/// 空闲时「现在发」就是普通输入：现在开。
#[tokio::test]
async fn start_now_when_idle_starts_the_input() {
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["ok"])]));
    let built = Setup::new(model.clone()).build().await;
    let mut events = built.session.subscribe(Some(0));
    assert!(matches!(
        built.session.send(start_now("c1", "hello")).await,
        Ack::Accepted { .. }
    ));
    until(&mut events, finished).await;
    assert_eq!(model.requests().len(), 1);
}
