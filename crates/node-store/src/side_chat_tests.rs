//! 侧聊的新会话（session-lifecycle.md §10.58）：Node 侧聊形状的会话行、只给模型看的复制、三种 base、侧聊边界、已有与别的会话、冷读。
use rust_cli_kernel_api::ids::{MessageId, SessionId};
use rust_cli_kernel_api::message::Message;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::store::side_chat::{BOUNDARY, SOURCE};
use rust_cli_kernel_api::store::{
    ForkReceipt, SessionKind, SessionStore, SideChatBase, SideChatFork, StoreError, StoreQueries,
};
use serde_json::{Value, json};

use crate::tests::{TestStore, assistant, batch, selection, started, text, user};

fn sid(id: &str) -> SessionId {
    SessionId::new(id)
}

fn request(base: SideChatBase) -> SideChatFork {
    SideChatFork {
        parent: sid("sess_1"),
        child: sid("sess_2"),
        created_at_ms: 36_000,
        base,
        selection: selection(),
        mode: PermissionMode::AcceptEdits,
        plan: false,
    }
}

/// 两轮结束，第三条输入落库了还没有回答（在跑的回合）。
async fn history(t: &TestStore) {
    started(t, "sess_1").await;
    for fact in [
        user("msg_1_u1", "first"),
        assistant("msg_2_a1", vec![text("one")], None),
        user("msg_3_u2", "second"),
        assistant("msg_4_a2", vec![text("two")], None),
    ] {
        t.store
            .commit(&sid("sess_1"), batch(None, vec![fact]))
            .await
            .unwrap();
    }
}

fn data(t: &TestStore, table: &str, session: &str) -> Vec<Value> {
    t.rows(&format!(
        "select data from {table} where session_id = '{session}' order by rowid"
    ))
    .into_iter()
    .map(|row| serde_json::from_str(row[0].as_str().unwrap()).unwrap())
    .collect()
}

/// 冷读出的输入与回答的文字（只看文字块），与最后一条消息。
async fn read(t: &TestStore, session: &str) -> (Vec<String>, Message) {
    let loaded = t.store.load(&sid(session)).await.unwrap();
    let texts = loaded
        .messages
        .iter()
        .filter_map(|message| {
            let content = match message {
                Message::User(user) => &user.content,
                Message::Assistant(assistant) => &assistant.content,
                _ => return None,
            };
            content.iter().find_map(|block| match block {
                rust_cli_kernel_api::message::ContentBlock::Text { text } => Some(text.clone()),
                _ => None,
            })
        })
        .collect();
    (texts, loaded.messages.last().cloned().unwrap())
}

/// 空闲时全部：Node 侧聊形状的会话行；复制的消息只给模型看；末尾是侧聊边界；没有时间线与分叉提示；冷读是侧聊这一种、边界是 attachment。
#[tokio::test]
async fn an_idle_parent_gives_all_of_its_history_as_model_only() {
    let t = TestStore::new();
    history(&t).await;
    let receipt = t.store.side_chat(request(SideChatBase::All)).await.unwrap();
    assert_eq!(receipt, ForkReceipt::Created { copied: 4 });
    let row = &t.rows(
        "select parent_id, task_type, slug, title, title_source, directory, permission
         from session where id = 'sess_2'",
    )[0];
    assert_eq!(row[0], json!("sess_1"));
    assert_eq!(row[1], json!("selection_side_chat"));
    assert_eq!(row[2], json!("sess_1-selection_side_chat-rs0"));
    assert_eq!(row[3], json!("Selection side chat"));
    assert_eq!(row[4], json!("generated"));
    assert_eq!(row[5], json!("/w"));
    let messages = data(&t, "message", "sess_2");
    // 四条复制的消息 + 侧聊边界；没有时间线、没有分叉提示。
    assert_eq!(messages.len(), 5);
    for copied in &messages[..4] {
        assert_eq!(copied["visibility"], "model-only");
        assert_eq!(copied["semantics"]["uiVisibility"], "hidden");
        assert_eq!(copied["semantics"]["providerVisibility"], "visible");
        assert_eq!(copied["semantics"]["transcriptVisibility"], "hidden");
    }
    assert_eq!(messages[0]["semantics"]["origin"], "real_user");
    assert_eq!(
        messages[0]["metadata"]["forkOrigin"],
        json!({"sessionId": "sess_1", "messageId": "msg_1_u1"})
    );
    let boundary = &messages[4];
    assert_eq!(boundary["source"], SOURCE);
    assert_eq!(boundary["synthetic"], true);
    assert_eq!(boundary["semantics"]["kind"], "system_reminder");
    assert_eq!(boundary["anchor"]["origin"], "synthetic");
    let parts = data(&t, "part", "sess_2");
    assert!(parts.iter().all(|part| part["type"] != "timeline"));
    let last = parts.last().unwrap();
    assert_eq!(last["text"], BOUNDARY);
    assert_eq!(
        last["metadata"],
        json!({"runtimeMessage": {"source": SOURCE}, "source": SOURCE, "visibility": "model-only"})
    );
    let loaded = t.store.load(&sid("sess_2")).await.unwrap();
    assert_eq!(loaded.meta.kind, SessionKind::SideChat);
    assert_eq!(loaded.meta.parent, Some(sid("sess_1")));
    assert_eq!(loaded.mode, Some(PermissionMode::AcceptEdits));
    let (texts, last) = read(&t, "sess_2").await;
    assert_eq!(texts, ["first", "one", "second", "two"]);
    let Message::Attachment(attachment) = last else {
        panic!("{last:?}");
    };
    assert_eq!(attachment.source, SOURCE);
    assert_eq!(attachment.text, rust_cli_base::reminder::wrap(BOUNDARY));
    // 侧聊不是列表会话：会话事实照样给出（种类是侧聊），列表没有它
    let facts = t.store.session_facts(&[sid("sess_2")]).await.unwrap();
    assert_eq!(facts[0].kind, SessionKind::SideChat);
    let recent = t
        .store
        .recent_session_facts(&loaded.meta.workspace, 10)
        .await
        .unwrap();
    assert!(recent.iter().all(|facts| facts.id != sid("sess_2")));
}

