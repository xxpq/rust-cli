//! 把一条排队的输入现在开成自己的一轮（session-lifecycle.md §10.13，v5 的 `queue.sendNow`；02 §4.2 的指名表）。

use std::sync::Arc;

use futures_util::stream::BoxStream;
use rust_cli_kernel_api::admission::Refusal;
use rust_cli_kernel_api::ids::{CommandId, TurnId};
use rust_cli_kernel_api::session_api::{
    Ack, Command, CommandEnvelope, InputFate, Joined, KernelEvent, QueueHold, SequencedEvent,
    SessionHandle,
};
use rust_cli_kernel_api::testing::{ScriptedModel, text_response};
use tokio::sync::Notify;

use crate::test_support::{Built, Setup, envelope, finished, submit, until};

fn promote(id: &str, command: &str, target: Option<&TurnId>) -> CommandEnvelope {
    envelope(
        id,
        Command::Promote {
            command: CommandId::new(command),
            target: target.cloned(),
        },
    )
}

async fn started(events: &mut BoxStream<'static, SequencedEvent>) -> TurnId {
    let seen = until(events, |event| {
        matches!(event, KernelEvent::TurnStarted { .. })
    })
    .await;
    match seen.last() {
        Some(KernelEvent::TurnStarted { turn, .. }) => turn.clone(),
        _ => unreachable!("until stops at TurnStarted"),
    }
}

/// T1 在模型请求上停着，排着 c2、c3；开关开（扣住队列）。
async fn two_queued_behind_t1() -> (
    Built,
    Arc<ScriptedModel>,
    Arc<Notify>,
    BoxStream<'static, SequencedEvent>,
    TurnId,
) {
    let gate = Arc::new(Notify::new());
    let model = Arc::new(
        ScriptedModel::new(vec![
            text_response(&["one"]),
            text_response(&["two"]),
            text_response(&["three"]),
        ])
        .gated_at(0, gate.clone()),
    );
    let mut setup = Setup::new(model.clone());
    setup.hold_queue = true;
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "first")).await;
    let t1 = started(&mut events).await;
    built.session.send(submit("c2", "second")).await;
    built.session.send(submit("c3", "third")).await;
    (built, model, gate, events, t1)
}

fn opened(events: &[KernelEvent], input: &str) -> bool {
    events.iter().any(|event| {
        matches!(event, KernelEvent::InputFate { input: id, fate: InputFate::Started { joined: Joined::Opened, .. }, .. } if id.as_str() == input)
    })
}

/// 指名的就是在跑的回合：中止它，这一项自己一轮先开（不论扣住），别的仍扣在队列里。
#[tokio::test]
async fn promoting_into_the_running_turn_stops_it_and_starts_the_item() {
    let (built, model, _gate, mut events, t1) = two_queued_behind_t1().await;
    assert!(matches!(
        built.session.send(promote("p1", "c3", Some(&t1))).await,
        Ack::Accepted { .. }
    ));
    let mut seen = until(&mut events, finished).await;
    seen.extend(until(&mut events, finished).await);
    assert!(opened(&seen, "c3"));
    assert_eq!(model.requests().len(), 2);
    let view = built.session.view().await.unwrap();
    assert_eq!(view.queue_hold, Some(QueueHold::Stopped));
    assert_eq!(view.queue.len(), 1);
    assert_eq!(view.queue[0].command, CommandId::new("c2"));
}

/// 空闲（扣住着）：立即开这一项；没指名回合也可以。
#[tokio::test]
async fn promoting_when_idle_starts_the_item_now() {
    let (built, model, _gate, mut events, _t1) = two_queued_behind_t1().await;
    built
        .session
        .send(envelope("s1", Command::Interrupt { target: None }))
        .await;
    until(&mut events, finished).await;
    assert!(matches!(
        built.session.send(promote("p1", "c2", None)).await,
        Ack::Accepted { .. }
    ));
    let seen = until(&mut events, finished).await;
    assert!(opened(&seen, "c2"));
    assert_eq!(model.requests().len(), 2);
    let view = built.session.view().await.unwrap();
    assert_eq!(view.queue.len(), 1);
    assert_eq!(view.queue[0].command, CommandId::new("c3"));
}

/// 在跑的不是指名的回合（或没指名）：拒绝，回合不受影响；不在队列里的：NotFound。
#[tokio::test]
async fn a_promote_for_another_turn_or_a_missing_item_is_refused() {
    let (built, _model, gate, _events, t1) = two_queued_behind_t1().await;
    assert_eq!(
        built.session.send(promote("p1", "c2", None)).await,
        Ack::Rejected {
            refusal: Refusal::StopTargetChanged {
                current: t1.clone()
            },
            reason: "that turn already ended; another turn is running".to_owned(),
        }
    );
    assert!(matches!(
        built.session.send(promote("p2", "c9", Some(&t1))).await,
        Ack::Rejected {
            refusal: Refusal::NotFound,
            ..
        }
    ));
    assert_eq!(built.session.view().await.unwrap().queue.len(), 2);
    assert!(built.session.view().await.unwrap().running.is_some());
    gate.notify_one();
}
