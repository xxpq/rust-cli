//! v5 的侧聊（session-lifecycle.md §10.58）：空闲时带全部；回合在跑时带到它的输入、不带之后的回答；调用方的 id、重发不再写、别的会话拒绝。
use std::sync::Arc;

use rust_cli_kernel_api::admission::Refusal;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::message::{ContentBlock, Message};
use rust_cli_kernel_api::session_api::{
    Ack, Command, CommandEnvelope, CommandResult, Forked, KernelEvent, SessionHandle,
};
use rust_cli_kernel_api::store::side_chat::SOURCE;
use rust_cli_kernel_api::store::{SessionKind, SessionStore};
use rust_cli_kernel_api::testing::{ScriptedModel, text_response};
use tokio::sync::Notify;

use crate::test_support::{Setup, envelope, finished, meta, submit, until};

fn side_chat(command: &str, session: &str) -> CommandEnvelope {
    envelope(
        command,
        Command::SideChatAs {
            session: SessionId::new(session),
        },
    )
}

fn forked(ack: Ack) -> Forked {
    match ack {
        Ack::Accepted {
            result: CommandResult::Forked(forked),
        } => forked,
        other => panic!("{other:?}"),
    }
}

/// 输入与回答的文字（只看文字块），与侧聊边界的来源。
fn shapes(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .filter_map(|message| match message {
            Message::User(user) => first_text(&user.content),
            Message::Assistant(assistant) => first_text(&assistant.content),
            Message::Attachment(attachment) => Some(attachment.source.clone()),
            Message::System(_) => None,
        })
        .collect()
}

fn first_text(content: &[ContentBlock]) -> Option<String> {
    content.iter().find_map(|block| match block {
        ContentBlock::Text { text } => Some(text.clone()),
        _ => None,
    })
}

/// 空闲时带全部，末尾是侧聊边界；新会话是侧聊这一种、父会话是它；同一个 id 再来一次（新的命令 id：跨重启的重发）什么都不写；
/// 同一个命令 id 是账本里的重复。
#[tokio::test]
async fn an_idle_session_gives_its_whole_history_once() {
    let model = Arc::new(ScriptedModel::new(vec![
        text_response(&["one"]),
        text_response(&["two"]),
    ]));
    let built = Setup::new(model).build().await;
    let mut events = built.session.subscribe(Some(0));
    for (id, text) in [("c1", "first"), ("c2", "second")] {
        built.session.send(submit(id, text)).await;
        until(&mut events, finished).await;
    }
    let ack = built.session.send(side_chat("side-1", "ses_side")).await;
    let child = SessionId::new("ses_side");
    assert_eq!(
        forked(ack.clone()),
        Forked {
            session: child.clone(),
            through: None,
            copied: 4,
        }
    );
    let loaded = built.store.load(&child).await.unwrap();
    assert_eq!(loaded.meta.kind, SessionKind::SideChat);
    assert_eq!(loaded.meta.parent, Some(meta().id));
    assert_eq!(
        shapes(&loaded.messages),
        ["first", "one", "second", "two", SOURCE]
    );
    let written = loaded.messages.len();
    assert_eq!(
        forked(built.session.send(side_chat("side-2", "ses_side")).await),
        Forked {
            session: child.clone(),
            through: None,
            copied: 0,
        }
    );
    assert_eq!(
        built.store.load(&child).await.unwrap().messages.len(),
        written
    );
    assert_eq!(
        built.session.send(side_chat("side-1", "ses_side")).await,
        Ack::Duplicate {
            original: Box::new(ack)
        }
    );
}

