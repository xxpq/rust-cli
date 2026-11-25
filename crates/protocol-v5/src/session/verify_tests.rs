//! 前端持有的 `requestVerification`（protocol-v5.md §35；21 §13.4 的竞态）：真实的 actor 与 Content-Length 分帧，测试扮演 Host
//! （握手时告知服务、应答 `service`）与窗口（订会话 topic 与待校验的索引、发 `interaction.resolve`）。内核是只给出视图的替身：
//! 这里只测前端的交互，内核的回合不在里面。
use std::sync::Arc;

use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use rust_cli_base::models::GLM_5_3_ACCOUNT;
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{EnvironmentId, SessionId, TraceId};
use rust_cli_kernel_api::model::{ModelError, ModelSelection};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::provider::{HeaderRequest, SuppliedAuth};
use rust_cli_kernel_api::session_api::{
    Ack, CommandEnvelope, CreateSession, Kernel, KernelError, SequencedEvent, SessionHandle,
    SessionView,
};
use rust_cli_kernel_api::store::{
    SessionFilter, SessionKind, SessionMeta, SessionSummary, StoreQueries,
};
use rust_cli_kernel_api::testing::{FixedClock, HandleOverride, MemoryStore};
use serde_json::{Value, json};
use tokio::task::JoinHandle;

use crate::consts::REQUEST_AUTH_LIMIT;
use crate::generated::contract::CONTRACT_DIGEST;
use crate::server::{WorkspaceKernel, Workspaces};
use crate::test_link::Harness;
use crate::topics::{WORKSPACE_VERIFICATIONS, session_topic, workspace_topic};

const ROOT: &str = "/Users/dev/app";
const SESSION: &str = "sess_1";
const NOW: u64 = 1_800_000_000_000;
const SERVICE: &str = "host.providers.requestAuth";
const COMMAND_V7: &str = "019a5f3c-7a2b-7c3d-8e4f-0000000000c1";
const SECOND_V7: &str = "019a5f3c-7a2b-7c3d-8e4f-0000000000c2";

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

/// 打开着的会话：给出视图，事件流不结束（没有回合）。
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
        Ok(view())
    }
}

/// 有一个会话的内核：冷读与打开都给同一份视图。
struct OneSession(Arc<MemoryStore>);

#[async_trait::async_trait]
impl Kernel for OneSession {
    async fn create_session(&self, _request: CreateSession) -> Result<SessionId, KernelError> {
        Err(KernelError::Unsupported {
            what: "creating sessions".to_owned(),
        })
    }

