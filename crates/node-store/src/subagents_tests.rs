//! 库里的子代理（session-lifecycle.md §10.46）：本工程写的关联（`Fact::SubagentLinked`）读回来，带子会话最后的回答；不是子代理的
//! 子会话不算。

use rust_cli_kernel_api::agents::{StoredCall, StoredSubagent};
use rust_cli_kernel_api::ids::{AgentId, MessageId, SessionId, ToolUseId};
use rust_cli_kernel_api::message::{
    AssistantMessage, ContentBlock, Message, StopReason, ToolResultContent, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::store::{Fact, SessionKind, SessionStore, StoreQueries};
use serde_json::json;

use std::borrow::Cow;

use rusqlite::Connection;
use rust_cli_node_records::branch::{Branch, select_branch};
use rust_cli_node_records::row::WithParts;
use rust_cli_node_records::rows::Rows;
use rust_cli_node_records::subagents::{SubagentCall, branch_calls, calls, resolve};

use crate::rows::Sqlite;
use crate::tests::{TestStore, assistant, batch, meta, started, text, user};

fn call(id: &str, tool_use: &str) -> Fact {
    Fact::MessageAppended {
        message: Message::Assistant(AssistantMessage {
            id: MessageId::new(id),
            content: vec![ContentBlock::ToolUse {
                id: ToolUseId::new(tool_use),
                name: "Agent".into(),
                input: json!({"subagent_type": "Explore", "description": "look around", "prompt": "look"}),
            }],
            model: "m".into(),
            stop_reason: Some(StopReason::ToolUse),
            usage: None,
        }),
    }
}

fn result(id: &str, owner: &str, tool_use: &str) -> Fact {
    Fact::MessageAppended {
        message: Message::User(UserMessage {
            id: MessageId::new(id),
            content: vec![ContentBlock::ToolResult {
                tool_use_id: ToolUseId::new(tool_use),
                content: vec![ToolResultContent::Text {
                    text: "Async agent launched successfully.".into(),
                }],
                is_error: false,
            }],
            origin: UserOrigin::ToolResult {
                assistant: MessageId::new(owner),
            },
        }),
    }
}

fn link(tool_use: &str, agent: &str) -> Fact {
    Fact::SubagentLinked {
        tool_use: ToolUseId::new(tool_use),
        agent: AgentId::new(agent),
        child: SessionId::new(format!("sess_subagent_{agent}")),
        agent_type: "Explore".into(),
    }
}

async fn child(t: &TestStore, id: &str, kind: SessionKind, answer: &str) {
    let mut child = meta(id, "/w", "");
    child.kind = kind;
    child.parent = Some(SessionId::new("sess_1"));
    t.store
        .create(child, crate::tests::batch(None, vec![]))
        .await
        .unwrap();
    let facts = vec![
        user(&format!("{id}_u"), "look"),
        assistant(&format!("{id}_a"), vec![text(answer)], None),
    ];
    t.store
        .commit(&SessionId::new(id), batch(None, facts))
        .await
        .unwrap();
}

#[tokio::test]
async fn a_linked_call_reads_back_with_the_childs_last_answer() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    child(
        &t,
        "sess_subagent_agent_1",
        SessionKind::Subagent,
        "  found it  ",
    )
    .await;
    // 关联到一个不是子代理的子会话的调用不算
    child(
        &t,
        "sess_subagent_agent_2",
        SessionKind::Interactive,
        "nope",
    )
    .await;
    let facts = vec![
        user("msg_u1", "Explore"),
        call("msg_a1", "toolu_1"),
        result("msg_r1", "msg_a1", "toolu_1"),
        link("toolu_1", "agent_1"),
        call("msg_a2", "toolu_2"),
        result("msg_r2", "msg_a2", "toolu_2"),
        link("toolu_2", "agent_2"),
    ];
    t.store
        .commit(&SessionId::new("sess_1"), batch(None, facts))
        .await
        .unwrap();
    let found = t.store.subagents(&SessionId::new("sess_1")).await.unwrap();
    let [only] = found.as_slice() else {
        panic!("expected one subagent, got {found:?}");
    };
    assert_eq!(
        only,
        &StoredSubagent {
            tool_use: ToolUseId::new("toolu_1"),
            child: SessionId::new("sess_subagent_agent_1"),
            agent: Some(AgentId::new("agent_1")),
            agent_type: Some("Explore".into()),
            description: Some("look around".into()),
            call: StoredCall::Completed,
            started_at_ms: only.started_at_ms,
            ended_at_ms: only.ended_at_ms,
            summary: Some("found it".into()),
        }
    );
    assert!(only.started_at_ms.is_some() && only.ended_at_ms.is_some());
}

