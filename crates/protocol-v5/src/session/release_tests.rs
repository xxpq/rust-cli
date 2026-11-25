//! 会话投影的释放与重建（protocol-v5.md §66；02 §8.2）：真实的 actor 与 Content-Length 分帧，测试扮演 Host（订阅、退订、发命令、查询）。
//! 内核是记下冷读次数的替身：冷读给每个会话一份视图，打开给一个事件流不结束的会话；改名与冷读可以各挂一个闸门（在途的命令、在读的视图）。
//!
//! 同一套链路另测 actor 的写出时机（protocol-v5.md §68.2）：通道一次转来的一串内核事件写成一帧。
//!
//! 「topic 还在不在」从外面看：物化了的会话，`rows.range` 在同一步按 topic 答（cursor 是 topic 的）；没有物化的走临时投影（epoch 带 `:read:`，§40.2）。

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use rust_cli_base::TaskScope;
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{EnvironmentId, MessageId, SessionId, TraceId};
use rust_cli_kernel_api::message::{ContentBlock, Message, UserMessage, UserOrigin};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::{
    Ack, CommandEnvelope, CreateSession, Kernel, KernelError, KernelEvent, SequencedEvent,
    SessionHandle, SessionView, StartSource,
};
use rust_cli_kernel_api::session_preferences::OpenStart;
use rust_cli_kernel_api::store::{
    Fact, FactBatch, SessionFilter, SessionKind, SessionMeta, SessionStore, SessionSummary,
    StoreError, StoreQueries,
};
use rust_cli_kernel_api::testing::{
    HandleOverride, MemoryArtifacts, MemoryStore, StoreCall, StoreHooks, StoreProxy,
};
use rust_cli_protocol_core::connection::{self, Options};
use rust_cli_protocol_core::framing::{Dialect, Framing};
use serde_json::{Value, json};
use tokio::io::duplex;
use tokio::sync::{Notify, mpsc};

use crate::generated::contract::CONTRACT_DIGEST;
use crate::server::{Actor, Settings, WorkspaceKernel, Workspaces};
use crate::session::lane::LaneEvent;
use crate::session::projection::{Origin, Projection, kit};
use crate::session::rows;
use crate::test_link::Harness;
use crate::topics::session_topic;
use kit::long_view;

const ROOT: &str = "/Users/dev/app";
/// 打开、退订的会话数。
const SESSIONS: usize = 3;
const RENAME: &str = "019a5f3c-7a2b-7c3d-8e4f-0000000000e1";
const ACTIVATE: &str = "019a5f3c-7a2b-7c3d-8e4f-0000000000e2";
/// 临时投影的 epoch 里的一段（§40.2）与重建的 topic 的 epoch 里的一段（§66.0 第 2 条）。
const READ_EPOCH: &str = ":read:";
const REBUILT_EPOCH: &str = ":session:1";
/// 突发的内核事件数：大于连接的初始信用（以前每条事件一帧，用完信用才折叠）。
const BURST: u64 = 3 * crate::consts::INITIAL_CREDIT;

fn sid(n: usize) -> String {
    format!("sess_{n}")
}

fn workspace() -> WorkspaceRef {
    WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new(ROOT),
    }
}

/// 一个会话的视图：一条输入（冷读与打开之后同一份）。
fn view(session: &SessionId) -> SessionView {
    let meta = SessionMeta {
        id: session.clone(),
        kind: SessionKind::Interactive,
        parent: None,
        workspace: workspace(),
        title: String::new(),
        trace: TraceId::new(session.as_str()),
        created_at_ms: 1,
    };
    let input = Message::User(UserMessage {
        id: MessageId::new(format!("msg_{}", session.as_str())),
        content: vec![ContentBlock::Text {
            text: "hello".to_owned(),
        }],
        origin: UserOrigin::Human,
    });
    SessionView::new(
        meta,
        vec![input],
        rust_cli_kernel_api::testing::selection(),
        PermissionMode::Default,
    )
}

/// 打开着的会话：命令都接受；事件流先给出 seq 在订阅位置之后的那几条（`Shelf::burst`），之后不结束；视图是长会话的（给了时）或一条输入的。
struct Open(SessionId, Vec<SequencedEvent>, Option<SessionView>);

#[async_trait::async_trait]
impl HandleOverride for Open {
    fn id(&self) -> &SessionId {
        &self.0
    }

    async fn send(&self, _command: CommandEnvelope) -> Ack {
        Ack::Accepted {
            result: Default::default(),
        }
    }

