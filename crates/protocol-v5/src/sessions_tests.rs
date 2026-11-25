//! 会话列表（protocol-v5.md §14）的进程内测试：真实的 actor 与分帧，存储是 kernel-api 的 `MemoryStore`（与 node-store 同一个
//! `FactChanges` 发布端，写之后事实变了就发）。一致性场景（真实进程）的库是空的，带数据的情形都在这里（§13 第 7 项）。
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rust_cli_base::models::GLM_5_3;
use rust_cli_base::sync::LockUnpoisoned;
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{EnvironmentId, SessionId, TraceId};
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::permission::{ExecutionState, PermissionMode};
use rust_cli_kernel_api::session_facts::{
    FactStream, MigrationSource, SessionFacts, SessionOutcome, TitleSource,
};
use rust_cli_kernel_api::store::{
    SessionKind, SessionMeta, SessionStore, StoreError, StoreQueries,
};
use rust_cli_kernel_api::testing::MemoryStore;
use serde_json::{Value, json};
use tokio::sync::Notify;

use crate::server::Workspaces;
use crate::sessions::fields;
use crate::test_link::{Harness, initialize};
use crate::topics::{WORKSPACE_SESSIONS, workspace_topic};

const LOCAL: &str = "local";
/// 两个本机工作区（绝对路径；Windows 的绝对路径要盘符）。
#[cfg(not(windows))]
const A: &str = "/Users/dev/a";
#[cfg(not(windows))]
const B: &str = "/Users/dev/b";
#[cfg(windows)]
const A: &str = r"C:\Users\dev\a";
#[cfg(windows)]
const B: &str = r"C:\Users\dev\b";

fn workspace(root: &str) -> WorkspaceRef {
    WorkspaceRef {
        environment: EnvironmentId::new(LOCAL),
        root: EnvPath::new(root),
    }
}

/// 入口的工作区端口：key 就是本机的根目录（cli 的 `env_assembly::workspace_at`）。
struct Local;

#[async_trait]
impl Workspaces for Local {
    async fn open(&self, _key: &str) -> Result<crate::server::WorkspaceKernel, String> {
        Ok(crate::test_link::kernel())
    }

    fn release(&self, _key: &str) -> bool {
        false
    }

