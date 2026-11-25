//! 冷打开会话之前经 Host 取附加目录（protocol-v5.md §37；session-dirs.md §12.4、§12.6）与会话偏好（§48；app-prefs.md §4.1、§5）：真实的
//! actor 与 Content-Length 分帧，测试扮演 Host（握手时告知服务、应答 `service`）与窗口（订会话 topic、发命令）。内核是记下建会话、打开与
//! 命令的替身：这里只测通道什么时候问、等回复时命令怎么排、建会话与打开带了什么。
use std::sync::{Arc, Mutex};

use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use rust_cli_base::sync::LockUnpoisoned;
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{EnvironmentId, SessionId, TraceId};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::{
    Ack, CommandEnvelope, CreateSession, Kernel, KernelError, SequencedEvent, SessionHandle,
    SessionView, StartSource,
};
use rust_cli_kernel_api::session_preferences::{OpenStart, SessionPreferences};
use rust_cli_kernel_api::store::{
    SessionFilter, SessionKind, SessionMeta, SessionSummary, StoreQueries,
};
use rust_cli_kernel_api::testing::{HandleOverride, MemoryStore};
use serde_json::{Value, json};

use crate::generated::contract::CONTRACT_DIGEST;
use crate::server::{WorkspaceKernel, Workspaces};
use crate::test_link::Harness;
use crate::topics::session_topic;

const ROOT: &str = "/Users/dev/app";
const OTHER_ROOT: &str = "/Users/dev/lib";
const SESSION: &str = "sess_1";
const SERVICE: &str = "host.session.executionDirectories";
const FIRST: &str = "019a5f3c-7a2b-7c3d-8e4f-0000000000d1";
const SECOND: &str = "019a5f3c-7a2b-7c3d-8e4f-0000000000d2";

/// 内核替身看到的：打开（带的目录）与发给会话的命令（id）。
#[derive(Debug, Clone, PartialEq)]
enum Seen {
    Opened(Vec<EnvPath>),
    Command(String),
}

type Log = Arc<Mutex<Vec<Seen>>>;

/// 内核替身收到的会话偏好：建会话带的、冷打开带的（app-prefs.md §4.2）。
#[derive(Debug, Clone, PartialEq)]
enum Start {
    Created(SessionPreferences),
    Opened(SessionPreferences),
}

type Starts = Arc<Mutex<Vec<Start>>>;

fn view() -> SessionView {
    let meta = SessionMeta {
        id: SessionId::new(SESSION),
        kind: SessionKind::Interactive,
        parent: None,
        workspace: workspace(ROOT),
        title: String::new(),
        trace: TraceId::new(SESSION),
        created_at_ms: 1,
    };
    SessionView::new(
        meta,
        Vec::new(),
        rust_cli_kernel_api::testing::selection(),
        PermissionMode::Default,
    )
}

fn workspace(key: &str) -> WorkspaceRef {
    WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new(key),
    }
}

/// 打开着的会话：记下命令，给出视图，事件流不结束。
struct Open(SessionId, Log);

#[async_trait::async_trait]
impl HandleOverride for Open {
    fn id(&self) -> &SessionId {
        &self.0
    }

    async fn send(&self, command: CommandEnvelope) -> Ack {
        self.1
            .lock_unpoisoned()
            .push(Seen::Command(command.id.as_str().to_owned()));
        Ack::Accepted {
            result: Default::default(),
        }
    }

    fn subscribe(&self, _after: Option<u64>) -> BoxStream<'static, SequencedEvent> {
        futures_util::stream::pending().boxed()
    }

    async fn view(&self) -> Result<SessionView, KernelError> {
        Ok(view())
    }
}

/// 有一个会话的内核：冷读给视图、不记；打开记下带的目录与偏好；建会话总成功，记下带的偏好。
struct OneSession(Arc<MemoryStore>, Log, Starts);

