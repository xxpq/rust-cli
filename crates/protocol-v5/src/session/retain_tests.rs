//! 冷会话的投影按最近使用保留、子代理清单只扫一遍（protocol-v5.md §70）：真实的 actor 与 Content-Length 分帧，测试扮演 Host（订阅、退订、
//! 命令、查询）。内核是记下冷读次数的替身；存储是 `MemoryStore`（会话都落了库，有会话事实）外面套一层数调用的代理：库里的子代理读了几次、
//! 会话事实读了几次。测试直接改 `MemoryStore` 就是「别的进程写了库」。
//!
//! 「保留着、复用了」从外面看：重订不回 pending、首帧的 epoch 与以前相同、冷读次数不变；「重建了」：回 pending、冷读加一、epoch 带 `:session:<n>`。

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use rust_cli_base::TaskScope;
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{AgentId, EnvironmentId, MessageId, SessionId, ToolUseId, TraceId};
use rust_cli_kernel_api::message::{
    AssistantMessage, ContentBlock, Message, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::{
    Ack, CommandEnvelope, CreateSession, Kernel, KernelError, SequencedEvent, SessionHandle,
    SessionView, StartSource,
};
use rust_cli_kernel_api::session_preferences::OpenStart;
use rust_cli_kernel_api::store::{
    Fact, FactBatch, RevertCursor, SessionFilter, SessionKind, SessionMeta, SessionStore,
    SessionSummary, Store, StoreError, StoreQueries,
};
use rust_cli_kernel_api::testing::{
    HandleOverride, MemoryArtifacts, MemoryStore, StoreCall, StoreHooks, StoreProxy,
};
use rust_cli_protocol_core::connection::{self, Options};
use rust_cli_protocol_core::framing::{Dialect, Framing};
use serde_json::{Value, json};
use tokio::io::duplex;
use tokio::sync::mpsc;

use crate::consts::RETAINED_SESSIONS_MAX;
use crate::generated::contract::CONTRACT_DIGEST;
use crate::server::{Actor, Settings, WorkspaceKernel, Workspaces};
use crate::session::lane::LaneEvent;
use crate::session::projection::kit;
use crate::session::reads;
use crate::session::retain::Stamp;
use crate::test_link::Harness;
use crate::topics::session_topic;

const ROOT: &str = "/Users/dev/app";
const RENAME: &str = "019a5f3c-7a2b-7c3d-8e4f-0000000000f1";
const ACTIVATE: &str = "019a5f3c-7a2b-7c3d-8e4f-0000000000f2";
/// 重建的 topic 的 epoch 里的一段（§66.0 第 2 条）。
const REBUILT_EPOCH: &str = ":session:1";
/// 界面查已结束子代理的总数时的一页（`SUBAGENT_TOTAL_PAGE`）。
const ENDED_LIMIT: usize = 20;
/// 父会话的子代理的子会话（`sess_subagent_<agentId>`）与它最后的回答。
const CHILD: &str = "sess_subagent_agent_1";
const CHILD_ANSWER: &str = "found it";

fn sid(n: usize) -> String {
    format!("sess_{n}")
}

fn workspace() -> WorkspaceRef {
    WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new(ROOT),
    }
}

fn meta(id: &str, kind: SessionKind, parent: Option<&str>) -> SessionMeta {
    SessionMeta {
        id: SessionId::new(id),
        kind,
        parent: parent.map(SessionId::new),
        workspace: workspace(),
        title: String::new(),
        trace: TraceId::new(id),
        created_at_ms: 1,
    }
}

fn input(id: &str) -> Message {
    Message::User(UserMessage {
        id: MessageId::new(id),
        content: vec![ContentBlock::Text {
            text: "hello".to_owned(),
        }],
        origin: UserOrigin::Human,
    })
}

fn batch(facts: Vec<Fact>) -> FactBatch {
    FactBatch {
        command: None,
        facts,
        turn: None,
    }
}