    fn workspace(&self, key: &str) -> WorkspaceRef {
        workspace(key)
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

async fn create(store: &MemoryStore, id: &str, root: &str, title: &str, at: u64) {
    store
        .create(
            SessionMeta {
                id: SessionId::new(id),
                kind: SessionKind::Interactive,
                parent: None,
                workspace: workspace(root),
                title: title.to_owned(),
                trace: TraceId::new(format!("trace-{id}")),
                created_at_ms: at,
            },
            rust_cli_kernel_api::testing::no_facts(),
        )
        .await
        .unwrap();
}

/// 包一层存储：读种子之前做一件事（模拟“订阅与读种子之间”的提交），或者让读种子失败；有闸门时读完先拿住结果。
#[derive(Default)]
struct Between {
    inner: Arc<MemoryStore>,
    before_seed: Mutex<Vec<(String, String)>>,
    fail_seed: std::sync::atomic::AtomicBool,
    gate: Option<Gate>,
    /// 读过几次种子。
    seeds: AtomicUsize,
}

/// 拿住读的闸门（模拟读排在会话库的连接锁上，例如别的进程拿着写锁）：读完先报“到了”，等测试放开才交出结果。不靠计时。
#[derive(Default)]
struct Gate {
    entered: Notify,
    release: Notify,
}

impl Between {
    async fn hold(&self) {
        if let Some(gate) = &self.gate {
            gate.entered.notify_one();
            gate.release.notified().await;
        }
    }

    fn gate(&self) -> &Gate {
        self.gate.as_ref().unwrap()
    }
}

#[async_trait]
impl StoreQueries for Between {
    async fn metering(
        &self,
        session: &SessionId,
    ) -> Result<rust_cli_kernel_api::metering::SessionMetering, StoreError> {
        self.inner.metering(session).await
    }

    async fn request_stats(
        &self,
        session: &SessionId,
    ) -> Result<rust_cli_kernel_api::metering::RequestStats, StoreError> {
        self.inner.request_stats(session).await
    }

    async fn activity(
        &self,
        query: rust_cli_kernel_api::metering::ActivityQuery,
    ) -> Result<rust_cli_kernel_api::metering::ActivityStats, StoreError> {
        self.inner.activity(query).await
    }

    async fn put_record(
        &self,
        session: &SessionId,
        record: &rust_cli_kernel_api::store::FrontendRecord,
    ) -> Result<(), StoreError> {
        self.inner.put_record(session, record).await
    }

    async fn records(
        &self,
        session: &SessionId,
        kind: &str,
    ) -> Result<Vec<rust_cli_kernel_api::store::FrontendRecord>, StoreError> {
        self.inner.records(session, kind).await
    }

    async fn session_facts(&self, sessions: &[SessionId]) -> Result<Vec<SessionFacts>, StoreError> {
        let found = self.inner.session_facts(sessions).await;
        self.hold().await;
        found
    }

    async fn branch_cut(&self, session: &SessionId) -> Result<Option<String>, StoreError> {
        self.inner.branch_cut(session).await
    }

    async fn recent_session_facts(
        &self,
        workspace: &WorkspaceRef,
        limit: usize,
    ) -> Result<Vec<SessionFacts>, StoreError> {
        let renames: Vec<(String, String)> =
            std::mem::take(&mut *self.before_seed.lock_unpoisoned());
        for (id, title) in renames {
            self.inner
                .rename(&SessionId::new(id), &title)
                .await
                .unwrap();
        }
        self.seeds.fetch_add(1, Ordering::SeqCst);
        if self.fail_seed.load(Ordering::SeqCst) {
            return Err(StoreError::Storage {
                message: "injected seed failure".to_owned(),
            });
        }
        let seed = self.inner.recent_session_facts(workspace, limit).await;
        self.hold().await;
        seed
    }

    fn session_fact_changes(&self) -> FactStream {
        self.inner.session_fact_changes()
    }
}

/// 握手、进程级部件建好（`store` 当存储查询）。
async fn ready(store: Arc<dyn StoreQueries>) -> Harness {
    let mut h = Harness::new();
    h.handle.ready(
        Arc::new(Local),
        store,
        Arc::new(rust_cli_kernel_api::testing::MemoryArtifacts::default()),
    );
    h.send(initialize()).await;
    h.next().await;
    h
}

fn sessions(root: &str) -> String {
    workspace_topic(root, WORKSPACE_SESSIONS)
}

fn subscribe(id: u64, sub: &str, topic: &str, cursor: Option<Value>) -> Value {
    let mut params = json!({"sub": sub, "topic": topic});
    if let Some(cursor) = cursor {
        params["cursor"] = cursor;
    }
    json!({"jsonrpc": "2.0", "id": id, "method": "subscribe", "params": params})
}

fn ping(id: u64) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "ping", "params": {}})
}

fn unsubscribe(id: u64, sub: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "unsubscribe", "params": {"sub": sub}})
}

fn shutdown(id: u64) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "shutdown", "params": {}})
}

/// 工作区级命令 `agent.release`（没打开过的工作区：`accepted{noop}`）。
fn release(id: u64, key: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "command", "params": {
        "commandId": format!("release-{id}"), "type": "agent.release",
        "target": {"hostId": "local", "workspaceKey": key}, "payload": {}}})
}

/// 拿住读种子的存储（种子里有 `s1`）与握手好、进程级部件建好的链路。
async fn held() -> (Arc<MemoryStore>, Arc<Between>, Harness) {
    let inner = Arc::new(MemoryStore::default());
    create(&inner, "s1", A, "one", 1).await;
    let store = Arc::new(Between {
        inner: inner.clone(),
        gate: Some(Gate::default()),
        ..Between::default()
    });
    let h = ready(store.clone()).await;
    (inner, store, h)
}

fn credit(sub: &str, n: u64) -> Value {
    json!({"jsonrpc": "2.0", "method": "credit", "params": {"sub": sub, "n": n}})
}

fn facts_query(id: u64, key: Option<&str>, ids: Value) -> Value {
    let mut target = json!({"hostId": "local"});
    if let Some(key) = key {
        target["workspaceKey"] = json!(key);
    }
    json!({"jsonrpc": "2.0", "id": id, "method": "query",
           "params": {"type": "sessions.facts", "target": target, "params": {"sessionIds": ids}}})
}