    fn subscribe(&self, after: Option<u64>) -> BoxStream<'static, SequencedEvent> {
        let after = after.unwrap_or(0);
        let ready: Vec<SequencedEvent> = self
            .1
            .iter()
            .filter(|event| event.seq > after)
            .cloned()
            .collect();
        futures_util::stream::iter(ready)
            .chain(futures_util::stream::pending())
            .boxed()
    }

    async fn view(&self) -> Result<SessionView, KernelError> {
        Ok(self.2.clone().unwrap_or_else(|| view(&self.0)))
    }
}

/// 内核替身：冷读计数；改名与冷读各自可以挂一个闸门（放开之前不回）；`long` 给出时冷读都给它（长会话，§67）；
/// `burst` 是打开的会话订事件时已经在流里的事件。
#[derive(Default)]
struct Shelf {
    store: Arc<MemoryStore>,
    snapshots: AtomicUsize,
    rename_gate: Option<Arc<Notify>>,
    snapshot_gate: Option<Arc<Notify>>,
    long: Option<SessionView>,
    burst: Vec<SequencedEvent>,
    /// 冷读只读最后一段（§73.2）：`session_tail` 从 `store`（已写进长会话）按段读，同 runtime 的组法。
    tail: bool,
}

#[async_trait::async_trait]
impl Kernel for Shelf {
    async fn create_session(&self, _request: CreateSession) -> Result<SessionId, KernelError> {
        Err(KernelError::Unsupported {
            what: "creating sessions".to_owned(),
        })
    }

    async fn open_session(
        &self,
        session: &SessionId,
    ) -> Result<Arc<dyn SessionHandle>, KernelError> {
        Ok(Arc::new(Open(
            session.clone(),
            self.burst.clone(),
            self.long.clone(),
        )))
    }

