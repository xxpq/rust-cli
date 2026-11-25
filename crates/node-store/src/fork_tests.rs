//! Node `/fork` 的新会话（session-lifecycle.md §6.5.2、node-store.md §4.8）：会话行、复制的范围、提示与时间线、
//! 冷读取、找不到目标时整批回滚。
use rust_cli_kernel_api::ids::{MessageId, SessionId, ToolUseId};
use rust_cli_kernel_api::message::Message;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::store::{Fact, RevertCursor, SessionFork, SessionStore};
use serde_json::{Value, json};

use crate::tests::{TestStore, assistant, batch, selection, started, text, user};

fn sid(id: &str) -> SessionId {
    SessionId::new(id)
}

fn request(through: &str) -> SessionFork {
    SessionFork {
        parent: sid("sess_1"),
        child: sid("sess_2"),
        created_at_ms: 36_000,
        through: MessageId::new(through),
        selection: selection(),
        mode: PermissionMode::AcceptEdits,
        plan: false,
    }
}

async fn three_turns(t: &TestStore) {
    started(t, "sess_1").await;
    for fact in [
        user("msg_1_u1", "first"),
        assistant("msg_2_a1", vec![text("one")], None),
        user("msg_3_u2", "second"),
        assistant("msg_4_a2", vec![text("two")], None),
        user("msg_5_u3", "third"),
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

#[tokio::test]
async fn the_fork_copies_up_to_the_target_and_adds_the_notice() {
    let t = TestStore::new();
    three_turns(&t).await;
    let receipt = t.store.fork(request("msg_3_u2")).await.unwrap();
    assert_eq!(
        receipt,
        rust_cli_kernel_api::store::ForkReceipt::Created { copied: 3 }
    );
    // 会话行：Node `buildForkedSessionInput`。
    let row = &t.rows(
        "select parent_id, task_type, slug, title, title_source, directory, version, permission
         from session where id = 'sess_2'",
    )[0];
    assert_eq!(row[0], json!("sess_1"));
    assert_eq!(row[1], json!("fork"));
    assert_eq!(row[2], json!("sess_1-fork-rs0"));
    assert_eq!(row[3], json!("Fork of first"));
    assert_eq!(row[4], json!("generated"));
    assert_eq!(row[5], json!("/w"));
    assert_eq!(
        serde_json::from_str::<Value>(row[7].as_str().unwrap()).unwrap(),
        json!({"mode": "edit"})
    );
    let messages = data(&t, "message", "sess_2");
    // 三条复制的消息 + 时间线 + 提示。
    assert_eq!(messages.len(), 5);
    assert_eq!(
        messages[0]["metadata"]["forkOrigin"],
        json!({"sessionId": "sess_1", "messageId": "msg_1_u1"})
    );
    assert_eq!(messages[3]["semantics"]["kind"], "timeline_event");
    assert_eq!(messages[4]["source"], "fork");
    assert_eq!(messages[4]["semantics"]["kind"], "fork_notice");
    let ids: Vec<String> = t
        .rows("select id from message where session_id = 'sess_2' order by sequence")
        .into_iter()
        .map(|row| row[0].as_str().unwrap().to_owned())
        .collect();
    // assistant 的 parentID 换成复制后的 user 消息。
    assert_eq!(messages[1]["parentID"], json!(ids[0]));
    assert!(ids.iter().all(|id| !id.starts_with("msg_1_")));
    let parts = data(&t, "part", "sess_2");
    let timeline = parts.iter().find(|p| p["type"] == "timeline").unwrap();
    assert_eq!(timeline["timelineType"], "session_fork");
    assert_eq!(timeline["anchorMessageId"], json!(ids[2]));
    assert_eq!(timeline["targetMessageId"], "msg_3_u2");
    // 分叉对话不恢复文件（Node 同样写 0），没有 checkpoint 的字段。
    assert_eq!(timeline["restoredFileCount"], 0);
    assert!(timeline.get("targetCheckpointId").is_none());
    assert_eq!(
        t.rows(
            "select id from part where session_id = 'sess_2'
               and json_extract(data, '$.type') = 'timeline'"
        ),
        [[json!("part_fork_sess_1_msg_3_u2_timeline")]]
    );
    let notice = parts
        .iter()
        .find(|p| p["metadata"]["source"] == "fork")
        .unwrap();
    assert_eq!(
        notice["text"],
        format!(
            "This session was forked from a previous session message.\nparentSessionId: sess_1\ntargetMessageId: msg_3_u2\nNo workspace checkpoint was restored for this fork.\n{}",
            super::CONTINUE
        )
    );
    assert_eq!(
        notice["metadata"]["forkContext"],
        json!({"kind": "session_fork", "parentSessionId": "sess_1",
            "targetMessageId": "msg_3_u2", "restoredFileCount": 0})
    );
    assert_eq!(
        notice["metadata"]["runtimeMessage"],
        json!({"source": "rewind_notice"})
    );
    // 冷读取：复制的历史，时间线跳过，提示是 `fork` 来源的 attachment。
    let loaded = t.store.load(&sid("sess_2")).await.unwrap();
    assert_eq!(loaded.mode, Some(PermissionMode::AcceptEdits));
    assert_eq!(loaded.meta.title, "Fork of first");
    let kinds: Vec<String> = loaded
        .messages
        .iter()
        .map(|message| match message {
            Message::User(_) => "user".to_owned(),
            Message::Assistant(_) => "assistant".to_owned(),
            Message::Attachment(attachment) => attachment.source.clone(),
            Message::System(_) => "system".to_owned(),
        })
        .collect();
    assert_eq!(kinds, ["user", "assistant", "user", "fork"]);
    let Message::Attachment(fork) = &loaded.messages[3] else {
        unreachable!()
    };
    assert!(
        fork.text
            .starts_with("<system-reminder>\nThis session was forked")
    );
    // 原会话不变。
    assert_eq!(data(&t, "message", "sess_1").len(), 5);
}

#[tokio::test]
async fn the_fork_follows_the_revert_cursor() {
    let t = TestStore::new();
    three_turns(&t).await;
    // 回退到第二条输入之前：fork 源分支里没有它。
    let cursor = Fact::RevertSet {
        cursor: RevertCursor {
            target: MessageId::new("msg_3_u2"),
            kept: vec![MessageId::new("msg_1_u1"), MessageId::new("msg_2_a1")],
            cut_after: MessageId::new("msg_5_u3"),
            generation: 1,
        },
    };
    t.store
        .commit(&sid("sess_1"), batch(None, vec![cursor]))
        .await
        .unwrap();
    let error = t.store.fork(request("msg_3_u2")).await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("Checkpoint message not found in session store: msg_3_u2"),
        "{error}"
    );
    // 整批回滚：没有留下会话行。
    assert!(
        t.rows("select id from session where id = 'sess_2'")
            .is_empty()
    );
    let receipt = t.store.fork(request("msg_2_a1")).await.unwrap();
    assert_eq!(
        receipt,
        rust_cli_kernel_api::store::ForkReceipt::Created { copied: 2 }
    );
    let parts = data(&t, "part", "sess_2");
    let notice = parts
        .iter()
        .find(|p| p["metadata"]["source"] == "fork")
        .unwrap();
    assert_eq!(
        notice["text"],
        format!(
            "This session was forked from a previous session message.\nparentSessionId: sess_1\ntargetMessageId: msg_2_a1\nNo workspace checkpoint was restored for this fork.\n{}",
            super::CONTINUE
        )
    );
    assert!(
        notice["metadata"]["forkContext"]
            .get("targetCheckpointId")
            .is_none()
    );
}

#[tokio::test]
async fn the_goal_state_is_copied_with_its_verifications() {
    let t = TestStore::new();
    three_turns(&t).await;
    {
        let conn = t.connection();
        conn.execute(
            "insert into session_target (session_id, target_id, objective, summary_title, status,
               token_budget, tokens_used, time_used_seconds, active_input_id, time_created, time_updated)
             values ('sess_1', 'target_1', 'ship it', null, 'complete', null, 5, 2, 'input_9', 1, 2)",
            [],
        )
        .unwrap();
        for (id, anchor) in [("v1", "msg_2_a1"), ("v2", "msg_4_a2"), ("v3", "msg_9_gone")] {
            let data = json!({"eventId": id, "payload": {"targetId": "target_1",
                "anchorAssistantMessageId": anchor, "anchorTurnId": "turn_1",
                "status": "completed", "verification": {"passed": false}}});
            conn.execute(
                "insert into session_entry (id, session_id, type, time_created, time_updated, data)
                 values (?, 'sess_1', 'target_completion_verification', 1, 1, ?)",
                rusqlite::params![id, data.to_string()],
            )
            .unwrap();
        }
    }
    t.store.fork(request("msg_3_u2")).await.unwrap();
    let target = &t.rows(
        "select target_id, objective, status, tokens_used, active_input_id from session_target
         where session_id = 'sess_2'",
    )[0];
    // 复制的验证没有通过，父目标是 complete：分出的目标回到 active（Node 的推导）。
    assert_eq!(
        target,
        &vec![
            json!("target_1"),
            json!("ship it"),
            json!("active"),
            json!(5),
            Value::Null
        ]
    );
    let copied = t.rows(
        "select data from session_entry where session_id = 'sess_2'
           and type = 'target_completion_verification'",
    );
    // 锚定 msg_4_a2（复制范围外）与已不存在的消息的验证都跳过。
    assert_eq!(copied.len(), 1);
    let payload: Value = serde_json::from_str(copied[0][0].as_str().unwrap()).unwrap();
    assert_eq!(payload["payload"]["originAnchorTurnId"], "turn_1");
    assert!(payload["payload"].get("anchorTurnId").is_none());
    assert_ne!(payload["payload"]["anchorAssistantMessageId"], "msg_2_a1");
}