/// 一帧里的全部 put：(id, data)。
fn puts(frame: &Value) -> Vec<(String, Value)> {
    frame["params"]["body"]["deltas"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| {
            assert_eq!(d["op"], json!("entity.put"), "{d}");
            assert_eq!(d["col"], json!("facts"), "{d}");
            (d["id"].as_str().unwrap().to_owned(), d["data"].clone())
        })
        .collect()
}

fn snapshot_ids(frame: &Value) -> Vec<String> {
    assert_eq!(
        frame["params"]["body"]["kind"],
        json!("snapshot"),
        "{frame}"
    );
    frame["params"]["body"]["snapshot"]["collections"]["facts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["id"].as_str().unwrap().to_owned())
        .collect()
}

/// 订阅之后读首帧与 `subscribed`（首帧在前）。
async fn first_frame(h: &mut Harness, id: u64, sub: &str, topic: &str) -> (Value, Value) {
    h.send(subscribe(id, sub, topic, None)).await;
    let frame = h.next().await;
    assert_eq!(frame["method"], json!("frame"), "{frame}");
    let subscribed = h.next().await;
    assert_eq!(subscribed["id"], json!(id), "{subscribed}");
    (frame, subscribed)
}

#[tokio::test]
async fn the_seed_is_the_first_snapshot_and_later_commits_follow_as_puts() {
    let store = Arc::new(MemoryStore::default());
    create(&store, "s1", A, "one", 1).await;
    create(&store, "s2", A, "two", 2).await;
    store.delete(&SessionId::new("s2")).await.unwrap();
    create(&store, "s3", B, "elsewhere", 3).await;
    let mut h = ready(store.clone()).await;

    let (frame, subscribed) = first_frame(&mut h, 2, "a", &sessions(A)).await;
    // 种子含软删除的行；别的工作区的不在
    assert_eq!(snapshot_ids(&frame), ["s1", "s2"]);
    let rows = &frame["params"]["body"]["snapshot"]["collections"]["facts"];
    assert_eq!(rows[0]["title"], json!("one"));
    assert_eq!(rows[0]["titleSource"], json!("firstInput"));
    assert_eq!(rows[0]["deletedAt"], Value::Null);
    assert!(rows[1]["deletedAt"].is_number(), "{rows}");
    // 种子是一批：两项的 v 都是那一批的 seq；subscribed 的 cursor 等于首帧
    assert_eq!(rows[0]["v"], frame["params"]["to"]);
    assert_eq!(rows[1]["v"], frame["params"]["to"]);
    assert_eq!(subscribed["result"]["cursor"]["seq"], frame["params"]["to"]);

    // 实时：新增、改名（custom）、软删除，各一帧一条 put
    create(&store, "s4", A, "four", 4).await;
    let added = h.next().await;
    assert_eq!(puts(&added)[0].0, "s4");
    store
        .rename(&SessionId::new("s1"), "renamed")
        .await
        .unwrap();
    let renamed = h.next().await;
    let (id, data) = &puts(&renamed)[0];
    assert_eq!(
        (id.as_str(), &data["title"], &data["titleSource"]),
        ("s1", &json!("renamed"), &json!("custom"))
    );
    store.delete(&SessionId::new("s1")).await.unwrap();
    let deleted = h.next().await;
    assert!(puts(&deleted)[0].1["deletedAt"].is_number(), "{deleted}");
    // v 随批次单调：后一帧的 v 大于前一帧
    let v = |frame: &Value| frame["params"]["body"]["deltas"][0]["v"].as_u64().unwrap();
    assert!(v(&added) < v(&renamed) && v(&renamed) < v(&deleted));

    // 别的工作区的提交不进这个 feed：actor 先处理已经到的变化，再处理之后的 ping。四帧用完了初始信用，先还一格，
    // 漏进来的帧才发得出来
    h.send(credit("a", 1)).await;
    create(&store, "s5", B, "b", 5).await;
    h.send(ping(9)).await;
    let mut between = Vec::new();
    h.reply(9, &mut between).await;
    assert!(between.is_empty(), "{between:?}");
}

#[tokio::test]
async fn two_workspaces_do_not_mix() {
    let store = Arc::new(MemoryStore::default());
    create(&store, "a1", A, "a", 1).await;
    create(&store, "b1", B, "b", 2).await;
    let mut h = ready(store.clone()).await;
    let (a, _) = first_frame(&mut h, 2, "a", &sessions(A)).await;
    let (b, _) = first_frame(&mut h, 3, "b", &sessions(B)).await;
    assert_eq!(snapshot_ids(&a), ["a1"]);
    assert_eq!(snapshot_ids(&b), ["b1"]);
    store.rename(&SessionId::new("b1"), "b!").await.unwrap();
    let frame = h.next().await;
    assert_eq!(frame["params"]["sub"], json!("b"));
    assert_eq!(puts(&frame)[0].0, "b1");
    h.send(ping(9)).await;
    let mut between = Vec::new();
    h.reply(9, &mut between).await;
    assert!(between.is_empty(), "{between:?}");
}

#[tokio::test]
async fn commits_between_subscribing_and_reading_the_seed_are_in_the_snapshot_and_not_replayed() {
    let inner = Arc::new(MemoryStore::default());
    create(&inner, "s1", A, "first", 1).await;
    let store = Arc::new(Between {
        inner: inner.clone(),
        ..Between::default()
    });
    // 读种子之前改两次名：变化流里排着 mid 与 last，种子里是 last
    *store.before_seed.lock_unpoisoned() = vec![
        ("s1".to_owned(), "mid".to_owned()),
        ("s1".to_owned(), "last".to_owned()),
    ];
    let mut h = ready(store).await;
    let (frame, _) = first_frame(&mut h, 2, "a", &sessions(A)).await;
    assert_eq!(
        frame["params"]["body"]["snapshot"]["collections"]["facts"][0]["title"],
        json!("last")
    );
    // 排着的旧值不再作为增量发出（不让列表退回 mid）
    h.send(ping(9)).await;
    let mut between = Vec::new();
    h.reply(9, &mut between).await;
    assert!(between.is_empty(), "{between:?}");
    // 之后的提交照常
    inner.rename(&SessionId::new("s1"), "after").await.unwrap();
    assert_eq!(puts(&h.next().await)[0].1["title"], json!("after"));
}

#[tokio::test]
async fn rapid_changes_of_one_session_queue_behind_the_credit_and_end_at_the_last_value() {
    let store = Arc::new(MemoryStore::default());
    create(&store, "s1", A, "0", 1).await;
    let mut h = ready(store.clone()).await;
    first_frame(&mut h, 2, "a", &sessions(A)).await;
    // 首帧用掉一格信用；再三条用完
    for title in ["1", "2", "3"] {
        store.rename(&SessionId::new("s1"), title).await.unwrap();
        h.next().await;
    }
    // 信用用完：这三次排着
    for title in ["4", "5", "6"] {
        store.rename(&SessionId::new("s1"), title).await.unwrap();
    }
    h.send(ping(9)).await;
    let mut between = Vec::new();
    h.reply(9, &mut between).await;
    assert!(between.is_empty(), "{between:?}");
    h.send(credit("a", 1)).await;
    let frame = h.next().await;
    let titles: Vec<Value> = puts(&frame)
        .into_iter()
        .map(|(_, data)| data["title"].clone())
        .collect();
    // 续传帧从当前内容拼（§68.3，01 §19.2）：同一会话至多一条 put，就是最后一次（以前三次都拼在帧里，§12 第 5 项）
    assert_eq!(titles, [json!("6")]);
}

#[tokio::test]
async fn a_subscription_before_the_process_parts_exist_is_pending_until_the_seed() {
    let store = Arc::new(MemoryStore::default());
    create(&store, "s1", A, "one", 1).await;
    let mut h = Harness::new();
    h.send(initialize()).await;
    h.next().await;
    h.send(subscribe(2, "a", &sessions(A), None)).await;
    let pending = h.next().await;
    assert_eq!(
        pending["result"],
        json!({"sub": "a", "pending": "source_unavailable"})
    );
    // 源还没有：查询回可重试的存储不可用
    h.send(facts_query(3, Some(A), json!(["s1"]))).await;
    let error = h.next().await;
    assert_eq!(
        error["error"]["data"]["code"],
        json!("precondition.storeUnavailable")
    );
    assert_eq!(error["error"]["data"]["retryable"], json!(true));
    // 建好：等着的订阅拿到快照
    h.handle.ready(
        Arc::new(Local),
        store,
        Arc::new(rust_cli_kernel_api::testing::MemoryArtifacts::default()),
    );
    let frame = h.next().await;
    assert_eq!(frame["params"]["sub"], json!("a"));
    assert_eq!(snapshot_ids(&frame), ["s1"]);
}

#[tokio::test]
async fn a_seed_that_does_not_read_fails_the_subscription() {
    let store = Arc::new(Between::default());
    store.fail_seed.store(true, Ordering::SeqCst);
    // 建好之前订阅：pending，建好时读失败 → subscription.failed
    let mut h = Harness::new();
    h.send(initialize()).await;
    h.next().await;
    h.send(subscribe(2, "a", &sessions(A), None)).await;
    h.next().await;
    h.handle.ready(
        Arc::new(Local),
        store.clone(),
        Arc::new(rust_cli_kernel_api::testing::MemoryArtifacts::default()),
    );
    let failed = h.next().await;
    assert_eq!(failed["method"], json!("subscription.failed"), "{failed}");
    assert_eq!(failed["params"]["sub"], json!("a"));
    assert_eq!(
        failed["params"]["error"]["code"],
        json!("precondition.storeUnavailable")
    );
    // 建好之后订阅：这次订阅回错误，不登记；下一次订阅重新读
    h.send(subscribe(3, "b", &sessions(A), None)).await;
    let error = h.next().await;
    assert_eq!(
        error["error"]["data"]["code"],
        json!("precondition.storeUnavailable")
    );
    store.fail_seed.store(false, Ordering::SeqCst);
    let (frame, _) = first_frame(&mut h, 4, "c", &sessions(A)).await;
    assert_eq!(snapshot_ids(&frame), Vec::<String>::new());
}

#[tokio::test]
async fn a_cursor_in_the_window_resumes_and_another_epoch_gets_a_snapshot() {
    let store = Arc::new(MemoryStore::default());
    create(&store, "s1", A, "one", 1).await;
    let mut h = ready(store.clone()).await;
    let (frame, _) = first_frame(&mut h, 2, "a", &sessions(A)).await;
    let at = json!({"epoch": frame["params"]["epoch"], "seq": frame["params"]["to"]});
    store.rename(&SessionId::new("s1"), "two").await.unwrap();
    h.next().await;
    // 断线重连（新的 sub）带首帧的 cursor：只补之后的那一批
    h.send(subscribe(3, "again", &sessions(A), Some(at.clone())))
        .await;
    let resumed = h.next().await;
    assert_eq!(
        resumed["params"]["body"]["kind"],
        json!("deltas"),
        "{resumed}"
    );
    assert_eq!(resumed["params"]["from"], at["seq"]);
    assert_eq!(puts(&resumed)[0].1["title"], json!("two"));
    h.next().await;
    // 上一个进程的 cursor：快照
    h.send(subscribe(
        4,
        "old",
        &sessions(A),
        Some(json!({"epoch": "rs-previous", "seq": 1})),
    ))
    .await;
    let snapshot = h.next().await;
    assert_eq!(snapshot_ids(&snapshot), ["s1"]);
}

#[tokio::test]
async fn sessions_facts_answers_each_id_in_order_inside_the_callers_workspace() {
    let store = Arc::new(MemoryStore::default());
    create(&store, "a1", A, "a", 1).await;
    create(&store, "a2", A, "gone", 2).await;
    store.delete(&SessionId::new("a2")).await.unwrap();
    create(&store, "b1", B, "b", 3).await;
    let mut h = ready(store).await;
    h.send(facts_query(
        2,
        Some(A),
        json!(["a1", "missing", "b1", "a2"]),
    ))
    .await;
    let reply = h.next().await;
    assert_eq!(reply["result"]["kind"], json!("ok"), "{reply}");
    let sessions = &reply["result"]["data"]["sessions"];
    let ids: Vec<&Value> = sessions
        .as_array()
        .unwrap()
        .iter()
        .map(|s| &s["sessionId"])
        .collect();
    assert_eq!(
        ids,
        [&json!("a1"), &json!("missing"), &json!("b1"), &json!("a2")]
    );
    assert_eq!(sessions[0]["fact"]["title"], json!("a"));
    // 库里没有的与别的工作区的都是 null
    assert_eq!(sessions[1]["fact"], Value::Null);
    assert_eq!(sessions[2]["fact"], Value::Null);
    // 删除了的照样有 fact
    assert!(sessions[3]["fact"]["deletedAt"].is_number(), "{sessions}");

    let code = |reply: &Value| reply["error"]["data"]["code"].clone();
    h.send(facts_query(3, None, json!(["a1"]))).await;
    assert_eq!(
        code(&h.next().await),
        json!("precondition.workspaceRequired")
    );
    h.send(facts_query(4, Some("relative/dir"), json!(["a1"])))
        .await;
    assert_eq!(code(&h.next().await), json!("precondition.invalid"));
    h.send(facts_query(5, Some(A), json!([]))).await;
    assert_eq!(code(&h.next().await), json!("precondition.invalidPayload"));
    // 上限是契约的 256（SESSIONS_FACTS_QUERY_MAX）
    let ids = |n: usize| json!((0..n).map(|i| format!("s{i}")).collect::<Vec<_>>());
    h.send(facts_query(6, Some(A), ids(256))).await;
    assert_eq!(
        h.next().await["result"]["data"]["sessions"]
            .as_array()
            .unwrap()
            .len(),
        256
    );
    h.send(facts_query(7, Some(A), ids(257))).await;
    let error = h.next().await;
    assert_eq!(code(&error), json!("precondition.invalidPayload"));
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("sessionIds"),
        "{error}"
    );
}

