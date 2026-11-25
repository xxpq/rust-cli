//! 准入（session-lifecycle.md §10.4，K-S1）：会话 actor 的拒绝带闭集的原因，文字照旧。

use std::sync::Arc;

use rust_cli_kernel_api::admission::Refusal;
use rust_cli_kernel_api::session_api::{Ack, Command, KernelEvent, SessionHandle};
use rust_cli_kernel_api::testing::{ScriptedModel, text_response};
use tokio::sync::Notify;

use crate::test_support::{Setup, envelope, submit, until};

/// 以前「忙」的拒绝只有文字：v5 前端只能解析它。
#[tokio::test]
async fn idle_only_commands_are_refused_as_busy_while_a_turn_runs() {
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
    assert_eq!(
        built
            .session
            .send(envelope("c2", Command::Compact { instructions: None }))
            .await,
        Ack::Rejected {
            refusal: Refusal::Busy,
            reason: "a turn is running; compact after it finishes".to_owned()
        }
    );
    gate.notify_one();
}

#[tokio::test]
async fn stopping_an_idle_session_is_not_running() {
    let built = Setup::new(Arc::new(ScriptedModel::new(Vec::new())))
        .build()
        .await;
    assert_eq!(
        built
            .session
            .send(envelope("c1", Command::Interrupt { target: None }))
            .await,
        Ack::Rejected {
            refusal: Refusal::NotRunning,
            reason: "no turn is running".to_owned()
        }
    );
}
