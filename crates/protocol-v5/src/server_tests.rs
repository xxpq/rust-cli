//! actor 的进程内测试：一致性场景（真实进程）覆盖不到的时序——释放之后才到的组装完成要丢掉；出站消息过入库的严格 schema。
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::EnvironmentId;
use rust_cli_kernel_api::testing::MemoryStore;
use rust_cli_protocol_core::schema::Node;
use serde_json::{Value, json};
use tokio::sync::Notify;

use super::*;
use crate::generated::contract::CONTRACT_DIGEST;
use crate::test_link::{Harness, initialize, initialize_tolerant};
use crate::topics::{WORKSPACE_AGENT, workspace_topic};

const CORE_SCHEMA: &str = include_str!("../schema/core@1.json");
const AGENT_SCHEMA: &str = include_str!("../schema/agent@1.json");

/// 入库的给 rust 的 schema 里一个具名 schema（编译失败 = 生成器漏过滤了校验器不认识的关键字）。
fn schema(file: &str, name: &str) -> Node {
    let file: Value = serde_json::from_str(file).unwrap();
    Node::compile(&file["schemas"][name]).unwrap_or_else(|error| panic!("{name}: {error}"))
}

#[test]
fn every_generated_schema_compiles_in_the_rust_validator() {
    for file in [CORE_SCHEMA, AGENT_SCHEMA] {
        let parsed: Value = serde_json::from_str(file).unwrap();
        for (name, node) in parsed["schemas"].as_object().unwrap() {
            Node::compile(node).unwrap_or_else(|error| panic!("{name}: {error}"));
        }
        // 降级值与不透明位置在 schema 之外（校验器看不到）
        assert!(parsed["fallbacks"].is_object());
        assert!(parsed["opaque"].is_array());
    }
}

/// 打开要等闸门、记次数的工作区端口。
struct Gated {
    gate: Notify,
    opened: AtomicUsize,
}

#[async_trait]
impl Workspaces for Gated {
    async fn open(&self, _key: &str) -> Result<crate::server::WorkspaceKernel, String> {
        self.gate.notified().await;
        self.opened.fetch_add(1, Ordering::SeqCst);
        Ok(crate::test_link::kernel())
    }

    fn release(&self, _key: &str) -> bool {
        true
    }

    fn workspace(&self, key: &str) -> WorkspaceRef {
        WorkspaceRef {
            environment: EnvironmentId::new("local"),
            root: EnvPath::new(key),
        }
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

#[tokio::test]
async fn outbound_messages_satisfy_the_strict_schemas() {
    let mut h = Harness::new();
    h.send(initialize()).await;
    let initialized = h.next().await;
    schema(CORE_SCHEMA, "InitializeResultSchema")
        .validate(&initialized["result"], "")
        .unwrap();
    assert_eq!(initialized["result"]["contract"], json!(CONTRACT_DIGEST));
    h.send(json!({"jsonrpc": "2.0", "id": 2, "method": "subscribe", "params": {"sub": "a", "topic": "agent"}}))
        .await;
    let frame = h.next().await;
    schema(CORE_SCHEMA, "FrameSchema")
        .validate(&frame["params"], "")
        .unwrap();
    // 进程级 agent 的状态是一个合法的 AgentState
    let state = &frame["params"]["body"]["snapshot"]["state"];
    schema(AGENT_SCHEMA, "AgentStateSchema")
        .validate(state, "")
        .unwrap();
    let subscribed = h.next().await;
    schema(CORE_SCHEMA, "SubscribedSchema")
        .validate(&subscribed["result"], "")
        .unwrap();
    h.send(json!({"jsonrpc": "2.0", "id": 3, "method": "subscribe", "params": {"sub": "b", "topic": "nowhere"}}))
        .await;
    let error = h.next().await;
    assert_eq!(
        error["error"]["code"],
        json!(crate::consts::PROTOCOL_ERROR_CODE)
    );
    schema(CORE_SCHEMA, "ProtocolErrorSchema")
        .validate(&error["error"]["data"], "")
        .unwrap();
}

#[tokio::test]
async fn an_assembly_that_finishes_after_a_release_is_dropped() {
    let mut h = Harness::new();
    let gated = Arc::new(Gated {
        gate: Notify::new(),
        opened: AtomicUsize::new(0),
    });
    h.handle.ready(
        gated.clone(),
        Arc::new(MemoryStore::default()),
        Arc::new(rust_cli_kernel_api::testing::MemoryArtifacts::default()),
    );
    h.send(initialize()).await;
    h.next().await;
    let key = if cfg!(windows) { r"C:\ws" } else { "/ws" };
    let topic = workspace_topic(key, WORKSPACE_AGENT);
    h.send(json!({"jsonrpc": "2.0", "id": 2, "method": "subscribe", "params": {"sub": "w", "topic": topic}}))
        .await;
    let mut frames = Vec::new();
    h.reply(2, &mut frames).await;
    // 组装还卡着（starting）就释放：效果是 stopped
    h.send(
        json!({"jsonrpc": "2.0", "id": 3, "method": "command", "params": {
        "commandId": "c1", "type": "agent.release",
        "target": {"hostId": "local", "workspaceKey": key}, "payload": {}}}),
    )
    .await;
    let ack = h.reply(3, &mut frames).await;
    assert_eq!(ack["result"]["noop"], json!(false));
    // 放开那次组装：它的完成已经过期，phase 不能回到 ready。单线程运行时里“完成计数加一”与“发完成消息”在同一次 poll，
    // 看到计数时消息已经在 actor 的队列里；actor 先处理内部消息，再处理之后到的 ping
    gated.gate.notify_one();
    while gated.opened.load(Ordering::SeqCst) == 0 {
        tokio::task::yield_now().await;
    }
    h.send(json!({"jsonrpc": "2.0", "id": 4, "method": "ping", "params": {}}))
        .await;
    h.reply(4, &mut frames).await;
    let phases: Vec<Value> = frames
        .iter()
        .filter(|f| f["method"] == json!("frame"))
        .flat_map(|f| {
            let body = &f["params"]["body"];
            if body["kind"] == json!("snapshot") {
                vec![body["snapshot"]["state"]["phase"].clone()]
            } else {
                body["deltas"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|d| d["patch"]["phase"].clone())
                    .collect()
            }
        })
        .collect();
    assert_eq!(gated.opened.load(Ordering::SeqCst), 1);
    assert_eq!(phases.last(), Some(&json!("stopped")), "{phases:?}");
    assert!(!phases.contains(&json!("ready")), "{phases:?}");
}

#[tokio::test]
async fn a_different_contract_is_rejected_with_both_digests_and_the_link_stays_uninitialized() {
    let mut h = Harness::new();
    h.send(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                  "params": {"clientId": "t", "caps": {"core": [1]}, "contract": "0000000000000000"}}))
        .await;
    let error = h.next().await;
    let data = &error["error"]["data"];
    assert_eq!(data["code"], json!("handshake.contractMismatch"));
    assert_eq!(
        data["details"]["contract"],
        json!({"client": "0000000000000000", "server": CONTRACT_DIGEST})
    );
    assert!(data["details"].get("code").is_none());
    h.send(json!({"jsonrpc": "2.0", "id": 2, "method": "ping"}))
        .await;
    assert_eq!(
        h.next().await["error"]["data"]["code"],
        json!("handshake.unauthorized")
    );
}

