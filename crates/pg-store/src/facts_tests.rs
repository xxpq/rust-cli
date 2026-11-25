use crate::tests::{batch, fixture, meta};
use rust_cli_kernel_api::ids::{ArtifactId, MachineName, MessageId, SessionId, ToolUseId};
use rust_cli_kernel_api::message::{
    AssistantMessage, ContentBlock, Message, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::store::*;
use rust_cli_kernel_api::testing::selection;
use serde_json::json;

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn operations_approvals_checkpoints_and_tool_references_round_trip() {
    let (_, store) = fixture("facts").await;
    let session = SessionId::new("facts");
    store.create(meta("facts"), batch("prompt")).await.unwrap();

    let call = ToolUseId::new("call");
    let assistant = Message::Assistant(AssistantMessage {
        id: MessageId::new("assistant"),
        content: vec![ContentBlock::ToolUse {
            id: call.clone(),
            name: "Write".into(),
            input: json!({"path":"a"}),
        }],
        model: "model".into(),
        usage: None,
        stop_reason: None,
    });
    let checkpoint = WorkspaceCheckpoint {
        id: "checkpoint".into(),
        prompt: MessageId::new("prompt"),
        assistant: assistant.id().clone(),
        tool_use: call.clone(),
        snapshot: ArtifactRef {
            session: session.clone(),
            id: ArtifactId::new("snapshot"),
            purpose: ArtifactPurpose::Checkpoint,
            media_type: "application/json".into(),
            size: 42,
        },
        machine: MachineName::new("sandbox"),
    };
    let facts = vec![
        Fact::MessageAppended {
            message: assistant.clone(),
        },
        Fact::CheckpointCreated {
            checkpoint: checkpoint.clone(),
        },
    ];
    store
        .commit(
            &session,
            FactBatch {
                command: None,
                turn: None,
                facts,
            },
        )
        .await
        .unwrap();
    store
        .put_record(
            &session,
            &FrontendRecord {
                kind: "operation_result".into(),
                key: "operation".into(),
                data: json!({"untrusted":true}),
            },
        )
        .await
        .unwrap();
    let loaded = store.load(&session).await.unwrap();
    assert_eq!(loaded.checkpoints, vec![checkpoint]);
    let result = Message::User(UserMessage {
        id: MessageId::new("result"),
        origin: UserOrigin::ToolResult {
            assistant: assistant.id().clone(),
        },
        content: vec![ContentBlock::ToolResult {
            tool_use_id: call,
            content: vec![],
            is_error: false,
        }],
    });
    store
        .commit(
            &session,
            FactBatch {
                command: None,
                turn: None,
                facts: vec![
                    Fact::MessageAppended {
                        message: result.clone(),
                    },
                    Fact::CheckpointsRestored {
                        checkpoints: vec!["checkpoint".into()],
                    },
                ],
            },
        )
        .await
        .unwrap();
    let loaded = store.load(&session).await.unwrap();
    assert!(loaded.checkpoints.is_empty());
    let child = SessionId::new("fork");
    store
        .fork(SessionFork {
            parent: session,
            child: child.clone(),
            through: assistant.id().clone(),
            selection: selection(),
            mode: PermissionMode::Default,
            plan: false,
            created_at_ms: 2_000,
        })
        .await
        .unwrap();
    let copied = store.load(&child).await.unwrap();
    assert!(
        matches!(&copied.messages[2], Message::User(user) if matches!(&user.origin, UserOrigin::ToolResult { assistant } if assistant == copied.messages[1].id()))
    );
}

/// 用量写入失败只回退它自己的 savepoint，同一事务里之前与之后的写入照常提交（usage.md §2.5）。
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn a_failed_usage_write_rolls_back_only_its_savepoint() {
    use rust_cli_node_records::rows::Rows;
    let (_, store) = fixture("usage-rollback").await;
    let tx = store.begin(false).await.unwrap();
    let (tx, ()) = store
        .with_rows(tx, |rows| {
            rows.write_setting(("project", "p", "permission", "mode"), "before".into(), 1)
                .unwrap();
            rows.savepoint().unwrap();
            // 违反 status 的 check 约束：这条语句失败，事务进入 aborted 状态，只能回到 savepoint。
            let failed = rows.record_model(&rust_cli_node_records::rows::ModelRow {
                id: "bad".into(),
                session_id: "missing".into(),
                status: "bogus",
                ..Default::default()
            });
            assert!(failed.is_err());
            rows.rollback_savepoint().unwrap();
            rows.write_setting(("project", "p", "permission", "ruleset"), "after".into(), 1)
                .unwrap();
            Ok(())
        })
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let tx = store.begin(true).await.unwrap();
    let (tx, values) = store
        .with_rows(tx, |rows| {
            Ok((
                rows.read_setting("project", "p", "permission", "mode")
                    .unwrap(),
                rows.read_setting("project", "p", "permission", "ruleset")
                    .unwrap(),
            ))
        })
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(values, (Some("before".into()), Some("after".into())));
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn out_of_range_usage_is_bounded_and_retention_is_bounded() {
    use rust_cli_kernel_api::metering::{CallStatus, ModelCall};
    use rust_cli_kernel_api::model::QuerySource;
    let (_, store) = fixture("usage-savepoint").await;
    let session = SessionId::new("usage");
    store.create(meta("usage"), batch("prompt")).await.unwrap();
    let call = ModelCall::new(
        MessageId::new("usage"),
        QuerySource::MainThread,
        selection(),
        0,
        u64::MAX,
        CallStatus::Completed,
    );
    let mut facts = batch("kept");
    facts.facts.push(Fact::ModelCalled { call: call.clone() });
    store.commit(&session, facts).await.unwrap();
    assert_eq!(store.load(&session).await.unwrap().messages.len(), 2);
    // 时刻超出范围时按 Node 的编排截到上限（node-records `usage`），这一行照常写入。
    assert_eq!(store.request_stats(&session).await.unwrap().requests, 1);
    let mut valid = call;
    valid.started_at_ms = 1_000;
    valid.completed_at_ms = 2_000;
    store
        .commit(
            &session,
            FactBatch {
                command: None,
                turn: None,
                facts: vec![Fact::ModelCalled { call: valid }],
            },
        )
        .await
        .unwrap();
    assert_eq!(store.request_stats(&session).await.unwrap().requests, 1);
    let mut later = store.clone();
    later.clock = std::sync::Arc::new(rust_cli_kernel_api::testing::FixedClock(
        31 * 24 * 60 * 60 * 1_000,
    ));
    assert_eq!(later.request_stats(&session).await.unwrap().requests, 0);
    assert_eq!(later.prune_usage(1).await.unwrap(), 1);
    assert_eq!(later.prune_usage(1).await.unwrap(), 0);
}
