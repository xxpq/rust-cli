//! 会话事实的变化（docs/specs/rust-cli/session-facts.md §3）：每个改了列表会话事实的写事务提交之后一项，按提交的顺序；
//! 不改事实的写、不是列表会话的写、失败的写都不发。
use futures_util::{FutureExt, StreamExt};
use rust_cli_kernel_api::agents::{TaskList, TaskListStore};
use rust_cli_kernel_api::ids::{MessageId, SessionId};
use rust_cli_kernel_api::metering::CallStatus;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_facts::{SessionFacts, SessionOutcome, TitleSource};
use rust_cli_kernel_api::store::{Fact, SessionFork, SessionKind, SessionStore, StoreQueries};
use serde_json::json;

use crate::tests::{TestStore, assistant, batch, meta, selection, text, user};

fn sid(id: &str) -> SessionId {
    SessionId::new(id)
}

async fn next(
    changes: &mut futures_util::stream::BoxStream<'static, SessionFacts>,
) -> SessionFacts {
    changes.next().await.expect("the change stream ended")
}

/// 已经发出的都取过了：再没有下一项。
fn drained(changes: &mut futures_util::stream::BoxStream<'static, SessionFacts>) {
    if let Some(Some(extra)) = changes.next().now_or_never() {
        panic!("unexpected change: {extra:?}");
    }
}

#[tokio::test]
async fn each_write_of_a_listed_session_sends_its_facts_in_commit_order() {
    let t = TestStore::new();
    let mut changes = t.store.session_fact_changes();
    t.store
        .create(meta("sess_1", "/w", ""), crate::tests::batch(None, vec![]))
        .await
        .unwrap();
    let created = next(&mut changes).await;
    assert_eq!(
        (created.id.as_str(), created.title_source),
        ("sess_1", TitleSource::Default)
    );
    // 换模型：不推进活动时间（Node `touchSession: false`），但配置是事实。
    let chosen = Fact::ModelSelected {
        selection: selection(),
    };
    t.store
        .commit(&sid("sess_1"), batch(None, vec![chosen]))
        .await
        .unwrap();
    assert_eq!(next(&mut changes).await.selection, Some(selection()));
    t.source.at(5000);
    t.store
        .commit(&sid("sess_1"), batch(None, vec![user("msg_1_u1", "Hello")]))
        .await
        .unwrap();
    let input = next(&mut changes).await;
    assert_eq!(
        (input.title.as_str(), input.title_source),
        ("Hello", TitleSource::FirstInput)
    );
    assert!(input.first_input_at_ms.is_some());
    assert_eq!(input.updated_at_ms, 5000);
    t.source.at(6000);
    let ended = Fact::TurnEnded {
        status: CallStatus::Error,
    };
    t.store
        .commit(&sid("sess_1"), batch(None, vec![ended]))
        .await
        .unwrap();
    assert_eq!(
        next(&mut changes).await.last_outcome,
        Some(SessionOutcome::Error)
    );
    t.store.rename(&sid("sess_1"), "Mine").await.unwrap();
    assert_eq!(next(&mut changes).await.title_source, TitleSource::Custom);
    t.source.at(7000);
    t.store.delete(&sid("sess_1")).await.unwrap();
    let deleted = next(&mut changes).await;
    assert_eq!(deleted.deleted_at_ms, Some(7000));
    // 每一项就是提交之后库里的值。
    assert_eq!(
        t.store.session_facts(&[sid("sess_1")]).await.unwrap(),
        [deleted]
    );
    drained(&mut changes);
}

/// 不改事实的写（`rust-cli/` entry、已删除的再删一次）、不是列表会话的写（子代理的子会话）、失败的写都不发。
#[tokio::test]
async fn writes_that_leave_the_listed_facts_alone_send_nothing() {
    let t = TestStore::new();
    t.store
        .create(meta("sess_1", "/w", ""), crate::tests::batch(None, vec![]))
        .await
        .unwrap();
    let mut changes = t.store.session_fact_changes();
    let entry = Fact::Entry {
        kind: "peer_message".into(),
        id: "m1".into(),
        data: json!({"text": "hi"}),
    };
    t.store
        .commit(&sid("sess_1"), batch(None, vec![entry]))
        .await
        .unwrap();
    let mut child = meta("sess_child", "/w", "");
    child.kind = SessionKind::Subagent;
    child.parent = Some(sid("sess_1"));
    t.store
        .create(child, crate::tests::batch(None, vec![]))
        .await
        .unwrap();
    t.store
        .commit(
            &sid("sess_child"),
            batch(None, vec![user("msg_c_u1", "Work")]),
        )
        .await
        .unwrap();
    // 改过名之后，生成的标题被拒绝（Node 的条件），整批不写。
    t.store.rename(&sid("sess_1"), "Mine").await.unwrap();
    let renamed = next(&mut changes).await;
    let generated = Fact::TitleGenerated {
        title: "Generated".into(),
        message: None,
    };
    let refused = t
        .store
        .commit(&sid("sess_1"), batch(None, vec![generated]))
        .await;
    assert!(refused.is_err(), "{refused:?}");
    t.store.delete(&sid("sess_1")).await.unwrap();
    assert!(next(&mut changes).await.deleted_at_ms.is_some());
    t.store.delete(&sid("sess_1")).await.unwrap();
    assert_eq!(renamed.title, "Mine");
    drained(&mut changes);
}

/// `/fork` 的新会话与任务清单的 todo 投影（Node `updateTodos` 推进活动时间）也是会话行的写。
#[tokio::test]
async fn a_fork_and_a_todo_projection_send_their_session() {
    let t = TestStore::new();
    t.store
        .create(meta("sess_1", "/w", ""), crate::tests::batch(None, vec![]))
        .await
        .unwrap();
    for fact in [
        user("msg_1_u1", "first"),
        assistant("msg_2_a1", vec![text("one")], None),
    ] {
        t.store
            .commit(&sid("sess_1"), batch(None, vec![fact]))
            .await
            .unwrap();
    }
    let mut changes = t.store.session_fact_changes();
    let fork = SessionFork {
        parent: sid("sess_1"),
        child: sid("sess_2"),
        created_at_ms: 36_000,
        through: MessageId::new("msg_2_a1"),
        selection: selection(),
        mode: PermissionMode::Default,
        plan: false,
    };
    t.store.fork(fork).await.unwrap();
    let forked = next(&mut changes).await;
    assert_eq!(
        (forked.id.as_str(), forked.kind),
        ("sess_2", SessionKind::Fork)
    );
    assert_eq!(forked.parent, Some(sid("sess_1")));
    t.source.at(90_000);
    TaskListStore::save(&t.store, &sid("sess_1"), &TaskList::default())
        .await
        .unwrap();
    let projected = next(&mut changes).await;
    assert_eq!(
        (projected.id.as_str(), projected.updated_at_ms),
        ("sess_1", 90_000)
    );
    drained(&mut changes);
}
