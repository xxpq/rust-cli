//! 换掉一条输入（session-lifecycle.md §10.53，v5 的编辑重发与重试）：回退到它之前，再以这条命令开新输入的回合；回合在跑时先停再换。

use std::sync::Arc;

use futures_util::stream::BoxStream;
use rust_cli_kernel_api::admission::Refusal;
use rust_cli_kernel_api::env_rewind::RestoreAction;
use rust_cli_kernel_api::ids::{CommandId, MachineName, MessageId};
use rust_cli_kernel_api::message::{Message, UserOrigin};
use rust_cli_kernel_api::session_api::{
    Ack, Command, CommandResult, InputFate, KernelEvent, QueueHold, RewindScope, RewoundFile,
    SequencedEvent, SessionHandle,
};
use rust_cli_kernel_api::store::Fact;
use rust_cli_kernel_api::testing::{ScriptedModel, text_response, tool_use_response};
use serde_json::json;
use tokio::sync::Notify;

use crate::rewind::tests::{Writing, file};
use crate::test_support::{Built, Setup, envelope, finished, submit, until};

fn replace(target: &MessageId, text: &str, restore_code: bool) -> Command {
    Command::ReplaceInput {
        target: target.clone(),
        text: text.to_owned(),
        extras: Default::default(),
        restore_code,
        stop_cut_works: false,
    }
}

/// 人类输入的消息 id，按顺序。
fn prompts(messages: &[Message]) -> Vec<MessageId> {
    messages
        .iter()
        .filter_map(|message| match message {
            Message::User(user) if user.origin == UserOrigin::Human => Some(user.id.clone()),
            _ => None,
        })
        .collect()
}

/// 人类输入的正文，按顺序。
fn prompt_texts(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .filter_map(|message| match message {
            Message::User(user) if user.origin == UserOrigin::Human => Some(text(&user.content)),
            _ => None,
        })
        .collect()
}

/// 两轮完成的会话（first → one，second → two），模型还剩 `more` 个回答。
async fn two_rounds(
    more: &[&str],
) -> (
    Built,
    Arc<ScriptedModel>,
    BoxStream<'static, SequencedEvent>,
) {
    let mut replies = vec![text_response(&["one"]), text_response(&["two"])];
    replies.extend(more.iter().map(|text| text_response(&[text])));
    let model = Arc::new(ScriptedModel::new(replies));
    let built = Setup::new(model.clone()).build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "first")).await;
    until(&mut events, finished).await;
    built.session.send(submit("c2", "second")).await;
    until(&mut events, finished).await;
    (built, model, events)
}

