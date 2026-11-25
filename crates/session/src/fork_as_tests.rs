//! v5 的 fork（session-lifecycle.md §10.57）：调用方给新会话的 id、重发不再写、id 被别的会话占着时拒绝、回合在跑时分更早的一轮；
//! 两种 fork 都复制 ExitPlanMode 的计划。
use std::sync::Arc;

use rust_cli_kernel_api::admission::Refusal;
use rust_cli_kernel_api::ids::{MessageId, SessionId, ToolUseId};
use rust_cli_kernel_api::message::{
    AssistantMessage, ContentBlock, Message, ToolResultContent, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::plan::ShownPlan;
use rust_cli_kernel_api::session_api::{
    Ack, Command, CommandEnvelope, CommandResult, Forked, KernelEvent, SessionHandle,
};
use rust_cli_kernel_api::store::{Fact, FactBatch, SessionKind, SessionStore};
use rust_cli_kernel_api::testing::{MemoryStore, ScriptedModel, text_response};
use serde_json::json;
use tokio::sync::Notify;

use crate::Opened;
use crate::test_support::{Setup, envelope, finished, meta, submit, until};

fn fork_as(command: &str, input: &MessageId, session: &str) -> CommandEnvelope {
    envelope(
        command,
        Command::ForkFromInputAs {
            input: input.clone(),
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

/// 输入与回答的文字（只看文字块）。
fn texts(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .filter_map(|message| {
            let content = match message {
                Message::User(user) => &user.content,
                Message::Assistant(assistant) => &assistant.content,
                _ => return None,
            };
            content.iter().find_map(|block| match block {
                ContentBlock::Text { text } => Some(text.clone()),
                _ => None,
            })
        })
        .collect()
}

fn id_of(messages: &[Message], text: &str) -> MessageId {
    messages
        .iter()
        .find(|message| texts(std::slice::from_ref(*message)) == [text])
        .map(|message| message.id().clone())
        .unwrap()
}

/// 新会话的 id 是调用方给的；同一个 id 再分一次（新的命令 id：跨重启的重发）什么都不写，回同一个会话；同一个命令 id 是账本里的重复。
#[tokio::test]
async fn a_fork_into_the_callers_id_is_written_once() {
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
    let messages = built.session.view().await.unwrap().messages;
    let (first, one) = (id_of(&messages, "first"), id_of(&messages, "one"));
    let ack = built
        .session
        .send(fork_as("fork-1", &first, "ses_child"))
        .await;
    let child = SessionId::new("ses_child");
    assert_eq!(
        forked(ack.clone()),
        Forked {
            session: child.clone(),
            through: Some(one),
            copied: 2,
        }
    );
    let loaded = built.store.load(&child).await.unwrap();
    assert_eq!(loaded.meta.kind, SessionKind::Fork);
    assert_eq!(loaded.meta.parent, Some(meta().id));
    assert_eq!(texts(&loaded.messages), ["first", "one"]);
    let written = loaded.messages.len();
    assert_eq!(
        forked(
            built
                .session
                .send(fork_as("fork-2", &first, "ses_child"))
                .await
        ),
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
        built
            .session
            .send(fork_as("fork-1", &first, "ses_child"))
            .await,
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
    let first = built.session.view().await.unwrap().messages[0].id().clone();
    assert_eq!(
        built
            .session
            .send(fork_as("fork-1", &first, "ses_other"))
            .await,
        Ack::Rejected {
            refusal: Refusal::Unavailable,
            reason: "session already exists: ses_other".to_owned(),
        }
    );
    let loaded = built.store.load(&other).await.unwrap();
    assert_eq!(loaded.meta.parent, None);
    assert!(loaded.messages.is_empty());
}

/// 回合在跑时：分更早、已经结束的一轮，父会话的回合照常跑完；分在跑的那一轮 → `Refusal::Busy`，什么都不写。
#[tokio::test]
async fn a_fork_beside_a_running_turn_takes_only_a_finished_turn() {
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
    let messages = built.session.view().await.unwrap().messages;
    let (first, second) = (id_of(&messages, "first"), id_of(&messages, "second"));
    let earlier = forked(
        built
            .session
            .send(fork_as("fork-1", &first, "ses_child"))
            .await,
    );
    assert_eq!(earlier.through, Some(id_of(&messages, "one")));
    let child = built
        .store
        .load(&SessionId::new("ses_child"))
        .await
        .unwrap();
    assert_eq!(texts(&child.messages), ["first", "one"]);
    assert!(built.session.view().await.unwrap().running.is_some());
    assert_eq!(
        built
            .session
            .send(fork_as("fork-2", &second, "ses_running"))
            .await,
        Ack::Rejected {
            refusal: Refusal::Busy,
            reason: super::TURN_RUNNING.to_owned(),
        }
    );
    assert!(
        built
            .store
            .load(&SessionId::new("ses_running"))
            .await
            .is_err()
    );
    // 父会话的回合没被打断
    gate.notify_one();
    until(&mut events, finished).await;
    let after = built.session.view().await.unwrap().messages;
    assert_eq!(texts(&after), ["first", "one", "second", "two"]);
}

/// 两种 fork 都把复制范围里的调用的计划带进新会话（§10.57）：`ForkFromInputAs` 经存储的分叉，`Fork` 在复制消息的那一批里。
#[tokio::test]
async fn the_plan_of_a_copied_call_goes_with_both_forks() {
    let call = ToolUseId::new("toolu_plan");
    let history = vec![
        Message::User(UserMessage {
            id: MessageId::new("msg_u1"),
            content: vec![ContentBlock::Text {
                text: "plan it".to_owned(),
            }],
            origin: UserOrigin::Human,
        }),
        Message::Assistant(AssistantMessage {
            id: MessageId::new("msg_a1"),
            content: vec![ContentBlock::ToolUse {
                id: call.clone(),
                name: "ExitPlanMode".to_owned(),
                input: json!({}),
            }],
            model: "m".to_owned(),
            stop_reason: None,
            usage: None,
        }),
        Message::User(UserMessage {
            id: MessageId::new("msg_r1"),
            content: vec![ContentBlock::ToolResult {
                tool_use_id: call.clone(),
                content: vec![ToolResultContent::Text {
                    text: "approved".to_owned(),
                }],
                is_error: false,
            }],
            origin: UserOrigin::ToolResult {
                assistant: MessageId::new("msg_a1"),
            },
        }),
        Message::Assistant(AssistantMessage {
            id: MessageId::new("msg_a2"),
            content: vec![ContentBlock::Text {
                text: "done".to_owned(),
            }],
            model: "m".to_owned(),
            stop_reason: None,
            usage: None,
        }),
    ];
    let plan = ShownPlan {
        tool_use: call,
        plan: "# the plan".to_owned(),
        path: "/work/plan.md".to_owned(),
    };
    let store = Arc::new(MemoryStore::default());
    store
        .create(meta(), rust_cli_kernel_api::testing::no_facts())
        .await
        .unwrap();
    let mut facts: Vec<Fact> = history
        .iter()
        .map(|message| Fact::MessageAppended {
            message: message.clone(),
        })
        .collect();
    facts.push(plan.fact());
    let batch = FactBatch {
        command: None,
        facts,
        turn: None,
    };
    store.commit(&meta().id, batch).await.unwrap();
    let mut setup = Setup::new(Arc::new(ScriptedModel::new(Vec::new())));
    setup.store = Some(store.clone());
    setup.messages = history;
    setup.plans = vec![plan.clone()];
    setup.opened = Opened::Loaded;
    let built = setup.build().await;
    let by_input = forked(
        built
            .session
            .send(fork_as("fork-1", &MessageId::new("msg_u1"), "ses_child"))
            .await,
    );
    assert_eq!(
        store.load(&by_input.session).await.unwrap().plans,
        std::slice::from_ref(&plan)
    );
    let whole = forked(
        built
            .session
            .send(envelope("fork-2", Command::Fork { at: None }))
            .await,
    );
    assert_eq!(store.load(&whole.session).await.unwrap().plans, [plan]);
}
