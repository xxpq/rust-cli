//! 空闲时关闭（docs/specs/rust-cli/session-lifecycle.md §3.3.2）：会话 actor 在自己的一步里判空闲，空闲就照 `close` 关；不空闲什么都不改，
//! 会话照常。模型在第一次请求上停着（`gated_at`），回合在跑的时刻是确定的。

use std::sync::Arc;

use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use rust_cli_kernel_api::hooks::{HookEventName, HookOutcome};
use rust_cli_kernel_api::session_api::{
    Command, EndReason, KernelEvent, SequencedEvent, SessionHandle,
};
use rust_cli_kernel_api::testing::{ScriptedModel, text_response};
use tokio::sync::Notify;

use crate::test_support::{Built, ScriptedHooks, Setup, envelope, finished, submit, until};

/// 跑过几次 SessionEnd。
fn ends(hooks: &ScriptedHooks) -> usize {
    hooks
        .seen()
        .iter()
        .filter(|event| event.name == HookEventName::SessionEnd)
        .count()
}

fn started(event: &KernelEvent) -> bool {
    matches!(event, KernelEvent::TurnStarted { .. })
}

/// 第一个回合停在模型请求上（`gate` 放行），之后的回合回一句话；`hold` 是停止之后扣住队列的开关。
async fn session(
    hold: bool,
) -> (
    Built,
    Arc<ScriptedHooks>,
    Arc<Notify>,
    BoxStream<'static, SequencedEvent>,
) {
    let gate = Arc::new(Notify::new());
    let model = Arc::new(
        ScriptedModel::new(vec![text_response(&["first"]), text_response(&["second"])])
            .gated_at(0, gate.clone()),
    );
    // 配了 SessionEnd 才会跑它（`hooks::session_end`）
    let hooks = Arc::new(ScriptedHooks::with(
        HookEventName::SessionEnd,
        HookOutcome::default(),
    ));
    let mut setup = Setup::new(model);
    setup.hooks = hooks.clone();
    setup.hold_queue = hold;
    let built = setup.build().await;
    let events = built.session.subscribe(Some(0));
    (built, hooks, gate, events)
}

/// 回合跑完、没有别的工作：关掉，SessionEnd 一次，actor 退出；关完再关也是关了，SessionEnd 不再跑。
#[tokio::test]
async fn an_idle_session_is_closed() {
    let (built, hooks, gate, mut events) = session(false).await;
    gate.notify_one();
    built.session.send(submit("c1", "hello")).await;
    until(&mut events, finished).await;
    assert!(built.session.close_idle(EndReason::Other).await);
    assert_eq!(ends(&hooks), 1);
    assert!(
        built.session.view().await.is_err(),
        "the actor must have stopped"
    );
    assert!(built.session.close_idle(EndReason::Other).await);
    assert_eq!(ends(&hooks), 1);
}

/// 回合在跑：不关，没有 SessionEnd，回合照常跑完；跑完之后再判就关了。
#[tokio::test]
async fn a_running_turn_keeps_the_session_open() {
    let (built, hooks, gate, mut events) = session(false).await;
    built.session.send(submit("c1", "hello")).await;
    until(&mut events, started).await;
    assert!(!built.session.close_idle(EndReason::Other).await);
    assert_eq!(ends(&hooks), 0);
    gate.notify_one();
    let seen = until(&mut events, finished).await;
    assert!(
        seen.iter().any(|event| matches!(
            event,
            KernelEvent::TurnFinished {
                outcome: rust_cli_kernel_api::session_api::TurnOutcome::Success,
                ..
            }
        )),
        "the turn must finish normally: {seen:?}"
    );
    assert!(built.session.view().await.is_ok());
    assert!(built.session.close_idle(EndReason::Other).await);
    assert_eq!(ends(&hooks), 1);
}

/// 停止之后扣住的队列（不忙，但输入调度里有项）：不关，排着的输入还在。
#[tokio::test]
async fn a_held_queue_keeps_the_session_open() {
    let (built, hooks, _gate, mut events) = session(true).await;
    built.session.send(submit("c1", "first")).await;
    until(&mut events, started).await;
    built.session.send(submit("c2", "second")).await;
    built
        .session
        .send(envelope("s1", Command::Interrupt { target: None }))
        .await;
    until(&mut events, |event| {
        matches!(event, KernelEvent::QueueHeld { hold: Some(_) })
    })
    .await;
    let view = built.session.view().await.unwrap();
    assert!(view.running.is_none());
    assert!(!built.session.close_idle(EndReason::Other).await);
    assert_eq!(ends(&hooks), 0);
    assert_eq!(built.session.view().await.unwrap().queue.len(), 1);
}

/// 判出不空闲时放开闩：同时等着的 `close` 醒来自己关（SessionEnd 一次，actor 退出）。旧的闩只认「有人关过」，`close` 会以为已经关了。
#[tokio::test]
async fn a_close_waiting_on_a_refused_idle_close_closes_the_session() {
    let (built, hooks, _gate, mut events) = session(false).await;
    built.session.send(submit("c1", "hello")).await;
    until(&mut events, started).await;
    let (idle, _) = tokio::join!(
        built.session.close_idle(EndReason::Other),
        built.session.close(EndReason::Other)
    );
    assert!(!idle);
    assert_eq!(ends(&hooks), 1);
    assert!(built.session.view().await.is_err());
}

/// 定时任务的替身：`scheduled` 是测试开关（有没有以后会到期的任务），不触发。
#[derive(Default)]
struct Scheduled(std::sync::atomic::AtomicBool);

#[async_trait::async_trait]
impl rust_cli_kernel_api::cron::CronSignals for Scheduled {
    fn wakeups(&self, _session: &rust_cli_kernel_api::ids::SessionId) -> BoxStream<'static, ()> {
        futures_util::stream::pending().boxed()
    }

    async fn take_due(
        &self,
        _session: &rust_cli_kernel_api::ids::SessionId,
    ) -> Vec<rust_cli_kernel_api::cron::CronFire> {
        Vec::new()
    }

    fn scheduled(&self, _session: &rust_cli_kernel_api::ids::SessionId) -> bool {
        self.0.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// 有以后会到期开回合的定时任务（`/loop`、CronCreate）：不关，会话照常；任务没有了之后再判就关（主会话定 2026-11-12，§3.3.2）。
#[tokio::test]
async fn a_session_with_a_scheduled_task_is_not_closed() {
    let cron = Arc::new(Scheduled::default());
    cron.0.store(true, std::sync::atomic::Ordering::SeqCst);
    let hooks = Arc::new(ScriptedHooks::with(
        HookEventName::SessionEnd,
        HookOutcome::default(),
    ));
    let mut setup = Setup::new(Arc::new(ScriptedModel::new(vec![text_response(&["done"])])));
    setup.hooks = hooks.clone();
    setup.cron = cron.clone();
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "hello")).await;
    until(&mut events, finished).await;
    assert!(!built.session.close_idle(EndReason::Other).await);
    assert_eq!(ends(&hooks), 0);
    assert!(built.session.view().await.is_ok());
    cron.0.store(false, std::sync::atomic::Ordering::SeqCst);
    assert!(built.session.close_idle(EndReason::Other).await);
    assert_eq!(ends(&hooks), 1);
}