/// 调用方给的 id 已经是别的会话：拒绝（`Unavailable`），那个会话不变。
#[tokio::test]
async fn a_callers_id_taken_by_another_session_is_refused() {
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["one"])]));
    let built = Setup::new(model).build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "first")).await;
    until(&mut events, finished).await;
    let other = SessionId::new("ses_other");
    built
        .store
        .create(
            rust_cli_kernel_api::store::SessionMeta {
                id: other.clone(),
                ..meta()
            },
            rust_cli_kernel_api::testing::no_facts(),
        )
        .await
        .unwrap();
    assert_eq!(
        built.session.send(side_chat("side-1", "ses_other")).await,
        Ack::Rejected {
            refusal: Refusal::Unavailable,
            reason: "session already exists: ses_other".to_owned(),
        }
    );
    let loaded = built.store.load(&other).await.unwrap();
    assert_eq!(loaded.meta.parent, None);
    assert!(loaded.messages.is_empty());
}

/// 回合在跑时：带到它的输入（含），这一轮之后落库的回答不带；父会话的回合照常跑完，侧聊不变。
#[tokio::test]
async fn a_side_chat_beside_a_running_turn_takes_its_input_only() {
    let gate = Arc::new(Notify::new());
    let model = Arc::new(
        ScriptedModel::new(vec![text_response(&["one"]), text_response(&["two"])])
            .gated_at(1, gate.clone()),
    );
    let built = Setup::new(model).build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "first")).await;
    until(&mut events, finished).await;
    built.session.send(submit("c2", "second")).await;
    // 第二轮的输入落库了（它的回合记录也在），回答等闸门
    until(&mut events, |event| {
        matches!(event, KernelEvent::InputCommitted { source } if source.command.as_str() == "c2")
    })
    .await;
    let side = forked(built.session.send(side_chat("side-1", "ses_side")).await);
    assert_eq!(side.copied, 3);
    let child = SessionId::new("ses_side");
    assert_eq!(
        shapes(&built.store.load(&child).await.unwrap().messages),
        ["first", "one", "second", SOURCE]
    );
    assert!(built.session.view().await.unwrap().running.is_some());
    gate.notify_one();
    until(&mut events, finished).await;
    let after = built.session.view().await.unwrap().messages;
    assert_eq!(shapes(&after), ["first", "one", "second", "two"]);
    assert_eq!(
        shapes(&built.store.load(&child).await.unwrap().messages),
        ["first", "one", "second", SOURCE]
    );
}

/// 会话层规则照 v4 继承（§10.58，主会话审定）：取的是父会话处理这条命令那一刻的快照，侧聊第一次冷加载时取走；之后父会话再改不影响已取的快照。
#[tokio::test]
async fn a_side_chat_takes_the_parents_session_rules_of_that_moment() {
    use rust_cli_kernel_api::permission::{PermissionRule, RuleBehavior, RuleSource, SourcedRule};
    use rust_cli_kernel_api::permission_updates::{PermissionUpdate, UpdatePlace};
    let add = |tool: &str| Command::UpdatePermissions {
        updates: vec![PermissionUpdate::AddRules {
            place: UpdatePlace::Session,
            behavior: RuleBehavior::Allow,
            rules: vec![PermissionRule {
                tool: tool.to_owned(),
                content: None,
            }],
        }],
    };
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["one"])]));
    let built = Setup::new(model).build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "first")).await;
    until(&mut events, finished).await;
    let ack = built.session.send(envelope("rules-1", add("Write"))).await;
    assert!(matches!(ack, Ack::Accepted { .. }), "{ack:?}");
    forked(built.session.send(side_chat("side-1", "ses_side")).await);
    // 开侧聊之后父会话再加一条：不进侧聊的快照
    let ack = built.session.send(envelope("rules-2", add("Bash"))).await;
    assert!(matches!(ack, Ack::Accepted { .. }), "{ack:?}");
    let inherited = built.forks.take(&SessionId::new("ses_side"));
    assert_eq!(
        inherited.rules,
        [SourcedRule {
            source: RuleSource::Session,
            behavior: RuleBehavior::Allow,
            rule: PermissionRule {
                tool: "Write".to_owned(),
                content: None,
            },
        }]
    );
    assert!(
        built
            .forks
            .take(&SessionId::new("ses_side"))
            .rules
            .is_empty()
    );
    assert_eq!(built.session.view().await.unwrap().session_rules.len(), 2);
}
