//! 冷读取的“当前轮以错误结束”（session-lifecycle.md §6.3.1）：按当前轮输入的 `turn_usage.status`。

use rust_cli_kernel_api::ids::{CommandId, MessageId, SessionId, TurnId};
use rust_cli_kernel_api::metering::CallStatus;
use rust_cli_kernel_api::store::{Fact, FactBatch, SessionStore, TurnAnchor, TurnRecord};

use crate::tests::{TestStore, started, user};

fn batch(facts: Vec<Fact>, prompt: &str, turn: &str) -> FactBatch {
    FactBatch {
        command: None::<CommandId>,
        facts,
        turn: Some(TurnAnchor {
            prompt: MessageId::new(prompt),
            turn: Some(TurnId::new(turn)),
        }),
    }
}

#[tokio::test]
async fn a_turn_that_ended_with_an_error_is_reported_until_the_next_input() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    let session = SessionId::new("sess_1");
    let ended = |status| Fact::TurnEnded { status };
    t.store
        .commit(
            &session,
            batch(vec![user("msg_u1", "one")], "msg_u1", "turn_1"),
        )
        .await
        .unwrap();
    assert!(!t.store.load(&session).await.unwrap().last_turn_failed);
    t.store
        .commit(
            &session,
            batch(vec![ended(CallStatus::Error)], "msg_u1", "turn_1"),
        )
        .await
        .unwrap();
    assert!(t.store.load(&session).await.unwrap().last_turn_failed);
    t.store
        .commit(
            &session,
            batch(vec![user("msg_u2", "two")], "msg_u2", "turn_2"),
        )
        .await
        .unwrap();
    assert!(!t.store.load(&session).await.unwrap().last_turn_failed);
    t.store
        .commit(
            &session,
            batch(vec![ended(CallStatus::Cancelled)], "msg_u2", "turn_2"),
        )
        .await
        .unwrap();
    assert!(!t.store.load(&session).await.unwrap().last_turn_failed);
}

/// 回合记录（session-lifecycle.md §10.1，K-I1）：会话 actor 写的 `rust-cli/turn` 按写入顺序读回，冷读据此把消息还原到回合。
#[tokio::test]
async fn turn_records_are_read_back_in_order() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    let session = SessionId::new("sess_1");
    let record = |turn: &str, first: &str| TurnRecord {
        turn: TurnId::new(turn),
        first: MessageId::new(first),
        origin: rust_cli_kernel_api::session_api::TurnOrigin::Input,
        started_at: 1_000,
        command: None,
        ended: None,
        fell_back: None,
        works: Vec::new(),
        compactions: Vec::new(),
    };
    let first = record("turn_1", "msg_u1");
    t.store
        .commit(
            &session,
            batch(
                vec![user("msg_u1", "one"), first.fact()],
                "msg_u1",
                "turn_1",
            ),
        )
        .await
        .unwrap();
    let second = record("turn_n", "msg_u2");
    t.store
        .commit(
            &session,
            batch(
                vec![user("msg_u2", "two"), second.fact()],
                "msg_u2",
                "turn_2",
            ),
        )
        .await
        .unwrap();
    assert_eq!(
        t.store.load(&session).await.unwrap().turns,
        [first.clone(), second.clone()]
    );
    // 回合结束时同一条记录带着结局、结束的时刻与用时、这个回合的文件改动再写（session-lifecycle.md §10.17、§10.36、§10.42）：是更新，位置不变
    let finished = TurnRecord {
        ended: Some(rust_cli_kernel_api::store::TurnEnded {
            outcome: rust_cli_kernel_api::session_api::InputEnd::Cancelled,
            cancelled: vec![rust_cli_kernel_api::ids::ToolUseId::new("toolu_1")],
            time: Some(rust_cli_kernel_api::session_api::TurnTime {
                ended_at: 9_000,
                active_ms: 4_000,
            }),
            files: vec![rust_cli_kernel_api::session_api::TurnFileChange {
                change: rust_cli_kernel_api::session_api::FileChange {
                    path: "/work/a.txt".to_owned(),
                    additions: 3,
                    deletions: 1,
                    machine: rust_cli_kernel_api::ids::MachineName::primary(),
                },
                writes: 2,
                tools: vec!["Edit".to_owned(), "Write".to_owned()],
            }],
        }),
        ..first
    };
    t.store
        .commit(&session, batch(vec![finished.fact()], "msg_u2", "turn_2"))
        .await
        .unwrap();
    assert_eq!(
        t.store.load(&session).await.unwrap().turns,
        [finished, second]
    );
}