/// 回合在跑、由人类输入开：带到这条输入（含），之后落库的回答不带；通知开的回合：到它之前。
#[tokio::test]
async fn a_running_turn_gives_its_input_or_nothing_of_it() {
    let t = TestStore::new();
    history(&t).await;
    let through = SideChatFork {
        child: sid("sess_through"),
        ..request(SideChatBase::Through(MessageId::new("msg_3_u2")))
    };
    assert_eq!(
        t.store.side_chat(through).await.unwrap(),
        ForkReceipt::Created { copied: 3 }
    );
    assert_eq!(read(&t, "sess_through").await.0, ["first", "one", "second"]);
    let before = SideChatFork {
        child: sid("sess_before"),
        ..request(SideChatBase::Before(MessageId::new("msg_3_u2")))
    };
    assert_eq!(
        t.store.side_chat(before).await.unwrap(),
        ForkReceipt::Created { copied: 2 }
    );
    assert_eq!(read(&t, "sess_before").await.0, ["first", "one"]);
    // 带到的输入不在分支里：报错，什么都不写
    let missing = SideChatFork {
        child: sid("sess_missing"),
        ..request(SideChatBase::Through(MessageId::new("msg_9_gone")))
    };
    assert!(matches!(
        t.store.side_chat(missing).await,
        Err(StoreError::Storage { .. })
    ));
    assert!(
        t.rows("select id from session where id = 'sess_missing'")
            .is_empty()
    );
}

/// 调用方重发同一个 id：已经是这个父会话分出的侧聊 → `Existing`，库不变；id 是别的会话（含同一个父会话的 fork）→ `Exists`。
#[tokio::test]
async fn a_side_chat_into_an_existing_id_writes_nothing() {
    let t = TestStore::new();
    history(&t).await;
    t.store.side_chat(request(SideChatBase::All)).await.unwrap();
    let count = || t.rows("select count(*) from message where session_id = 'sess_2'");
    let written = count();
    assert_eq!(
        t.store.side_chat(request(SideChatBase::All)).await.unwrap(),
        ForkReceipt::Existing
    );
    assert_eq!(count(), written);
    let fork = rust_cli_kernel_api::store::SessionFork {
        parent: sid("sess_1"),
        child: sid("sess_fork"),
        created_at_ms: 36_000,
        through: MessageId::new("msg_2_a1"),
        selection: selection(),
        mode: PermissionMode::Default,
        plan: false,
    };
    t.store.fork(fork).await.unwrap();
    let taken = SideChatFork {
        child: sid("sess_fork"),
        ..request(SideChatBase::All)
    };
    assert!(matches!(
        t.store.side_chat(taken).await,
        Err(StoreError::Exists { .. })
    ));
}

/// 侧聊不复制目标：继承的消息去掉 `anchor.goalBoundary`（Node 在克隆之前去掉），也不写 session_target。
#[tokio::test]
async fn the_side_chat_drops_the_goal_boundary() {
    let t = TestStore::new();
    history(&t).await;
    t.connection()
        .execute(
            "update message set data = json_set(data, '$.anchor.goalBoundary',
               json('{\"kind\":\"snapshot\",\"target\":{\"targetID\":\"t\",\"sessionID\":\"sess_1\"}}'))
             where id = 'msg_1_u1'",
            [],
        )
        .unwrap();
    t.store.side_chat(request(SideChatBase::All)).await.unwrap();
    let messages = data(&t, "message", "sess_2");
    assert!(messages[0]["anchor"].get("goalBoundary").is_none());
    assert_eq!(messages[0]["anchor"]["origin"], "realUser");
    assert!(
        t.rows("select session_id from session_target where session_id = 'sess_2'")
            .is_empty()
    );
}
