//! 会话 actor 记下的 hook 运行（session-lifecycle.md §10.51）：回合里的按到达先后编号、在回合结束之前；SessionStart 挂在之后第一个输入开的
//! 回合上；被拦下的输入带消息 id 与原文、不扣住队列；视图带记下的。

use std::sync::Arc;

use rust_cli_kernel_api::hook_runs::{HookOrigin, HookRun, HookRunOutcome, TurnHooks};
use rust_cli_kernel_api::hooks::{HookEventName, HookOutcome};
use rust_cli_kernel_api::ids::MessageId;
use rust_cli_kernel_api::message::{ContentBlock, Message, UserMessage, UserOrigin};
use rust_cli_kernel_api::session_api::{
    Command, KernelEvent, SessionHandle, TurnOrigin, TurnOutcome,
};
use rust_cli_kernel_api::testing::{ScriptedModel, text_response};
use tokio::sync::Notify;

use crate::test_support::{ScriptedHooks, Setup, envelope, finished, submit, until};

fn run(outcome: HookRunOutcome) -> HookRun {
    HookRun {
        index: 0,
        origin: HookOrigin::User,
        command: "hook.sh".to_owned(),
        status_message: None,
        outcome,
        reason: None,
        stderr: None,
        started_at: 10,
        ended_at: 20,
    }
}

fn ran(events: &[KernelEvent]) -> Vec<TurnHooks> {
    events
        .iter()
        .filter_map(|event| match event {
            KernelEvent::HooksRan { hooks } => Some(hooks.clone()),
            _ => None,
        })
        .collect()
}

fn started_turn(events: &[KernelEvent], origin: TurnOrigin) -> Option<String> {
    events.iter().find_map(|event| match event {
        KernelEvent::TurnStarted {
            turn, origin: seen, ..
        } if *seen == origin => Some(turn.as_str().to_owned()),
        _ => None,
    })
}

/// 修 bug（19 §47.3 R-HOOK-ROWS）：以前 hook 的运行出不了内核，v5 画不出 hook 行。回合里跑的 UserPromptSubmit 挂在这一轮上、在
/// `TurnFinished` 之前；视图带着它（由视图建的投影与活会话同值）。
#[tokio::test]
async fn prompt_hooks_are_recorded_on_the_turn_before_it_ends() {
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["ok"])]));
    let hooks = ScriptedHooks::with(
        HookEventName::UserPromptSubmit,
        HookOutcome {
            runs: vec![run(HookRunOutcome::Success)],
            ..HookOutcome::default()
        },
    );
    let mut setup = Setup::new(model);
    setup.hooks = Arc::new(hooks);
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "hello")).await;
    let seen = until(&mut events, finished).await;
    let turn = started_turn(&seen, TurnOrigin::Input).unwrap();
    let recorded = ran(&seen);
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].turn.as_str(), turn);
    assert_eq!(recorded[0].seq, 0);
    assert_eq!(
        recorded[0].invocation.event,
        HookEventName::UserPromptSubmit
    );
    assert_eq!(recorded[0].invocation.runs, [run(HookRunOutcome::Success)]);
    assert_eq!(recorded[0].blocked, None);
    assert_eq!(built.session.view().await.unwrap().hooks, recorded);
}

/// 没有跑 hook（`runs` 为空）时什么都不发。
#[tokio::test]
async fn nothing_is_recorded_when_no_hook_ran() {
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["ok"])]));
    let mut setup = Setup::new(model);
    setup.hooks = Arc::new(ScriptedHooks::with(
        HookEventName::UserPromptSubmit,
        HookOutcome::default(),
    ));
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "hello")).await;
    let seen = until(&mut events, finished).await;
    assert!(ran(&seen).is_empty());
    assert!(built.session.view().await.unwrap().hooks.is_empty());
}

