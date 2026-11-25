//! 会话 actor 持有当前轮并随每个批次交给存储（node-store.md §4，修复线 F6，审查 R5-3）。
use rust_cli_kernel_api::ids::{MessageId, TurnId};
use rust_cli_kernel_api::message::{Message, UserOrigin};
use rust_cli_kernel_api::session_api::SessionHandle;
use rust_cli_kernel_api::store::{SessionStore, TurnAnchor};
use rust_cli_kernel_api::testing::{NoticeEnvironment, ScriptedModel, shell_notice, text_response};

use super::*;
use crate::Opened;
use crate::test_support::{Setup, finished, submit, until};

fn prompts(messages: &[Message]) -> Vec<MessageId> {
    messages
        .iter()
        .filter_map(|message| match message {
            Message::User(user) if user.origin == UserOrigin::Human => Some(user.id.clone()),
            _ => None,
        })
        .collect()
}

/// 以前每个批次都不带锚点（存储逐行从库里推导）。
#[tokio::test]
async fn every_batch_of_a_turn_names_the_prompt_that_opened_it() {
    let model = Arc::new(ScriptedModel::new(vec![
        text_response(&["one"]),
        text_response(&["two"]),
    ]));
    let built = Setup::new(model).build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("cmd_1", "first")).await;
    until(&mut events, finished).await;
    built.session.send(submit("cmd_2", "second")).await;
    until(&mut events, finished).await;
    let id = built.session.id().clone();
    let view = built.session.view().await.unwrap();
    let Ok([first, second]) = <[MessageId; 2]>::try_from(prompts(&view.messages)) else {
        panic!("two prompts expected");
    };
    let turns = built.store.batch_turns(&id);
    assert!(turns.len() >= 4, "{turns:?}");
    assert!(turns.iter().all(Option::is_some), "{turns:?}");
    let anchors: Vec<TurnAnchor> = turns.into_iter().flatten().collect();
    // 第一条输入之后的批次都指向它，直到第二条输入；轮次 ID 换了一次。
    let at = anchors
        .iter()
        .position(|anchor| anchor.prompt == second)
        .unwrap();
    assert!(anchors[..at].iter().all(|anchor| anchor.prompt == first));
    assert!(anchors[at..].iter().all(|anchor| anchor.prompt == second));
    assert_ne!(anchors[0].turn, anchors[at].turn);
    assert!(
        anchors[..at]
            .iter()
            .all(|anchor| anchor.turn == anchors[0].turn)
    );
}

/// 冷加载的会话从库里的当前轮起步：后台通知开的回合仍属于它（usage.md D7）。
#[tokio::test]
async fn a_notice_turn_of_a_loaded_session_keeps_the_stored_turn() {
    let env = Arc::new(NoticeEnvironment::default());
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["seen"])]));
    let mut setup = Setup::new(model);
    setup.set_env(env.clone());
    setup.opened = Opened::Loaded;
    let stored = TurnAnchor {
        prompt: MessageId::new("msg_stored"),
        turn: Some(TurnId::new("turn_stored")),
    };
    setup.turn = Some(stored.clone());
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    env.finish(shell_notice(1, "b1"));
    until(&mut events, finished).await;
    let turns = built.store.batch_turns(built.session.id());
    assert!(!turns.is_empty());
    assert!(
        turns.iter().all(|turn| turn.as_ref() == Some(&stored)),
        "{turns:?}"
    );
}

fn started(events: &[KernelEvent]) -> Vec<TurnId> {
    events
        .iter()
        .filter_map(|event| match event {
            KernelEvent::TurnStarted { turn, .. } => Some(turn.clone()),
            _ => None,
        })
        .collect()
}

/// K-I1（session-lifecycle.md §10.1）：以前 `TurnStarted` 与落库的 `anchor.turnId` 各生成一个 id。
#[tokio::test]
async fn the_started_turn_is_the_stored_turn() {
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["one"])]));
    let built = Setup::new(model).build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("cmd_1", "first")).await;
    let seen = until(&mut events, finished).await;
    let [turn] = <[TurnId; 1]>::try_from(started(&seen)).unwrap();
    let turns = built.store.batch_turns(built.session.id());
    assert!(!turns.is_empty());
    assert!(
        turns
            .iter()
            .all(|anchor| anchor.as_ref().and_then(|a| a.turn.as_ref()) == Some(&turn)),
        "{turns:?}"
    );
}

/// K-I1：每个回合（人类输入的与通知开的）一条回合记录，活的视图与冷读给出同一份。
#[tokio::test]
async fn every_turn_is_recorded_and_read_back_cold() {
    let env = Arc::new(NoticeEnvironment::default());
    let model = Arc::new(ScriptedModel::new(vec![
        text_response(&["one"]),
        text_response(&["seen"]),
    ]));
    let mut setup = Setup::new(model);
    setup.set_env(env.clone());
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("cmd_1", "first")).await;
    let mut seen = until(&mut events, finished).await;
    env.finish(shell_notice(1, "b1"));
    seen.extend(until(&mut events, finished).await);
    let turns = started(&seen);
    assert_eq!(turns.len(), 2, "{seen:?}");
    let view = built.session.view().await.unwrap();
    let recorded: Vec<TurnId> = view
        .turns
        .iter()
        .map(|record| record.turn.clone())
        .collect();
    assert_eq!(recorded, turns);
    // 每条记录的 `first` 是这个回合第一条落库的消息：人类输入，与通知的 meta 消息。
    let firsts: Vec<&MessageId> = view.turns.iter().map(|record| &record.first).collect();
    let human = prompts(&view.messages);
    assert_eq!(firsts[0], &human[0]);
    let notice = view
        .messages
        .iter()
        .find(|message| matches!(message, Message::User(user) if matches!(user.origin, UserOrigin::Meta { .. })))
        .unwrap();
    assert_eq!(firsts[1], notice.id());
    let cold = built.store.load(built.session.id()).await.unwrap();
    assert_eq!(cold.turns, view.turns);
}