impl OneSession {
    fn handle(&self, directories: Vec<EnvPath>) -> Arc<dyn SessionHandle> {
        self.1.lock_unpoisoned().push(Seen::Opened(directories));
        Arc::new(Open(SessionId::new(SESSION), self.1.clone()))
    }
}

#[async_trait::async_trait]
impl Kernel for OneSession {
    async fn create_session(&self, request: CreateSession) -> Result<SessionId, KernelError> {
        self.2
            .lock_unpoisoned()
            .push(Start::Created(request.preferences));
        Ok(SessionId::new(SESSION))
    }

    async fn open_session(
        &self,
        _session: &SessionId,
    ) -> Result<Arc<dyn SessionHandle>, KernelError> {
        Ok(self.handle(Vec::new()))
    }

    async fn open_session_with(
        &self,
        _session: &SessionId,
        _source: StartSource,
        start: OpenStart,
    ) -> Result<Arc<dyn SessionHandle>, KernelError> {
        self.2
            .lock_unpoisoned()
            .push(Start::Opened(start.preferences));
        Ok(self.handle(start.directories))
    }

    async fn list_sessions(
        &self,
        _filter: SessionFilter,
    ) -> Result<Vec<SessionSummary>, KernelError> {
        Ok(Vec::new())
    }

    fn queries(&self) -> Arc<dyn StoreQueries> {
        self.0.clone()
    }

    async fn session_snapshot(&self, _session: &SessionId) -> Result<SessionView, KernelError> {
        Ok(view())
    }
}

struct Local(Arc<OneSession>);

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

/// 一条链路：Host 来的请求与给窗口的回复分开收好（帧还信用，读循环不卡）。
struct Link {
    h: Harness,
    log: Log,
    starts: Starts,
    services: Vec<Value>,
    replies: Vec<Value>,
}

impl Link {
    /// 握手（告知 `host_services`）、订会话 topic 并等订阅的回复（会话进了会话表，冷读不起会话）。
    async fn new(host_services: &[&str]) -> Self {
        let mut link = Self::linked(host_services).await;
        link.h
            .send(json!({"jsonrpc": "2.0", "id": 2, "method": "subscribe",
                         "params": {"sub": "s", "topic": session_topic(ROOT, SESSION)}}))
            .await;
        link.reply(2).await;
        link
    }

    /// 只握手。
    async fn linked(host_services: &[&str]) -> Self {
        let log = Log::default();
        let starts = Starts::default();
        let kernel = Arc::new(OneSession(Arc::default(), log.clone(), starts.clone()));
        let mut h = Harness::new();
        h.handle.ready(
            Arc::new(Local(kernel)),
            Arc::new(MemoryStore::default()),
            Arc::new(rust_cli_kernel_api::testing::MemoryArtifacts::default()),
        );
        h.send(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                      "params": {"clientId": "host",
                                 "caps": {"core": [1], "agent": [1], "session": [1], "session.queue": [1]},
                                 "contract": CONTRACT_DIGEST, "hostServices": host_services}}))
            .await;
        assert!(h.next().await["result"].is_object());
        Self {
            h,
            log,
            starts,
            services: Vec::new(),
            replies: Vec::new(),
        }
    }

    async fn pump(&mut self) {
        let message = self.h.next().await;
        if message["method"] == "frame" {
            let sub = message["params"]["sub"].clone();
            self.h
                .send(json!({"jsonrpc": "2.0", "method": "credit", "params": {"sub": sub, "n": 1}}))
                .await;
        } else if message["method"] == "service" {
            self.services.push(message);
        } else {
            self.replies.push(message);
        }
    }

    async fn reply(&mut self, id: u64) -> Value {
        loop {
            if let Some(index) = self.replies.iter().position(|reply| reply["id"] == id) {
                return self.replies.remove(index);
            }
            self.pump().await;
        }
    }

    async fn service(&mut self) -> Value {
        loop {
            if !self.services.is_empty() {
                return self.services.remove(0);
            }
            self.pump().await;
        }
    }

    /// 一条要打开会话的命令（`queue.setAutoDrain`：通道交给内核，不看投影）。
    async fn command(&mut self, id: u64, command: &str) {
        self.h
            .send(
                json!({"jsonrpc": "2.0", "id": id, "method": "command", "params": {
                "commandId": command, "type": "queue.setAutoDrain",
                "target": {"hostId": "local", "workspaceKey": ROOT, "sessionId": SESSION},
                "payload": {"autoDrain": true}}}),
            )
            .await;
    }

    /// 读到对 `id` 的 ping 回复为止：之前收到的都收好了。actor 按到达顺序处理，所以那之前的命令都已经交给了通道。
    async fn pinged(&mut self, id: u64) {
        self.h
            .send(json!({"jsonrpc": "2.0", "id": id, "method": "ping"}))
            .await;
        self.reply(id).await;
    }

    fn seen(&self) -> Vec<Seen> {
        self.log.lock_unpoisoned().clone()
    }
}

