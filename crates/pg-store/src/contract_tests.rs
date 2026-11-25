use std::sync::Arc;

use rust_cli_kernel_api::ids::{CommandId, MessageId, SessionId};
use rust_cli_kernel_api::input_journal::*;
use rust_cli_kernel_api::message::{
    AssistantMessage, CompactBoundary, CompactTrigger, ContentBlock, Message, PreservedSegment,
    SystemMessage, Usage, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::metering::{CallStatus, ModelCall};
use rust_cli_kernel_api::model::QuerySource;
use rust_cli_kernel_api::permission::{
    ExecutionState, PermissionMode, PermissionRule, RuleBehavior,
};
use rust_cli_kernel_api::session_api::{Ack, Command, CommandEnvelope, Delivery};
use rust_cli_kernel_api::store::*;

use crate::session::DeleteState;

use crate::StoreAccessContext;
use rust_cli_kernel_api::testing::{FixedClock, selection};
use serde_json::json;

use crate::tests::{batch, fixture, meta, project};

fn text(id: &str, text: &str, origin: UserOrigin) -> Message {
    Message::User(UserMessage {
        id: MessageId::new(id),
        content: vec![ContentBlock::Text { text: text.into() }],
        origin,
    })
}
fn answer(id: &str) -> Message {
    Message::Assistant(AssistantMessage {
        id: MessageId::new(id),
        content: vec![ContentBlock::Text {
            text: "answer".into(),
        }],
        model: "test".into(),
        stop_reason: None,
        usage: None,
    })
}
fn facts(facts: Vec<Fact>) -> FactBatch {
    FactBatch {
        command: None,
        facts,
        turn: None,
    }
}
fn appended(message: Message) -> Fact {
    Fact::MessageAppended { message }
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn input_promotion_rolls_back_with_message_and_deduplicates_command() {
    let (_, store) = fixture("input-contract").await;
    let session = meta("inputs").id;
    store
        .create(meta("inputs"), facts(Vec::new()))
        .await
        .unwrap();

    let envelope = CommandEnvelope {
        id: CommandId::new("command"),
        command: Command::Submit {
            text: "keep this".into(),
            extras: Default::default(),
            delivery: Delivery::Queue,
            target: None,
        },
    };
    let admission = store.admit(&session, &envelope).await.unwrap();
    assert!(!admission.duplicate);
    assert!(matches!(admission.input.receipt, Ack::Accepted { .. }));
    let message = text("promoted", "keep this", UserOrigin::Human);
    let promotion = Fact::InputsPromoted {
        commands: vec![envelope.id.clone()],
        message: message.id().clone(),
    };
    let bad = facts(vec![
        appended(message.clone()),
        promotion.clone(),
        Fact::Entry {
            kind: String::new(),
            id: "bad".into(),
            data: json!({}),
        },
    ]);
    assert!(store.commit(&session, bad).await.is_err());
    assert!(store.load(&session).await.unwrap().messages.is_empty());
    assert_eq!(store.pending(&session).await.unwrap().len(), 1);
    let good = facts(vec![appended(message), promotion]);
    store.commit(&session, good).await.unwrap();
    assert!(store.pending(&session).await.unwrap().is_empty());
    assert_eq!(store.load(&session).await.unwrap().messages.len(), 1);
    assert!(store.admit(&session, &envelope).await.unwrap().duplicate);
    let changed = CommandEnvelope {
        command: Command::Submit {
            text: "changed".into(),
            extras: Default::default(),
            delivery: Delivery::Queue,
            target: None,
        },
        ..envelope
    };
    assert!(matches!(
        store.admit(&session, &changed).await,
        Err(StoreError::Conflict { .. })
    ));
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn user_permissions_and_soft_deletion_preserve_contracts() {
    let (database, owner, tenant) = crate::tests::tenant_fixture("user-permissions").await;
    let session = meta("shared").id;
    owner.create(meta("shared"), batch("first")).await.unwrap();
    let access = StoreAccessContext::verified(&tenant, u64::MAX).unwrap();
    let reopened = database
        .bind(
            access,
            vec![project()],
            Arc::new(FixedClock(9_000)),
            Arc::new(crate::tests::SequentialUuids::default()),
        )
        .unwrap();

    let before = owner
        .list(SessionFilter {
            workspace: project().workspace,
            limit: 10,
        })
        .await
        .unwrap()[0]
        .updated_at_ms;
    reopened.rename(&session, "reopened title").await.unwrap();
    assert_eq!(
        owner
            .list(SessionFilter {
                workspace: project().workspace,
                limit: 10
            })
            .await
            .unwrap()[0]
            .updated_at_ms,
        before
    );
    assert_eq!(
        reopened.check_delete(&session).await.unwrap(),
        DeleteState::Live
    );
    let rule = PermissionRule {
        tool: "Read".into(),
        content: None,
    };
    let update = ProjectRulesChange {
        behavior: RuleBehavior::Allow,
        add: vec![rule.clone()],
        remove: Vec::new(),
    };
    reopened
        .commit(
            &session,
            facts(vec![Fact::PermissionRulesAdded {
                behavior: RuleBehavior::Allow,
                rules: vec![rule.clone()],
            }]),
        )
        .await
        .unwrap();
    assert_eq!(
        reopened
            .session_rules(&session)
            .await
            .unwrap()
            .unwrap()
            .allow,
        vec![rule]
    );
    assert_eq!(
        owner
            .project_rules(&project().workspace)
            .await
            .unwrap()
            .allow
            .len(),
        1
    );
    owner
        .change_project_rules(&project().workspace, update)
        .await
        .unwrap();
    owner.delete(&session).await.unwrap();
    owner.delete(&session).await.unwrap();
    assert!(matches!(
        owner.load(&session).await,
        Err(StoreError::NotFound { .. })
    ));
    assert!(
        owner
            .list(SessionFilter {
                workspace: project().workspace,
                limit: 10
            })
            .await
            .unwrap()
            .is_empty()
    );
    assert!(owner.commit(&session, batch("late-message")).await.is_err());
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn history_compaction_fork_records_and_usage_are_preserved() {
    let (_, store) = fixture("history").await;
    let session = meta("history").id;
    store
        .create(
            meta("history"),
            facts(vec![
                Fact::ModelSelected {
                    selection: selection(),
                },
                Fact::ExecutionChanged {
                    state: ExecutionState::from_parts(PermissionMode::AcceptEdits, true),
                },
                appended(text("u1", "old", UserOrigin::Human)),
                appended(answer("a1")),
                appended(text("u2", "preserved", UserOrigin::Human)),
                appended(answer("a2")),
            ]),
        )
        .await
        .unwrap();

    let boundary = Message::System(SystemMessage::CompactBoundary(CompactBoundary {
        id: MessageId::new("boundary"),
        trigger: CompactTrigger::Manual,
        pre_tokens: 100,
        summary: MessageId::new("summary"),
        preserved: Some(PreservedSegment {
            head: MessageId::new("u2"),
            tail: MessageId::new("a2"),
            anchor: MessageId::new("summary"),
        }),
        discovered_tools: Vec::new(),
    }));
    let call = ModelCall {
        first_token_at_ms: Some(1_100),
        usage: Some(Usage {
            input_tokens: 10,
            output_tokens: 40,
            cache_creation_input_tokens: 30,
            cache_read_input_tokens: 20,
        }),
        ..ModelCall::new(
            MessageId::new("a3"),
            QuerySource::MainThread,
            selection(),
            1_000,
            1_400,
            CallStatus::Completed,
        )
    };
    store
        .commit(
            &session,
            facts(vec![
                appended(boundary),
                appended(text("summary", "summary", UserOrigin::CompactSummary)),
                appended(text("u3", "new", UserOrigin::Human)),
                appended(answer("a3")),
                Fact::ModelCalled { call },
            ]),
        )
        .await
        .unwrap();
    let loaded = store.load(&session).await.unwrap();
    assert_eq!(
        loaded
            .earlier
            .iter()
            .map(|m| m.id().as_str())
            .collect::<Vec<_>>(),
        ["u1", "a1"]
    );
    assert_eq!(
        loaded
            .messages
            .iter()
            .map(|m| m.id().as_str())
            .collect::<Vec<_>>(),
        ["boundary", "summary", "u2", "a2", "u3", "a3"]
    );
    assert_eq!(loaded.mode, Some(PermissionMode::AcceptEdits));
    assert!(loaded.plan);
    let stats = store.request_stats(&session).await.unwrap();
    assert_eq!(
        (stats.requests, stats.input_tokens, stats.cache_read_tokens),
        (1, 60, 20)
    );
    assert_eq!(stats.last.unwrap().generation_ms(), Some(300));
    assert_eq!(store.metering(&session).await.unwrap().api_duration_ms, 400);
    let record = FrontendRecord {
        kind: "test".into(),
        key: "key".into(),
        data: json!({"value":1}),
    };
    store.put_record(&session, &record).await.unwrap();
    assert_eq!(store.records(&session, "test").await.unwrap(), vec![record]);
    let fork = SessionFork {
        parent: session.clone(),
        child: meta("forked").id,
        created_at_ms: 2_000,
        through: MessageId::new("a3"),
        selection: selection(),
        mode: PermissionMode::AcceptEdits,
        plan: true,
    };
    store.fork(fork.clone()).await.unwrap();
    let child = store.load(&fork.child).await.unwrap();
    assert!(child.messages.iter().any(|message| matches!(message,Message::User(user) if user.content.iter().any(|block|matches!(block,ContentBlock::Text{text} if text=="new")))));
    assert!(child.checkpoints.is_empty());
    // 复制的消息换了新 ID；压缩边界的 `boundaryId` 照 Node 的 fork 复制规则保留原值（node-store `fork_clone::boundary`）。
    assert!(child.messages.iter().all(|m| {
        matches!(m, Message::System(SystemMessage::CompactBoundary(_)))
            || !loaded.messages.iter().any(|old| old.id() == m.id())
    }));
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn rls_rejects_direct_unscoped_queries_and_pool_context_is_local() {
    let (_, store) = fixture("rls").await;
    store
        .create(meta("protected"), batch("message"))
        .await
        .unwrap();
    let count: i64 = sqlx::query_scalar("select count(*) from zcode.message")
        .fetch_one(store.orm.get_postgres_connection_pool())
        .await
        .unwrap();
    assert_eq!(
        count, 0,
        "the runtime role must not see rows without a transaction tenant"
    );
    let tx = store.begin(true).await.unwrap();
    assert_eq!(
        crate::orm::scalar::<i64>("select count(*) from zcode.message")
            .fetch_one(&tx)
            .await
            .unwrap(),
        1
    );
    tx.commit().await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("select count(*) from zcode.message")
            .fetch_one(store.orm.get_postgres_connection_pool())
            .await
            .unwrap(),
        0
    );
    let tx = store.begin(false).await.unwrap();
    crate::orm::native("select set_config('zcode.tenant_id','foreign',true)")
        .execute(&tx)
        .await
        .unwrap();
    assert!(crate::orm::native("insert into zcode.message(tenant_id,id,session_id,time_created,time_updated,data) values('foreign','bad','protected',0,0,'{}')").execute(&tx).await.is_err(),"composite FK must reject a parent from another tenant");
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn input_deduplicates_by_command_id_and_rejects_changed_content() {
    let (_, store) = fixture("input-identity").await;
    let session = SessionId::new("input");
    store.create(meta("input"), batch("initial")).await.unwrap();

    let first = CommandEnvelope {
        id: CommandId::new("one"),
        command: Command::Submit {
            text: "hello".into(),
            extras: Default::default(),
            delivery: Delivery::Steer,
            target: None,
        },
    };
    store.admit(&session, &first).await.unwrap();
    let second = CommandEnvelope {
        id: CommandId::new("two"),
        ..first.clone()
    };
    store.admit(&session, &second).await.unwrap();
    assert!(store.admit(&session, &second).await.unwrap().duplicate);
    let changed = CommandEnvelope {
        command: Command::Submit {
            text: "different".into(),
            extras: Default::default(),
            delivery: Delivery::Queue,
            target: None,
        },
        ..second
    };
    assert!(matches!(
        store.admit(&session, &changed).await,
        Err(StoreError::Conflict { .. })
    ));
    let retry = CommandEnvelope {
        command: Command::Submit {
            text: "hello".into(),
            extras: Default::default(),
            delivery: Delivery::Queue,
            target: None,
        },
        ..first
    };
    assert!(store.admit(&session, &retry).await.unwrap().duplicate);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn common_session_store_contract() {
    let (_, store) = fixture("common-contract").await;
    rust_cli_kernel_api::testing::store_contract::verify(&store, meta("sess_contract"))
        .await
        .unwrap();
}

/// 库里的子代理在 PG 上（PERF-SWITCH，node-store.md §5.4）：父会话只读消息 id 与 Agent 的 tool part（`json_path` 筛），
/// 读出的关联与子会话最后的回答同本机；回退掉调用之后不再列出。
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn subagents_read_only_the_agent_parts_of_the_branch() {
    use rust_cli_kernel_api::ids::{AgentId, ToolUseId};
    use rust_cli_kernel_api::message::{StopReason, ToolResultContent};
    let (_, store) = fixture("subagents").await;
    let parent = meta("sess_parent");
    store
        .create(parent.clone(), facts(Vec::new()))
        .await
        .unwrap();
    let child = SessionMeta {
        kind: SessionKind::Subagent,
        parent: Some(parent.id.clone()),
        ..meta("sess_subagent_agent_1")
    };
    store
        .create(
            child.clone(),
            facts(vec![appended(answer("msg_child_answer"))]),
        )
        .await
        .unwrap();
    let tool_use = ToolUseId::new("toolu_1");
    let call = Message::Assistant(AssistantMessage {
        id: MessageId::new("msg_call"),
        content: vec![ContentBlock::ToolUse {
            id: tool_use.clone(),
            name: "Agent".into(),
            input: json!({"description": "look around", "prompt": "look"}),
        }],
        model: "test".into(),
        stop_reason: Some(StopReason::ToolUse),
        usage: None,
    });
    let result = Message::User(UserMessage {
        id: MessageId::new("msg_result"),
        content: vec![ContentBlock::ToolResult {
            tool_use_id: tool_use.clone(),
            content: vec![ToolResultContent::Text {
                text: "launched".into(),
            }],
            is_error: false,
        }],
        origin: UserOrigin::ToolResult {
            assistant: MessageId::new("msg_call"),
        },
    });
    let linked = Fact::SubagentLinked {
        tool_use: tool_use.clone(),
        agent: AgentId::new("agent_1"),
        child: child.id.clone(),
        agent_type: "Explore".into(),
    };
    store
        .commit(
            &parent.id,
            facts(vec![
                appended(text("msg_ask", "explore", UserOrigin::Human)),
                appended(call),
                appended(result),
                linked,
            ]),
        )
        .await
        .unwrap();
    let found = store.subagents(&parent.id).await.unwrap();
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].tool_use, tool_use);
    assert_eq!(found[0].child, child.id);
    assert_eq!(found[0].summary.as_deref(), Some("answer"));
    let cursor = RevertCursor {
        target: MessageId::new("msg_ask"),
        kept: Vec::new(),
        cut_after: MessageId::new("msg_result"),
        generation: 1,
    };
    store
        .commit(&parent.id, facts(vec![Fact::RevertSet { cursor }]))
        .await
        .unwrap();
    assert_eq!(store.subagents(&parent.id).await.unwrap(), Vec::new());
}

/// 按 id 读历史的一段在 PG 上（PERF-WINDOW，node-store.md §5.5）：与本机同一份编排（`load::slice`），只读给出的消息与它们的 part；
/// 工具结果随它的 assistant；库里没有的 id 不给出；与整份冷读里这几条相同。
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn history_reads_only_the_given_messages() {
    use rust_cli_kernel_api::ids::ToolUseId;
    use rust_cli_kernel_api::message::{StopReason, ToolResultContent};
    let (_, store) = fixture("history").await;
    let session = meta("sess_history");
    store
        .create(session.clone(), facts(Vec::new()))
        .await
        .unwrap();
    let call = Message::Assistant(AssistantMessage {
        id: MessageId::new("msg_call"),
        content: vec![ContentBlock::ToolUse {
            id: ToolUseId::new("toolu_1"),
            name: "Read".into(),
            input: json!({"file_path": "/w/a"}),
        }],
        model: "test".into(),
        stop_reason: Some(StopReason::ToolUse),
        usage: None,
    });
    let result = Message::User(UserMessage {
        id: MessageId::new("msg_result"),
        content: vec![ContentBlock::ToolResult {
            tool_use_id: ToolUseId::new("toolu_1"),
            content: vec![ToolResultContent::Text { text: "a".into() }],
            is_error: false,
        }],
        origin: UserOrigin::ToolResult {
            assistant: MessageId::new("msg_call"),
        },
    });
    store
        .commit(
            &session.id,
            facts(vec![
                appended(text("msg_ask", "read it", UserOrigin::Human)),
                appended(call),
                appended(result),
                appended(answer("msg_answer")),
            ]),
        )
        .await
        .unwrap();
    let whole = store.load(&session.id).await.unwrap().messages;
    let records = [MessageId::new("msg_call"), MessageId::new("msg_gone")];
    let slice = store.history(&session.id, &records).await.unwrap();
    let wanted: Vec<Message> = whole
        .into_iter()
        .filter(|message| message.record().as_str() == "msg_call")
        .collect();
    assert_eq!(slice.messages, wanted);
    assert!(slice.turns.is_empty());
}

/// 范围读在 PG 上（NAV-DIR，node-store.md §5.6）：与本机同一份编排（`range::read`）；最后一段只带它的消息、`earlier` 与段首；读全部与冷读相同。
#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn range_reads_the_tail_and_pages_back() {
    use rust_cli_kernel_api::store::HistorySpan;
    let (_, store) = fixture("range").await;
    let session = meta("sess_range");
    store
        .create(session.clone(), facts(Vec::new()))
        .await
        .unwrap();
    store
        .commit(
            &session.id,
            facts(vec![
                appended(text("msg_ask_1", "one", UserOrigin::Human)),
                appended(answer("msg_answer_1")),
                appended(text("msg_ask_2", "two", UserOrigin::Human)),
                appended(answer("msg_answer_2")),
            ]),
        )
        .await
        .unwrap();
    let tail = store
        .range(
            &session.id,
            HistorySpan::Before {
                floor: None,
                records: 2,
            },
        )
        .await
        .unwrap();
    let ids: Vec<&str> = tail
        .loaded
        .messages
        .iter()
        .map(|m| m.id().as_str())
        .collect();
    assert_eq!(ids, ["msg_ask_2", "msg_answer_2"]);
    assert!(tail.earlier);
    assert_eq!(
        tail.floor.as_ref().map(MessageId::as_str),
        Some("msg_ask_2")
    );
    let whole = store
        .range(
            &session.id,
            HistorySpan::Before {
                floor: None,
                records: usize::MAX,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        whole.loaded.messages,
        store.load(&session.id).await.unwrap().messages
    );
    assert!(!whole.earlier);
}
