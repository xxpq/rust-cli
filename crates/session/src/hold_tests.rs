//! 停止或失败之后扣住队列（session-lifecycle.md §10.6，K-S2）：开关开时回合结束不开下一条输入，输入要先等用户决定。

use std::sync::Arc;

use futures_util::stream::BoxStream;
use rust_cli_kernel_api::admission::Refusal;
use rust_cli_kernel_api::ids::CommandId;
use rust_cli_kernel_api::model::ModelError;
use rust_cli_kernel_api::session_api::{
    Ack, Command, KernelEvent, QueueHold, SequencedEvent, SessionHandle,
};
use rust_cli_kernel_api::testing::{ScriptedModel, text_response};
use tokio::sync::Notify;

use crate::test_support::{Built, Setup, envelope, finished, submit, until};

/// 第一个回合在模型请求上停着，排着一条输入 c2；`hold` 是开关。
async fn queued_behind_a_turn(
    first: Vec<Result<rust_cli_kernel_api::model::ModelEvent, ModelError>>,
    hold: bool,
) -> (
    Built,
    Arc<ScriptedModel>,
    Arc<Notify>,
    BoxStream<'static, SequencedEvent>,
) {
    let gate = Arc::new(Notify::new());
    let model = Arc::new(
        ScriptedModel::new(vec![first, text_response(&["second"])]).gated_at(0, gate.clone()),
    );
    let mut setup = Setup::new(model.clone());
    setup.hold_queue = hold;
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "first")).await;
    until(&mut events, |event| {
        matches!(event, KernelEvent::TurnStarted { .. })
    })
    .await;
    built.session.send(submit("c2", "second")).await;
    (built, model, gate, events)
}

fn held(event: &KernelEvent) -> bool {
    matches!(event, KernelEvent::QueueHeld { hold: Some(_) })
}

/// 以前停止之后一定自动开下一条（`turn.rs` 的 `start_next`）。
#[tokio::test]
async fn a_stop_holds_the_queue_until_the_user_resumes_it() {
    let (built, model, _gate, mut events) =
        queued_behind_a_turn(text_response(&["one"]), true).await;
    built
        .session
        .send(envelope("s1", Command::Interrupt { target: None }))
        .await;
    let seen = until(&mut events, held).await;
    assert!(seen.iter().any(finished));
    assert!(matches!(
        seen.last(),
        Some(KernelEvent::QueueHeld {
            hold: Some(QueueHold::Stopped)
        })
    ));
    let view = built.session.view().await.unwrap();
    assert!(view.running.is_none());
    assert_eq!(view.queue_hold, Some(QueueHold::Stopped));
    assert_eq!(view.queue.len(), 1);
    // 扣住时新的输入要先等用户决定（草稿留在前端）。
    assert!(matches!(
        built.session.send(submit("c3", "third")).await,
        Ack::Rejected {
            refusal: Refusal::QueueChoiceRequired,
            ..
        }
    ));
    // 放开：排着的 c2 开下一个回合。
    built
        .session
        .send(envelope("a1", Command::SetAutoDrain { enabled: true }))
        .await;
    until(&mut events, finished).await;
    assert_eq!(model.requests().len(), 2);
    assert_eq!(built.session.view().await.unwrap().queue_hold, None);
}

#[tokio::test]
async fn a_failed_turn_holds_the_queue_too() {
    let failure = vec![Err(ModelError::Overloaded {
        message: "busy".to_owned(),
    })];
    let (built, _model, gate, mut events) = queued_behind_a_turn(failure, true).await;
    gate.notify_one();
    let seen = until(&mut events, held).await;
    assert!(matches!(
        seen.last(),
        Some(KernelEvent::QueueHeld {
            hold: Some(QueueHold::TurnFailed)
        })
    ));
    // 取回扣住的最后一条：队列空了，扣住随之放开。
    built
        .session
        .send(envelope(
            "d1",
            Command::Dequeue {
                command: CommandId::new("c2"),
            },
        ))
        .await;
    assert_eq!(built.session.view().await.unwrap().queue_hold, None);
}

/// 开关关着（TUI、`-p`）：停止之后照旧开下一条。
#[tokio::test]
async fn without_the_switch_a_stop_still_drains_the_queue() {
    let (built, model, _gate, mut events) =
        queued_behind_a_turn(text_response(&["one"]), false).await;
    built
        .session
        .send(envelope("s1", Command::Interrupt { target: None }))
        .await;
    until(&mut events, finished).await;
    until(&mut events, finished).await;
    assert_eq!(model.requests().len(), 2);
    assert_eq!(built.session.view().await.unwrap().queue_hold, None);
}