fn opened(directories: &[&str]) -> Seen {
    Seen::Opened(directories.iter().map(|dir| EnvPath::new(*dir)).collect())
}

/// 冷打开之前问 Host（外壳与参数都是这个会话）；回复之前同一个会话的两条命令都不进内核，回复之后先打开（带着目录）、再按到达顺序进。
#[tokio::test]
async fn a_cold_open_waits_for_the_host_directories_and_keeps_the_commands_in_order() {
    let mut link = Link::new(&[SERVICE]).await;
    link.command(3, FIRST).await;
    link.command(4, SECOND).await;
    let asked = link.service().await;
    assert_eq!(asked["params"]["type"], SERVICE);
    assert_eq!(
        asked["params"]["target"],
        json!({"workspaceKey": ROOT, "sessionId": SESSION})
    );
    assert_eq!(asked["params"]["params"], json!({"sessionId": SESSION}));
    link.pinged(5).await;
    assert!(link.seen().is_empty(), "{:?}", link.seen());
    assert!(link.replies.is_empty(), "{:?}", link.replies);
    link.h
        .send(json!({"jsonrpc": "2.0", "id": asked["id"],
                     "result": {"executionDirectories": [OTHER_ROOT]}}))
        .await;
    assert_eq!(link.reply(3).await["result"]["kind"], "accepted");
    assert_eq!(link.reply(4).await["result"]["kind"], "accepted");
    assert_eq!(
        link.seen(),
        [
            opened(&[OTHER_ROOT]),
            Seen::Command(FIRST.to_owned()),
            Seen::Command(SECOND.to_owned()),
        ]
    );
}

/// Host 没告知这个服务（旧 Host）：不发请求，照没有附加目录打开（同以前）。
#[tokio::test]
async fn without_the_declared_service_the_session_opens_without_directories() {
    let mut link = Link::new(&[]).await;
    link.command(3, FIRST).await;
    assert_eq!(link.reply(3).await["result"]["kind"], "accepted");
    assert!(link.services.is_empty(), "{:?}", link.services);
    assert_eq!(link.seen(), [opened(&[]), Seen::Command(FIRST.to_owned())]);
}

/// Host 的失败（`internal.failed`）：照没有附加目录打开，命令照常。
#[tokio::test]
async fn a_host_failure_opens_the_session_without_directories() {
    let mut link = Link::new(&[SERVICE]).await;
    link.command(3, FIRST).await;
    let asked = link.service().await;
    link.h
        .send(json!({"jsonrpc": "2.0", "id": asked["id"],
                     "error": {"code": -32603, "message": "project.host-mismatch"}}))
        .await;
    assert_eq!(link.reply(3).await["result"]["kind"], "accepted");
    assert_eq!(link.seen(), [opened(&[]), Seen::Command(FIRST.to_owned())]);
}