/// 读种子被拿住（例如别的进程拿着库的写锁，本进程的读排在连接锁上）：actor 照常回 `ping` 与别的工作区的命令；同一个工作区再订
/// 不重复读；放开之后两个订阅各拿首帧再拿 `subscribed`，拿住期间的提交在首帧里、不再作为增量重发（protocol-v5.md §4.1、§14.4）。
#[tokio::test]
async fn a_held_seed_read_does_not_hold_up_the_link() {
    let (inner, store, mut h) = held().await;
    h.send(subscribe(2, "a", &sessions(A), None)).await;
    store.gate().entered.notified().await;
    // 种子已经读出、还没交回：这时的提交只在变化流里
    inner
        .rename(&SessionId::new("s1"), "renamed")
        .await
        .unwrap();
    create(&inner, "s2", A, "two", 2).await;
    h.send(ping(3)).await;
    assert_eq!(h.next().await["id"], json!(3));
    h.send(release(4, B)).await;
    let ack = h.next().await;
    assert_eq!(ack["id"], json!(4), "{ack}");
    assert_eq!(ack["result"]["kind"], json!("accepted"), "{ack}");
    h.send(subscribe(5, "b", &sessions(A), None)).await;
    h.send(ping(6)).await;
    assert_eq!(h.next().await["id"], json!(6));

    store.gate().release.notify_one();
    for sub in ["a", "b"] {
        let frame = h.next().await;
        assert_eq!(frame["params"]["sub"], json!(sub), "{frame}");
        assert_eq!(snapshot_ids(&frame), ["s1", "s2"]);
        assert_eq!(
            frame["params"]["body"]["snapshot"]["collections"]["facts"][0]["title"],
            json!("renamed")
        );
    }
    assert_eq!(h.next().await["id"], json!(2));
    assert_eq!(h.next().await["id"], json!(5));
    assert_eq!(store.seeds.load(Ordering::SeqCst), 1);
    // 并进首帧的提交不重发；之后的提交两个订阅各一条 put
    h.send(ping(7)).await;
    let mut between = Vec::new();
    h.reply(7, &mut between).await;
    assert!(between.is_empty(), "{between:?}");
    inner.rename(&SessionId::new("s2"), "later").await.unwrap();
    for sub in ["a", "b"] {
        let frame = h.next().await;
        assert_eq!(frame["params"]["sub"], json!(sub), "{frame}");
        assert_eq!(puts(&frame)[0].1["title"], json!("later"));
    }
}