/// 扣住队列时「这条现在开、队列继续扣住」（session-lifecycle.md §10.13，v5 的 `keepQueue`）：以前只能放开扣住（先开队头）
/// 或取回队列，没有办法只开新的这一条。
#[tokio::test]
async fn submit_keeping_queue_starts_now_and_keeps_the_hold() {
    let (built, model, _gate, mut events) =
        queued_behind_a_turn(text_response(&["one"]), true).await;
    built
        .session
        .send(envelope("s1", Command::Interrupt { target: None }))
        .await;
    until(&mut events, held).await;
    let keep = envelope(
        "k1",
        Command::SubmitKeepingQueue {
            text: "now".to_owned(),
            extras: Default::default(),
            expected_held: None,
        },
    );
    assert!(matches!(
        built.session.send(keep).await,
        Ack::Accepted { .. }
    ));
    until(&mut events, finished).await;
    // 新的这一条用了第二个回复；c2 仍扣在队列里
    assert_eq!(model.requests().len(), 2);
    let view = built.session.view().await.unwrap();
    assert_eq!(view.queue_hold, Some(QueueHold::Stopped));
    assert_eq!(view.queue.len(), 1);
    assert_eq!(view.queue[0].command, CommandId::new("c2"));
}

/// 回合在跑时「现在开」没有意义：拒绝（Busy），不排队。
#[tokio::test]
async fn submit_keeping_queue_is_refused_while_a_turn_runs() {
    let (built, _model, gate, _events) = queued_behind_a_turn(text_response(&["one"]), true).await;
    let keep = envelope(
        "k1",
        Command::SubmitKeepingQueue {
            text: "now".to_owned(),
            extras: Default::default(),
            expected_held: None,
        },
    );
    assert!(matches!(
        built.session.send(keep).await,
        Ack::Rejected {
            refusal: Refusal::Busy,
            ..
        }
    ));
    gate.notify_one();
}

/// 两种处置的前置条件（session-lifecycle.md §10.16）：前端看到的扣住的队列与现在的不同时什么都不做，回 `HeldQueueChanged`
/// 带现在扣住的；相符时照常。以前 v5 前端把它接受后丢弃，按过时的队列视图处置。
#[tokio::test]
async fn a_disposition_checks_the_held_queue_it_was_shown() {
    let (built, model, _gate, mut events) =
        queued_behind_a_turn(text_response(&["one"]), true).await;
    built
        .session
        .send(envelope("s1", Command::Interrupt { target: None }))
        .await;
    until(&mut events, held).await;
    let stale = Some(vec![CommandId::new("c9")]);
    for (id, command) in [
        (
            "k1",
            Command::SubmitKeepingQueue {
                text: "now".to_owned(),
                extras: Default::default(),
                expected_held: stale.clone(),
            },
        ),
        (
            "k2",
            Command::SubmitDiscardingQueue {
                text: "now".to_owned(),
                extras: Default::default(),
                expected_held: stale.clone(),
            },
        ),
    ] {
        let ack = built.session.send(envelope(id, command)).await;
        assert!(
            matches!(
                &ack,
                Ack::Rejected {
                    refusal: Refusal::HeldQueueChanged { current },
                    ..
                } if *current == [CommandId::new("c2")]
            ),
            "{ack:?}"
        );
    }
    let view = built.session.view().await.unwrap();
    assert_eq!(view.queue.len(), 1);
    assert!(view.running.is_none());
    // 相符：取空扣住的队列（c2 取回）、放开扣住，这一条现在开。
    let discard = Command::SubmitDiscardingQueue {
        text: "now".to_owned(),
        extras: Default::default(),
        expected_held: Some(vec![CommandId::new("c2")]),
    };
    assert!(matches!(
        built.session.send(envelope("d1", discard)).await,
        Ack::Accepted { .. }
    ));
    let seen = until(&mut events, finished).await;
    assert!(seen.iter().any(|event| matches!(
        event,
        KernelEvent::InputFate {
            input,
            fate: rust_cli_kernel_api::session_api::InputFate::Dropped { .. },
            ..
        } if *input == CommandId::new("c2")
    )));
    assert!(
        seen.iter()
            .any(|event| matches!(event, KernelEvent::QueueHeld { hold: None }))
    );
    assert_eq!(model.requests().len(), 2);
    let view = built.session.view().await.unwrap();
    assert!(view.queue.is_empty());
    assert_eq!(view.queue_hold, None);
}