#[tokio::test]
async fn a_session_without_subagents_has_none_and_a_missing_one_is_not_found() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    assert_eq!(
        t.store.subagents(&SessionId::new("sess_1")).await.unwrap(),
        Vec::new()
    );
    assert!(t.store.subagents(&SessionId::new("sess_9")).await.is_err());
}

/// 回退掉的调用不算（只按回退游标选分支）：父会话的消息只按 id 选分支、选中的逐条解码（node-store.md §5.4），回退之后只剩游标之前的那一个。
#[tokio::test]
async fn a_call_rewound_away_is_not_listed() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    child(&t, "sess_subagent_agent_1", SessionKind::Subagent, "first").await;
    child(&t, "sess_subagent_agent_3", SessionKind::Subagent, "second").await;
    let session = SessionId::new("sess_1");
    let facts = vec![
        user("msg_u1", "Explore"),
        call("msg_a1", "toolu_1"),
        result("msg_r1", "msg_a1", "toolu_1"),
        link("toolu_1", "agent_1"),
        user("msg_u2", "Explore again"),
        call("msg_a2", "toolu_3"),
        result("msg_r2", "msg_a2", "toolu_3"),
        link("toolu_3", "agent_3"),
    ];
    t.store.commit(&session, batch(None, facts)).await.unwrap();
    let listed = |found: Vec<StoredSubagent>| -> Vec<String> {
        found
            .into_iter()
            .map(|subagent| subagent.child.as_str().to_owned())
            .collect()
    };
    assert_eq!(
        listed(t.store.subagents(&session).await.unwrap()),
        ["sess_subagent_agent_1", "sess_subagent_agent_3"]
    );
    let cursor = rust_cli_kernel_api::store::RevertCursor {
        target: MessageId::new("msg_u2"),
        kept: Vec::new(),
        cut_after: MessageId::new("msg_r2"),
        generation: 1,
    };
    t.store
        .commit(&session, batch(None, vec![Fact::RevertSet { cursor }]))
        .await
        .unwrap();
    let found = t.store.subagents(&session).await.unwrap();
    assert_eq!(listed(found.clone()), ["sess_subagent_agent_1"]);
    assert_eq!(found[0].summary.as_deref(), Some("first"));
}

/// PERF-SWITCH 之前的读法（`stored` 读全部消息与 part、按回退游标选分支、把选中的消息整份解码再取调用）：新读法只读消息 id 与 Agent 的
/// tool part，要与它逐项相同（node-store.md §5.4）。
fn decoding_everything(t: &TestStore, parent: &SessionId) -> Vec<SubagentCall> {
    let conn = t.connection();
    let rows = Sqlite(&conn);
    let row = rows.session(parent.as_str()).unwrap().unwrap();
    let decoded: Vec<WithParts> = rows
        .messages_raw(parent.as_str())
        .unwrap()
        .iter()
        .map(|record| record.decode().unwrap())
        .collect();
    let branch = Branch::from_revert(row.revert.as_ref());
    calls(&select_branch(
        decoded.iter().map(Cow::Borrowed).collect(),
        &branch,
    ))
}

/// Node 写的一条消息与它的 part（`saveMessage` / `savePart` 的形状，库里的 data 不带 id 与会话）。
fn node_message(conn: &Connection, id: &str, at: i64, parts: Vec<serde_json::Value>) {
    let info = json!({"id": id, "sessionID": "sess_1", "role": "assistant",
                      "time": {"created": at, "completed": at + 1}});
    crate::repo::messages::save_message(conn, &info, at).unwrap();
    for (index, mut part) in parts.into_iter().enumerate() {
        part["id"] = json!(format!("{id}_p{index}"));
        part["messageID"] = json!(id);
        part["sessionID"] = json!("sess_1");
        crate::repo::messages::save_part(conn, &part, at).unwrap();
    }
}

fn node_tool(tool: &str, call: &str, state: serde_json::Value) -> serde_json::Value {
    json!({"type": "tool", "tool": tool, "callID": call, "state": state})
}