/// `sessions.facts` 的读被拿住：之后的 `ping` 先得到回复，放开之后查询回正确的结果（§14.6）。
#[tokio::test]
async fn a_held_facts_query_does_not_hold_up_ping() {
    let (_, store, mut h) = held().await;
    h.send(facts_query(2, Some(A), json!(["s1"]))).await;
    store.gate().entered.notified().await;
    h.send(ping(3)).await;
    assert_eq!(h.next().await["id"], json!(3));
    store.gate().release.notify_one();
    let reply = h.next().await;
    assert_eq!(reply["id"], json!(2), "{reply}");
    assert_eq!(
        reply["result"]["data"]["sessions"][0]["fact"]["title"],
        json!("one")
    );
}

/// 读种子被拿住时 `shutdown`：之后的请求照常被拒（actor 没停），`shutdown` 的回复等那次读，排在首帧与 `subscribed` 之后（§5.2）。
#[tokio::test]
async fn shutdown_waits_for_a_held_seed_read() {
    let (_, store, mut h) = held().await;
    h.send(subscribe(2, "a", &sessions(A), None)).await;
    store.gate().entered.notified().await;
    h.send(shutdown(3)).await;
    h.send(ping(4)).await;
    let rejected = h.next().await;
    assert_eq!(rejected["id"], json!(4), "{rejected}");
    assert_eq!(
        rejected["error"]["data"]["code"],
        json!("link.protocolViolation")
    );
    store.gate().release.notify_one();
    let frame = h.next().await;
    assert_eq!(snapshot_ids(&frame), ["s1"]);
    assert_eq!(h.next().await["id"], json!(2));
    let done = h.next().await;
    assert_eq!(done["id"], json!(3), "{done}");
    assert_eq!(done["result"], json!({}));
}

