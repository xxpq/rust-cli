//! 会话的只读查询（protocol-v5.md §40）：分页与快照窗口同一个全序、`at` 的过期、计划目录、文件改动只认回合头；actor 里物化了的同一步答、
//! 改写历史之后旧的 `at` 过期；没有物化的会话经任务建临时投影（每次一个新 epoch、不登记 topic），`shutdown` 等在途的读。文件改动的明细
//! （§44）：数字取回合记录、预算之内放 `patches`，物化了的与没有物化的都经任务读内核。

use std::sync::Arc;

use async_trait::async_trait;
use rust_cli_base::TaskScope;
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{EnvironmentId, MessageId, SessionId, ToolUseId, TraceId, TurnId};
use rust_cli_kernel_api::message::{
    AssistantMessage, ContentBlock, Message, ToolResultContent, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::{
    InputEnd, Kernel, KernelError, SessionHandle, SessionView, TurnFileDiff, TurnOrigin, TurnTime,
};
use rust_cli_kernel_api::store::{
    SessionFilter, SessionKind, SessionMeta, SessionSummary, StoreQueries, TurnEnded, TurnRecord,
};
use rust_cli_kernel_api::testing::{MemoryArtifacts, MemoryStore};
use rust_cli_protocol_core::connection::{self, Options};
use rust_cli_protocol_core::framing::{Dialect, Framing};
use serde_json::{Value, json};
use tokio::io::duplex;
use tokio::sync::{Notify, mpsc};

use super::*;
use crate::consts::SESSION_SNAPSHOT_ROWS;
use crate::server::{Settings, WorkspaceKernel, Workspaces};
use crate::session::lane::LaneEvent;
use crate::session::projection::Origin;
use crate::test_link::{Harness, initialize};

const SID: &str = "ses_1";
#[cfg(not(windows))]
const KEY: &str = "/Users/dev/a";
#[cfg(windows)]
const KEY: &str = r"C:\Users\dev\a";

fn workspace() -> WorkspaceRef {
    WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new(KEY),
    }
}

fn user(id: &str, text: &str) -> Message {
    Message::User(UserMessage {
        id: MessageId::new(id),
        content: vec![ContentBlock::Text {
            text: text.to_owned(),
        }],
        origin: UserOrigin::Human,
    })
}

fn assistant(id: &str, content: Vec<ContentBlock>) -> Message {
    Message::Assistant(AssistantMessage {
        id: MessageId::new(id),
        content,
        model: rust_cli_base::models::GLM_5_3.to_owned(),
        stop_reason: None,
        usage: None,
    })
}

fn answer_text(id: &str, text: &str) -> Message {
    assistant(
        id,
        vec![ContentBlock::Text {
            text: text.to_owned(),
        }],
    )
}

fn tool_call(id: &str, call: &str, name: &str) -> Message {
    assistant(
        id,
        vec![ContentBlock::ToolUse {
            id: ToolUseId::new(call),
            name: name.to_owned(),
            input: json!({}),
        }],
    )
}

fn result(id: &str, assistant: &str, call: &str, is_error: bool) -> Message {
    Message::User(UserMessage {
        id: MessageId::new(id),
        content: vec![ContentBlock::ToolResult {
            tool_use_id: ToolUseId::new(call),
            content: vec![ToolResultContent::Text {
                text: "result".to_owned(),
            }],
            is_error,
        }],
        origin: UserOrigin::ToolResult {
            assistant: MessageId::new(assistant),
        },
    })
}

/// 第 n 个回合：用户输入 + 一条回答（三行：回合头、用户输入、正文）。
fn turn(n: u64) -> (Vec<Message>, TurnRecord) {
    let first = format!("msg_u{n}");
    let messages = vec![
        user(&first, &format!("q{n}")),
        answer_text(&format!("msg_a{n}"), &format!("a{n}")),
    ];
    (messages, record(n, &first))
}

fn record(n: u64, first: &str) -> TurnRecord {
    TurnRecord {
        turn: TurnId::new(format!("turn_{n}")),
        first: MessageId::new(first),
        origin: TurnOrigin::Input,
        started_at: n * 100,
        command: None,
        ended: Some(TurnEnded {
            outcome: InputEnd::Completed,
            cancelled: Vec::new(),
            time: Some(TurnTime {
                ended_at: n * 100 + 50,
                active_ms: 10,
            }),
            files: Vec::new(),
        }),
        fell_back: None,
        works: Vec::new(),
        compactions: Vec::new(),
    }
}

