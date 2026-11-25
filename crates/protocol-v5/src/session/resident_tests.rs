//! 热会话按上限回收（protocol-v5.md §69）：真实的 actor 与 Content-Length 分帧，测试扮演 Host（订阅、发命令、查询）。内核是替身：每次打开给一个
//! 新句柄，句柄的事件流由测试喂（回合开始、结束）；`close_idle` 记下关了谁，`busy` 里的会话拒绝（会话 actor 判出不空闲）；关闭与子代理的读
//! 各可以挂一个闸门（关到一半、读到一半）。
//!
//! 激活一个会话 = 发一条要运行时的命令（`queue.setAutoDrain`）：通道打开会话、交来活视图，命令回来之后会话从忙变闲。

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use rust_cli_base::sync::LockUnpoisoned;
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{EnvironmentId, MessageId, SessionId, TraceId, TurnId};
use rust_cli_kernel_api::message::{ContentBlock, Message, UserMessage, UserOrigin};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::{
    Ack, CommandEnvelope, CreateSession, EndReason, KernelError, KernelEvent, SequencedEvent,
    SessionHandle, SessionView, StartSource, TurnOrigin, TurnOutcome, TurnTime,
};
use rust_cli_kernel_api::session_preferences::OpenStart;
use rust_cli_kernel_api::store::{
    SessionFilter, SessionKind, SessionMeta, SessionSummary, StoreQueries,
};
use rust_cli_kernel_api::testing::{HandleOverride, MemoryArtifacts, MemoryStore};
use serde_json::{Value, json};
use tokio::sync::{Notify, mpsc, watch};

use crate::consts::HOT_SESSIONS_MAX;
use crate::generated::contract::CONTRACT_DIGEST;
use crate::server::{WorkspaceKernel, Workspaces};
use crate::test_link::Harness;
use crate::topics::session_topic;

const ROOT: &str = "/Users/dev/app";
/// 临时投影的 epoch 里的一段（§40.2）：topic 释放之后的查询走临时投影。
const READ_EPOCH: &str = ":read:";
const TURN: &str = "turn_1";
/// 等关闭的上限：只决定「等多久算失败」，不参与判定（同 `test_link` 的读上限）。
const WAIT_BUDGET: std::time::Duration = std::time::Duration::from_secs(5);

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

fn turn_started() -> KernelEvent {
    KernelEvent::TurnStarted {
        turn: TurnId::new(TURN),
        command: None,
        origin: TurnOrigin::Input,
        started_at: 10,
        works: Vec::new(),
    }
}

fn turn_finished() -> KernelEvent {
    KernelEvent::TurnFinished {
        turn: TurnId::new(TURN),
        outcome: TurnOutcome::Success,
        usage: None,
        time: TurnTime {
            ended_at: 20,
            active_ms: 10,
        },
        files: Vec::new(),
    }
}

/// 内核替身的可变部分。
#[derive(Default)]
struct Ledger {
    /// 每个会话被打开的次数。
    opens: BTreeMap<String, usize>,
    /// `close_idle` 的调用，按顺序。
    closes: Vec<String>,
    /// 每个打开着的会话的事件入口与下一个 seq。
    feeds: BTreeMap<String, (mpsc::UnboundedSender<SequencedEvent>, u64)>,
}

/// 内核替身。
struct Shelf {
    store: Arc<MemoryStore>,
    ledger: Mutex<Ledger>,
    /// 会话 actor 判出不空闲的会话（`close_idle` 回 false）。
    busy: Mutex<BTreeSet<String>>,
    /// 发命令时先开一个回合的会话（同真实的内核：效果事件先于回执进流，回执回来时回合在跑）。
    turns_on_send: Mutex<BTreeSet<String>>,
    /// `close_idle` 调用过的次数（等「关过 N 次」用）。
    closed: watch::Sender<usize>,
    /// 挂着时 `close_idle` 等它放行（关到一半）。
    close_gate: Option<Arc<Notify>>,
    /// 挂着时子代理的读等它放行（读到一半）。
    read_gate: Option<Arc<Notify>>,
}