/// 冷读给出的视图：一条输入（与库里的内容无关，投影只看视图）。
fn view(session: &SessionId) -> SessionView {
    SessionView::new(
        meta(session.as_str(), SessionKind::Interactive, None),
        vec![input(&format!("msg_{}", session.as_str()))],
        rust_cli_kernel_api::testing::selection(),
        PermissionMode::Default,
    )
}

/// 存储调用的计数：库里的子代理、会话事实。
#[derive(Default)]
struct Counts {
    subagents: AtomicUsize,
    facts: AtomicUsize,
}

#[async_trait::async_trait]
impl StoreHooks for Counts {
    async fn before(&self, call: StoreCall<'_>) -> Result<(), StoreError> {
        match call {
            StoreCall::Subagents(_) => self.subagents.fetch_add(1, Ordering::SeqCst),
            StoreCall::SessionFacts(_) => self.facts.fetch_add(1, Ordering::SeqCst),
            _ => 0,
        };
        Ok(())
    }
}

/// 打开着的会话：命令都接受；事件流不结束。
struct Open(SessionId);

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

    fn subscribe(&self, _after: Option<u64>) -> BoxStream<'static, SequencedEvent> {
        futures_util::stream::pending().boxed()
    }

    async fn view(&self) -> Result<SessionView, KernelError> {
        Ok(view(&self.0))
    }
}