/// 用户消息里的文字块接起来。
fn text(content: &[rust_cli_kernel_api::message::ContentBlock]) -> String {
    content
        .iter()
        .filter_map(|block| match block {
            rust_cli_kernel_api::message::ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn rewound(ack: &Ack) -> (&[RewoundFile], usize) {
    match ack {
        Ack::Accepted {
            result: CommandResult::Rewound { restored, removed },
        } => (restored, *removed),
        other => panic!("{other:?}"),
    }
}

/// 空闲：对话截在目标之前，这条命令开新输入的回合；回执在回合开了之后。
#[tokio::test]
async fn replacing_an_input_cuts_before_it_and_starts_the_new_one() {
    let (built, model, mut events) = two_rounds(&["edited"]).await;
    let view = built.session.view().await.unwrap();
    let second = prompts(&view.messages)[1].clone();
    let ack = built
        .session
        .send(envelope("e1", replace(&second, "second again", false)))
        .await;
    assert_eq!(rewound(&ack), (&[][..], 2));
    let seen = until(&mut events, finished).await;
    // 先改写，再开回合：开回合的命令就是这条命令
    let rewritten = seen
        .iter()
        .position(|e| matches!(e, KernelEvent::HistoryRewritten { .. }))
        .unwrap();
    let started = seen
        .iter()
        .position(|e| matches!(e, KernelEvent::TurnStarted { command, .. } if *command == Some(CommandId::new("e1"))))
        .unwrap();
    assert!(rewritten < started, "{seen:?}");
    assert!(seen.iter().any(|e| matches!(e, KernelEvent::InputFate { input, fate: InputFate::Started { .. }, .. } if input.as_str() == "e1")));
    let view = built.session.view().await.unwrap();
    assert_eq!(prompt_texts(&view.messages), ["first", "second again"]);
    // 新输入的消息记着这条命令（效果记录，§10.3）
    let new = prompts(&view.messages)[1].clone();
    assert!(
        view.sources
            .iter()
            .any(|s| s.message == new && s.command == CommandId::new("e1"))
    );
    // 模型只看到截过的历史与新输入
    let last = serde_json::to_string(&model.requests().last().unwrap().messages).unwrap();
    assert!(
        last.contains("second again") && !last.contains("\"two\""),
        "{last}"
    );
}

/// 更早的输入也能换：它之后的都截掉。
#[tokio::test]
async fn replacing_an_earlier_input_cuts_everything_after_it() {
    let (built, _model, mut events) = two_rounds(&["edited"]).await;
    let first = prompts(&built.session.view().await.unwrap().messages)[0].clone();
    let ack = built
        .session
        .send(envelope("e1", replace(&first, "first again", false)))
        .await;
    assert_eq!(rewound(&ack).1, 4);
    until(&mut events, finished).await;
    let view = built.session.view().await.unwrap();
    assert_eq!(prompt_texts(&view.messages), ["first again"]);
}

/// 与「Rewind 之后再 Submit」写的事实相同：一批游标，之后是普通输入的批次（Node 读到的是同一种记录）。
#[tokio::test]
async fn replacing_writes_the_same_facts_as_rewinding_then_submitting() {
    let (replaced, _m1, mut events) = two_rounds(&["edited"]).await;
    let second = prompts(&replaced.session.view().await.unwrap().messages)[1].clone();
    replaced
        .session
        .send(envelope("e1", replace(&second, "second again", false)))
        .await;
    until(&mut events, finished).await;

    let (composed, _m2, mut events) = two_rounds(&["edited"]).await;
    let second = prompts(&composed.session.view().await.unwrap().messages)[1].clone();
    let rewind = Command::Rewind {
        target: second,
        scope: RewindScope::Conversation,
    };
    composed.session.send(envelope("e1", rewind)).await;
    composed.session.send(submit("e1b", "second again")).await;
    until(&mut events, finished).await;

    let id = crate::test_support::meta().id;
    let kinds = |built: &Built| -> Vec<String> {
        built
            .store
            .facts(&id)
            .iter()
            .map(|fact| match fact {
                Fact::MessageAppended { message } => format!("message:{}", message_kind(message)),
                Fact::RevertSet { cursor } => format!("revert:{}", cursor.kept.len()),
                other => serde_json::to_value(other).unwrap()["type"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
            })
            .collect()
    };
    assert_eq!(kinds(&replaced), kinds(&composed));
}

fn message_kind(message: &Message) -> String {
    match message {
        Message::User(user) => format!("user:{}", text(&user.content)),
        Message::Assistant(_) => "assistant".to_owned(),
        Message::Attachment(attachment) => format!("attachment:{}", attachment.source),
        Message::System(_) => "system".to_owned(),
    }
}

/// 回合在跑：同停止一样中止它（v5 扣住排队的输入），回合结束后先换；回执等到新回合开了，中间不开别的回合。
#[tokio::test]
async fn replacing_during_a_turn_stops_it_then_starts_the_new_input() {
    let gate = Arc::new(Notify::new());
    let model = Arc::new(
        ScriptedModel::new(vec![text_response(&["one"]), text_response(&["edited"])])
            .gated_at(0, gate),
    );
    let mut setup = Setup::new(model.clone());
    setup.hold_queue = true;
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "first")).await;
    until(&mut events, |e| {
        matches!(e, KernelEvent::MessageDone { .. })
    })
    .await;
    built.session.send(submit("c2", "queued")).await;
    let first = prompts(&built.session.view().await.unwrap().messages)[0].clone();
    let ack = built
        .session
        .send(envelope("e1", replace(&first, "first again", false)))
        .await;
    assert!(
        matches!(
            ack,
            Ack::Accepted {
                result: CommandResult::Rewound { .. }
            }
        ),
        "{ack:?}"
    );
    let mut seen = until(&mut events, finished).await;
    seen.extend(until(&mut events, finished).await);
    let starts: Vec<_> = seen
        .iter()
        .filter_map(|e| match e {
            KernelEvent::TurnStarted { command, .. } => command.clone(),
            _ => None,
        })
        .collect();
    assert_eq!(
        starts,
        [CommandId::new("e1")],
        "only the new input opened a turn"
    );
    let view = built.session.view().await.unwrap();
    assert_eq!(prompt_texts(&view.messages), ["first again"]);
    assert_eq!(view.queue_hold, Some(QueueHold::Stopped));
    assert_eq!(view.queue.len(), 1);
    assert_eq!(view.queue[0].command, CommandId::new("c2"));
    assert_eq!(model.requests().len(), 2);
}

/// 空闲、队列扣住：换掉照常做，扣住的照旧扣住（v4 的 editKeepsQueue）。
#[tokio::test]
async fn a_held_queue_stays_held() {
    let gate = Arc::new(Notify::new());
    let model = Arc::new(
        ScriptedModel::new(vec![text_response(&["one"]), text_response(&["edited"])])
            .gated_at(0, gate),
    );
    let mut setup = Setup::new(model.clone());
    setup.hold_queue = true;
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "first")).await;
    until(&mut events, |e| {
        matches!(e, KernelEvent::MessageDone { .. })
    })
    .await;
    built.session.send(submit("c2", "queued")).await;
    built
        .session
        .send(envelope("s1", Command::Interrupt { target: None }))
        .await;
    until(&mut events, finished).await;
    let first = prompts(&built.session.view().await.unwrap().messages)[0].clone();
    let ack = built
        .session
        .send(envelope("e1", replace(&first, "first again", false)))
        .await;
    assert!(matches!(ack, Ack::Accepted { .. }), "{ack:?}");
    until(&mut events, finished).await;
    let view = built.session.view().await.unwrap();
    assert_eq!(view.queue_hold, Some(QueueHold::Stopped));
    assert_eq!(view.queue.len(), 1);
}