impl Shelf {
    /// 往一个打开着的会话的事件流里推一个事件。
    fn push(&self, session: &str, event: KernelEvent) {
        let mut ledger = self.ledger.lock_unpoisoned();
        let (feed, seq) = ledger.feeds.get_mut(session).expect("the session is open");
        *seq += 1;
        let _ = feed.send(SequencedEvent {
            seq: *seq,
            event,
            revision: *seq,
        });
    }

    fn opens(&self, session: &str) -> usize {
        self.ledger
            .lock_unpoisoned()
            .opens
            .get(session)
            .copied()
            .unwrap_or(0)
    }

    fn closes(&self) -> Vec<String> {
        self.ledger.lock_unpoisoned().closes.clone()
    }

    /// 等到 `close_idle` 一共被调用过 `count` 次（等不到算失败）。
    async fn closed(&self, count: usize) {
        let mut closed = self.closed.subscribe();
        tokio::time::timeout(WAIT_BUDGET, closed.wait_for(|closed| *closed >= count))
            .await
            .unwrap_or_else(|_| panic!("expected {count} closes, saw {:?}", self.closes()))
            .unwrap();
    }
}

impl Default for Shelf {
    fn default() -> Self {
        Self {
            store: Arc::default(),
            ledger: Mutex::default(),
            busy: Mutex::default(),
            turns_on_send: Mutex::default(),
            closed: watch::Sender::new(0),
            close_gate: None,
            read_gate: None,
        }
    }
}

/// 打开着的会话：命令都接受；事件流是测试喂的那一条。
struct Open {
    id: SessionId,
    shelf: Arc<Shelf>,
    events: Mutex<Option<mpsc::UnboundedReceiver<SequencedEvent>>>,
}

#[async_trait::async_trait]
impl HandleOverride for Open {
    fn id(&self) -> &SessionId {
        &self.id
    }

    async fn send(&self, _command: CommandEnvelope) -> Ack {
        if self
            .shelf
            .turns_on_send
            .lock_unpoisoned()
            .contains(self.id.as_str())
        {
            self.shelf.push(self.id.as_str(), turn_started());
        }
        Ack::Accepted {
            result: Default::default(),
        }
    }

    fn subscribe(&self, _after: Option<u64>) -> BoxStream<'static, SequencedEvent> {
        match self.events.lock_unpoisoned().take() {
            Some(events) => futures_util::stream::unfold(events, |mut events| async move {
                events.recv().await.map(|event| (event, events))
            })
            .boxed(),
            None => futures_util::stream::pending().boxed(),
        }
    }

    async fn view(&self) -> Result<SessionView, KernelError> {
        Ok(view(&self.id))
    }

    async fn close_idle(&self, _reason: EndReason) -> bool {
        if let Some(gate) = &self.shelf.close_gate {
            gate.notified().await;
        }
        let idle = !self.shelf.busy.lock_unpoisoned().contains(self.id.as_str());
        {
            let mut ledger = self.shelf.ledger.lock_unpoisoned();
            ledger.closes.push(self.id.as_str().to_owned());
            if idle {
                ledger.feeds.remove(self.id.as_str());
            }
        }
        self.shelf.closed.send_modify(|closed| *closed += 1);
        idle
    }

    async fn subagents(
        &self,
    ) -> Result<Vec<rust_cli_kernel_api::panels::SubagentEntry>, KernelError> {
        if let Some(gate) = &self.shelf.read_gate {
            gate.notified().await;
        }
        Ok(Vec::new())
    }
}

/// 内核替身的 `Kernel`：打开给新句柄（记次数），冷读给同一份视图。
struct Opener(Arc<Shelf>);

#[async_trait::async_trait]
impl rust_cli_kernel_api::session_api::Kernel for Opener {
    async fn create_session(&self, _request: CreateSession) -> Result<SessionId, KernelError> {
        Err(KernelError::Unsupported {
            what: "creating sessions".to_owned(),
        })
    }

    async fn open_session(
        &self,
        session: &SessionId,
    ) -> Result<Arc<dyn SessionHandle>, KernelError> {
        self.open_session_with(session, StartSource::Resume, OpenStart::default())
            .await
    }