/// 按用户输入分叉（session-lifecycle.md §6.5.2，F75）：复制到工具轮最后一条回答。Node 的 assistant 行带着自己的
/// 工具调用与结果，所以结果都在；排在回答之后的中断标记不复制；提示是分叉对话的正文。
#[tokio::test]
async fn a_fork_through_the_last_answer_of_a_tool_turn_keeps_its_tool_results() {
    use rust_cli_kernel_api::message::{
        AssistantMessage, ContentBlock, StopReason, ToolResultContent, UserMessage, UserOrigin,
    };
    let t = TestStore::new();
    started(&t, "sess_1").await;
    let calls = Fact::MessageAppended {
        message: Message::Assistant(AssistantMessage {
            id: MessageId::new("msg_2_a1"),
            content: vec![ContentBlock::ToolUse {
                id: ToolUseId::new("toolu_1"),
                name: "Read".into(),
                input: json!({"file_path": "/w/a.txt"}),
            }],
            model: "m".into(),
            stop_reason: Some(StopReason::ToolUse),
            usage: None,
        }),
    };
    let result = Fact::MessageAppended {
        message: Message::User(UserMessage {
            id: MessageId::new("msg_3_r1"),
            content: vec![ContentBlock::ToolResult {
                tool_use_id: ToolUseId::new("toolu_1"),
                content: vec![ToolResultContent::Text {
                    text: "contents".into(),
                }],
                is_error: false,
            }],
            origin: UserOrigin::ToolResult {
                assistant: MessageId::new("msg_2_a1"),
            },
        }),
    };
    for fact in [
        user("msg_1_u1", "first"),
        calls,
        result,
        assistant("msg_4_a2", vec![text("read it")], None),
        user(
            "msg_5_i1",
            rust_cli_kernel_api::user_input::INTERRUPT_TEXTS[0],
        ),
        user("msg_6_u2", "second"),
    ] {
        t.store
            .commit(&sid("sess_1"), batch(None, vec![fact]))
            .await
            .unwrap();
    }
    let receipt = t.store.fork(request("msg_4_a2")).await.unwrap();
    assert_eq!(
        receipt,
        rust_cli_kernel_api::store::ForkReceipt::Created { copied: 3 }
    );
    let loaded = t.store.load(&sid("sess_2")).await.unwrap();
    let shapes: Vec<String> = loaded
        .messages
        .iter()
        .map(|message| match message {
            Message::User(user) => match &user.origin {
                UserOrigin::ToolResult { .. } => "result".to_owned(),
                _ => "user".to_owned(),
            },
            Message::Assistant(_) => "assistant".to_owned(),
            Message::Attachment(attachment) => attachment.source.clone(),
            Message::System(_) => "system".to_owned(),
        })
        .collect();
    assert_eq!(shapes, ["user", "assistant", "result", "assistant", "fork"]);
    let Message::Attachment(notice) = &loaded.messages[4] else {
        unreachable!()
    };
    assert!(
        notice.text.starts_with(
            "<system-reminder>\nThis session was forked from a previous session message.\nparentSessionId: sess_1\ntargetMessageId: msg_4_a2\n"
        ),
        "{}",
        notice.text
    );
}

