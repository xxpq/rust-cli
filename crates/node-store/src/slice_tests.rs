//! 按 id 读历史的一段（node-store.md §5.5）：读出的内核消息与冷读取里这些消息的那几条相同（保留段的副本只差用量），
//! 工具结果随它的 assistant、压缩边界随它的摘要；输出头部与起止只取这一段的；回合记录取开头在其中的；库里没有的 id 不给出。

use std::collections::BTreeMap;

use rust_cli_kernel_api::ids::{MessageId, SessionId, ToolUseId, TurnId};
use rust_cli_kernel_api::message::{
    ContentBlock, Message, ToolResultContent, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::session_api::TurnOrigin;
use rust_cli_kernel_api::store::{Fact, SessionStore, StoreQueries, TurnRecord};
use serde_json::json;

use crate::hydrate_tests::{database, fixture};
use crate::tests::{TestStore, assistant, batch, started, text, user};

/// 按存储的消息分组（同一条里的先后不变）：工具结果按位置键排在别处时，片里紧跟它的 assistant，组内的内容相同。
fn grouped(messages: &[Message]) -> BTreeMap<String, Vec<Message>> {
    let mut groups: BTreeMap<String, Vec<Message>> = BTreeMap::new();
    for message in messages {
        let mut message = message.clone();
        // 保留段在冷读取里是改过用量的副本（`invalidated`）；片读原件。前端的行不读用量（node-store.md §5.5）
        if let Message::Assistant(assistant) = &mut message {
            assistant.usage = None;
        }
        groups
            .entry(message.record().as_str().to_owned())
            .or_default()
            .push(message);
    }
    groups
}

/// Node 写的会话（回退、压缩点与保留段、工具、失败）：每一条、每相邻三条存储的消息读出的片，与冷读取（earlier 接上活动历史）里这些消息相同。
#[test]
fn a_slice_matches_the_cold_load_for_those_records() {
    let fixture = fixture();
    for case in ["basic", "compacted", "rewound", "rich", "failures"] {
        let (_dir, conn) = database(&fixture[case]["tables"]);
        let session = SessionId::new(fixture[case]["sessionID"].as_str().unwrap());
        let rows = crate::rows::Sqlite(&conn);
        let (loaded, _) = crate::load::load(&rows, &session).unwrap();
        let shown: Vec<Message> = loaded.earlier.into_iter().chain(loaded.messages).collect();
        let mut records: Vec<MessageId> = shown.iter().map(|m| m.record().clone()).collect();
        records.dedup();
        assert!(records.len() > 1, "{case}");
        for width in [1, 3] {
            for part in records.windows(width.min(records.len())) {
                let slice = crate::load::slice(&rows, &session, part).unwrap();
                let wanted: Vec<Message> = shown
                    .iter()
                    .filter(|m| part.contains(m.record()))
                    .cloned()
                    .collect();
                assert_eq!(
                    grouped(&slice.messages),
                    grouped(&wanted),
                    "{case} {part:?}"
                );
                let calls: Vec<&ToolUseId> =
                    slice.messages.iter().flat_map(Message::tool_uses).collect();
                assert!(
                    slice
                        .output_heads
                        .iter()
                        .all(|head| calls.contains(&&head.tool_use)),
                    "{case} {part:?}"
                );
                assert!(
                    slice
                        .tool_times
                        .iter()
                        .all(|time| calls.contains(&&time.tool_use)),
                    "{case} {part:?}"
                );
            }
        }
    }
}

fn record(turn: &str, first: &str) -> Fact {
    TurnRecord::opened(
        TurnId::new(turn),
        MessageId::new(first),
        TurnOrigin::Input,
        1,
    )
    .fact()
}

/// 本工程写的两个回合：片只带开头在其中的回合记录；库里没有的 id 不给出；工具结果随 assistant。
#[tokio::test]
async fn a_slice_of_written_turns_carries_their_turn_records() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    let call = ContentBlock::ToolUse {
        id: ToolUseId::new("toolu_1"),
        name: "Read".into(),
        input: json!({"file_path": "/w/a"}),
    };
    let result = Fact::MessageAppended {
        message: Message::User(UserMessage {
            id: MessageId::new("msg_r1"),
            content: vec![ContentBlock::ToolResult {
                tool_use_id: ToolUseId::new("toolu_1"),
                content: vec![ToolResultContent::Text { text: "a".into() }],
                is_error: false,
            }],
            origin: UserOrigin::ToolResult {
                assistant: MessageId::new("msg_a1"),
            },
        }),
    };
    let first = vec![
        user("msg_u1", "one"),
        record("turn_1", "msg_u1"),
        assistant("msg_a1", vec![call], None),
        result,
        assistant("msg_a1b", vec![text("done")], None),
    ];
    let second = vec![
        user("msg_u2", "two"),
        record("turn_2", "msg_u2"),
        assistant("msg_a2", vec![text("second")], None),
    ];
    let session = SessionId::new("sess_1");
    for facts in [first, second] {
        t.store.commit(&session, batch(None, facts)).await.unwrap();
    }
    let ids = |ids: &[&str]| ids.iter().map(|id| MessageId::new(*id)).collect::<Vec<_>>();
    let slice = t
        .store
        .history(&session, &ids(&["msg_u1", "msg_a1", "msg_a1b", "msg_gone"]))
        .await
        .unwrap();
    let got: Vec<&str> = slice.messages.iter().map(|m| m.id().as_str()).collect();
    assert_eq!(got[..2], ["msg_u1", "msg_a1"]);
    assert_eq!(got.last(), Some(&"msg_a1b"));
    assert!(
        slice
            .messages
            .iter()
            .any(|m| matches!(m, Message::User(UserMessage {
        origin: UserOrigin::ToolResult { assistant }, .. }) if assistant.as_str() == "msg_a1"))
    );
    let turns: Vec<&str> = slice.turns.iter().map(|turn| turn.turn.as_str()).collect();
    assert_eq!(turns, ["turn_1"]);
    let later = t
        .store
        .history(&session, &ids(&["msg_u2", "msg_a2"]))
        .await
        .unwrap();
    let turns: Vec<&str> = later.turns.iter().map(|turn| turn.turn.as_str()).collect();
    assert_eq!(turns, ["turn_2"]);
    assert!(later.output_heads.is_empty() && later.tool_times.is_empty());
}
