//! NodeStore 与 PgSessionStore 复用的会话合同，不要求二者复制物理表结构。
use crate::ids::{MessageId, SessionId};
use crate::message::{ContentBlock, Message, UserMessage, UserOrigin};
use crate::permission::{ExecutionState, PermissionMode};
use crate::store::{
    Fact, FactBatch, FrontendRecord, SessionFork, SessionMeta, SessionOrder, SessionQuery, Store,
};
use crate::testing::selection;

pub async fn verify(store: &dyn Store, meta: SessionMeta) -> Result<(), crate::store::StoreError> {
    let id = meta.id.clone();
    let initial = FactBatch {
        command: None,
        turn: None,
        facts: vec![
            Fact::ModelSelected {
                selection: selection(),
            },
            Fact::ExecutionChanged {
                state: ExecutionState::from_parts(PermissionMode::Default, true),
            },
        ],
    };
    store.create(meta.clone(), initial).await?;
    let message = Message::User(UserMessage {
        id: MessageId::new("msg_contract"),
        origin: UserOrigin::Human,
        content: vec![ContentBlock::Text {
            text: "contract input".into(),
        }],
    });
    store
        .commit(
            &id,
            FactBatch {
                command: None,
                turn: None,
                facts: vec![Fact::MessageAppended {
                    message: message.clone(),
                }],
            },
        )
        .await?;
    let loaded = store.load(&id).await?;
    assert_eq!(loaded.messages, vec![message.clone()]);
    assert_eq!(loaded.model, Some(selection()));
    assert_eq!(loaded.mode, Some(PermissionMode::Default));
    assert!(loaded.plan);
    let query = SessionQuery {
        environment: meta.workspace.environment,
        root: Some(meta.workspace.root),
        order: SessionOrder::Updated,
        limit: 1,
    };
    let before = store.query_sessions(query.clone()).await?;
    assert_eq!(before.sessions[0].id, id);
    store.rename(&id, "renamed").await?;
    let after = store.query_sessions(query.clone()).await?;
    assert_eq!(after.sessions[0].title, "renamed");
    assert_eq!(
        before.sessions[0].updated_at_ms,
        after.sessions[0].updated_at_ms
    );
    let record = FrontendRecord {
        kind: "contract/record".into(),
        key: "key".into(),
        data: serde_json::json!({"ok": true}),
    };
    store.put_record(&id, &record).await?;
    assert_eq!(
        store.records(&id, &record.kind).await?,
        vec![record.clone()]
    );
    let replaced = FrontendRecord {
        kind: "contract/renamed-kind".into(),
        ..record.clone()
    };
    store.put_record(&id, &replaced).await?;
    assert!(store.records(&id, &record.kind).await?.is_empty());
    assert_eq!(store.records(&id, &replaced.kind).await?, vec![replaced]);
    let child = SessionId::new("sess_contract_fork");
    let fork = SessionFork {
        parent: id.clone(),
        child: child.clone(),
        through: message.id().clone(),
        selection: selection(),
        mode: PermissionMode::Default,
        plan: true,
        created_at_ms: 2_000,
    };
    assert_eq!(
        store.fork(fork).await?,
        crate::store::ForkReceipt::Created { copied: 1 }
    );
    let forked = store.load(&child).await?;
    assert_eq!(forked.meta.parent, Some(id.clone()));
    assert_eq!(forked.messages.len(), 2);
    assert_ne!(forked.messages[0].id(), message.id());
    assert!(
        matches!(&forked.messages[1], Message::Attachment(notice) if notice.source == "fork" && notice.text.contains("No workspace checkpoint was restored"))
    );
    store.delete(&id).await?;
    assert!(store.load(&id).await.is_err());
    let page = store.query_sessions(query).await?;
    assert!(page.sessions.iter().all(|session| session.id != id));
    Ok(())
}