#[tokio::test]
async fn a_strict_link_rejects_unknown_fields_and_a_tolerant_link_strips_them() {
    let subscribe = json!({"jsonrpc": "2.0", "id": 2, "method": "subscribe",
                           "params": {"sub": "a", "topic": "agent", "extra": 1}});
    let mut strict = Harness::new();
    strict.send(initialize()).await;
    strict.next().await;
    strict.send(subscribe.clone()).await;
    let error = strict.next().await;
    assert_eq!(
        error["error"]["data"]["code"],
        json!("link.protocolViolation")
    );
    assert_eq!(error["error"]["data"]["outcome"], json!("not_applied"));
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("extra"),
        "{error}"
    );

    let mut tolerant = Harness::new();
    tolerant.send(initialize_tolerant()).await;
    tolerant.next().await;
    tolerant.send(subscribe).await;
    let mut frames = Vec::new();
    let reply = tolerant.reply(2, &mut frames).await;
    assert_eq!(reply["result"]["sub"], json!("a"));
}

#[tokio::test]
async fn a_bad_credit_on_a_strict_link_ends_the_link_with_exit() {
    let mut h = Harness::new();
    h.send(initialize()).await;
    h.next().await;
    h.send(
        json!({"jsonrpc": "2.0", "method": "credit", "params": {"sub": "a", "n": 1, "extra": 1}}),
    )
    .await;
    let exit = h.next().await;
    assert_eq!(exit["method"], json!("exit"));
    assert_eq!(exit["params"], json!({"reason": "protocol_violation"}));
}

#[tokio::test]
async fn after_shutdown_requests_are_violations_and_no_frame_follows_the_reply() {
    let mut h = Harness::new();
    h.send(initialize()).await;
    h.next().await;
    h.send(json!({"jsonrpc": "2.0", "id": 2, "method": "subscribe", "params": {"sub": "a", "topic": "agent"}}))
        .await;
    let mut frames = Vec::new();
    h.reply(2, &mut frames).await;
    h.send(json!({"jsonrpc": "2.0", "id": 3, "method": "shutdown", "params": {}}))
        .await;
    assert_eq!(h.reply(3, &mut frames).await["result"], json!({}));
    // 关闭中的发布不挂新标记（03 §3.4 ②）：存储的启动事实变了，也不再有帧
    h.handle.storage(Storage::Ready);
    h.send(json!({"jsonrpc": "2.0", "id": 4, "method": "ping"}))
        .await;
    let next = h.next().await;
    assert_eq!(next["id"], json!(4), "{next}");
    assert_eq!(
        next["error"]["data"]["code"],
        json!("link.protocolViolation")
    );
}
