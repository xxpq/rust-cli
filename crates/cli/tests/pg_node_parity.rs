//! 对拍（docs/specs/rust-cli/pg-store.md §12.3）：同一组会话操作分别提交到本机 NodeStore（SQLite）与云端
//! PgSessionStore（PostgreSQL），两边的 Node 表逐行相同：`data` 等 JSON 列逐字节相同，只去掉云端独有的列（租户、行序、
//! 租约）。两边用同一个固定时钟与同一串顺序 uuid。默认 ignored：需要 `ZCODE_TEST_POSTGRES_URL`（本机独立测试库）。
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    reason = "集成测试：临时目录与专用测试库的连接串"
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use rust_cli_base::secret::RedactedString;
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{EnvironmentId, MessageId, SessionId, ToolUseId, TraceId};
use rust_cli_kernel_api::message::{
    AssistantMessage, AttachmentMessage, ContentBlock, Message, StopReason, ToolResultContent,
    Usage, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::metering::{CallStatus, ModelCall, ToolCall};
use rust_cli_kernel_api::model::QuerySource;
use rust_cli_kernel_api::permission::{
    ExecutionState, PermissionMode, PermissionRule, RuleBehavior,
};
use rust_cli_kernel_api::store::{
    Fact, FactBatch, SessionFork, SessionKind, SessionMeta, SessionStore, TurnAnchor,
};
use rust_cli_kernel_api::testing::{FixedClock, selection};
use rust_cli_node_store::{NodeIds, NodeStore, UuidSource};
use rust_cli_paths::{StoragePaths, StorageSettings};
use rust_cli_pg_store::{PgDatabase, PgSessionStore};
use rust_cli_pg_store::{StoreAccessContext, StoreProject};
use serde_json::{Value, json};

const NOW: u64 = 1_790_219_323_612;
const ROOT: &str = "/workspace/parity";
const TABLES: [&str; 8] = [
    "session",
    "message",
    "part",
    "session_entry",
    "local_setting",
    "model_usage",
    "tool_usage",
    "turn_usage",
];
/// 只在云端的列（pg-store.md §12.1）。
const CLOUD_ONLY: [&str; 5] = [
    "tenant_id",
    "row_seq",
    "owner_id",
    "owner_epoch",
    "lease_until",
];

struct Sequence(AtomicU64);
impl UuidSource for Sequence {
    fn uuid(&self) -> String {
        format!("uuid{}", self.0.fetch_add(1, Ordering::SeqCst))
    }
}

fn ids() -> Arc<NodeIds> {
    Arc::new(NodeIds::with_uuids(
        Arc::new(FixedClock(NOW)),
        Arc::new(Sequence(AtomicU64::new(1))),
    ))
}

fn workspace() -> WorkspaceRef {
    WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new(ROOT),
    }
}

fn meta() -> SessionMeta {
    SessionMeta {
        id: SessionId::new("sess_parity"),
        kind: SessionKind::Interactive,
        parent: None,
        workspace: workspace(),
        title: String::new(),
        trace: TraceId::new("trace_parity"),
        created_at_ms: NOW,
    }
}

/// 消息 ID 带创建时间（Node `createMessageId`）。
fn id(n: u64, tail: &str) -> MessageId {
    MessageId::new(format!(
        "msg_{}_{tail}",
        rust_cli_base::js::base36(NOW - 10_000 + n)
    ))
}

fn user(n: u64, text: &str) -> Message {
    Message::User(UserMessage {
        id: id(n, "user"),
        origin: UserOrigin::Human,
        content: vec![ContentBlock::Text { text: text.into() }],
    })
}

fn batch(facts: Vec<Fact>, turn: Option<TurnAnchor>) -> FactBatch {
    FactBatch {
        command: Some(rust_cli_kernel_api::ids::CommandId::new("cmd_parity")),
        turn,
        facts,
    }
}