    async fn open_session_with(
        &self,
        session: &SessionId,
        _source: StartSource,
        _start: OpenStart,
    ) -> Result<Arc<dyn SessionHandle>, KernelError> {
        Ok(Arc::new(Open(
            session.clone(),
            self.burst.clone(),
            self.long.clone(),
        )))
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

    async fn rename_session(
        &self,
        _session: &SessionId,
        _title: String,
    ) -> Result<(), KernelError> {
        if let Some(gate) = &self.rename_gate {
            gate.notified().await;
        }
        Ok(())
    }

    async fn session_snapshot(&self, session: &SessionId) -> Result<SessionView, KernelError> {
        if let Some(gate) = &self.snapshot_gate {
            gate.notified().await;
        }
        self.snapshots.fetch_add(1, Ordering::SeqCst);
        Ok(self.long.clone().unwrap_or_else(|| view(session)))
    }

    async fn session_tail(
        &self,
        session: &SessionId,
        records: usize,
    ) -> Result<rust_cli_kernel_api::session_api::SessionTail, KernelError> {
        if !self.tail {
            return Ok(rust_cli_kernel_api::session_api::SessionTail {
                view: self.session_snapshot(session).await?,
                subagents: Vec::new(),
                earlier: false,
            });
        }
        let span = rust_cli_kernel_api::store::HistorySpan::Before {
            floor: None,
            records,
        };
        let range = self.store.range(session, span).await?;
        let loaded = range.loaded;
        let view = SessionView {
            turns: loaded.turns,
            inputs: loaded.inputs,
            sources: loaded.sources,
            output_heads: loaded.output_heads,
            tool_times: loaded.tool_times,
            plans: loaded.plans,
            ..SessionView::new(
                loaded.meta,
                loaded.earlier.into_iter().chain(loaded.messages).collect(),
                rust_cli_kernel_api::testing::selection(),
                PermissionMode::Default,
            )
        };
        Ok(rust_cli_kernel_api::session_api::SessionTail {
            view,
            subagents: range.subagents,
            earlier: range.earlier,
        })
    }
}

struct Local(Arc<Shelf>);

#[async_trait::async_trait]
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

/// 一条链路：帧（还信用、按 sub 收好）与回复分开收。
/// 数按 id 读历史的一段（§71.2）与范围读（§73.2）。
#[derive(Default)]
struct Slices(AtomicUsize, AtomicUsize);

#[async_trait::async_trait]
impl StoreHooks for Slices {
    async fn before(&self, call: StoreCall<'_>) -> Result<(), StoreError> {
        match call {
            StoreCall::History(..) => self.0.fetch_add(1, Ordering::SeqCst),
            StoreCall::Range(..) => self.1.fetch_add(1, Ordering::SeqCst),
            _ => 0,
        };
        Ok(())
    }
}

/// 把长会话的视图写进存储（消息、回合记录、工具的起止）：按片重建从这里读（§71.2）。
async fn seeded(view: &SessionView) -> Arc<MemoryStore> {
    let store = Arc::new(MemoryStore::default());
    let batch = |facts| FactBatch {
        command: None,
        facts,
        turn: None,
    };
    let meta = SessionMeta {
        id: SessionId::new(sid(0)),
        workspace: workspace(),
        ..view.meta.clone()
    };
    store.create(meta, batch(Vec::new())).await.unwrap();
    let messages = view
        .messages
        .iter()
        .cloned()
        .map(|message| Fact::MessageAppended { message });
    let turns = view
        .turns
        .iter()
        .map(rust_cli_kernel_api::store::TurnRecord::fact);
    let times = view.tool_times.iter().map(|time| Fact::ToolCalled {
        call: rust_cli_kernel_api::metering::ToolCall {
            tool_use: time.tool_use.clone(),
            name: rust_cli_kernel_api::tool_names::READ.to_owned(),
            started_at_ms: time.started_at,
            completed_at_ms: time.ended_at,
            status: rust_cli_kernel_api::metering::CallStatus::Completed,
            output_bytes: 0,
        },
    });
    let facts = messages.chain(turns).chain(times).collect();
    store
        .commit(&SessionId::new(sid(0)), batch(facts))
        .await
        .unwrap();
    store
}

struct Link {
    h: Harness,
    shelf: Arc<Shelf>,
    slices: Arc<Slices>,
    frames: Vec<Value>,
    replies: Vec<Value>,
    next: u64,
}

impl Link {
    async fn new(shelf: Shelf) -> Self {
        let shelf = Arc::new(shelf);
        // 冷读按段读时内核与查询读同一个库（测试先把长会话写进 `Shelf::store`）
        let store = match (&shelf.long, shelf.tail) {
            (Some(_), true) => shelf.store.clone(),
            (Some(long), false) => seeded(long).await,
            (None, _) => Arc::new(MemoryStore::default()),
        };
        let slices = Arc::new(Slices::default());
        let mut h = Harness::new();
        h.handle.ready(
            Arc::new(Local(shelf.clone())),
            Arc::new(StoreProxy::new(store, slices.clone())),
            Arc::new(MemoryArtifacts::default()),
        );
        h.send(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                      "params": {"clientId": "host",
                                 "caps": {"core": [1], "agent": [1], "session": [1], "session.queue": [1]},
                                 "contract": CONTRACT_DIGEST}}))
            .await;
        assert!(h.next().await["result"].is_object());
        Self {
            h,
            shelf,
            slices,
            frames: Vec::new(),
            replies: Vec::new(),
            next: 10,
        }
    }

    async fn pump(&mut self) {
        let message = self.h.next().await;
        if message["method"] == "frame" {
            let sub = message["params"]["sub"].clone();
            self.h
                .send(json!({"jsonrpc": "2.0", "method": "credit", "params": {"sub": sub, "n": 1}}))
                .await;
            self.frames.push(message);
        } else {
            self.replies.push(message);
        }
    }

    async fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.send(method, params).await;
        self.reply(id).await
    }

    async fn send(&mut self, method: &str, params: Value) -> u64 {
        self.next += 1;
        let id = self.next;
        self.h
            .send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await;
        id
    }

    async fn reply(&mut self, id: u64) -> Value {
        loop {
            if let Some(index) = self.replies.iter().position(|reply| reply["id"] == id) {
                return self.replies.remove(index);
            }
            self.pump().await;
        }
    }

    /// 这个 sub 的第一帧（快照）。
    async fn snapshot(&mut self, sub: &str) -> Value {
        loop {
            if let Some(index) = self
                .frames
                .iter()
                .position(|frame| frame["params"]["sub"] == sub)
            {
                return self.frames.remove(index)["params"].clone();
            }
            self.pump().await;
        }
    }

    async fn subscribe(&mut self, sub: &str, session: &str) -> Value {
        self.request(
            "subscribe",
            json!({"sub": sub, "topic": session_topic(ROOT, session)}),
        )
        .await
    }

    async fn unsubscribe(&mut self, sub: &str) {
        self.request("unsubscribe", json!({"sub": sub})).await;
    }

    /// `rows.range` 的 cursor 的 epoch：物化了的是 topic 的，没有物化的是临时投影的（带 `:read:`）。
    async fn range_epoch(&mut self, session: &str) -> String {
        let reply = self
            .request(
                "query",
                json!({"type": "rows.range",
                       "target": {"hostId": "local", "workspaceKey": ROOT, "sessionId": session},
                       "params": {"limit": 2}}),
            )
            .await;
        reply["result"]["cursor"]["epoch"]
            .as_str()
            .unwrap_or_else(|| panic!("{reply}"))
            .to_owned()
    }

    fn snapshots(&self) -> usize {
        self.shelf.snapshots.load(Ordering::SeqCst)
    }
}