/// 严格链路上回复多一个键（对端违约）：照没有附加目录打开。
#[tokio::test]
async fn a_reply_off_the_strict_schema_opens_the_session_without_directories() {
    let mut link = Link::new(&[SERVICE]).await;
    link.command(3, FIRST).await;
    let asked = link.service().await;
    link.h
        .send(json!({"jsonrpc": "2.0", "id": asked["id"],
                     "result": {"executionDirectories": [OTHER_ROOT], "extra": true}}))
        .await;
    assert_eq!(link.reply(3).await["result"]["kind"], "accepted");
    assert_eq!(link.seen(), [opened(&[]), Seen::Command(FIRST.to_owned())]);
}

/// 建会话之后接上：不问 Host（建会话带的就是 Host 预留时算的），之后的命令也不问。
#[tokio::test]
async fn a_created_session_does_not_ask_the_host() {
    let mut link = Link::linked(&[SERVICE]).await;
    link.h
        .send(
            json!({"jsonrpc": "2.0", "id": 3, "method": "command", "params": {
            "commandId": FIRST, "type": "session.create",
            "target": {"hostId": "local", "workspaceKey": ROOT},
            "payload": {"sessionId": SESSION, "executionDirectories": [OTHER_ROOT]}}}),
        )
        .await;
    assert_eq!(link.reply(3).await["result"]["kind"], "accepted");
    link.command(4, SECOND).await;
    assert_eq!(link.reply(4).await["result"]["kind"], "accepted");
    assert!(link.services.is_empty(), "{:?}", link.services);
    assert_eq!(link.seen(), [opened(&[]), Seen::Command(SECOND.to_owned())]);
}

/// App 的会话偏好（§48）。
const PREFERENCES: &str = "host.preferences.get";
const OFF: SessionPreferences = SessionPreferences {
    auto_memory: false,
    tool_search: false,
};
const MEMORY_ON: SessionPreferences = SessionPreferences {
    auto_memory: true,
    tool_search: false,
};

impl Link {
    fn starts(&self) -> Vec<Start> {
        self.starts.lock_unpoisoned().clone()
    }

    /// 窗口建会话（`session.create`）。
    async fn create(&mut self, id: u64) {
        self.h
            .send(
                json!({"jsonrpc": "2.0", "id": id, "method": "command", "params": {
                "commandId": FIRST, "type": "session.create",
                "target": {"hostId": "local", "workspaceKey": ROOT},
                "payload": {"sessionId": SESSION}}}),
            )
            .await;
    }

    async fn answer(&mut self, asked: &Value, result: Value) {
        self.h
            .send(json!({"jsonrpc": "2.0", "id": asked["id"], "result": result}))
            .await;
    }
}

/// 建会话之前问 Host 的偏好（外壳是这个会话，参数为空）；回复之前同一个会话的命令不进内核，回复之后建会话带着它、命令按序进。
/// `toolSearchMode` 的 `auto` 也是开（一律本地，app-prefs.md D3）。
#[tokio::test]
async fn a_new_session_waits_for_the_host_preferences_and_keeps_the_commands_in_order() {
    let mut link = Link::linked(&[PREFERENCES]).await;
    link.create(3).await;
    link.command(4, SECOND).await;
    let asked = link.service().await;
    assert_eq!(asked["params"]["type"], PREFERENCES);
    assert_eq!(
        asked["params"]["target"],
        json!({"workspaceKey": ROOT, "sessionId": SESSION})
    );
    assert_eq!(asked["params"]["params"], json!({}));
    link.pinged(5).await;
    assert!(link.starts().is_empty(), "{:?}", link.starts());
    assert!(link.seen().is_empty(), "{:?}", link.seen());
    link.answer(
        &asked,
        json!({"memoryEnabled": false, "toolSearchMode": "auto"}),
    )
    .await;
    assert_eq!(link.reply(3).await["result"]["kind"], "accepted");
    assert_eq!(link.reply(4).await["result"]["kind"], "accepted");
    assert_eq!(
        link.starts()[0],
        Start::Created(SessionPreferences {
            auto_memory: false,
            tool_search: true,
        })
    );
    assert_eq!(link.seen(), [opened(&[]), Seen::Command(SECOND.to_owned())]);
    assert!(link.services.is_empty(), "{:?}", link.services);
}