/// 一组覆盖主要写路径的操作：模型选择与执行状态、用户输入、attachment、带工具调用的 assistant、工具结果、用量、回合结束、
/// 生成标题、内部 entry 与操作意图、项目规则与模式，最后从第一条输入 fork。
async fn scenario(store: &dyn SessionStore) {
    let initial = batch(
        vec![
            Fact::ModelSelected {
                selection: selection(),
            },
            Fact::ExecutionChanged {
                state: ExecutionState::from_parts(PermissionMode::AcceptEdits, false),
            },
        ],
        None,
    );
    store.create(meta(), initial).await.unwrap();
    let session = meta().id;

    let prompt = id(1, "user");
    let assistant = id(3, "assistant");
    let call = ToolUseId::new("toolu_parity");
    let turn = TurnAnchor {
        prompt: prompt.clone(),
        turn: Some(rust_cli_kernel_api::ids::TurnId::new("turn_parity")),
    };
    let model_call = ModelCall {
        first_token_at_ms: Some(NOW - 8_500),
        stop_reason: Some(StopReason::ToolUse),
        tool_calls: 1,
        usage: Some(Usage {
            input_tokens: 10,
            output_tokens: 5,
            cache_creation_input_tokens: 2,
            cache_read_input_tokens: 3,
        }),
        ..ModelCall::new(
            assistant.clone(),
            QuerySource::MainThread,
            selection(),
            NOW - 9_000,
            NOW - 8_000,
            CallStatus::Completed,
        )
    };
    let facts = vec![
        Fact::MessageAppended {
            message: user(1, "please read the file"),
        },
        Fact::MessageAppended {
            message: Message::Attachment(AttachmentMessage {
                id: id(2, "attachment"),
                source: "date_change".into(),
                text: "<system-reminder>today</system-reminder>".into(),
                data: json!({"date": "2026-10-29"}),
            }),
        },
        Fact::MessageAppended {
            message: Message::Assistant(AssistantMessage {
                id: assistant.clone(),
                content: vec![
                    ContentBlock::Thinking {
                        thinking: "thinking".into(),
                        signature: Some("sig".into()),
                    },
                    ContentBlock::Text {
                        text: "reading".into(),
                    },
                    ContentBlock::ToolUse {
                        id: call.clone(),
                        name: "Read".into(),
                        input: json!({"file_path": "/workspace/parity/a.txt"}),
                    },
                ],
                model: "test".into(),
                stop_reason: Some(StopReason::ToolUse),
                usage: Some(Usage {
                    input_tokens: 10,
                    output_tokens: 5,
                    cache_creation_input_tokens: 2,
                    cache_read_input_tokens: 3,
                }),
            }),
        },
        Fact::ModelCalled {
            call: model_call.clone(),
        },
        Fact::ToolCalled {
            call: ToolCall {
                tool_use: call.clone(),
                name: "Read".into(),
                started_at_ms: NOW - 7_500,
                completed_at_ms: NOW - 7_000,
                status: CallStatus::Completed,
                output_bytes: 5,
            },
        },
        Fact::MessageAppended {
            message: Message::User(UserMessage {
                id: id(4, "result"),
                origin: UserOrigin::ToolResult {
                    assistant: assistant.clone(),
                },
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: call,
                    content: vec![ToolResultContent::Text {
                        text: "hello".into(),
                    }],
                    is_error: false,
                }],
            }),
        },
        Fact::MessageAppended {
            message: Message::Assistant(AssistantMessage {
                id: id(5, "final"),
                content: vec![ContentBlock::Text {
                    text: "the file says hello".into(),
                }],
                model: "test".into(),
                stop_reason: Some(StopReason::EndTurn),
                usage: None,
            }),
        },
        Fact::TurnEnded {
            status: CallStatus::Completed,
        },
        Fact::Entry {
            kind: "pending_interaction".into(),
            id: "ask_parity".into(),
            data: json!({"kind": "question", "text": "ok?"}),
        },
    ];
    store
        .commit(&session, batch(facts, Some(turn)))
        .await
        .unwrap();
    store
        .commit(
            &session,
            batch(
                vec![
                    Fact::TitleGenerated {
                        title: "Read a file".into(),
                        message: Some(prompt.clone()),
                    },
                    Fact::PermissionRulesAdded {
                        behavior: RuleBehavior::Allow,
                        rules: vec![PermissionRule {
                            tool: "Read".into(),
                            content: None,
                        }],
                    },
                    Fact::ProjectModeSet {
                        mode: PermissionMode::AcceptEdits,
                    },
                ],
                None,
            ),
        )
        .await
        .unwrap();
    store
        .fork(SessionFork {
            parent: session.clone(),
            child: SessionId::new("sess_parity_fork"),
            created_at_ms: NOW + 1_000,
            through: prompt,
            selection: selection(),
            mode: PermissionMode::AcceptEdits,
            plan: false,
        })
        .await
        .unwrap();
}

/// 去掉云端独有的列，按整行文本排序（行序由 `sequence` 等列体现，比较不依赖读取顺序）。
fn normalized(rows: Vec<Value>) -> Vec<String> {
    let mut out: Vec<String> = rows
        .into_iter()
        .map(|row| {
            // 列按名字排序（两边读出的列序不同）；列值原样比较，JSON 列是文本，逐字节比较。
            let columns: std::collections::BTreeMap<String, Value> = row
                .as_object()
                .unwrap()
                .iter()
                .filter(|(key, _)| !CLOUD_ONLY.contains(&key.as_str()))
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect();
            serde_json::to_string(&columns).unwrap()
        })
        .collect();
    out.sort();
    out
}

#[tokio::test]
#[ignore = "needs ZCODE_TEST_POSTGRES_URL for the isolated test database"]
async fn sqlite_and_postgres_write_the_same_node_rows() {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let paths = StoragePaths::resolve(&StorageSettings::default(), &base, &base);
    let node = NodeStore::new(paths, ids()).with_version("rust-cli-cloud");
    scenario(&node).await;

    let database = PgDatabase::connect_loopback_test(
        &RedactedString::new(std::env::var("ZCODE_TEST_POSTGRES_URL").unwrap()),
        4,
    )
    .await
    .unwrap();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let access = StoreAccessContext::verified(&format!("parity-{nanos}"), u64::MAX).unwrap();
    // 项目 ID 取本机按目录算出的值（Node `projectIdFromDirectory`），两边的 `project_id` 列才相同。
    let project = StoreProject {
        id: "proj_workspace-parity".into(),
        workspace: workspace(),
    };
    let pg: PgSessionStore = database
        .bind(access, vec![project], Arc::new(FixedClock(NOW)), ids())
        .unwrap();
    scenario(&pg).await;

    for table in TABLES {
        let local = normalized(node.table_rows(table).await.unwrap());
        let cloud = normalized(pg.table_rows(table).await.unwrap());
        assert!(
            !local.is_empty() || table == "tool_usage",
            "{table} has no rows"
        );
        assert_eq!(local, cloud, "table {table} differs");
    }
}