fn command(id: &str, kind: &str, session: &str, payload: Value) -> Value {
    json!({"commandId": id, "type": kind,
           "target": {"hostId": "local", "workspaceKey": ROOT, "sessionId": session},
           "payload": payload})
}

/// 打开 N 个冷会话再都退订：topic 都注销（之后的查询走临时投影）；再订阅先回 pending、从存储重建、首帧是快照、epoch 换新。
#[tokio::test]
async fn cold_sessions_nobody_watches_are_released_and_rebuilt_from_storage() {
    let mut link = Link::new(Shelf::default()).await;
    let mut epochs = Vec::new();
    for n in 0..SESSIONS {
        let sub = format!("s{n}");
        let subscribed = link.subscribe(&sub, &sid(n)).await;
        assert_eq!(subscribed["result"]["pending"], "source_unavailable");
        let frame = link.snapshot(&sub).await;
        assert_eq!(frame["body"]["kind"], "snapshot");
        epochs.push(frame["epoch"].as_str().unwrap().to_owned());
        // 物化了：查询按 topic 答
        assert_eq!(link.range_epoch(&sid(n)).await, epochs[n]);
    }
    assert_eq!(link.snapshots(), SESSIONS);
    for n in 0..SESSIONS {
        link.unsubscribe(&format!("s{n}")).await;
    }
    for n in 0..SESSIONS {
        let epoch = link.range_epoch(&sid(n)).await;
        assert!(
            epoch.contains(READ_EPOCH),
            "{epoch}: the topic is still there"
        );
    }
    // 每次临时查询读一次库；再订阅从存储重建（再读一次），新 epoch 拿快照
    assert_eq!(link.snapshots(), SESSIONS * 2);
    let again = link.subscribe("again", &sid(0)).await;
    assert_eq!(again["result"]["pending"], "source_unavailable");
    let frame = link.snapshot("again").await;
    assert_eq!(frame["body"]["kind"], "snapshot");
    let rebuilt = frame["epoch"].as_str().unwrap();
    assert_ne!(rebuilt, epochs[0]);
    assert!(rebuilt.ends_with(REBUILT_EPOCH), "{rebuilt}");
    assert_eq!(link.snapshots(), SESSIONS * 2 + 1);
    assert_eq!(link.range_epoch(&sid(0)).await, rebuilt);
}

/// 命令打开过的会话（运行时开着）退订之后不释放：再订阅直接拿帧，不回 pending。
#[tokio::test]
async fn a_session_with_its_runtime_open_stays() {
    let mut link = Link::new(Shelf::default()).await;
    link.subscribe("s", &sid(0)).await;
    link.snapshot("s").await;
    let ack = link
        .request(
            "command",
            command(
                ACTIVATE,
                "queue.setAutoDrain",
                &sid(0),
                json!({"autoDrain": true}),
            ),
        )
        .await;
    assert_eq!(ack["result"]["kind"], "accepted", "{ack}");
    link.unsubscribe("s").await;
    let again = link.subscribe("again", &sid(0)).await;
    assert!(again["result"].get("pending").is_none(), "{again}");
    assert!(!link.range_epoch(&sid(0)).await.contains(READ_EPOCH));
}