/// 效果记录（session-lifecycle.md §10.3，K-A2）：带命令 id 的批次里的 user 消息与合成消息（工具边界并入的插话是
/// `queued_command` attachment）都写 `anchor.sourceCommandId`，冷读按写入顺序交回。以前合成消息不写。
#[tokio::test]
async fn command_ids_of_inputs_and_attachments_are_read_back() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    let session = SessionId::new("sess_1");
    let commit = |facts, command: &str| FactBatch {
        command: Some(CommandId::new(command)),
        ..batch(facts, "msg_u1", "turn_1")
    };
    t.store
        .commit(&session, commit(vec![user("msg_u1", "one")], "c1"))
        .await
        .unwrap();
    let steered = Fact::MessageAppended {
        message: rust_cli_kernel_api::message::Message::Attachment(
            rust_cli_kernel_api::message::AttachmentMessage {
                id: MessageId::new("msg_q1"),
                source: rust_cli_kernel_api::hooks::queued_command::SOURCE.to_owned(),
                text: "also b".to_owned(),
                data: serde_json::json!({}),
            },
        ),
    };
    t.store
        .commit(&session, commit(vec![steered], "c2"))
        .await
        .unwrap();
    let loaded = t.store.load(&session).await.unwrap();
    // 按消息交回（session-lifecycle.md §10.31）：命令账本读它（§10.33），前端据它给用户行写 `sourceCommandId`，与活会话的
    // `InputCommitted` 同值
    let source = |message: &str, command: &str| rust_cli_kernel_api::store::InputSource {
        message: MessageId::new(message),
        command: CommandId::new(command),
    };
    assert_eq!(
        loaded.sources,
        [source("msg_u1", "c1"), source("msg_q1", "c2")]
    );
}

/// 回合记录带开回合的命令（session-lifecycle.md §10.31）：写了读得回；以前写的记录没有这个键，读成 None。
#[tokio::test]
async fn a_turn_record_keeps_the_command_that_opened_it() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    let session = SessionId::new("sess_1");
    let opened = TurnRecord {
        turn: TurnId::new("turn_1"),
        first: MessageId::new("msg_u1"),
        origin: rust_cli_kernel_api::session_api::TurnOrigin::Compaction,
        started_at: 1_000,
        command: Some(CommandId::new("c1")),
        ended: None,
        fell_back: None,
        works: Vec::new(),
        compactions: Vec::new(),
    };
    let older = Fact::Entry {
        kind: rust_cli_kernel_api::store::entry_kinds::TURN.to_owned(),
        id: "turn_0".to_owned(),
        data: serde_json::json!({"first": "msg_u0", "origin": "input", "startedAt": 900}),
    };
    t.store
        .commit(
            &session,
            batch(
                vec![
                    user("msg_u0", "zero"),
                    older,
                    user("msg_u1", "one"),
                    opened.fact(),
                ],
                "msg_u1",
                "turn_1",
            ),
        )
        .await
        .unwrap();
    let turns = t.store.load(&session).await.unwrap().turns;
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0].command, None);
    assert_eq!(turns[1], opened);
}

/// ExitPlanMode 拿给用户批准的计划（session-lifecycle.md §10.43）：与结果同批写的 `rust-cli/plan` 按写入顺序读回 `LoadedSession.plans`。
#[tokio::test]
async fn shown_plans_are_read_back_in_order() {
    let t = TestStore::new();
    started(&t, "sess_1").await;
    let session = SessionId::new("sess_1");
    let shown = |tool_use: &str, plan: &str| rust_cli_kernel_api::plan::ShownPlan {
        tool_use: rust_cli_kernel_api::ids::ToolUseId::new(tool_use),
        plan: plan.to_owned(),
        path: "/work/.zcode/plans/plan-sess_1.md".to_owned(),
    };
    let entry = |plan: &rust_cli_kernel_api::plan::ShownPlan| Fact::Entry {
        kind: rust_cli_kernel_api::store::entry_kinds::PLAN.to_owned(),
        id: plan.tool_use.as_str().to_owned(),
        data: serde_json::json!({"plan": plan.plan, "path": plan.path}),
    };
    let (first, second) = (shown("toolu_2", "# B"), shown("toolu_1", "# A"));
    t.store
        .commit(
            &session,
            batch(
                vec![user("msg_u1", "one"), entry(&first), entry(&second)],
                "msg_u1",
                "turn_1",
            ),
        )
        .await
        .unwrap();
    assert_eq!(t.store.load(&session).await.unwrap().plans, [first, second]);
}
