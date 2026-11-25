//! 回合的结局写进回合记录（session-lifecycle.md §10.17）：冷读的回合头按真实结局给出；停止时在等回答的工具是「取消」，不是「出错」。

use super::*;

use rust_cli_kernel_api::session_api::{InputEnd, SessionHandle, TurnTime};
use rust_cli_kernel_api::store::{SessionStore, TurnEnded};

/// 停止在等回答的工具：它的结束带 `cancelled`，回合记录写上被中断与这个工具（以前回合记录没有结局，冷读一律「完成」，工具读成
/// 「出错」，RUST-SESSION 记的）。
#[tokio::test]
async fn a_stop_on_a_pending_ask_records_the_turn_and_the_tool_as_cancelled() {
    let scope = TaskScope::root("test", |_| {});
    let (store, _model, session) = asking_session(&scope).await;
    let mut events = session.subscribe(Some(0));
    session.send(submit("c1", "ask me")).await;
    asked(&mut events).await;
    session
        .send(CommandEnvelope {
            id: CommandId::new("s1"),
            command: Command::Interrupt { target: None },
        })
        .await;
    let seen = until_finished(&mut events, 1).await;
    assert!(seen.iter().any(|event| matches!(
        event,
        KernelEvent::ToolDone { tool_use, cancelled: true, .. } if tool_use.as_str() == "toolu_1"
    )));
    // 时钟固定在 0：结束的时刻与用时都是 0（不为 0 的在 duration_tests.rs）
    let ended = Some(TurnEnded {
        outcome: InputEnd::Cancelled,
        cancelled: vec![rust_cli_kernel_api::ids::ToolUseId::new("toolu_1")],
        time: Some(TurnTime {
            ended_at: 0,
            active_ms: 0,
        }),
        files: Vec::new(),
    });
    let view = session.view().await.unwrap();
    assert_eq!(view.turns.len(), 1);
    assert_eq!(view.turns[0].ended, ended);
    // 冷读：同一条记录在库里只有一份（回合结束时再写的是更新）
    let loaded = store.load(&meta().id).await.unwrap();
    assert_eq!(loaded.turns.len(), 1);
    assert_eq!(loaded.turns[0].ended, ended);
}

/// 照常结束的回合：结局是完成，没有取消的工具。
#[tokio::test]
async fn a_finished_turn_records_its_outcome() {
    let scope = TaskScope::root("test", |_| {});
    let (store, _model, session) = asking_session(&scope).await;
    let mut events = session.subscribe(Some(0));
    session.send(submit("c1", "ask me")).await;
    let interaction = asked(&mut events).await;
    session.send(answer("c2", interaction)).await;
    until_finished(&mut events, 1).await;
    let loaded = store.load(&meta().id).await.unwrap();
    assert_eq!(
        loaded.turns[0].ended,
        Some(TurnEnded {
            outcome: InputEnd::Completed,
            cancelled: Vec::new(),
            time: Some(TurnTime {
                ended_at: 0,
                active_ms: 0,
            }),
            files: Vec::new(),
        })
    );
}
