//! 改排队的项与重排（session-lifecycle.md §10.21、§10.30，v5 的 `queue.edit` / `queue.reorder`）：改正文时前端看到的整项是前置条件，
//! 位置不是项的字段；挪动只看项与锚点在不在。

use std::sync::Arc;

use futures_util::stream::BoxStream;
use rust_cli_kernel_api::admission::Refusal;
use rust_cli_kernel_api::ids::CommandId;
use rust_cli_kernel_api::session_api::{
    Ack, Command, KernelEvent, QueuedInput, SequencedEvent, SessionHandle,
};
use rust_cli_kernel_api::testing::{ScriptedModel, text_response};
use tokio::sync::Notify;

use crate::test_support::{Built, Setup, envelope, finished, submit, until};

/// 前端看到的这一项：视图里排队的那一份（v5 的投影就是它的投影）。
async fn seen(built: &Built, command: &str) -> QueuedInput {
    built
        .session
        .view()
        .await
        .unwrap()
        .queue
        .into_iter()
        .find(|item| item.command.as_str() == command)
        .unwrap()
}

fn edit(
    id: &str,
    command: &str,
    text: &str,
    expected: QueuedInput,
) -> rust_cli_kernel_api::session_api::CommandEnvelope {
    envelope(
        id,
        Command::EditQueued {
            command: CommandId::new(command),
            edit: Box::new(rust_cli_kernel_api::session_api::QueuedEdit::text(text)),
            guard: rust_cli_kernel_api::session_api::EditGuard::Seen {
                expected: Box::new(expected),
            },
        },
    )
}

fn move_before(
    id: &str,
    command: &str,
    before: Option<&str>,
) -> rust_cli_kernel_api::session_api::CommandEnvelope {
    envelope(
        id,
        Command::MoveQueued {
            command: CommandId::new(command),
            before: before.map(CommandId::new),
        },
    )
}

fn refused(ack: &Ack) -> Option<&Refusal> {
    match ack {
        Ack::Rejected { refusal, .. } => Some(refusal),
        _ => None,
    }
}

async fn queue(built: &Built) -> Vec<(String, String)> {
    built
        .session
        .view()
        .await
        .unwrap()
        .queue
        .into_iter()
        .map(|item| (item.command.as_str().to_owned(), item.text))
        .collect()
}

fn pairs(items: &[(&str, &str)]) -> Vec<(String, String)> {
    items
        .iter()
        .map(|(id, text)| ((*id).to_owned(), (*text).to_owned()))
        .collect()
}

/// T1 在模型请求上停着，排着 `texts` 里的每一条（c2、c3……）。
async fn queued_behind_t1(
    texts: &[&str],
) -> (
    Built,
    Arc<ScriptedModel>,
    Arc<Notify>,
    BoxStream<'static, SequencedEvent>,
) {
    let gate = Arc::new(Notify::new());
    let model = Arc::new(
        ScriptedModel::new(vec![text_response(&["one"]), text_response(&["two"])])
            .gated_at(0, gate.clone()),
    );
    let built = Setup::new(model.clone()).build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "first")).await;
    until(&mut events, |event| {
        matches!(event, KernelEvent::TurnStarted { .. })
    })
    .await;
    for (index, text) in texts.iter().enumerate() {
        built
            .session
            .send(submit(&format!("c{}", index + 2), text))
            .await;
    }
    (built, model, gate, events)
}

/// 改正文：位置不变；按旧的那一份再改一次被拒、什么都不改（两条命令背靠背）；不在队列里的 NotFound。开下一轮时用改过的正文。
#[tokio::test]
async fn editing_changes_the_text_in_place_and_a_stale_edit_changes_nothing() {
    let (built, model, gate, mut events) = queued_behind_t1(&["second", "third"]).await;
    let before = seen(&built, "c2").await;
    let ack = built
        .session
        .send(edit("e1", "c2", "edited", before.clone()))
        .await;
    assert!(matches!(ack, Ack::Accepted { .. }), "{ack:?}");
    let ack = built
        .session
        .send(edit("e2", "c2", "again", before.clone()))
        .await;
    assert_eq!(refused(&ack), Some(&Refusal::Stale));
    let ack = built.session.send(edit("e3", "c9", "x", before)).await;
    assert_eq!(refused(&ack), Some(&Refusal::NotFound));
    assert_eq!(
        queue(&built).await,
        pairs(&[("c2", "edited"), ("c3", "third")])
    );
    gate.notify_one();
    until(&mut events, finished).await;
    until(&mut events, finished).await;
    let next = serde_json::to_string(&model.requests()[1].messages).unwrap();
    assert!(next.contains("edited"), "{next}");
    assert!(!next.contains("second"), "{next}");
}