/// 内核替身：冷读计数；改名、删除只改库（同 node-store）。
struct Shelf {
    store: Arc<MemoryStore>,
    queries: Arc<StoreProxy<Arc<Counts>>>,
    counts: Arc<Counts>,
    snapshots: AtomicUsize,
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
        Ok(Arc::new(Open(session.clone())))
    }

    async fn open_session_with(
        &self,
        session: &SessionId,
        _source: StartSource,
        _start: OpenStart,
    ) -> Result<Arc<dyn SessionHandle>, KernelError> {
        Ok(Arc::new(Open(session.clone())))
    }

    async fn list_sessions(
        &self,
        _filter: SessionFilter,
    ) -> Result<Vec<SessionSummary>, KernelError> {
        Ok(Vec::new())
    }

    fn queries(&self) -> Arc<dyn StoreQueries> {
        self.queries.clone()
    }

    async fn rename_session(&self, session: &SessionId, title: String) -> Result<(), KernelError> {
        self.store.rename(session, &title).await.map_err(Into::into)
    }

    async fn delete_session(&self, session: &SessionId) -> Result<(), KernelError> {
        self.store.delete(session).await.map_err(Into::into)
    }

    async fn session_snapshot(&self, session: &SessionId) -> Result<SessionView, KernelError> {
        self.snapshots.fetch_add(1, Ordering::SeqCst);
        Ok(view(session))
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

/// 库里有 `n` 个落了库的会话；第 0 个派生过一个子代理（`SubagentLinked`，子会话最后有回答）。
async fn shelf(n: usize) -> Arc<Shelf> {
    let store = Arc::new(MemoryStore::default());
    for index in 0..n {
        let id = sid(index);
        let mut facts = vec![Fact::MessageAppended {
            message: input(&format!("msg_{id}")),
        }];
        if index == 0 {
            facts.push(Fact::SubagentLinked {
                tool_use: ToolUseId::new("toolu_1"),
                agent: AgentId::new("agent_1"),
                child: SessionId::new(CHILD),
                agent_type: "Explore".to_owned(),
            });
        }
        let parent = meta(&id, SessionKind::Interactive, None);
        store.create(parent, batch(facts)).await.unwrap();
    }
    let answer = Message::Assistant(AssistantMessage {
        id: MessageId::new("msg_child_answer"),
        content: vec![ContentBlock::Text {
            text: CHILD_ANSWER.to_owned(),
        }],
        model: "m".to_owned(),
        stop_reason: None,
        usage: None,
    });
    let child = meta(CHILD, SessionKind::Subagent, Some(&sid(0)));
    store
        .create(
            child,
            batch(vec![Fact::MessageAppended { message: answer }]),
        )
        .await
        .unwrap();
    let counts = Arc::new(Counts::default());
    let inner: Arc<dyn Store> = store.clone();
    let queries = Arc::new(StoreProxy::new(inner, counts.clone()));
    Arc::new(Shelf {
        store,
        queries,
        counts,
        snapshots: AtomicUsize::new(0),
    })
}

/// 一条链路：帧（还信用、按 sub 收好）与回复分开收。
struct Link {
    h: Harness,
    shelf: Arc<Shelf>,
    counts: Arc<Counts>,
    frames: Vec<Value>,
    replies: Vec<Value>,
    next: u64,
}

impl Link {
    async fn new(shelf: Arc<Shelf>) -> Self {
        let mut h = Harness::new();
        let counts = shelf.counts.clone();
        h.handle.ready(
            Arc::new(Local(shelf.clone())),
            shelf.queries.clone(),
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
            counts,
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
        self.next += 1;
        let id = self.next;
        self.h
            .send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await;
        loop {
            if let Some(index) = self.replies.iter().position(|reply| reply["id"] == id) {
                return self.replies.remove(index);
            }
            self.pump().await;
        }
    }

    /// 这个 sub 的下一帧。
    async fn frame(&mut self, sub: &str) -> Value {
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

    async fn subscribe(&mut self, sub: &str, session: &str, cursor: Option<Value>) -> Value {
        let mut params = json!({"sub": sub, "topic": session_topic(ROOT, session)});
        if let Some(cursor) = cursor {
            params["cursor"] = cursor;
        }
        self.request("subscribe", params).await
    }

    async fn unsubscribe(&mut self, sub: &str) {
        self.request("unsubscribe", json!({"sub": sub})).await;
    }

    /// 订阅、拿首帧、退订：这个会话看过一次（首帧的 epoch 与 cursor）。
    async fn visit(&mut self, sub: &str, session: &str) -> Value {
        self.subscribe(sub, session, None).await;
        let frame = self.frame(sub).await;
        self.unsubscribe(sub).await;
        json!({"epoch": frame["epoch"], "seq": frame["to"]})
    }

    async fn subagents(&mut self, session: &str) -> Value {
        let reply = self
            .request(
                "query",
                json!({"type": "session.subagents",
                       "target": {"hostId": "local", "workspaceKey": ROOT, "sessionId": session},
                       "params": {"endedLimit": ENDED_LIMIT}}),
            )
            .await;
        reply["result"]["data"].clone()
    }

    async fn command(&mut self, id: &str, kind: &str, session: &str, payload: Value) -> Value {
        self.request(
            "command",
            json!({"commandId": id, "type": kind,
                   "target": {"hostId": "local", "workspaceKey": ROOT, "sessionId": session},
                   "payload": payload}),
        )
        .await
    }

    fn snapshots(&self) -> usize {
        self.shelf.snapshots.load(Ordering::SeqCst)
    }

    fn subagent_reads(&self) -> usize {
        self.counts.subagents.load(Ordering::SeqCst)
    }
}

/// 没有会话运行时的目录：运行时的表为空 + 库里的那份（以前经通道、没有 handle 时的答案）。
async fn expected_directory(shelf: &Shelf) -> Value {
    let stored = shelf
        .store
        .subagents(&SessionId::new(sid(0)))
        .await
        .unwrap();
    reads::subagents(Vec::new(), stored, None, ENDED_LIMIT).unwrap()
}

/// 一次冷切换里库里的子代理只读一次（§70.0 第 1 条）：冷读读一次，之后界面的 `session.subagents` 按投影里的那份答，答案与以前逐项相同，
/// 再查也不读库。旧代码在这里读两次，之后每查一次再读一次。
#[tokio::test]
async fn a_cold_switch_reads_the_stored_subagents_once() {
    let shelf = shelf(1).await;
    let expected = expected_directory(&shelf).await;
    assert_eq!(expected["ended"]["total"], 1, "{expected}");
    assert_eq!(expected["ended"]["items"][0]["summary"], CHILD_ANSWER);
    let mut link = Link::new(shelf).await;
    link.subscribe("s", &sid(0), None).await;
    link.frame("s").await;
    assert_eq!(link.subagents(&sid(0)).await, expected);
    assert_eq!(link.subagent_reads(), 1);
    assert_eq!(link.subagents(&sid(0)).await, expected);
    assert_eq!(link.subagent_reads(), 1);
}

/// 没有物化的会话（临时投影，§40.2）：一次查询一次冷读、一次子代理的读（旧代码两次）。
#[tokio::test]
async fn a_query_on_an_unmaterialized_session_reads_the_stored_subagents_once() {
    let shelf = shelf(1).await;
    let expected = expected_directory(&shelf).await;
    let mut link = Link::new(shelf).await;
    assert_eq!(link.subagents(&sid(0)).await, expected);
    assert_eq!((link.snapshots(), link.subagent_reads()), (1, 1));
}

/// 命令经过通道之后（这里是改名）投影不带库里的事实：`session.subagents` 照旧经通道读库；退订之后释放，不进保留。
#[tokio::test]
async fn a_session_touched_by_a_command_reads_the_store_and_is_released() {
    let shelf = shelf(1).await;
    let expected = expected_directory(&shelf).await;
    let mut link = Link::new(shelf).await;
    link.subscribe("s", &sid(0), None).await;
    link.frame("s").await;
    let ack = link
        .command(
            RENAME,
            "session.rename",
            &sid(0),
            json!({"title": "renamed"}),
        )
        .await;
    assert_eq!(ack["result"]["kind"], "accepted", "{ack}");
    assert_eq!(link.subagents(&sid(0)).await, expected);
    assert_eq!(link.subagent_reads(), 2);
    link.unsubscribe("s").await;
    let again = link.subscribe("again", &sid(0), None).await;
    assert_eq!(again["result"]["pending"], "source_unavailable", "{again}");
    let frame = link.frame("again").await;
    assert!(
        frame["epoch"].as_str().unwrap().ends_with(REBUILT_EPOCH),
        "{frame}"
    );
}

/// 退订之后在上限内重订（§70.0 第 2 条）：不回 pending、不冷读，首帧是同一 epoch 的快照；带同一 epoch cursor 的重订什么都不缺，
/// 不发帧、`subscribed` 带原来的 cursor。
#[tokio::test]
async fn a_kept_session_comes_back_with_the_same_epoch_and_no_cold_read() {
    let mut link = Link::new(shelf(1).await).await;
    let cursor = link.visit("s", &sid(0)).await;
    assert_eq!(link.snapshots(), 1);
    let again = link.subscribe("again", &sid(0), None).await;
    assert!(again["result"].get("pending").is_none(), "{again}");
    let frame = link.frame("again").await;
    assert_eq!(frame["body"]["kind"], "snapshot");
    assert_eq!(frame["epoch"], cursor["epoch"]);
    assert_eq!(link.snapshots(), 1);
    link.unsubscribe("again").await;
    let resumed = link
        .subscribe("resumed", &sid(0), Some(cursor.clone()))
        .await;
    assert_eq!(resumed["result"]["cursor"], cursor, "{resumed}");
    link.request("ping", json!({})).await;
    assert!(
        !link
            .frames
            .iter()
            .any(|frame| frame["params"]["sub"] == "resumed"),
        "{:?}",
        link.frames
    );
    assert_eq!(link.snapshots(), 1);
}

/// 保留的超过上限（§70.0 第 2 条）：依次看过 9 个会话，最早的那个被释放（重订时冷读、新 epoch），其余 8 个复用。
#[tokio::test]
async fn the_oldest_kept_session_is_released_past_the_cap() {
    let sessions = RETAINED_SESSIONS_MAX + 1;
    let mut link = Link::new(shelf(sessions).await).await;
    let mut epochs = Vec::new();
    for n in 0..sessions {
        epochs.push(link.visit(&format!("s{n}"), &sid(n)).await["epoch"].clone());
    }
    assert_eq!(link.snapshots(), sessions);
    for (n, epoch) in epochs.iter().enumerate().skip(1) {
        let again = link.subscribe(&format!("again{n}"), &sid(n), None).await;
        assert!(again["result"].get("pending").is_none(), "{n}: {again}");
        assert_eq!(&link.frame(&format!("again{n}")).await["epoch"], epoch);
    }
    assert_eq!(link.snapshots(), sessions);
    let first = link.subscribe("again0", &sid(0), None).await;
    assert_eq!(first["result"]["pending"], "source_unavailable", "{first}");
    let frame = link.frame("again0").await;
    assert!(
        frame["epoch"].as_str().unwrap().ends_with(REBUILT_EPOCH),
        "{frame}"
    );
    assert_eq!(link.snapshots(), sessions + 1);
}

/// 保留期间别的进程写了这个会话（库里的活动时间推进、或改了名）：重订时事实不同，回 pending、冷读、换 epoch（§70.3）。
#[tokio::test]
async fn a_kept_session_changed_in_the_store_is_rebuilt() {
    let shelf = shelf(2).await;
    let mut link = Link::new(shelf.clone()).await;
    let first = link.visit("a", &sid(0)).await;
    let second = link.visit("b", &sid(1)).await;
    shelf.store.set_updated(&SessionId::new(sid(0)), 2);
    shelf
        .store
        .rename(&SessionId::new(sid(1)), "renamed elsewhere")
        .await
        .unwrap();
    for (sub, session, before) in [("a2", sid(0), first), ("b2", sid(1), second)] {
        let again = link.subscribe(sub, &session, None).await;
        assert_eq!(again["result"]["pending"], "source_unavailable", "{again}");
        let frame = link.frame(sub).await;
        assert_ne!(frame["epoch"], before["epoch"]);
        assert!(
            frame["epoch"].as_str().unwrap().ends_with(REBUILT_EPOCH),
            "{frame}"
        );
    }
    assert_eq!(link.snapshots(), 4);
}

/// 保留期间别的进程只写了回退游标（回退之后还没发新消息）：两边写游标都不推进活动时间，会话事实不变；库里的分支切点变了，
/// 重订时照样重建、换 epoch（§70.4）。只比会话事实时这里复用旧投影，显示回退之前的历史。
#[tokio::test]
async fn a_kept_session_rewound_elsewhere_is_rebuilt() {
    let shelf = shelf(1).await;
    let mut link = Link::new(shelf.clone()).await;
    let before = link.visit("s", &sid(0)).await;
    let session = SessionId::new(sid(0));
    let facts = shelf
        .store
        .session_facts(std::slice::from_ref(&session))
        .await
        .unwrap();
    let cursor = RevertCursor {
        target: MessageId::new(format!("msg_{}", sid(0))),
        kept: Vec::new(),
        cut_after: MessageId::new(format!("msg_{}", sid(0))),
        generation: 1,
    };
    shelf
        .store
        .commit(&session, batch(vec![Fact::RevertSet { cursor }]))
        .await
        .unwrap();
    assert_eq!(
        shelf
            .store
            .session_facts(std::slice::from_ref(&session))
            .await
            .unwrap(),
        facts,
        "the branch cut alone does not change the session facts"
    );
    let again = link.subscribe("again", &sid(0), None).await;
    assert_eq!(again["result"]["pending"], "source_unavailable", "{again}");
    let frame = link.frame("again").await;
    assert_ne!(frame["epoch"], before["epoch"]);
    assert!(
        frame["epoch"].as_str().unwrap().ends_with(REBUILT_EPOCH),
        "{frame}"
    );
    assert_eq!(link.snapshots(), 2);
}

/// 保留期间别的进程删了这个会话：重订回 pending，再收 `subscription.failed{admission.sessionDeleted}`（同冷读读出删除）。
#[tokio::test]
async fn a_kept_session_deleted_elsewhere_fails_the_subscription() {
    let shelf = shelf(1).await;
    let mut link = Link::new(shelf.clone()).await;
    link.visit("s", &sid(0)).await;
    shelf.store.delete(&SessionId::new(sid(0))).await.unwrap();
    let again = link.subscribe("again", &sid(0), None).await;
    assert_eq!(again["result"]["pending"], "source_unavailable", "{again}");
    let failed = loop {
        if let Some(index) = link.replies.iter().position(|message| {
            message["method"] == "subscription.failed" && message["params"]["sub"] == "again"
        }) {
            break link.replies.remove(index);
        }
        link.pump().await;
    };
    assert_eq!(
        failed["params"]["error"]["code"], "admission.sessionDeleted",
        "{failed}"
    );
    assert_eq!(link.snapshots(), 1);
}

/// 保留中的会话被本进程激活（这里是要运行时的命令）：丢掉保留的，按活视图建、换 epoch；之后它是热的，退订也不释放、不进保留。
#[tokio::test]
async fn an_activated_kept_session_drops_what_was_kept() {
    let mut link = Link::new(shelf(1).await).await;
    let cursor = link.visit("s", &sid(0)).await;
    let ack = link
        .command(
            ACTIVATE,
            "queue.setAutoDrain",
            &sid(0),
            json!({"autoDrain": true}),
        )
        .await;
    assert_eq!(ack["result"]["kind"], "accepted", "{ack}");
    let again = link.subscribe("again", &sid(0), None).await;
    assert!(again["result"].get("pending").is_none(), "{again}");
    let frame = link.frame("again").await;
    assert_ne!(frame["epoch"], cursor["epoch"]);
    assert!(
        frame["epoch"].as_str().unwrap().ends_with(REBUILT_EPOCH),
        "{frame}"
    );
    assert_eq!(
        frame["body"]["snapshot"]["state"]["control"]["runtime"],
        "ready"
    );
    assert_eq!(link.snapshots(), 1);
}

/// actor 级：本进程删除保留中的会话，保留的那份丢掉（释放次数加一）；挤出时被挤的那个同样释放，别的不动。
#[tokio::test]
async fn deleting_or_evicting_a_kept_session_drops_it() {
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
    let shelf = shelf(RETAINED_SESSIONS_MAX + 1).await;
    actor.process_ready(Arc::new(Local(shelf.clone())), shelf.queries.clone());
    let facts = shelf
        .store
        .session_facts(&[SessionId::new(sid(0))])
        .await
        .unwrap();
    let kept = |actor: &Actor| {
        actor
            .sessions
            .slots
            .values()
            .filter(|slot| slot.retention.kept.is_some())
            .count()
    };
    for n in 0..=RETAINED_SESSIONS_MAX {
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
            origin: crate::session::projection::Origin {
                stamp: facts
                    .first()
                    .cloned()
                    .map(|facts| Stamp { facts, cut: None }),
                ..kit::origin(false)
            },
        };
        actor.on_lane(&sid(n), generation, viewed);
        actor.unsubscribe(json!({"sub": format!("s{n}")})).unwrap();
    }
    assert_eq!(kept(&actor), RETAINED_SESSIONS_MAX);
    assert!(actor.sessions.slots[&sid(0)].retention.kept.is_none());
    assert_eq!(actor.sessions.slots[&sid(0)].releases, 1);
    actor.mark_deleted(&sid(1));
    assert!(actor.sessions.slots[&sid(1)].retention.kept.is_none());
    assert_eq!(actor.sessions.slots[&sid(1)].releases, 1);
    assert_eq!(kept(&actor), RETAINED_SESSIONS_MAX - 1);
    assert!(
        (0..=RETAINED_SESSIONS_MAX)
            .all(|n| !actor.topics.contains_key(&session_topic(ROOT, &sid(n))))
    );
}