/// 本工程写的（metadata 关联、同一子会话两次）、Node 写的（结果文字里的 `agentId:`、早期的 JSON 结果、失败、被停、没有结果）、
/// 不是派生子代理的 part（别的工具、类型不是 tool）、一条消息里几个调用的先后，回退前后：新读法读出的调用与整段解码的逐项相同，
/// 子会话解析之后的目录也相同。
#[tokio::test]
async fn the_branch_read_matches_decoding_everything() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    for (id, kind) in [
        ("sess_subagent_agent_1", SessionKind::Subagent),
        ("sess_subagent_agent_9", SessionKind::Subagent),
        ("sess_subagent_json", SessionKind::Subagent),
        ("sess_subagent_agent_4", SessionKind::Subagent),
        ("sess_subagent_agent_5", SessionKind::Interactive),
    ] {
        child(&t, id, kind, &format!("answer of {id}")).await;
    }
    let session = SessionId::new("sess_1");
    let ours = vec![
        user("msg_u1", "Explore"),
        call("msg_a1", "toolu_1"),
        result("msg_r1", "msg_a1", "toolu_1"),
        link("toolu_1", "agent_1"),
        // 同一个子会话的第二次调用：后写的覆盖，位置取第一次
        call("msg_a2", "toolu_2"),
        result("msg_r2", "msg_a2", "toolu_2"),
        link("toolu_2", "agent_1"),
    ];
    t.store.commit(&session, batch(None, ours)).await.unwrap();
    let conn = t.connection();
    node_message(
        &conn,
        "msg_n1",
        5000,
        vec![
            json!({"type": "text", "text": "before the calls"}),
            node_tool(
                "Task",
                "toolu_n1",
                json!({"status": "completed", "input": {"prompt": "dig"},
                "output": "Async agent launched successfully.\nagentId: agent_9 (internal ID - do not mention)\n",
                "time": {"start": 1, "end": 2}}),
            ),
            node_tool(
                "Bash",
                "toolu_bash",
                json!({"status": "completed", "input": {"command": "ls"}, "output": "agentId: agent_7"}),
            ),
            json!({"type": "subtask", "tool": "Agent", "agent": "Explore", "prompt": "not a call"}),
            node_tool(
                "Agent",
                "toolu_json",
                json!({"status": "completed", "input": {"description": "json one"},
                "output": "{\"agentId\":\"agent_j\",\"childSessionId\":\"sess_subagent_json\",\"agentType\":\"Plan\"}",
                "time": {"start": 3, "end": 4}}),
            ),
        ],
    );
    node_message(
        &conn,
        "msg_n2",
        6000,
        vec![
            node_tool(
                "Agent",
                "toolu_err",
                json!({"status": "error", "input": {"description": "stopped"},
                "error": "Interrupted by the user", "metadata": {"agentId": "agent_4"}, "time": {"start": 5, "end": 6}}),
            ),
            node_tool(
                "subagent",
                "toolu_run",
                json!({"status": "running", "input": {"prompt": "still going"},
                "metadata": {"agentId": "agent_5"}, "time": {"start": 7}}),
            ),
        ],
    );
    let new = branch_calls(&Sqlite(&conn), &session).unwrap();
    assert_eq!(new, decoding_everything(&t, &session));
    let children: Vec<&str> = new.iter().map(|call| call.child.as_str()).collect();
    assert_eq!(
        children,
        [
            "sess_subagent_agent_1",
            "sess_subagent_agent_9",
            "sess_subagent_json",
            "sess_subagent_agent_4",
            "sess_subagent_agent_5"
        ]
    );
    assert_eq!(new[0].tool_use, ToolUseId::new("toolu_2"));
    assert_eq!(
        t.store.subagents(&session).await.unwrap(),
        resolve(&Sqlite(&conn), decoding_everything(&t, &session)).unwrap()
    );
    // 回退掉 Node 写的两条之后：两种读法照样相同
    let cursor = rust_cli_kernel_api::store::RevertCursor {
        target: MessageId::new("msg_a2"),
        kept: Vec::new(),
        cut_after: MessageId::new("msg_n2"),
        generation: 1,
    };
    t.store
        .commit(&session, batch(None, vec![Fact::RevertSet { cursor }]))
        .await
        .unwrap();
    let rewound = branch_calls(&Sqlite(&conn), &session).unwrap();
    assert_eq!(rewound, decoding_everything(&t, &session));
    assert_eq!(rewound.len(), 1, "{rewound:?}");
}
