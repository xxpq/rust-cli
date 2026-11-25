//! 回合的来源与开始时刻（session-lifecycle.md §10.8，K-T1、K-T2）：活的 `TurnStarted` 与冷读的回合记录同值。

use std::sync::Arc;

use rust_cli_kernel_api::session_api::{Command, KernelEvent, SessionHandle, TurnOrigin};
use rust_cli_kernel_api::store::SessionStore;
use rust_cli_kernel_api::testing::{NoticeEnvironment, ScriptedModel, shell_notice, text_response};

use crate::test_support::{Setup, envelope, finished, submit, until};

fn origins(events: &[KernelEvent]) -> Vec<(TurnOrigin, u64)> {
    events
        .iter()
        .filter_map(|event| match event {
            KernelEvent::TurnStarted {
                origin, started_at, ..
            } => Some((*origin, *started_at)),
            _ => None,
        })
        .collect()
}

/// 以前 `TurnStarted` 不带来源与时刻：前端分不出用户的回合与后台结果开的回合。
#[tokio::test]
async fn turns_carry_their_origin_and_start_time_live_and_cold() {
    let env = Arc::new(NoticeEnvironment::default());
    let model = Arc::new(ScriptedModel::new(vec![
        text_response(&["one"]),
        text_response(&["seen"]),
    ]));
    let mut setup = Setup::new(model);
    setup.set_env(env.clone());
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "first")).await;
    let mut seen = until(&mut events, finished).await;
    env.finish(shell_notice(1, "b1"));
    seen.extend(until(&mut events, finished).await);
    // 测试的时钟固定在 1000（test_support）。
    assert_eq!(
        origins(&seen),
        [(TurnOrigin::Input, 1_000), (TurnOrigin::Notice, 1_000)]
    );
    let view = built.session.view().await.unwrap();
    let recorded: Vec<(TurnOrigin, u64)> = view
        .turns
        .iter()
        .map(|record| (record.origin, record.started_at))
        .collect();
    assert_eq!(recorded, origins(&seen));
    let cold = built.store.load(built.session.id()).await.unwrap();
    assert_eq!(cold.turns, view.turns);
}

#[tokio::test]
async fn a_compaction_turn_says_so() {
    let built = Setup::new(Arc::new(ScriptedModel::new(Vec::new())))
        .build()
        .await;
    let mut events = built.session.subscribe(Some(0));
    built
        .session
        .send(envelope("c1", Command::Compact { instructions: None }))
        .await;
    let seen = until(&mut events, |event| {
        matches!(event, KernelEvent::TurnStarted { .. })
    })
    .await;
    assert_eq!(origins(&seen), [(TurnOrigin::Compaction, 1_000)]);
}

/// 后台结果开的回合带开它的后台工作（session-lifecycle.md §10.50，19 §47 R-NOTICE-ORIGIN）：活的 `TurnStarted`、视图的回合记录与库里读回的同值；
/// 用户输入开的回合不带。以前不带，前端要读通知的文字才知道这一轮是哪项后台工作的结果。
#[tokio::test]
async fn a_notice_turn_names_the_works_that_opened_it_live_and_cold() {
    use rust_cli_kernel_api::background::NoticedWork;
    use rust_cli_kernel_api::manage::BackgroundKind;

    let env = Arc::new(NoticeEnvironment::default());
    let model = Arc::new(ScriptedModel::new(vec![
        text_response(&["one"]),
        text_response(&["seen"]),
    ]));
    let mut setup = Setup::new(model);
    setup.set_env(env.clone());
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "first")).await;
    let mut seen = until(&mut events, finished).await;
    env.finish(shell_notice(1, "b1"));
    seen.extend(until(&mut events, finished).await);
    let started: Vec<Vec<NoticedWork>> = seen
        .iter()
        .filter_map(|event| match event {
            KernelEvent::TurnStarted { works, .. } => Some(works.clone()),
            _ => None,
        })
        .collect();
    let work = NoticedWork {
        id: "b1".to_owned(),
        kind: BackgroundKind::Shell,
        description: "Run b1".to_owned(),
    };
    assert_eq!(started, [Vec::new(), vec![work.clone()]]);
    let view = built.session.view().await.unwrap();
    assert_eq!(view.turns[1].works, [work]);
    assert!(view.turns[0].works.is_empty());
    let cold = built.store.load(built.session.id()).await.unwrap();
    assert_eq!(cold.turns, view.turns);
}