/// 一轮「写 a.txt」的会话，模型多一个回答给换掉之后的新回合。
async fn wrote_with_a_spare_reply() -> (
    Built,
    Arc<rust_cli_kernel_api::testing::MemoryEnvironment>,
    MessageId,
    BoxStream<'static, SequencedEvent>,
) {
    let model = Arc::new(ScriptedModel::new(vec![
        tool_use_response(
            "toolu_1",
            "Write",
            &json!({"file_path": "/work/a.txt", "content": "new\n"}),
        ),
        text_response(&["done"]),
        text_response(&["edited"]),
    ]));
    let env = crate::test_support::env();
    env.set_file("/work/a.txt", "old\n");
    let mut setup = Setup::new(model);
    setup.set_env(env.clone());
    setup.tools = vec![Arc::new(Writing)];
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "write it")).await;
    until(&mut events, finished).await;
    let prompt = prompts(&built.session.view().await.unwrap().messages)[0].clone();
    (built, env, prompt, events)
}

/// 带代码：先恢复文件，再截对话（同一批提交），再开新回合。
#[tokio::test]
async fn replacing_with_code_restores_the_files_first() {
    let (built, env, prompt, mut events) = wrote_with_a_spare_reply().await;
    let ack = built
        .session
        .send(envelope("e1", replace(&prompt, "write it again", true)))
        .await;
    let (restored, removed) = rewound(&ack);
    assert_eq!(
        restored,
        [RewoundFile {
            path: rust_cli_kernel_api::env::EnvPath::new("/work/a.txt"),
            action: RestoreAction::Restore,
            machine: MachineName::primary(),
        }]
    );
    assert_eq!(removed, 4);
    assert_eq!(file(&env, "/work/a.txt").as_deref(), Some("old\n"));
    until(&mut events, finished).await;
    let facts = built.store.facts(&crate::test_support::meta().id);
    let revert = facts
        .iter()
        .position(|f| matches!(f, Fact::RevertSet { cursor } if cursor.target == prompt))
        .unwrap();
    assert!(
        matches!(&facts[revert - 1], Fact::CheckpointsRestored { checkpoints } if checkpoints.len() == 1)
    );
    let view = built.session.view().await.unwrap();
    assert_eq!(prompt_texts(&view.messages), ["write it again"]);
}

/// 文件在写过之后又被改过：恢复不了，文件、对话都不动，也不开回合。
#[tokio::test]
async fn a_file_that_cannot_be_restored_changes_nothing() {
    let (built, env, prompt, _events) = wrote_with_a_spare_reply().await;
    env.set_file("/work/a.txt", "edited by hand\n");
    let ack = built
        .session
        .send(envelope("e1", replace(&prompt, "write it again", true)))
        .await;
    assert!(
        matches!(&ack, Ack::Rejected { refusal: Refusal::Unavailable, reason }
        if reason == "Cannot restore code: /work/a.txt was changed after ZCode edited it"),
        "{ack:?}"
    );
    assert_eq!(
        file(&env, "/work/a.txt").as_deref(),
        Some("edited by hand\n")
    );
    let view = built.session.view().await.unwrap();
    assert_eq!(view.messages.len(), 4);
    assert!(view.running.is_none());
}

/// 目标不是可选的输入 → NotFound；已有「现在发」在等 → Busy；同一个 id 重发回原结果。
#[tokio::test]
async fn a_missing_target_a_pending_preemption_and_a_repeat() {
    let (built, _model, mut events) = two_rounds(&["edited"]).await;
    let ack = built
        .session
        .send(envelope(
            "e0",
            replace(&MessageId::new("msg_none"), "x", false),
        ))
        .await;
    assert!(
        matches!(
            ack,
            Ack::Rejected {
                refusal: Refusal::NotFound,
                ..
            }
        ),
        "{ack:?}"
    );
    let second = prompts(&built.session.view().await.unwrap().messages)[1].clone();
    let first = built
        .session
        .send(envelope("e1", replace(&second, "second again", false)))
        .await;
    until(&mut events, finished).await;
    let again = built
        .session
        .send(envelope("e1", replace(&second, "second again", false)))
        .await;
    assert_eq!(
        again,
        Ack::Duplicate {
            original: Box::new(first)
        }
    );
}