    async fn open_session(
        &self,
        _session: &SessionId,
    ) -> Result<Arc<dyn SessionHandle>, KernelError> {
        Ok(Arc::new(Open(SessionId::new(SESSION))))
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

    async fn delete_session(&self, _session: &SessionId) -> Result<(), KernelError> {
        Ok(())
    }
}

struct Local;

#[async_trait::async_trait]
impl Workspaces for Local {
    async fn open(&self, _key: &str) -> Result<WorkspaceKernel, String> {
        Ok(WorkspaceKernel {
            kernel: Arc::new(OneSession(Arc::default())),
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

/// 一次运行里各帧按 sub 收好的样子，与还没取走的请求、回复。
struct Link {
    h: Harness,
    frames: Vec<Value>,
    services: Vec<Value>,
    replies: Vec<Value>,
}

impl Link {
    /// 握手（告知 requestAuth）、订会话 topic（s）与这个工作区的待校验索引（i），等两份快照。
    async fn new() -> Self {
        let mut h = Harness::new();
        h.handle.ready(
            Arc::new(Local),
            Arc::new(MemoryStore::default()),
            Arc::new(rust_cli_kernel_api::testing::MemoryArtifacts::default()),
        );
        h.send(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                      "params": {"clientId": "host", "caps": {"core": [1], "agent": [1], "session": [1]},
                                 "contract": CONTRACT_DIGEST, "hostServices": [SERVICE]}}))
            .await;
        assert!(h.next().await["result"].is_object());
        let mut link = Self {
            h,
            frames: Vec::new(),
            services: Vec::new(),
            replies: Vec::new(),
        };
        link.subscribe(2, "s", &session_topic(ROOT, SESSION)).await;
        link.subscribe(3, "i", &workspace_topic(ROOT, WORKSPACE_VERIFICATIONS))
            .await;
        link
    }

    async fn subscribe(&mut self, id: u64, sub: &str, topic: &str) {
        self.h
            .send(json!({"jsonrpc": "2.0", "id": id, "method": "subscribe",
                         "params": {"sub": sub, "topic": topic}}))
            .await;
        self.reply(id).await;
    }

    /// 读一条消息，按种类收好（帧还信用，读循环不卡）。
    async fn pump(&mut self) {
        let message = self.h.next().await;
        if message["method"] == "frame" {
            let sub = message["params"]["sub"].clone();
            self.h
                .send(json!({"jsonrpc": "2.0", "method": "credit", "params": {"sub": sub, "n": 1}}))
                .await;
            self.frames.push(message);
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

    async fn answer(&mut self, asked: &Value, result: Value) {
        self.h
            .send(json!({"jsonrpc": "2.0", "id": asked["id"], "result": result}))
            .await;
    }

    /// 等到 `sub` 上出现一个满足 `op` 的增量，返回它。
    async fn delta(&mut self, sub: &str, op: &str, col: &str) -> Value {
        loop {
            let found = self.frames.iter().find_map(|frame| {
                if frame["params"]["sub"] != sub {
                    return None;
                }
                frame["params"]["body"]["deltas"]
                    .as_array()?
                    .iter()
                    .find(|delta| delta["op"] == op && delta["col"] == col)
                    .cloned()
            });
            if let Some(delta) = found {
                self.frames.retain(|frame| frame["params"]["sub"] != sub);
                return delta;
            }
            self.pump().await;
        }
    }

    async fn resolve(&mut self, id: u64, command: &str, entity: &str, answer: Value) -> Value {
        self.h
            .send(
                json!({"jsonrpc": "2.0", "id": id, "method": "command", "params": {
                "commandId": command, "type": "interaction.resolve",
                "target": {"hostId": "local", "workspaceKey": ROOT, "sessionId": SESSION},
                // v 故意不对：前端持有的校验按 id 裁决（§35.2）
                "expect": {"col": "interactions", "id": entity, "v": 999},
                "payload": {"answer": answer}}}),
            )
            .await;
        self.reply(id).await
    }

    fn ask(&self) -> JoinHandle<Result<SuppliedAuth, ModelError>> {
        let credentials = self.h.handle.credentials(Arc::new(FixedClock(NOW)));
        tokio::spawn(async move { credentials.request_auth(request()).await })
    }

    /// 第一次取凭据回 needsUser，等实体与索引行出现，返回实体 id。
    async fn needs_user(&mut self) -> String {
        let asked = self.service().await;
        self.answer(
            &asked,
            json!({"kind": "needsUser", "challenge": {"scene": "s1"}}),
        )
        .await;
        let put = self.delta("s", "entity.put", "interactions").await;
        let id = put["id"].as_str().unwrap().to_owned();
        let row = self.delta("i", "entity.put", "verifications").await;
        assert_eq!(row["id"], id.as_str());
        id
    }
}

fn request() -> HeaderRequest {
    HeaderRequest {
        selection: ModelSelection::new("account:zai-start-plan", GLM_5_3_ACCOUNT),
        account_type: "zai".into(),
        mode: "start-plan".into(),
        session: Some(SessionId::new(SESSION)),
        root: None,
        rejected: None,
        denied: None,
        execution: None,
    }
}

fn failure(result: Result<SuppliedAuth, ModelError>) -> String {
    match result {
        Err(ModelError::Authentication { message }) => message,
        other => panic!("expected an authentication failure, got {other:?}"),
    }
}

/// needsUser → 会话 topic 的实体与索引行 → 窗口应答 token（`v` 不对也按 id 认）→ 同一批移除 → 第二次取带上 token → 凭据。
#[tokio::test]
async fn the_token_answers_the_check_and_goes_back_to_the_host() {
    let mut link = Link::new().await;
    let asking = link.ask();
    let asked = link.service().await;
    link.answer(
        &asked,
        json!({"kind": "needsUser", "challenge": {"scene": "s1"}}),
    )
    .await;
    let put = link.delta("s", "entity.put", "interactions").await;
    let id = put["id"].as_str().unwrap().to_owned();
    assert!(id.starts_with("rv-"), "{id}");
    assert_eq!(put["data"]["kind"], "requestVerification");
    assert_eq!(
        put["data"]["payload"],
        json!({"provider": "account:zai-start-plan", "challenge": {"scene": "s1"}})
    );
    let deadline = NOW + u64::try_from(REQUEST_AUTH_LIMIT.as_millis()).unwrap();
    assert_eq!(put["data"]["expiresAt"], deadline);
    let row = link.delta("i", "entity.put", "verifications").await;
    assert_eq!(
        row["data"],
        json!({"sessionId": SESSION, "provider": "account:zai-start-plan",
               "challenge": {"scene": "s1"}, "expiresAt": deadline})
    );
    let ack = link
        .resolve(10, COMMAND_V7, &id, json!({"token": "tok"}))
        .await;
    assert_eq!(ack["result"]["kind"], "accepted", "{ack}");
    assert_eq!(
        link.delta("s", "entity.remove", "interactions").await["id"],
        id.as_str()
    );
    assert_eq!(
        link.delta("i", "entity.remove", "verifications").await["id"],
        id.as_str()
    );
    let second = link.service().await;
    let params = &second["params"]["params"];
    assert_eq!(params["verificationToken"], "tok");
    assert!(params.get("denied").is_none());
    link.answer(
        &second,
        json!({"kind": "headers", "headers": {"x-check": "ok"}, "apiKey": "sk-1"}),
    )
    .await;
    let auth = asking.await.unwrap().unwrap();
    assert_eq!(auth.api_key.unwrap().expose(), "sk-1");
    assert_eq!(auth.headers[0].0, "x-check");
    // 带着校验结果换来的凭据只能用一次：模型层按厂商的规则解释它的空回复（provider-errors.md §3.7）
    assert!(auth.single_use);
    // 先到者胜：同一个实体的第二个应答
    let late = link
        .resolve(11, SECOND_V7, &id, json!({"token": "again"}))
        .await;
    assert_eq!(
        late["result"]["code"], "precondition.alreadyResolved",
        "{late}"
    );
}

/// 校验没做成：说明就是这次取凭据的失败文字，不再取第二次。
#[tokio::test]
async fn a_failure_answer_ends_the_request_with_its_message() {
    let mut link = Link::new().await;
    let asking = link.ask();
    let id = link.needs_user().await;
    let ack = link
        .resolve(
            10,
            COMMAND_V7,
            &id,
            json!({"failure": "the check script did not load"}),
        )
        .await;
    assert_eq!(ack["result"]["kind"], "accepted", "{ack}");
    assert_eq!(
        failure(asking.await.unwrap()),
        "the check script did not load"
    );
    assert!(link.services.is_empty());
}

/// 应答的形状：`token`、`failure` 恰好一个，否则 invalidPayload，实体留着。
#[tokio::test]
async fn an_answer_with_both_or_neither_is_refused_and_the_check_stays() {
    let mut link = Link::new().await;
    let _asking = link.ask();
    let id = link.needs_user().await;
    for (n, answer) in [json!({}), json!({"token": "t", "failure": "f"})]
        .into_iter()
        .enumerate()
    {
        let command = format!("019a5f3c-7a2b-7c3d-8e4f-00000000001{n}");
        let refused = link.resolve(20 + n as u64, &command, &id, answer).await;
        assert_eq!(
            refused["result"]["code"], "precondition.invalidPayload",
            "{refused}"
        );
    }
    let ack = link
        .resolve(30, COMMAND_V7, &id, json!({"token": "tok"}))
        .await;
    assert_eq!(ack["result"]["kind"], "accepted", "{ack}");
}

/// 回合停了（取凭据的任务被丢掉）：守卫撤回实体与索引行；之后的应答 `alreadyResolved`（21 §13.4 第 10 行）。
#[tokio::test]
async fn a_dropped_request_withdraws_the_check() {
    let mut link = Link::new().await;
    let asking = link.ask();
    let id = link.needs_user().await;
    asking.abort();
    assert_eq!(
        link.delta("s", "entity.remove", "interactions").await["id"],
        id.as_str()
    );
    assert_eq!(
        link.delta("i", "entity.remove", "verifications").await["id"],
        id.as_str()
    );
    let late = link
        .resolve(10, COMMAND_V7, &id, json!({"token": "tok"}))
        .await;
    assert_eq!(
        late["result"]["code"], "precondition.alreadyResolved",
        "{late}"
    );
}

/// 到了截止时刻：任务的等待超时、守卫撤回，失败文字说校验没有在时限内完成（虚拟时钟）。
#[tokio::test(start_paused = true)]
async fn the_deadline_withdraws_the_check() {
    let mut link = Link::new().await;
    let asking = link.ask();
    let id = link.needs_user().await;
    tokio::time::advance(REQUEST_AUTH_LIMIT).await;
    assert!(failure(asking.await.unwrap()).contains("not completed in time"));
    assert_eq!(
        link.delta("s", "entity.remove", "interactions").await["id"],
        id.as_str()
    );
    assert_eq!(
        link.delta("i", "entity.remove", "verifications").await["id"],
        id.as_str()
    );
}

/// 带着 token 还回 needsUser：鉴权失败，不开第二个交互（21 §6 第 15 行）。
#[tokio::test]
async fn needs_user_again_with_a_token_fails_without_a_second_check() {
    let mut link = Link::new().await;
    let asking = link.ask();
    let id = link.needs_user().await;
    link.resolve(10, COMMAND_V7, &id, json!({"token": "tok"}))
        .await;
    let second = link.service().await;
    link.answer(&second, json!({"kind": "needsUser", "challenge": {}}))
        .await;
    assert!(failure(asking.await.unwrap()).contains("not accepted"));
    // 移除那一批之后没有新的实体
    link.delta("s", "entity.remove", "interactions").await;
    assert!(
        link.frames
            .iter()
            .all(|frame| frame["params"]["sub"] != "s")
    );
}

/// 投影重建（激活：冷读的投影由活视图重建）时有待回答的校验：实体并进新内容，索引行不变，之后照样能应答（21 §13.4 第 14 行）。
#[tokio::test]
async fn a_rebuilt_projection_keeps_the_pending_check() {
    let mut link = Link::new().await;
    let asking = link.ask();
    let id = link.needs_user().await;
    // config.setModel 激活会话：通道打开会话、取活视图，actor 用它重建投影（选择相同，命令本身是 noop）
    let selection = rust_cli_kernel_api::testing::selection();
    link.h
        .send(
            json!({"jsonrpc": "2.0", "id": 40, "method": "command", "params": {
            "commandId": SECOND_V7, "type": "config.setModel",
            "target": {"hostId": "local", "workspaceKey": ROOT, "sessionId": SESSION},
            "payload": {"provider": selection.provider, "model": selection.model}}}),
        )
        .await;
    link.reply(40).await;
    // 重建确实发生了：冷读的投影换成活视图建的，control.runtime 从 cold 变 ready
    let rebuilt = link.frames.iter().any(|frame| {
        frame["params"]["sub"] == "s"
            && frame["params"]["body"]["deltas"]
                .as_array()
                .is_some_and(|deltas| {
                    deltas
                        .iter()
                        .any(|delta| delta["patch"]["control"]["runtime"] == "ready")
                })
    });
    assert!(rebuilt, "{:?}", link.frames);
    let ack = link
        .resolve(41, COMMAND_V7, &id, json!({"token": "tok"}))
        .await;
    assert_eq!(ack["result"]["kind"], "accepted", "{ack}");
    let second = link.service().await;
    assert_eq!(second["params"]["params"]["verificationToken"], "tok");
    link.answer(
        &second,
        json!({"kind": "headers", "headers": {}, "apiKey": "sk-2"}),
    )
    .await;
    assert!(asking.await.unwrap().is_ok());
    // 索引行没有因为重建而重发
    let index_puts = link
        .frames
        .iter()
        .filter(|frame| frame["params"]["sub"] == "i")
        .flat_map(|frame| {
            frame["params"]["body"]["deltas"]
                .as_array()
                .cloned()
                .unwrap_or_default()
        })
        .filter(|delta| delta["op"] == "entity.put")
        .count();
    assert_eq!(index_puts, 0);
}

/// 晚到的订阅者（窗口重载、应答方后挂上）：索引的快照里有待回答的行（21 §13.4 第 13 行）。
#[tokio::test]
async fn a_late_index_subscriber_gets_the_pending_check_in_its_snapshot() {
    let mut link = Link::new().await;
    let _asking = link.ask();
    let id = link.needs_user().await;
    link.subscribe(50, "late", &workspace_topic(ROOT, WORKSPACE_VERIFICATIONS))
        .await;
    loop {
        if let Some(frame) = link
            .frames
            .iter()
            .find(|frame| frame["params"]["sub"] == "late")
        {
            let rows = &frame["params"]["body"]["snapshot"]["collections"]["verifications"];
            assert_eq!(rows[0]["id"], id.as_str());
            assert_eq!(rows[0]["sessionId"], SESSION);
            return;
        }
        link.pump().await;
    }
}

/// 校验进行中会话被删除：实体与索引行同一次回执里移除，取凭据的任务以「没被应答就结束了」失败（21 §13.4 第 17 行）。
#[tokio::test]
async fn deleting_the_session_ends_the_pending_check() {
    let mut link = Link::new().await;
    let asking = link.ask();
    let id = link.needs_user().await;
    link.h
        .send(
            json!({"jsonrpc": "2.0", "id": 70, "method": "command", "params": {
            "commandId": SECOND_V7, "type": "session.delete",
            "target": {"hostId": "local", "workspaceKey": ROOT, "sessionId": SESSION},
            "payload": {}}}),
        )
        .await;
    assert_eq!(link.reply(70).await["result"]["kind"], "accepted");
    assert_eq!(
        link.delta("s", "entity.remove", "interactions").await["id"],
        id.as_str()
    );
    assert_eq!(
        link.delta("i", "entity.remove", "verifications").await["id"],
        id.as_str()
    );
    assert!(failure(asking.await.unwrap()).contains("ended before it was answered"));
}

/// 会话列表的 live：待回答的校验算进计数，会话因它是 hot（没有回合在跑）；应答之后行移除（§35.2）。
#[tokio::test]
async fn a_pending_check_makes_the_session_hot_in_the_live_list() {
    let mut link = Link::new().await;
    link.subscribe(
        60,
        "l",
        &workspace_topic(ROOT, crate::topics::WORKSPACE_SESSIONS),
    )
    .await;
    let asking = link.ask();
    let id = link.needs_user().await;
    let live = link.delta("l", "entity.put", "live").await;
    assert_eq!(live["id"], SESSION);
    assert_eq!(live["data"]["pendingCount"], 1);
    assert_eq!(live["data"]["pendingInteraction"]["id"], id.as_str());
    assert_eq!(
        live["data"]["pendingInteraction"]["kind"],
        "requestVerification"
    );
    assert!(live["data"]["pendingInteraction"].get("toolName").is_none());
    link.resolve(61, COMMAND_V7, &id, json!({"failure": "no"}))
        .await;
    assert_eq!(
        link.delta("l", "entity.remove", "live").await["id"],
        SESSION
    );
    assert!(asking.await.unwrap().is_err());
}