/// 冷打开同时问目录与偏好（两个请求，各答各的、先答哪个都行）；两个都到了才打开，打开带着两者。
#[tokio::test]
async fn a_cold_open_asks_for_the_directories_and_the_preferences_together() {
    let mut link = Link::new(&[SERVICE, PREFERENCES]).await;
    link.command(3, FIRST).await;
    let first = link.service().await;
    let second = link.service().await;
    let (directories, preferences) = if first["params"]["type"] == SERVICE {
        (first, second)
    } else {
        (second, first)
    };
    assert_eq!(preferences["params"]["type"], PREFERENCES);
    link.answer(
        &preferences,
        json!({"memoryEnabled": true, "toolSearchMode": "off"}),
    )
    .await;
    link.pinged(4).await;
    assert!(link.seen().is_empty(), "{:?}", link.seen());
    link.answer(&directories, json!({"executionDirectories": [OTHER_ROOT]}))
        .await;
    assert_eq!(link.reply(3).await["result"]["kind"], "accepted");
    assert_eq!(link.starts(), [Start::Opened(MEMORY_ON)]);
    assert_eq!(
        link.seen(),
        [opened(&[OTHER_ROOT]), Seen::Command(FIRST.to_owned())]
    );
}

/// Host 没告知偏好的服务：不发请求，建会话与冷打开都按契约的缺省（记忆关、ToolSearch 关），不按内核的缺省（都开）。
#[tokio::test]
async fn without_the_declared_service_a_session_takes_the_contract_defaults() {
    let mut link = Link::linked(&[]).await;
    link.create(3).await;
    assert_eq!(link.reply(3).await["result"]["kind"], "accepted");
    let mut cold = Link::new(&[]).await;
    cold.command(3, FIRST).await;
    assert_eq!(cold.reply(3).await["result"]["kind"], "accepted");
    assert!(link.services.is_empty() && cold.services.is_empty());
    assert_eq!(link.starts()[0], Start::Created(OFF));
    assert_eq!(cold.starts(), [Start::Opened(OFF)]);
}

/// Host 的失败、严格链路上多一个键（对端违约）：同样按契约的缺省，命令照常。
#[tokio::test]
async fn a_host_failure_or_an_off_schema_reply_takes_the_contract_defaults() {
    for result in [
        json!({"error": {"code": -32603, "message": "settings unavailable"}}),
        json!({"result": {"memoryEnabled": true, "toolSearchMode": "local", "extra": true}}),
    ] {
        let mut link = Link::new(&[PREFERENCES]).await;
        link.command(3, FIRST).await;
        let asked = link.service().await;
        let mut reply = json!({"jsonrpc": "2.0", "id": asked["id"]});
        reply
            .as_object_mut()
            .unwrap()
            .extend(result.as_object().unwrap().clone());
        link.h.send(reply).await;
        assert_eq!(link.reply(3).await["result"]["kind"], "accepted");
        assert_eq!(link.starts(), [Start::Opened(OFF)]);
    }
}

/// 契约的缺省就是 `CONTRACT_DEFAULT_PREFERENCES`：Host 一项都没给（`{}`）时读出来的偏好与它相同；不认识的模式是契约的降级值 off。
#[test]
fn the_contract_defaults_match_the_fallback_preferences() {
    let read = |value: Value| {
        crate::host_services::preferences_of(&serde_json::from_value(value).unwrap())
    };
    assert_eq!(
        read(json!({})),
        crate::host_services::CONTRACT_DEFAULT_PREFERENCES
    );
    assert!(!read(json!({"toolSearchMode": "native"})).tool_search);
    assert!(read(json!({"toolSearchMode": "local"})).tool_search);
}