/// 回合记录与输入记录随复制的消息进新会话（session-lifecycle.md §10.18）：`first` 与键换成新的消息 id，回合 id 与结局不变；
/// 指向没复制的消息的不复制。以前新会话里那些消息不属于任何回合（02 §12.7 审定）。
#[tokio::test]
async fn the_fork_carries_the_turn_and_input_records_of_the_copied_messages() {
    use rust_cli_kernel_api::ids::TurnId;
    use rust_cli_kernel_api::input_extras::{InputRecord, Quote};
    use rust_cli_kernel_api::session_api::{InputEnd, TurnOrigin};
    use rust_cli_kernel_api::store::{TurnEnded, TurnRecord};

    let t = TestStore::new();
    three_turns(&t).await;
    let turn = |id: &str, first: &str| TurnRecord {
        turn: TurnId::new(id),
        first: MessageId::new(first),
        origin: TurnOrigin::Input,
        started_at: 1,
        command: None,
        ended: Some(TurnEnded {
            outcome: InputEnd::Completed,
            cancelled: Vec::new(),
            time: Some(rust_cli_kernel_api::session_api::TurnTime {
                ended_at: 9,
                active_ms: 5,
            }),
            files: Vec::new(),
        }),
        fell_back: None,
        works: Vec::new(),
        compactions: Vec::new(),
    };
    let quoted = InputRecord {
        quotes: vec![Quote {
            text: "q".to_owned(),
            path: None,
            sender_name: None,
            sender_id: None,
            message_id: None,
            sent_at_ms: None,
        }],
        ..InputRecord::new(MessageId::new("msg_3_u2"))
    };
    let records = vec![
        turn("turn_1", "msg_1_u1").fact(),
        turn("turn_2", "msg_3_u2").fact(),
        turn("turn_3", "msg_5_u3").fact(),
        quoted.fact(),
    ];
    t.store
        .commit(&sid("sess_1"), batch(None, records))
        .await
        .unwrap();
    t.store.fork(request("msg_4_a2")).await.unwrap();
    let loaded = t.store.load(&sid("sess_2")).await.unwrap();
    let id_of = |text: &str| {
        loaded
            .messages
            .iter()
            .find(|message| format!("{message:?}").contains(text))
            .map(|message| message.id().clone())
            .unwrap()
    };
    let (first, second) = (id_of("\"first\""), id_of("\"second\""));
    assert_ne!(second, MessageId::new("msg_3_u2"));
    assert_eq!(
        loaded.turns,
        [
            TurnRecord {
                first,
                ..turn("turn_1", "msg_1_u1")
            },
            TurnRecord {
                first: second.clone(),
                ..turn("turn_2", "msg_3_u2")
            },
        ]
    );
    assert_eq!(
        loaded.inputs,
        [InputRecord {
            message: second,
            ..quoted
        }]
    );
}