/// 冷会话的命令在途时退订：不释放（`Done` 要它的表项与 topic）；命令回来之后释放。
#[tokio::test]
async fn a_command_in_flight_holds_the_projection_until_it_is_done() {
    let gate = Arc::new(Notify::new());
    let mut link = Link::new(Shelf {
        rename_gate: Some(gate.clone()),
        ..Shelf::default()
    })
    .await;
    link.subscribe("s", &sid(0)).await;
    let epoch = link.snapshot("s").await["epoch"]
        .as_str()
        .unwrap()
        .to_owned();
    let renaming = link
        .send(
            "command",
            command(
                RENAME,
                "session.rename",
                &sid(0),
                json!({"title": "renamed"}),
            ),
        )
        .await;
    link.unsubscribe("s").await;
    assert_eq!(link.range_epoch(&sid(0)).await, epoch);
    gate.notify_one();
    let ack = link.reply(renaming).await;
    assert_eq!(ack["result"]["kind"], "accepted", "{ack}");
    let after = link.range_epoch(&sid(0)).await;
    assert!(
        after.contains(READ_EPOCH),
        "{after}: released after the command came back"
    );
}

/// 读视图在途时最后一个订阅走了：视图回来之后不登记 topic。
#[tokio::test]
async fn a_view_read_after_the_last_subscriber_left_registers_nothing() {
    let gate = Arc::new(Notify::new());
    let mut link = Link::new(Shelf {
        snapshot_gate: Some(gate.clone()),
        ..Shelf::default()
    })
    .await;
    link.subscribe("s", &sid(0)).await;
    link.unsubscribe("s").await;
    // 放开在读的那一次；之后的查询再读一次（也要放开）
    gate.notify_one();
    let querying = link
        .send(
            "query",
            json!({"type": "rows.range",
                   "target": {"hostId": "local", "workspaceKey": ROOT, "sessionId": sid(0)},
                   "params": {"limit": 2}}),
        )
        .await;
    gate.notify_one();
    let reply = link.reply(querying).await;
    let epoch = reply["result"]["cursor"]["epoch"].as_str().unwrap();
    assert!(epoch.contains(READ_EPOCH), "{epoch}");
    assert!(link.frames.is_empty(), "{:?}", link.frames);
}

/// 同一个 sub 改订别的会话：旧的会话没人订了，释放。
#[tokio::test]
async fn moving_a_sub_to_another_session_releases_the_old_one() {
    let mut link = Link::new(Shelf::default()).await;
    link.subscribe("s", &sid(0)).await;
    link.snapshot("s").await;
    link.subscribe("s", &sid(1)).await;
    link.snapshot("s").await;
    let old = link.range_epoch(&sid(0)).await;
    assert!(old.contains(READ_EPOCH), "{old}");
    assert!(!link.range_epoch(&sid(1)).await.contains(READ_EPOCH));
}

/// 表项计数（actor 级）：N 个冷会话物化之后都退订，会话 topic 与投影都是 0；会话表只留记工作区的表项。
#[tokio::test]
async fn released_sessions_leave_no_topic_or_projection_behind() {
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
    actor.process_ready(
        Arc::new(Local(Arc::new(Shelf::default()))),
        Arc::new(MemoryStore::default()),
    );
    let topics = |actor: &Actor| {
        (0..SESSIONS)
            .filter(|n| actor.topics.contains_key(&session_topic(ROOT, &sid(*n))))
            .count()
    };
    for n in 0..SESSIONS {
        let subscribe = serde_json::from_value(
            json!({"sub": format!("s{n}"), "topic": session_topic(ROOT, &sid(n))}),
        )
        .unwrap();
        actor
            .subscribe_session(&json!(n), subscribe, ROOT, &sid(n))
            .unwrap();
        let generation = actor.sessions.slots[&sid(n)].lane.as_ref().unwrap().1;
        let viewed = LaneEvent::Viewed {
            view: Box::new(view(&SessionId::new(sid(n)))),
            origin: Origin {
                live: false,
                draft: false,
                deleted: false,
                title_source: rust_cli_kernel_api::session_facts::TitleSource::Default,
                mcp: Value::Null,
                subagents: Vec::new(),
                stamp: None,
                ..Default::default()
            },
        };
        actor.on_lane(&sid(n), generation, viewed);
    }
    assert_eq!(topics(&actor), SESSIONS);
    for n in 0..SESSIONS {
        actor.unsubscribe(json!({"sub": format!("s{n}")})).unwrap();
    }
    assert_eq!(topics(&actor), 0);
    let held = actor
        .sessions
        .slots
        .values()
        .filter(|slot| slot.projection.is_some() || slot.lane.is_some())
        .count();
    assert_eq!(held, 0);
    assert_eq!(actor.sessions.workspace_of(&sid(0)), Some(ROOT));
}