fn view(messages: Vec<Message>, turns: Vec<TurnRecord>) -> SessionView {
    let meta = SessionMeta {
        id: SessionId::new(SID),
        kind: SessionKind::Interactive,
        parent: None,
        workspace: workspace(),
        title: String::new(),
        trace: TraceId::new(SID),
        created_at_ms: 1,
    };
    SessionView {
        turns,
        ..SessionView::new(
            meta,
            messages,
            rust_cli_kernel_api::testing::selection(),
            PermissionMode::Default,
        )
    }
}

/// `count` 个回合的冷读视图。
fn turns(count: u64) -> SessionView {
    let (messages, records): (Vec<_>, Vec<_>) = (1..=count).map(turn).unzip();
    view(messages.into_iter().flatten().collect(), records)
}

fn origin() -> Origin {
    Origin {
        live: false,
        draft: false,
        deleted: false,
        title_source: rust_cli_kernel_api::session_facts::TitleSource::Default,
        mcp: Value::Null,
        subagents: Vec::new(),
        stamp: None,
        ..Default::default()
    }
}

/// 由视图建的会话 topic（同 actor 的写法：带快照窗口）。
fn published(view: &SessionView) -> Publisher {
    let (_, content) = Projection::build(view, origin());
    let mut publisher = Publisher::new(session_topic(KEY, SID), "e").with_window(
        ROWS,
        CREATED_AT_SEQ,
        SESSION_SNAPSHOT_ROWS,
        usize::MAX,
    );
    publisher.adopt(content);
    publisher
}

fn range(before: Option<&str>, limit: usize) -> Answered {
    Answered::Range {
        before: before.map(str::to_owned),
        limit,
    }
}

/// 覆盖整段历史的投影（`earlier` 为假，§73.3）：只按 Publisher 答的查询用它。
fn complete() -> Projection {
    Projection::build(&turns(0), origin()).0
}

/// 这一步就答完的查询（行的分页、计划目录、stale）。
fn ready(
    publisher: &Publisher,
    request: &Answered,
    at: Option<&Cursor>,
    session: &str,
) -> Result<Value, Fault> {
    match answer(publisher, &complete(), request, at, session)? {
        Answer::Ready(value) => Ok(value),
        other => panic!("not answered in the step: {other:?}"),
    }
}

fn ok_data(value: &Value) -> &Value {
    assert_eq!(value["kind"], json!("ok"), "{value}");
    &value["data"]
}

fn row_ids(data: &Value) -> Vec<&str> {
    data[ROWS_KEY]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["id"].as_str().unwrap())
        .collect()
}

/// 分页与快照窗口同一个全序；每一页从回合的开头起（protocol-v5.md §71.1，PERF-WINDOW 改：以前按行数切，第一页是 `msg_a2` 起）。
#[test]
fn rows_page_back_from_the_tail_in_the_order_of_the_snapshot() {
    let publisher = published(&turns(3));
    let tail = ready(&publisher, &range(None, 4), None, SID).unwrap();
    assert_eq!(row_ids(ok_data(&tail)), ["turn_3", "msg_u3", "msg_a3"]);
    assert_eq!(ok_data(&tail)[HAS_MORE_KEY], json!(true));
    assert_eq!(tail["cursor"], json!(publisher.cursor()));
    let older = ready(&publisher, &range(Some("turn_3"), 4), None, SID).unwrap();
    assert_eq!(row_ids(ok_data(&older)), ["turn_2", "msg_u2", "msg_a2"]);
    // 一页里没有回合的开头（一个回合比一页长）：照旧按行数
    let inside = ready(&publisher, &range(Some("turn_3"), 2), None, SID).unwrap();
    assert_eq!(row_ids(ok_data(&inside)), ["msg_u2", "msg_a2"]);
    let first = ready(&publisher, &range(Some("turn_2"), 4), None, SID).unwrap();
    assert_eq!(row_ids(ok_data(&first)), ["turn_1", "msg_u1", "msg_a1"]);
    assert_eq!(ok_data(&first)[HAS_MORE_KEY], json!(false));
    // 行是快照里的写法：带 id、v 与 kind
    assert_eq!(
        ok_data(&first)[ROWS_KEY][0]["kind"],
        json!(kinds::TURN_HEADER)
    );
    assert_eq!(ok_data(&first)[ROWS_KEY][0]["v"], json!(1));
    let missing = ready(&publisher, &range(Some("msg_x"), 4), None, SID).unwrap_err();
    assert_eq!(missing.code, codes::PRECONDITION_NOT_FOUND);
}