/// 调用方给的 id 已经是这个父会话分出的 fork（重发同一次分叉，session-lifecycle.md §10.57）：回 `Existing`，什么都不再写。
#[tokio::test]
async fn an_existing_fork_of_the_same_parent_is_not_written_again() {
    use rust_cli_kernel_api::store::ForkReceipt;
    let t = TestStore::new();
    three_turns(&t).await;
    assert_eq!(
        t.store.fork(request("msg_4_a2")).await.unwrap(),
        ForkReceipt::Created { copied: 4 }
    );
    let count = |table: &str| {
        t.rows(&format!(
            "select count(*) from {table} where session_id = 'sess_2'"
        ))[0][0]
            .clone()
    };
    let (messages, parts) = (count("message"), count("part"));
    assert_eq!(
        t.store.fork(request("msg_4_a2")).await.unwrap(),
        ForkReceipt::Existing
    );
    assert_eq!((count("message"), count("part")), (messages, parts));
    assert_eq!(
        t.rows("select count(*) from session where parent_id = 'sess_1'"),
        [[json!(1)]]
    );
}

/// 调用方给的 id 是别的会话：拒绝，不改写那个会话。以前分叉对会话行是 upsert：那个会话的行被改成 fork、复制的消息追加进它。
#[tokio::test]
async fn a_child_id_taken_by_another_session_is_refused() {
    use rust_cli_kernel_api::store::StoreError;
    let t = TestStore::new();
    three_turns(&t).await;
    started(&t, "sess_2").await;
    t.store
        .commit(&sid("sess_2"), batch(None, vec![user("msg_9_u1", "mine")]))
        .await
        .unwrap();
    let before = t.rows("select parent_id, task_type, title from session where id = 'sess_2'");
    assert_eq!(
        t.store.fork(request("msg_4_a2")).await,
        Err(StoreError::Exists {
            session: sid("sess_2")
        })
    );
    assert_eq!(
        t.rows("select parent_id, task_type, title from session where id = 'sess_2'"),
        before
    );
    assert_eq!(
        t.rows("select count(*) from message where session_id = 'sess_2'"),
        [[json!(1)]]
    );
}

