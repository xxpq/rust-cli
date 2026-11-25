//! 草稿：延迟落库（docs/specs/rust-cli/session-facts.md §4，02 §11 M2）。不带消息的批次留在会话 actor 里；第一批带消息的
//! 提交把会话行与攒下的事实一起落库，这时才有会话事实；放弃的草稿不留痕。
use std::sync::Arc;
use std::time::Duration;

use futures_util::{FutureExt, StreamExt};
use rust_cli_kernel_api::ids::{CommandId, SessionId};
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::session_api::{
    Ack, Command, CommandEnvelope, CreateSession, Delivery, EndReason, KernelEvent, Persistence,
};
use rust_cli_kernel_api::store::{Fact, StoreQueries};
use rust_cli_kernel_api::testing::{MemoryStore, ScriptedModel, selection, text_response};

use super::*;
use crate::tests::{kernel, workspace};

fn draft(id: &str) -> CreateSession {
    CreateSession {
        id: Some(SessionId::new(id)),
        workspace: workspace(),
        selection: selection(),
        mode: PermissionMode::Default,
        plan: false,
        persistence: Persistence::Deferred,
        additional_directories: Vec::new(),
        preferences: Default::default(),
    }
}

fn envelope(id: &str, command: Command) -> CommandEnvelope {
    CommandEnvelope {
        id: CommandId::new(id),
        command,
    }
}

#[tokio::test]
async fn a_draft_is_stored_with_its_first_input() {
    let store = Arc::new(MemoryStore::default());
    let mut changes = store.session_fact_changes();
    let kernel = kernel(
        store.clone(),
        Arc::new(ScriptedModel::new(vec![text_response(&["hi"])])),
    );
    let id = kernel.create_session(draft("sess_draft")).await.unwrap();
    let session = kernel.open_session(&id).await.unwrap();
    let other = ModelSelection::new("p", "other");
    let chosen = session
        .send(envelope(
            "c1",
            Command::SetModel {
                selection: other.clone(),
            },
        ))
        .await;
    assert!(matches!(chosen, Ack::Accepted { .. }), "{chosen:?}");
    // 换模型照常生效（视图与事件），但库里还没有这个会话，也没有会话事实。
    assert_eq!(session.view().await.unwrap().selection, other);
    assert!(
        store
            .session_facts(std::slice::from_ref(&id))
            .await
            .unwrap()
            .is_empty()
    );
    assert!(changes.next().now_or_never().is_none());
    let mut events = session.subscribe(None);
    let submit = Command::Submit {
        text: "hello".to_owned(),
        extras: Default::default(),
        delivery: Delivery::Queue,
        target: None,
    };
    session.send(envelope("c2", submit)).await;
    loop {
        let event = tokio::time::timeout(Duration::from_secs(5), events.next())
            .await
            .unwrap()
            .unwrap();
        if matches!(event.event, KernelEvent::TurnFinished { .. }) {
            break;
        }
    }
    // 会话行是第一项，草稿里攒下的配置跟在后面，然后是首条输入：同一个事务。
    let facts = store.facts(&id);
    assert!(
        matches!(&facts[0], Fact::SessionCreated { meta } if meta.id == id),
        "{facts:?}"
    );
    let chosen_at = facts
        .iter()
        .position(|fact| matches!(fact, Fact::ModelSelected { selection } if *selection == other))
        .expect("the draft's model choice is stored");
    let input_at = facts
        .iter()
        .position(|fact| matches!(fact, Fact::MessageAppended { .. }))
        .unwrap();
    assert!(chosen_at < input_at);
    let first = changes.next().await.unwrap();
    assert_eq!((first.id.clone(), first.selection), (id, Some(other)));
    assert!(first.first_input_at_ms.is_some());
}

#[tokio::test]
async fn an_abandoned_draft_leaves_nothing() {
    let store = Arc::new(MemoryStore::default());
    let mut changes = store.session_fact_changes();
    let kernel = kernel(store.clone(), Arc::new(ScriptedModel::new(Vec::new())));
    let id = kernel.create_session(draft("sess_draft")).await.unwrap();
    let session = kernel.open_session(&id).await.unwrap();
    let renamed = session
        .send(envelope(
            "c1",
            Command::Rename {
                title: "Later".to_owned(),
            },
        ))
        .await;
    assert!(matches!(renamed, Ack::Accepted { .. }), "{renamed:?}");
    session.close(EndReason::Other).await;
    assert!(store.facts(&id).is_empty());
    assert!(store.session_facts(&[id]).await.unwrap().is_empty());
    assert!(changes.next().now_or_never().is_none());
}