#[test]
fn a_stale_at_gets_the_current_cursor_instead_of_rows() {
    let publisher = published(&turns(1));
    let other = Cursor {
        epoch: "other".to_owned(),
        seq: 1,
    };
    for request in [range(None, 2), Answered::Plans] {
        let stale = ready(&publisher, &request, Some(&other), SID).unwrap();
        assert_eq!(
            stale,
            json!({"kind": "stale", "cursor": publisher.cursor()})
        );
    }
    let fresh = ready(&publisher, &range(None, 2), Some(&publisher.cursor()), SID).unwrap();
    assert_eq!(row_ids(ok_data(&fresh)), ["msg_u1", "msg_a1"]);
}

/// 计划目录（§42.1，照 v4 的条件）：有结局、带计划文字的 ExitPlanMode 行，最新的在前；没有计划文字的（计划文件为空、停止时还在等回答）不进。
/// 以前不看计划文字（行上没有它）。
#[test]
fn the_plan_directory_holds_ended_exit_plan_mode_rows_with_a_plan_newest_first() {
    let messages = vec![
        user("msg_u1", "plan"),
        tool_call("msg_t1", "toolu_p1", EXIT_PLAN_MODE),
        result("msg_r1", "msg_t1", "toolu_p1", true),
        tool_call("msg_t2", "toolu_read", "Read"),
        result("msg_r2", "msg_t2", "toolu_read", false),
        tool_call("msg_t3", "toolu_p2", EXIT_PLAN_MODE),
        result("msg_r3", "msg_t3", "toolu_p2", false),
        tool_call("msg_t4", "toolu_p3", EXIT_PLAN_MODE),
        result("msg_r4", "msg_t4", "toolu_p3", false),
        answer_text("msg_a1", "done"),
    ];
    let shown = |call: &str, plan: &str| rust_cli_kernel_api::plan::ShownPlan {
        tool_use: ToolUseId::new(call),
        plan: plan.to_owned(),
        path: format!("{KEY}/.zcode/plans/plan-{SID}.md"),
    };
    let mut view = view(messages, vec![record(1, "msg_u1")]);
    view.plans = vec![
        shown("toolu_p1", "# A"),
        shown("toolu_p2", "# B"),
        shown("toolu_p3", " \n"),
    ];
    let publisher = published(&view);
    let plans = ready(&publisher, &Answered::Plans, None, SID).unwrap();
    let plans = ok_data(&plans)[PLANS_KEY].as_array().unwrap().clone();
    let summary: Vec<(&str, &str, &str)> = plans
        .iter()
        .map(|row| {
            (
                row["id"].as_str().unwrap(),
                row["status"].as_str().unwrap(),
                row["input"]["plan"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            ("toolu_p2", tool_status::COMPLETED, "# B"),
            ("toolu_p1", tool_status::ERROR, "# A")
        ]
    );
    let none = ready(&published(&turns(1)), &Answered::Plans, None, SID).unwrap();
    assert_eq!(ok_data(&none)[PLANS_KEY], json!([]));
}

#[test]
fn params_follow_the_contract() {
    let error = request(SessionQuery::Range, json!({"limit": 201})).unwrap_err();
    assert_eq!(error.code, codes::PRECONDITION_INVALID_PAYLOAD);
    assert!(error.message.starts_with("limit"), "{}", error.message);
    let Parsed::Request(Request::Answered(Answered::Range { before, limit })) =
        request(SessionQuery::Range, json!({"extra": 1})).unwrap()
    else {
        panic!("not a range");
    };
    // 缺省一页 200（契约的缺省），未知字段忽略
    assert_eq!((before, limit), (None, 200));
    let error = request(SessionQuery::FileChanges, json!({})).unwrap_err();
    assert_eq!(error.code, codes::PRECONDITION_INVALID_PAYLOAD);
}

// ---------- actor 里的两条路 ----------

/// 冷读的内核：`session_snapshot` 给出这份视图，`turn_file_diffs` 给出这份明细（只认 `turn_1`）；有闸门时等它放开（在途的读）。
struct Snapshots {
    view: SessionView,
    diffs: Vec<TurnFileDiff>,
    gate: Option<Arc<Notify>>,
    store: Arc<MemoryStore>,
}

#[async_trait]
impl Kernel for Snapshots {
    async fn create_session(
        &self,
        _request: rust_cli_kernel_api::session_api::CreateSession,
    ) -> Result<SessionId, KernelError> {
        Err(KernelError::Unsupported {
            what: "sessions".to_owned(),
        })
    }

    async fn open_session(
        &self,
        session: &SessionId,
    ) -> Result<Arc<dyn SessionHandle>, KernelError> {
        Err(KernelError::NotFound {
            session: session.clone(),
        })
    }

    async fn list_sessions(
        &self,
        _filter: SessionFilter,
    ) -> Result<Vec<SessionSummary>, KernelError> {
        Ok(Vec::new())
    }

    fn queries(&self) -> Arc<dyn StoreQueries> {
        self.store.clone()
    }

    async fn session_snapshot(&self, session: &SessionId) -> Result<SessionView, KernelError> {
        if let Some(gate) = &self.gate {
            gate.notified().await;
        }
        if session.as_str() == SID {
            Ok(self.view.clone())
        } else {
            Err(KernelError::NotFound {
                session: session.clone(),
            })
        }
    }

    async fn turn_file_diffs(
        &self,
        session: &SessionId,
        turn: &TurnId,
    ) -> Result<Vec<TurnFileDiff>, KernelError> {
        if let Some(gate) = &self.gate {
            gate.notified().await;
        }
        if session.as_str() != SID {
            return Err(KernelError::NotFound {
                session: session.clone(),
            });
        }
        Ok(if turn.as_str() == "turn_1" {
            self.diffs.clone()
        } else {
            Vec::new()
        })
    }
}

struct Local(Arc<Snapshots>);

#[async_trait]
impl Workspaces for Local {
    async fn open(&self, _key: &str) -> Result<WorkspaceKernel, String> {
        Ok(WorkspaceKernel {
            kernel: self.0.clone(),
            selection: rust_cli_kernel_api::testing::selection(),
            mode: PermissionMode::Default,
        })
    }

    fn release(&self, _key: &str) -> bool {
        false
    }

    fn workspace(&self, _key: &str) -> WorkspaceRef {
        workspace()
    }

    async fn mcp_pending(&self, _key: &str) -> Vec<crate::McpPending> {
        Vec::new()
    }

    async fn trust_mcp(
        &self,
        _key: &str,
        _decision: crate::McpDecision,
    ) -> Result<crate::McpTrusted, crate::McpTrustRefusal> {
        Err(crate::McpTrustRefusal::Unavailable(
            "this test has no trust record".to_owned(),
        ))
    }
}

fn local(view: SessionView, gate: Option<Arc<Notify>>) -> Arc<Local> {
    local_with(view, Vec::new(), gate)
}

fn local_with(
    view: SessionView,
    diffs: Vec<TurnFileDiff>,
    gate: Option<Arc<Notify>>,
) -> Arc<Local> {
    Arc::new(Local(Arc::new(Snapshots {
        view,
        diffs,
        gate,
        store: Arc::default(),
    })))
}

fn query_message(id: u64, kind: &str, params: Value, at: Option<Value>) -> Value {
    let mut params = json!({"type": kind, "target": {"hostId": "local", "workspaceKey": KEY, "sessionId": SID},
                            "params": params});
    if let Some(at) = at {
        params["at"] = at;
    }
    json!({"jsonrpc": "2.0", "id": id, "method": "query", "params": params})
}

/// 改写历史（通道的 `Rebuilt` 换了内容）之后，早于它的 `at` 过期；用返回的 cursor 重发一次就是新内容。
#[tokio::test]
async fn a_rewrite_makes_an_earlier_at_stale() {
    let scope = TaskScope::root("test", |_| {});
    let options = Options {
        framing: Framing::ContentLength,
        dialect: Dialect::JsonRpc2,
        ..Options::default()
    };
    let (out, _host) = duplex(1 << 20);
    let (peer, _writer) = connection::start(out, &scope.spawner(), options);
    let (events, _inbox) = mpsc::unbounded_channel();
    let mut actor = Actor::new(peer, Settings::default(), &scope.spawner(), events);
    actor.process_ready(local(turns(2), None), Arc::new(MemoryStore::default()));
    actor.lane(KEY, SID).unwrap();
    let generation = actor.sessions.slots[SID].lane.as_ref().unwrap().1;
    let viewed = LaneEvent::Viewed {
        view: Box::new(turns(2)),
        origin: origin(),
    };
    actor.on_lane(SID, generation, viewed);
    let envelope = |at: Option<Cursor>| -> QueryEnvelope {
        serde_json::from_value(json!({"type": ROWS_RANGE,
            "target": {"hostId": "local", "workspaceKey": KEY, "sessionId": SID},
            "params": {"limit": 2}, "at": at}))
        .unwrap()
    };
    // 物化了的：这一步里答，带 topic 的 cursor
    let first = actor
        .session_query(&json!(1), SessionQuery::Range, envelope(None))
        .unwrap()
        .unwrap();
    let cursor: Cursor = serde_json::from_value(first["cursor"].clone()).unwrap();
    assert_eq!(cursor.epoch, actor.epoch);
    // 改写历史：第一个回合没了
    let (messages, record) = turn(2);
    actor.on_lane(
        SID,
        generation,
        LaneEvent::Rebuilt(Box::new(view(messages, vec![record]))),
    );
    let stale = actor
        .session_query(&json!(2), SessionQuery::Range, envelope(Some(cursor)))
        .unwrap()
        .unwrap();
    assert_eq!(stale["kind"], json!("stale"));
    let current: Cursor = serde_json::from_value(stale["cursor"].clone()).unwrap();
    let again = actor
        .session_query(&json!(3), SessionQuery::Range, envelope(Some(current)))
        .unwrap()
        .unwrap();
    assert_eq!(row_ids(ok_data(&again)), ["msg_u2", "msg_a2"]);
    assert_eq!(ok_data(&again)[HAS_MORE_KEY], json!(true));
}

/// 没有物化的会话：读视图的任务建临时投影答（每次一个新 epoch），不登记 topic（之后的订阅照常物化，先回 pending）。
#[tokio::test]
async fn a_session_without_a_topic_is_read_into_a_throwaway_projection() {
    let mut h = Harness::new();
    h.handle.ready(
        local(turns(2), None),
        Arc::new(MemoryStore::default()),
        Arc::new(MemoryArtifacts::default()),
    );
    h.send(initialize()).await;
    h.next().await;
    let mut frames = Vec::new();
    h.send(query_message(2, ROWS_RANGE, json!({"limit": 2}), None))
        .await;
    let first = h.reply(2, &mut frames).await;
    assert_eq!(row_ids(ok_data(&first["result"])), ["msg_u2", "msg_a2"]);
    let cursor = first["result"]["cursor"].clone();
    let epoch = cursor["epoch"].as_str().unwrap().to_owned();
    assert!(epoch.contains(READ_EPOCH), "{epoch}");
    // 带 at：一定过期，每次一个新的临时 epoch（02 §2.3：重发一次之后由调用方结算）
    h.send(query_message(
        3,
        ROWS_RANGE,
        json!({"limit": 2}),
        Some(cursor),
    ))
    .await;
    let stale = h.reply(3, &mut frames).await;
    assert_eq!(stale["result"]["kind"], json!("stale"));
    let again = stale["result"]["cursor"].clone();
    assert_ne!(again["epoch"], json!(epoch));
    h.send(query_message(
        4,
        ROWS_RANGE,
        json!({"limit": 2}),
        Some(again.clone()),
    ))
    .await;
    let still = h.reply(4, &mut frames).await;
    assert_eq!(still["result"]["kind"], json!("stale"));
    assert_ne!(still["result"]["cursor"]["epoch"], again["epoch"]);
    // 别的会话：通道与查询同一个冷读，找不到 → notFound
    let mut missing = query_message(5, SESSION_PLANS, json!({}), None);
    missing["params"]["target"]["sessionId"] = json!("ses_missing");
    h.send(missing).await;
    let missing = h.reply(5, &mut frames).await;
    assert_eq!(
        missing["error"]["data"]["code"],
        json!(codes::PRECONDITION_NOT_FOUND)
    );
    // 查询没有登记 topic：订阅先回 pending，再拿快照
    let topic = session_topic(KEY, SID);
    h.send(json!({"jsonrpc": "2.0", "id": 6, "method": "subscribe", "params": {"sub": "s", "topic": topic}}))
        .await;
    let subscribed = h.reply(6, &mut frames).await;
    assert_eq!(subscribed["result"]["pending"], json!("source_unavailable"));
    assert!(frames.is_empty(), "{frames:?}");
}

/// `shutdown` 的回复等在途的读视图回来（计数，§40.3）。
#[tokio::test]
async fn shutdown_waits_for_a_session_read() {
    let gate = Arc::new(Notify::new());
    let mut h = Harness::new();
    h.handle.ready(
        local(turns(1), Some(gate.clone())),
        Arc::new(MemoryStore::default()),
        Arc::new(MemoryArtifacts::default()),
    );
    h.send(initialize()).await;
    h.next().await;
    h.send(query_message(2, SESSION_PLANS, json!({}), None))
        .await;
    h.send(json!({"jsonrpc": "2.0", "id": 3, "method": "shutdown", "params": {}}))
        .await;
    // ping 在 shutdown 之后不收；它的回复说明 actor 已经处理过 shutdown，而这时查询与 shutdown 都还没有回复
    h.send(json!({"jsonrpc": "2.0", "id": 4, "method": "ping", "params": {}}))
        .await;
    let refused = h.next().await;
    assert_eq!(refused["id"], json!(4));
    gate.notify_one();
    let plans = h.next().await;
    assert_eq!(plans["id"], json!(2));
    assert_eq!(ok_data(&plans["result"])[PLANS_KEY], json!([]));
    assert_eq!(h.next().await["id"], json!(3));
}

// ---------- 要会话运行时的读与调试记录（§43） ----------

fn runtime_envelope(kind: &str, params: Value, at: Option<Cursor>) -> QueryEnvelope {
    serde_json::from_value(json!({"type": kind,
        "target": {"hostId": "local", "workspaceKey": KEY, "sessionId": SID},
        "params": params, "at": at}))
    .unwrap()
}

/// 物化了的会话：调试记录在这一步里答（通道转来的内核事件喂它）；要会话运行时的读交给通道（这条通道没有打开会话 → 冷的答案），
/// 结果经 `Event::SessionRead` 回来、记一笔在途的读；`at` 过期时照旧在这一步回 stale，不交给通道。
#[tokio::test]
async fn runtime_reads_go_through_the_lane_and_the_debug_record_is_answered_at_once() {
    use rust_cli_kernel_api::metering::{CallStatus, ModelCall};
    use rust_cli_kernel_api::model::QuerySource;
    use rust_cli_kernel_api::session_api::{KernelEvent, SequencedEvent};

    let scope = TaskScope::root("test", |_| {});
    let options = Options {
        framing: Framing::ContentLength,
        dialect: Dialect::JsonRpc2,
        ..Options::default()
    };
    let (out, _host) = duplex(1 << 20);
    let (peer, _writer) = connection::start(out, &scope.spawner(), options);
    let (events, mut inbox) = mpsc::unbounded_channel();
    let mut actor = Actor::new(peer, Settings::default(), &scope.spawner(), events);
    actor.process_ready(local(turns(1), None), Arc::new(MemoryStore::default()));
    actor.lane(KEY, SID).unwrap();
    let generation = actor.sessions.slots[SID].lane.as_ref().unwrap().1;
    let viewed = LaneEvent::Viewed {
        view: Box::new(turns(1)),
        origin: origin(),
    };
    actor.on_lane(SID, generation, viewed);
    let call = ModelCall::new(
        MessageId::new("msg_req"),
        QuerySource::MainThread,
        rust_cli_kernel_api::testing::selection(),
        10,
        20,
        CallStatus::Completed,
    );
    let metered = SequencedEvent {
        seq: 9,
        event: KernelEvent::ModelCalled { call },
        revision: 0,
    };
    actor.on_lane(SID, generation, LaneEvent::Kernel(Box::new(metered)));
    let debug = actor
        .session_query(
            &json!(1),
            SessionQuery::Debug,
            runtime_envelope(SESSION_DEBUG, json!({}), None),
        )
        .unwrap()
        .unwrap();
    let entries = ok_data(&debug)["networkEntries"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["requestId"], json!("msg_req"));
    // 视图的 trace 是会话 id（测试的视图）
    assert_eq!(entries[0]["traceId"], json!(SID));
    let cursor: Cursor = serde_json::from_value(debug["cursor"].clone()).unwrap();
    let pending = actor
        .session_query(
            &json!(2),
            SessionQuery::BashOutput,
            runtime_envelope(
                WORK_BASH_OUTPUT,
                json!({"workId": "b1"}),
                Some(cursor.clone()),
            ),
        )
        .unwrap();
    assert_eq!(pending, None);
    assert_eq!(actor.sessions.reads, 1);
    // 通道的读带会话 id 回来（在途的读挡着热会话的回收，protocol-v5.md §69.3）
    let Some(crate::server::Event::RuntimeRead { sid, reply, result }) = inbox.recv().await else {
        panic!("no session read came back");
    };
    assert_eq!(sid, SID);
    assert_eq!(reply, json!(2));
    let result = result.unwrap();
    assert_eq!(
        ok_data(&result),
        &json!({"kind": "unavailable", "workId": "b1"})
    );
    assert_eq!(result["cursor"], json!(cursor));
    let other = Cursor {
        epoch: "other".to_owned(),
        seq: 1,
    };
    let stale = actor
        .session_query(
            &json!(3),
            SessionQuery::Subagents,
            runtime_envelope(SESSION_SUBAGENTS, json!({}), Some(other)),
        )
        .unwrap()
        .unwrap();
    assert_eq!(stale["kind"], json!("stale"));
    assert_eq!(actor.sessions.reads, 1);
}

/// 没有物化的会话：三条都经读视图的任务给冷的答案（照 Node 不恢复冷会话），带临时 epoch；参数照契约校验。
#[tokio::test]
async fn runtime_reads_of_a_session_without_a_topic_are_cold() {
    let mut h = Harness::new();
    h.handle.ready(
        local(turns(1), None),
        Arc::new(MemoryStore::default()),
        Arc::new(MemoryArtifacts::default()),
    );
    h.send(initialize()).await;
    h.next().await;
    let mut frames = Vec::new();
    h.send(query_message(
        2,
        WORK_BASH_OUTPUT,
        json!({"workId": "b1"}),
        None,
    ))
    .await;
    let output = h.reply(2, &mut frames).await;
    assert_eq!(
        ok_data(&output["result"]),
        &json!({"kind": "unavailable", "workId": "b1"})
    );
    let epoch = output["result"]["cursor"]["epoch"].as_str().unwrap();
    assert!(epoch.contains(READ_EPOCH), "{epoch}");
    h.send(query_message(
        3,
        SESSION_SUBAGENTS,
        json!({"endedLimit": 5}),
        None,
    ))
    .await;
    let directory = h.reply(3, &mut frames).await;
    assert_eq!(ok_data(&directory["result"])["ended"]["total"], json!(0));
    h.send(query_message(4, SESSION_DEBUG, json!({}), None))
        .await;
    let debug = h.reply(4, &mut frames).await;
    assert_eq!(
        ok_data(&debug["result"]),
        &json!({"sessionId": SID, "rounds": [], "networkEntries": [], "cache": null})
    );
    h.send(query_message(5, WORK_BASH_OUTPUT, json!({}), None))
        .await;
    let invalid = h.reply(5, &mut frames).await;
    assert_eq!(
        invalid["error"]["data"]["code"],
        json!(codes::PRECONDITION_INVALID_PAYLOAD)
    );
    h.send(query_message(
        6,
        SESSION_SUBAGENTS,
        json!({"endedLimit": 101}),
        None,
    ))
    .await;
    let invalid = h.reply(6, &mut frames).await;
    assert_eq!(
        invalid["error"]["data"]["code"],
        json!(codes::PRECONDITION_INVALID_PAYLOAD)
    );
}

// 文件改动的明细（§44）放在分出的文件里（800 行上限）。
#[path = "queries_files_tests.rs"]
mod files;