/// 重排：挪到锚点之前、挪到最后；锚点是它自己时不变；锚点不在、项不在时 NotFound。挪动不比内容（§10.30）：别人刚改过正文的项照样挪。
#[tokio::test]
async fn moving_puts_the_item_before_the_anchor_or_last() {
    let (built, _model, gate, _events) = queued_behind_t1(&["second", "third", "fourth"]).await;
    let ack = built
        .session
        .send(move_before("m1", "c4", Some("c2")))
        .await;
    assert!(matches!(ack, Ack::Accepted { .. }), "{ack:?}");
    assert_eq!(
        queue(&built).await,
        pairs(&[("c4", "fourth"), ("c2", "second"), ("c3", "third")])
    );
    let current = seen(&built, "c2").await;
    built
        .session
        .send(edit("e1", "c2", "edited", current))
        .await;
    let ack = built.session.send(move_before("m3", "c2", None)).await;
    assert!(matches!(ack, Ack::Accepted { .. }), "{ack:?}");
    assert_eq!(
        queue(&built).await,
        pairs(&[("c4", "fourth"), ("c3", "third"), ("c2", "edited")])
    );
    let ack = built
        .session
        .send(move_before("m4", "c3", Some("c3")))
        .await;
    assert!(matches!(ack, Ack::Accepted { .. }), "{ack:?}");
    let ack = built
        .session
        .send(move_before("m5", "c3", Some("c9")))
        .await;
    assert_eq!(refused(&ack), Some(&Refusal::NotFound));
    let ack = built.session.send(move_before("m6", "c9", None)).await;
    assert_eq!(refused(&ack), Some(&Refusal::NotFound));
    assert_eq!(
        queue(&built).await,
        pairs(&[("c4", "fourth"), ("c3", "third"), ("c2", "edited")])
    );
    gate.notify_one();
}

/// 13 Q24：排在前面的项出队之后，按出队之前看到的那一份改后面的项照样生效——位置不是项的字段，别的项走了不是冲突。
/// 以前比（正文，位置），这一条被拒（`Stale`）。
#[tokio::test]
async fn a_dequeue_ahead_keeps_the_item_and_an_edit_with_what_was_seen_applies() {
    let (built, _model, gate, _events) = queued_behind_t1(&["second", "third"]).await;
    let before = seen(&built, "c3").await;
    built
        .session
        .send(envelope(
            "d1",
            Command::Dequeue {
                command: CommandId::new("c2"),
            },
        ))
        .await;
    let ack = built.session.send(edit("e1", "c3", "edited", before)).await;
    assert!(matches!(ack, Ack::Accepted { .. }), "{ack:?}");
    assert_eq!(queue(&built).await, pairs(&[("c3", "edited")]));
    gate.notify_one();
}

/// 排队的压缩没有正文可改（Unavailable）；挪动照常。
#[tokio::test]
async fn a_queued_compaction_moves_but_is_not_editable() {
    let (built, _model, gate, _events) = queued_behind_t1(&["second"]).await;
    built
        .session
        .send(envelope("k1", Command::CompactOrQueue { selection: None }))
        .await;
    let compaction = seen(&built, "k1").await;
    let ack = built
        .session
        .send(edit("e1", "k1", "text", compaction))
        .await;
    assert_eq!(refused(&ack), Some(&Refusal::Unavailable));
    let ack = built
        .session
        .send(move_before("m1", "k1", Some("c2")))
        .await;
    assert!(matches!(ack, Ack::Accepted { .. }), "{ack:?}");
    assert_eq!(queue(&built).await, pairs(&[("k1", ""), ("c2", "second")]));
    gate.notify_one();
}
