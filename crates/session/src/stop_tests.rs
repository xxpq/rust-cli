//! 指名停止与指名插话（session-lifecycle.md §10.5，K-D1）：命令只作用于它指名的回合；结果只取决于 actor 处理它时在跑的回合。

use std::sync::Arc;

use futures_util::stream::BoxStream;
use rust_cli_kernel_api::admission::Refusal;
use rust_cli_kernel_api::ids::TurnId;
use rust_cli_kernel_api::session_api::{
    Ack, Command, CommandEnvelope, CommandResult, Delivery, KernelEvent, Running, SequencedEvent,
    SessionHandle, TurnOutcome,
};
use rust_cli_kernel_api::testing::{ScriptedModel, text_response};
use tokio::sync::Notify;

use crate::test_support::{Built, Setup, envelope, finished, submit, until};

fn stop(id: &str, target: &TurnId) -> CommandEnvelope {
    envelope(
        id,
        Command::Interrupt {
            target: Some(target.clone()),
        },
    )
}

fn guide(id: &str, text: &str, target: &TurnId) -> CommandEnvelope {
    envelope(
        id,
        Command::Submit {
            text: text.to_owned(),
            extras: Default::default(),
            delivery: Delivery::Steer,
            target: Some(target.clone()),
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

/// 第一个回合 T1 跑完，第二个回合 T2（排队的 c2 开的）停在模型请求上。
async fn second_turn_running() -> (
    Built,
    Arc<Notify>,
    BoxStream<'static, SequencedEvent>,
    TurnId,
    TurnId,
) {
    let (first, second) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
    let model = Arc::new(
        ScriptedModel::new(vec![text_response(&["one"]), text_response(&["two"])])
            .gated_at(0, first.clone())
            .gated_at(1, second.clone()),
    );
    let built = Setup::new(model).build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "first")).await;
    let t1 = started(&mut events).await;
    built.session.send(submit("c2", "second")).await;
    first.notify_one();
    let t2 = started(&mut events).await;
    (built, second, events, t1, t2)
}

#[tokio::test]
async fn a_named_stop_interrupts_its_turn() {
    let gate = Arc::new(Notify::new());
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["ok"])]).gated_at(0, gate));
    let built = Setup::new(model).build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "hello")).await;
    let t1 = started(&mut events).await;
    assert!(matches!(
        built.session.send(stop("s1", &t1)).await,
        Ack::Accepted { .. }
    ));
    let seen = until(&mut events, finished).await;
    assert!(matches!(
        seen.last(),
        Some(KernelEvent::TurnFinished {
            outcome: TurnOutcome::Interrupted,
            ..
        })
    ));
    // 回合已经结束：再停它什么都不做（重放与迟到都安全）。以前空闲时的停止一律拒绝。
    assert_eq!(
        built.session.send(stop("s2", &t1)).await,
        Ack::Accepted {
            result: CommandResult::Unchanged
        }
    );
}

/// 以前停止不指名回合：迟到的停止打断了排队输入刚开的下一个回合。
#[tokio::test]
async fn a_late_stop_leaves_the_next_turn_alone() {
    let (built, second, mut events, t1, t2) = second_turn_running().await;
    assert_eq!(
        built.session.send(stop("s1", &t1)).await,
        Ack::Rejected {
            refusal: Refusal::StopTargetChanged { current: t2 },
            reason: "that turn already ended; another turn is running".to_owned()
        }
    );
    second.notify_one();
    let seen = until(&mut events, finished).await;
    assert!(matches!(
        seen.last(),
        Some(KernelEvent::TurnFinished {
            outcome: TurnOutcome::Success,
            ..
        })
    ));
}

/// 指名 T1 的插话在 T2 在跑时排到回合结束后（`Queue`），绝不并入 T2。
#[tokio::test]
async fn a_late_guide_waits_instead_of_joining_another_turn() {
    let (built, second, _events, t1, _t2) = second_turn_running().await;
    built.session.send(guide("g1", "also this", &t1)).await;
    let queue = built.session.view().await.unwrap().queue;
    assert_eq!(queue.len(), 1);
    assert_eq!(queue[0].delivery, Delivery::Queue);
    second.notify_one();
}

/// 指名的回合已经结束、没有回合在跑：按普通输入开回合（之前到与之后到结果相同，02 §11.2 D）。
#[tokio::test]
async fn a_guide_after_its_turn_ended_starts_a_turn() {
    let model = Arc::new(ScriptedModel::new(vec![
        text_response(&["one"]),
        text_response(&["two"]),
    ]));
    let built = Setup::new(model.clone()).build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "first")).await;
    let t1 = started(&mut events).await;
    until(&mut events, finished).await;
    assert!(matches!(
        built.session.send(guide("g1", "also this", &t1)).await,
        Ack::Accepted { .. }
    ));
    until(&mut events, finished).await;
    assert_eq!(model.requests().len(), 2);
}

/// 视图带在跑的回合（session-lifecycle.md §10.13）：回合中途订阅的前端据它发指名的停止。以前视图只有 `running: bool`。
/// 带的是回合的开始（§10.33），与 `TurnStarted` 同值：开回合的输入已经落库，开始在回合记录里。「在不在跑」与「跑的是哪个回合」
/// 是同一个字段（§10.35）：回合开始之前与结束之后都是 None。
#[tokio::test]
async fn the_view_names_the_running_turn() {
    let gate = Arc::new(Notify::new());
    let model =
        Arc::new(ScriptedModel::new(vec![text_response(&["ok"])]).gated_at(0, gate.clone()));
    let built = Setup::new(model).build().await;
    let mut events = built.session.subscribe(Some(0));
    assert_eq!(built.session.view().await.unwrap().running, None);
    built.session.send(submit("c1", "hello")).await;
    let seen = until(&mut events, |event| {
        matches!(event, KernelEvent::TurnStarted { .. })
    })
    .await;
    let Some(KernelEvent::TurnStarted {
        turn,
        command,
        origin,
        started_at,
        ..
    }) = seen.last().cloned()
    else {
        unreachable!("until stops at TurnStarted");
    };
    // 开回合的输入落库（它的 MessageDone）之后，开始就在回合记录里
    until(&mut events, |event| {
        matches!(event, KernelEvent::MessageDone { .. })
    })
    .await;
    let view = built.session.view().await.unwrap();
    assert_eq!(view.turns.len(), 1);
    assert_eq!(
        view.running,
        Some(Running::Turn(rust_cli_kernel_api::session_api::TurnStart {
            turn,
            command,
            origin,
            started_at,
            works: Vec::new(),
        }))
    );
    gate.notify_one();
    until(&mut events, finished).await;
    assert_eq!(built.session.view().await.unwrap().running, None);
}