/// 退出码 2 拦下的输入（不落库）：hook 行带它的消息 id 与原文，v5 照 v4 给它一行；回合的结局是 `PromptBlocked`（带那句警告，TUI 照失败显示），
/// 不扣住排队的输入（照 v4：被 hook 拦下的输入是成功结束的回合，下一条照常开）。
#[tokio::test]
async fn a_blocked_prompt_carries_its_text_and_does_not_hold_the_queue() {
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["second"])]));
    let gate = Arc::new(Notify::new());
    let entered = Arc::new(Notify::new());
    let hooks = Arc::new(ScriptedHooks {
        gate: Some(gate.clone()),
        entered: Some(entered.clone()),
        ..ScriptedHooks::with(
            HookEventName::UserPromptSubmit,
            HookOutcome {
                blocking_error: Some("no".into()),
                notices: vec!["UserPromptSubmit operation blocked by hook:\nno".into()],
                runs: vec![run(HookRunOutcome::Blocked)],
                ..HookOutcome::default()
            },
        )
    });
    let mut setup = Setup::new(model);
    setup.hooks = hooks.clone();
    setup.hold_queue = true;
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "secret plan")).await;
    entered.notified().await;
    // 第二条排在被拦下的那一轮后面
    built.session.send(submit("c2", "next")).await;
    gate.notify_one();
    let seen = until(&mut events, finished).await;
    assert!(matches!(
        seen.last(),
        Some(KernelEvent::TurnFinished {
            outcome: TurnOutcome::PromptBlocked { .. },
            ..
        })
    ));
    let recorded = ran(&seen);
    let blocked = recorded[0].blocked.as_ref().unwrap();
    assert_eq!(blocked.text, "secret plan");
    // 消息 id 是这一轮开回合的消息（它没有落库）
    assert!(built.session.view().await.unwrap().messages.is_empty());
    assert!(blocked.message.as_str().starts_with("msg_"), "{blocked:?}");
    // 以前这里扣住队列（`QueueHeld{TurnFailed}`），第二条要等用户决定、等不到它的回合；现在照常开
    let next = until(&mut events, |event| {
        matches!(event, KernelEvent::TurnStarted { .. })
    })
    .await;
    assert!(
        !next
            .iter()
            .any(|event| matches!(event, KernelEvent::QueueHeld { hold: Some(_) })),
        "{next:?}"
    );
    entered.notified().await;
    gate.notify_one();
    until(&mut events, finished).await;
}

/// SessionStart 不在回合里跑：先待挂（视图里没有），压缩开的回合不收，之后第一个输入开的回合开始时挂上去（照 v4 挂在下一条用户看得见的回合上）。
#[tokio::test]
async fn session_start_hooks_wait_for_the_first_input_turn() {
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["ok"])]));
    let hooks = ScriptedHooks::with(
        HookEventName::SessionStart,
        HookOutcome {
            messages: vec![Message::User(UserMessage {
                id: MessageId::new("msg_start"),
                content: vec![ContentBlock::Text {
                    text: "start context".into(),
                }],
                origin: UserOrigin::Meta {
                    source: "hook_additional_context".into(),
                },
            })],
            runs: vec![run(HookRunOutcome::Success)],
            ..HookOutcome::default()
        },
    );
    let mut setup = Setup::new(model);
    setup.hooks = Arc::new(hooks);
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    // 启动的后台工作提交了 hook 消息：SessionStart 已经跑完，待挂
    until(&mut events, |event| {
        matches!(event, KernelEvent::MessageDone { .. })
    })
    .await;
    assert!(built.session.view().await.unwrap().hooks.is_empty());
    // 压缩以失败结束（替身的压缩器），这一轮不收 SessionStart
    built
        .session
        .send(envelope("k1", Command::Compact { instructions: None }))
        .await;
    let compacted = until(&mut events, finished).await;
    assert!(started_turn(&compacted, TurnOrigin::Compaction).is_some());
    assert!(ran(&compacted).is_empty());
    assert!(built.session.view().await.unwrap().hooks.is_empty());
    built.session.send(submit("c1", "hello")).await;
    let seen = until(&mut events, finished).await;
    let turn = started_turn(&seen, TurnOrigin::Input).unwrap();
    let recorded = ran(&seen);
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].turn.as_str(), turn);
    assert_eq!(recorded[0].invocation.event, HookEventName::SessionStart);
    // 紧跟在这一轮的开始之后
    let at = |pick: &dyn Fn(&KernelEvent) -> bool| seen.iter().position(pick).unwrap();
    assert_eq!(
        at(&|event| matches!(event, KernelEvent::HooksRan { .. })),
        at(&|event| matches!(event, KernelEvent::TurnStarted { .. })) + 1
    );
}