/// 读种子被拿住时退订、释放这个工作区：退订的订阅不再有帧，它的请求照样有回复；释放不影响会话列表；别的订阅照常（§14.4）。
#[tokio::test]
async fn a_subscription_dropped_while_its_seed_is_held_gets_no_frames() {
    let (inner, store, mut h) = held().await;
    h.send(subscribe(2, "a", &sessions(A), None)).await;
    store.gate().entered.notified().await;
    h.send(unsubscribe(3, "a")).await;
    assert_eq!(h.next().await["id"], json!(3));
    h.send(release(4, A)).await;
    assert_eq!(h.next().await["id"], json!(4));
    h.send(subscribe(5, "b", &sessions(A), None)).await;
    // ping 的回复说明它之前的订阅已经记下（actor 按到达顺序处理对端的消息）
    h.send(ping(6)).await;
    assert_eq!(h.next().await["id"], json!(6));
    store.gate().release.notify_one();
    let frame = h.next().await;
    assert_eq!(frame["params"]["sub"], json!("b"), "{frame}");
    assert_eq!(snapshot_ids(&frame), ["s1"]);
    let dropped = h.next().await;
    assert_eq!(dropped["id"], json!(2), "{dropped}");
    assert_eq!(dropped["result"]["sub"], json!("a"));
    assert_eq!(h.next().await["id"], json!(5));
    inner.rename(&SessionId::new("s1"), "two").await.unwrap();
    let frame = h.next().await;
    assert_eq!(frame["params"]["sub"], json!("b"), "{frame}");
    h.send(ping(7)).await;
    let mut between = Vec::new();
    h.reply(7, &mut between).await;
    assert!(between.is_empty(), "{between:?}");
}