/// 订阅中的会话按窗口驻留（§67）：快照之后从尾到头翻完，每一行与全部整行的投影逐字段相同；碰到骨架的那几页在任务里从存储按片重建（§71.2）：
/// 只读这一页的消息，不再整份冷读。
#[tokio::test]
async fn paging_back_through_outlined_rows_matches_a_whole_projection() {
    const TURNS: usize = 20;
    const OUTPUT_BYTES: usize = 100;
    const PAGE: usize = 50;
    let long = long_view(TURNS, OUTPUT_BYTES);
    let (_, content) = Projection::build(&long, kit::origin(false));
    let mut whole = kit::publisher();
    whole.adopt(content);
    let expected = whole
        .page(rows::ROWS, rows::CREATED_AT_SEQ, None, usize::MAX)
        .unwrap()
        .items;
    let mut link = Link::new(Shelf {
        long: Some(long),
        ..Shelf::default()
    })
    .await;
    link.subscribe("s", &sid(0)).await;
    let frame = link.snapshot("s").await;
    let mut seen: Vec<Value> = frame["body"]["snapshot"]["collections"]["rows"]
        .as_array()
        .unwrap()
        .clone();
    // 快照的尾窗从回合的开头起（§71.1）
    assert!(seen.len() <= crate::consts::SESSION_SNAPSHOT_ROWS);
    assert_eq!(seen[0]["kind"], json!(rows::kinds::TURN_HEADER));
    let mut pages = 0;
    loop {
        pages += 1;
        let reply = link
            .request(
                "query",
                json!({"type": "rows.range",
                       "target": {"hostId": "local", "workspaceKey": ROOT, "sessionId": sid(0)},
                       "params": {"beforeId": seen[0]["id"], "limit": PAGE}}),
            )
            .await;
        let data = &reply["result"]["data"];
        let mut page = data["rows"]
            .as_array()
            .unwrap_or_else(|| panic!("{reply}"))
            .clone();
        page.extend(seen);
        seen = page;
        if data["hasMore"] != json!(true) {
            break;
        }
    }
    assert_eq!(seen, expected);
    // 整份冷读只有订阅那一次（PERF-WINDOW 改：以前碰到骨架的每一页各一次）；碰到骨架的每一页读一次片
    assert_eq!(link.snapshots(), 1);
    assert_eq!(link.slices.0.load(Ordering::SeqCst), pages);
}

/// 这一帧里最后一个 `meta` 的标题（state.set 的 patch）。
fn title_in(frame: &Value) -> Option<String> {
    frame["params"]["body"]["deltas"]
        .as_array()?
        .iter()
        .rev()
        .find_map(|delta| delta["patch"]["meta"]["title"].as_str())
        .map(str::to_owned)
}

/// 一次处理一帧（protocol-v5.md §68.2，01 §19.3，用户定 R2）：打开会话时事件流里已有一串改标题的事件，通道一口气转给 actor；
/// actor 处理完醒来那一刻已到的全部再写出站，订阅只收到一帧、带最后的标题。以前每条事件一帧，信用（初始 4）用完才折叠，
/// 这里会有好几帧带标题。
#[tokio::test]
async fn a_burst_of_kernel_events_goes_out_as_one_frame() {
    let burst = (1..=BURST)
        .map(|n| SequencedEvent {
            seq: n,
            event: KernelEvent::TitleChanged {
                title: format!("title {n}"),
            },
            revision: n,
        })
        .collect();
    let mut link = Link::new(Shelf {
        burst,
        ..Shelf::default()
    })
    .await;
    link.subscribe("s", &sid(0)).await;
    link.snapshot("s").await;
    let ack = link
        .request(
            "command",
            command(
                ACTIVATE,
                "queue.setAutoDrain",
                &sid(0),
                json!({"autoDrain": true}),
            ),
        )
        .await;
    assert_eq!(ack["result"]["kind"], "accepted", "{ack}");
    let last = format!("title {BURST}");
    while !link
        .frames
        .iter()
        .any(|frame| title_in(frame).as_deref() == Some(last.as_str()))
    {
        link.pump().await;
    }
    let titled: Vec<&Value> = link
        .frames
        .iter()
        .filter(|frame| frame["params"]["sub"] == "s" && title_in(frame).is_some())
        .collect();
    assert_eq!(titled.len(), 1, "{titled:?}");
}

// 冷读只读最后一段（protocol-v5.md §73）的同一套链路测试在分出的文件里（1000 行上限）。
#[path = "release_tail_tests.rs"]
mod tail;