/// 复制的用户消息保留原来的命令（Node `cloneMessageAnchorForFork`，session-lifecycle.md §10.57）：子会话冷读的 `sources` 指向原命令。
#[tokio::test]
async fn the_copied_inputs_keep_their_commands() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    for (command, fact) in [
        (Some("cmd_1"), user("msg_1_u1", "first")),
        (None, assistant("msg_2_a1", vec![text("one")], None)),
        (Some("cmd_2"), user("msg_3_u2", "second")),
        (None, assistant("msg_4_a2", vec![text("two")], None)),
    ] {
        t.store
            .commit(&sid("sess_1"), batch(command, vec![fact]))
            .await
            .unwrap();
    }
    t.store.fork(request("msg_4_a2")).await.unwrap();
    let loaded = t.store.load(&sid("sess_2")).await.unwrap();
    let commands: Vec<&str> = loaded
        .sources
        .iter()
        .map(|source| source.command.as_str())
        .collect();
    assert_eq!(commands, ["cmd_1", "cmd_2"]);
    // 指向的是子会话里换了 id 的那两条输入
    let inputs: Vec<&MessageId> = loaded
        .messages
        .iter()
        .filter(|message| matches!(message, Message::User(_)))
        .map(Message::id)
        .collect();
    let keyed: Vec<&MessageId> = loaded
        .sources
        .iter()
        .map(|source| &source.message)
        .collect();
    assert_eq!(keyed, inputs);
    assert!(!keyed.contains(&&MessageId::new("msg_1_u1")));
}

/// ExitPlanMode 的计划随 fork 走（session-lifecycle.md §10.57）：复制范围里的调用的 `rust-cli/plan` entry 写进子会话，键（调用 id）不变；
/// 范围外的不跟。以前不复制：子会话冷读时计划行没有计划文字。
#[tokio::test]
async fn the_plans_of_the_copied_calls_go_with_the_fork() {
    use rust_cli_kernel_api::message::ContentBlock;
    use rust_cli_kernel_api::plan::ShownPlan;
    use rust_cli_kernel_api::store::entry_kinds;
    let t = TestStore::new();
    started(&t, "sess_1").await;
    let call = |id: &str| ContentBlock::ToolUse {
        id: ToolUseId::new(id),
        name: "ExitPlanMode".into(),
        input: json!({}),
    };
    let plan = |id: &str, text: &str| Fact::Entry {
        kind: entry_kinds::PLAN.to_owned(),
        id: id.to_owned(),
        data: json!({"plan": text, "path": "/w/plan.md"}),
    };
    for fact in [
        user("msg_1_u1", "first"),
        assistant("msg_2_a1", vec![call("toolu_p1")], None),
        plan("toolu_p1", "copied"),
        assistant("msg_3_a1", vec![text("one")], None),
        user("msg_4_u2", "second"),
        assistant("msg_5_a2", vec![call("toolu_p2")], None),
        plan("toolu_p2", "left behind"),
    ] {
        t.store
            .commit(&sid("sess_1"), batch(None, vec![fact]))
            .await
            .unwrap();
    }
    t.store.fork(request("msg_3_a1")).await.unwrap();
    let loaded = t.store.load(&sid("sess_2")).await.unwrap();
    assert_eq!(
        loaded.plans,
        [ShownPlan {
            tool_use: ToolUseId::new("toolu_p1"),
            plan: "copied".to_owned(),
            path: "/w/plan.md".to_owned(),
        }]
    );
}