    async fn open_session_with(
        &self,
        session: &SessionId,
        _source: StartSource,
        _start: OpenStart,
    ) -> Result<Arc<dyn SessionHandle>, KernelError> {
        let (feed, events) = mpsc::unbounded_channel();
        {
            let mut ledger = self.0.ledger.lock_unpoisoned();
            *ledger.opens.entry(session.as_str().to_owned()).or_default() += 1;
            ledger.feeds.insert(session.as_str().to_owned(), (feed, 0));
        }
        Ok(Arc::new(Open {
            id: session.clone(),
            shelf: self.0.clone(),
            events: Mutex::new(Some(events)),
        }))
    }

    async fn list_sessions(
        &self,
        _filter: SessionFilter,
    ) -> Result<Vec<SessionSummary>, KernelError> {
        Ok(Vec::new())
    }

    fn queries(&self) -> Arc<dyn StoreQueries> {
        self.0.store.clone()
    }

    async fn session_snapshot(&self, session: &SessionId) -> Result<SessionView, KernelError> {
        Ok(view(session))
    }
}

struct Local(Arc<Shelf>);

#[async_trait::async_trait]
impl Workspaces for Local {
    async fn open(&self, _key: &str) -> Result<WorkspaceKernel, String> {
        Ok(WorkspaceKernel {
            kernel: Arc::new(Opener(self.0.clone())),
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

/// 一帧里 `control.runtime` 的值（快照的状态，或增量里的 `state.set`）；这一帧没改它时为 None。
fn runtime_in(frame: &Value) -> Option<String> {
    let body = &frame["params"]["body"];
    if body["kind"] == "snapshot" {
        return body["snapshot"]["state"]["control"]["runtime"]
            .as_str()
            .map(str::to_owned);
    }
    body["deltas"].as_array()?.iter().find_map(|delta| {
        (delta["op"] == "state.set")
            .then(|| delta["patch"]["control"]["runtime"].as_str())
            .flatten()
            .map(str::to_owned)
    })
}

/// 一条链路：帧（还信用）与回复分开收。
struct Link {
    h: Harness,
    shelf: Arc<Shelf>,
    frames: Vec<Value>,
    replies: Vec<Value>,
    next: u64,
}

impl Link {
    async fn new(shelf: Shelf) -> Self {
        let shelf = Arc::new(shelf);
        let mut h = Harness::new();
        h.handle.ready(
            Arc::new(Local(shelf.clone())),
            Arc::new(MemoryStore::default()),
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

    async fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.send(method, params).await;
        self.reply(id).await
    }

    /// 订阅一个会话 topic，等它的第一帧，返回那时的 `control.runtime`。
    async fn subscribe(&mut self, sub: &str, session: &str) -> String {
        self.request(
            "subscribe",
            json!({"sub": sub, "topic": session_topic(ROOT, session)}),
        )
        .await;
        self.runtime(sub).await
    }

    /// 等这个 sub 上下一个改了 `control.runtime` 的帧，返回新值（之前收下的帧先看）。
    async fn runtime(&mut self, sub: &str) -> String {
        loop {
            while let Some(index) = self
                .frames
                .iter()
                .position(|frame| frame["params"]["sub"] == sub)
            {
                let frame = self.frames.remove(index);
                if let Some(runtime) = runtime_in(&frame) {
                    return runtime;
                }
            }
            self.pump().await;
        }
    }

    /// 发一条激活会话的命令（不等回复）。
    async fn send_activate(&mut self, n: usize, command: &str) -> u64 {
        self.send(
            "command",
            json!({"commandId": command, "type": "queue.setAutoDrain",
                   "target": {"hostId": "local", "workspaceKey": ROOT, "sessionId": sid(n)},
                   "payload": {"autoDrain": true}}),
        )
        .await
    }

    /// 激活一个会话，等命令回来。
    async fn activate(&mut self, n: usize) {
        let command = format!("019a5f3c-7a2b-7c3d-8e4f-{:012x}", self.next + 1);
        let id = self.send_activate(n, &command).await;
        let ack = self.reply(id).await;
        assert_eq!(ack["result"]["kind"], "accepted", "{ack}");
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
}

/// 激活第 9 个时，最早用完的第 1 个被关；没人订阅它，topic 随之释放，再订阅是冷的。旧代码上 9 个都热、没有关闭。
#[tokio::test]
async fn the_ninth_activation_closes_the_least_recently_used_idle_session() {
    let mut link = Link::new(Shelf::default()).await;
    for n in 0..HOT_SESSIONS_MAX {
        link.activate(n).await;
    }
    assert!(link.shelf.closes().is_empty(), "eight hot sessions fit");
    link.activate(HOT_SESSIONS_MAX).await;
    link.shelf.closed(1).await;
    assert_eq!(link.shelf.closes(), [sid(0)]);
    assert_eq!(link.subscribe("s0", &sid(0)).await, "cold");
}

/// 在跑的会话不关：第 1 个回合在跑时，关的是第 2 个。
#[tokio::test]
async fn a_running_session_is_not_closed() {
    let mut link = Link::new(Shelf::default()).await;
    link.subscribe("s0", &sid(0)).await;
    link.activate(0).await;
    link.runtime("s0").await;
    link.shelf.push(&sid(0), turn_started());
    assert_eq!(link.runtime("s0").await, "busy");
    for n in 1..=HOT_SESSIONS_MAX {
        link.activate(n).await;
    }
    link.shelf.closed(1).await;
    assert_eq!(link.shelf.closes(), [sid(1)]);
}

/// 有人在看的排在后面（主会话定 2026-11-12，§69.0 第 2 条）：第 1 个最久没用但有订阅者，关的是没人订阅的里最久没用的第 2 个；订阅不算「用」。
#[tokio::test]
async fn a_watched_session_is_closed_after_the_unwatched_ones() {
    let mut link = Link::new(Shelf::default()).await;
    link.subscribe("s0", &sid(0)).await;
    for n in 0..=HOT_SESSIONS_MAX {
        link.activate(n).await;
    }
    assert_eq!(link.runtime("s0").await, "ready");
    link.shelf.closed(1).await;
    assert_eq!(link.shelf.closes(), [sid(1)]);
    // 第 10 个：再关一个，仍是没人订阅的（第 3 个），不是有人在看的第 1 个
    link.activate(HOT_SESSIONS_MAX + 1).await;
    link.shelf.closed(2).await;
    assert_eq!(link.shelf.closes(), [sid(1), sid(2)]);
}

/// 空闲的都有人在看时，在它们里关最久没用的。
#[tokio::test]
async fn when_every_idle_session_is_watched_the_least_recently_used_one_is_closed() {
    let mut link = Link::new(Shelf::default()).await;
    for n in 0..HOT_SESSIONS_MAX {
        link.subscribe(&format!("s{n}"), &sid(n)).await;
    }
    for n in 0..=HOT_SESSIONS_MAX {
        link.activate(n).await;
    }
    assert_eq!(link.runtime("s0").await, "ready");
    assert_eq!(link.runtime("s0").await, "cold");
    assert_eq!(link.shelf.closes(), [sid(0)]);
}

/// 都在跑时照常激活、不关（超限）；之后第一个跑完的被关。
#[tokio::test]
async fn when_none_is_idle_the_first_to_finish_is_closed() {
    let shelf = Shelf::default();
    shelf
        .turns_on_send
        .lock_unpoisoned()
        .extend((0..=HOT_SESSIONS_MAX).map(sid));
    let mut link = Link::new(shelf).await;
    for n in 0..=HOT_SESSIONS_MAX {
        link.activate(n).await;
    }
    assert_eq!(
        link.shelf.opens(&sid(HOT_SESSIONS_MAX)),
        1,
        "activated over the cap"
    );
    let finishing = 3;
    link.subscribe("s", &sid(finishing)).await;
    link.shelf.push(&sid(finishing), turn_finished());
    assert_eq!(link.runtime("s").await, "ready");
    assert_eq!(link.runtime("s").await, "cold");
    assert_eq!(link.shelf.closes(), [sid(finishing)]);
}

/// 关掉的会话再来命令：重新打开（第二次打开），投影换回活的；超限再关下一个最久没用的。
#[tokio::test]
async fn a_closed_session_opens_again_on_the_next_command() {
    let mut link = Link::new(Shelf::default()).await;
    for n in 0..=HOT_SESSIONS_MAX {
        link.activate(n).await;
    }
    link.shelf.closed(1).await;
    assert_eq!(link.subscribe("s0", &sid(0)).await, "cold");
    link.activate(0).await;
    assert_eq!(link.runtime("s0").await, "ready");
    assert_eq!(link.shelf.opens(&sid(0)), 2);
    link.shelf.closed(2).await;
    assert_eq!(link.shelf.closes(), [sid(0), sid(1)]);
}

/// 会话 actor 判出不空闲：会话留热、「用」记成现在；下一次复查先关别的。
#[tokio::test]
async fn a_refused_close_keeps_the_session_and_the_next_check_closes_others() {
    let shelf = Shelf::default();
    shelf.busy.lock_unpoisoned().insert(sid(0));
    let mut link = Link::new(shelf).await;
    for n in 0..=HOT_SESSIONS_MAX {
        link.activate(n).await;
    }
    link.shelf.closed(1).await;
    assert_eq!(link.shelf.closes(), [sid(0)]);
    // 第 10 个：超出两个，关的是 2、3（不是刚拒绝的 1）
    link.activate(HOT_SESSIONS_MAX + 1).await;
    link.shelf.closed(3).await;
    assert_eq!(link.shelf.closes(), [sid(0), sid(1), sid(2)]);
    assert!(!link.range_epoch(&sid(0)).await.contains(READ_EPOCH));
}

/// 关到一半来了这个会话的订阅与命令：订阅照常拿到帧（topic 还在，还是 ready）；命令排在通道里关闭之后，先关（投影变冷），再重新打开（投影换回活的），
/// 命令照常回来。
#[tokio::test]
async fn a_command_right_after_the_close_opens_the_session_again() {
    let gate = Arc::new(Notify::new());
    let mut link = Link::new(Shelf {
        close_gate: Some(gate.clone()),
        ..Shelf::default()
    })
    .await;
    for n in 0..=HOT_SESSIONS_MAX {
        link.activate(n).await;
    }
    assert_eq!(link.subscribe("s0", &sid(0)).await, "ready");
    let id = link
        .send_activate(0, "019a5f3c-7a2b-7c3d-8e4f-0000000000a0")
        .await;
    gate.notify_one();
    assert_eq!(link.runtime("s0").await, "cold");
    assert_eq!(link.runtime("s0").await, "ready");
    let ack = link.reply(id).await;
    assert_eq!(ack["result"]["kind"], "accepted", "{ack}");
    assert_eq!(link.shelf.opens(&sid(0)), 2);
    assert_eq!(link.shelf.closes()[0], sid(0));
}

/// 关掉的会话没有订阅：topic 随之释放（§66），之后的查询走临时投影。
#[tokio::test]
async fn a_closed_session_nobody_watches_is_released() {
    let mut link = Link::new(Shelf::default()).await;
    for n in 0..=HOT_SESSIONS_MAX {
        link.activate(n).await;
    }
    link.shelf.closed(1).await;
    assert_eq!(link.shelf.closes(), [sid(0)]);
    assert!(link.range_epoch(&sid(0)).await.contains(READ_EPOCH));
    assert!(!link.range_epoch(&sid(1)).await.contains(READ_EPOCH));
}

/// 要运行时的读在途的会话不关：第 1 个的子代理读还没回来，关的是第 2 个。
#[tokio::test]
async fn a_session_with_a_runtime_read_in_flight_is_not_closed() {
    let gate = Arc::new(Notify::new());
    let mut link = Link::new(Shelf {
        read_gate: Some(gate.clone()),
        ..Shelf::default()
    })
    .await;
    link.activate(0).await;
    let reading = link
        .send(
            "query",
            json!({"type": "session.subagents",
                   "target": {"hostId": "local", "workspaceKey": ROOT, "sessionId": sid(0)},
                   "params": {}}),
        )
        .await;
    for n in 1..=HOT_SESSIONS_MAX {
        link.activate(n).await;
    }
    link.shelf.closed(1).await;
    assert_eq!(link.shelf.closes(), [sid(1)]);
    gate.notify_one();
    let reply = link.reply(reading).await;
    assert!(reply["result"].is_object(), "{reply}");
}