#[test]
fn the_projection_maps_every_kernel_fact() {
    let facts = SessionFacts {
        id: SessionId::new("s1"),
        kind: SessionKind::Fork,
        parent: Some(SessionId::new("p1")),
        workspace: workspace(A),
        title: "t".to_owned(),
        title_source: TitleSource::Generated,
        created_at_ms: 1,
        updated_at_ms: 2,
        last_outcome: Some(SessionOutcome::Interrupted),
        deleted_at_ms: Some(3),
        first_input_at_ms: Some(4),
        migration: Some(MigrationSource::ImportedHistory),
        selection: Some(ModelSelection {
            reasoning: Some("high".to_owned()),
            ..ModelSelection::new("zhipu", GLM_5_3)
        }),
        execution: Some(ExecutionState {
            mode: PermissionMode::AcceptEdits,
            plan: true,
        }),
    };
    assert_eq!(
        serde_json::to_value(fields(&facts)).unwrap(),
        json!({
            "title": "t", "titleSource": "generated", "createdAt": 1.0, "updatedAt": 2.0,
            "lastOutcome": "cancelled", "deletedAt": 3.0, "parentSessionId": "p1", "firstInputAt": 4.0,
            "migrationSource": "claudeCode", "config": {"model": GLM_5_3, "mode": "edit", "thought": "high"},
        })
    );
    let bare = SessionFacts {
        last_outcome: Some(SessionOutcome::Completed),
        selection: None,
        execution: None,
        ..facts
    };
    let value = serde_json::to_value(fields(&bare)).unwrap();
    assert_eq!(value["lastOutcome"], json!("success"));
    assert_eq!(
        value["config"],
        json!({"model": "", "mode": "", "thought": ""})
    );
}
