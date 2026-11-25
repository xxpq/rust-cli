//! `mcp.trust.set` 与 `mcpAdmission`（18-mcp-trust.md §12.5、§12.6）：命令的校验与拒绝码、结果的 noop、提示的形状。活会话的
//! `mcpAdmission` 帧排在 ACK 之前由真实进程测（`crates/cli/tests/app_project_mcp.rs`）：进程内的内核替身没有会话。
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rust_cli_base::sync::LockUnpoisoned;
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::EnvironmentId;
use rust_cli_kernel_api::testing::MemoryStore;
use serde_json::{Value, json};
use tokio::sync::Notify;

use super::*;
use crate::server::{WorkspaceKernel, Workspaces};
use crate::test_link::{Harness, initialize};

const DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
#[cfg(not(windows))]
const KEY: &str = "/Users/dev/repo";
#[cfg(windows)]
const KEY: &str = r"C:\Users\dev\repo";

/// 入口的替身：`trust_mcp` 回事先给的结果，记下收到的决定。有闸门时先等它打开（替身里的“锁被别的写者拿着”）。
struct Trust {
    answer: Result<McpTrusted, McpTrustRefusal>,
    decisions: Mutex<Vec<(String, McpDecision)>>,
    gate: Option<Notify>,
}

#[async_trait]
impl Workspaces for Trust {
    async fn open(&self, _key: &str) -> Result<WorkspaceKernel, String> {
        Ok(crate::test_link::kernel())
    }

    fn release(&self, _key: &str) -> bool {
        false
    }

    fn workspace(&self, key: &str) -> WorkspaceRef {
        WorkspaceRef {
            environment: EnvironmentId::new("local"),
            root: EnvPath::new(key),
        }
    }

    async fn mcp_pending(&self, _key: &str) -> Vec<McpPending> {
        Vec::new()
    }

    async fn trust_mcp(
        &self,
        key: &str,
        decision: McpDecision,
    ) -> Result<McpTrusted, McpTrustRefusal> {
        self.decisions
            .lock_unpoisoned()
            .push((key.to_owned(), decision));
        if let Some(gate) = &self.gate {
            gate.notified().await;
        }
        self.answer.clone()
    }
}

fn command(target: Value, payload: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": 2, "method": "command", "params": {
        "commandId": "c1", "type": "mcp.trust.set", "target": target, "payload": payload,
    }})
}

fn target() -> Value {
    json!({"hostId": "local", "workspaceKey": KEY})
}

fn payload() -> Value {
    json!({"serverName": "evil", "declarationDigest": DIGEST, "trusted": true})
}

/// 握手、（可选地）报进程级部件建好，发一条命令，回它的结果。
async fn run(trust: Option<Arc<Trust>>, message: Value) -> Value {
    let mut h = Harness::new();
    if let Some(trust) = trust {
        h.handle.ready(
            trust,
            Arc::new(MemoryStore::default()),
            Arc::new(rust_cli_kernel_api::testing::MemoryArtifacts::default()),
        );
    }
    h.send(initialize()).await;
    h.next().await;
    h.send(message).await;
    let reply = h.reply(2, &mut Vec::new()).await;
    reply["result"].clone()
}

fn answering(answer: Result<McpTrusted, McpTrustRefusal>) -> Arc<Trust> {
    Arc::new(Trust {
        answer,
        decisions: Mutex::default(),
        gate: None,
    })
}

/// 写入卡在闸门上的入口（成功、没有待信任的声明）。
fn held() -> Arc<Trust> {
    Arc::new(Trust {
        answer: Ok(McpTrusted {
            changed: true,
            pending: Vec::new(),
        }),
        decisions: Mutex::default(),
        gate: Some(Notify::new()),
    })
}

/// 握手、报进程级部件建好。
async fn ready(trust: Arc<Trust>) -> Harness {
    let mut h = Harness::new();
    h.handle.ready(
        trust,
        Arc::new(MemoryStore::default()),
        Arc::new(rust_cli_kernel_api::testing::MemoryArtifacts::default()),
    );
    h.send(initialize()).await;
    h.next().await;
    h
}

fn request(id: u64, method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

/// 写入在等锁时，actor 照常处理别的请求（18 §12.6）：`ping` 先得到回复，写入完成之后才有这条命令的 ACK。
///
/// 缺陷修复（主会话审查 MCP-TRUST-A）：以前 actor 在 `mcp.trust.set` 上等入口写完，入口要等 Node 的目录锁（最多 8 s），这条链路上
/// 全部工作区、全部会话的请求与通道事件都排在后面。
#[tokio::test]
async fn a_held_write_does_not_hold_the_link() {
    let trust = held();
    let mut h = ready(trust.clone()).await;
    h.send(command(target(), payload())).await;
    h.send(request(3, "ping", json!({}))).await;
    let first = h.next().await;
    assert_eq!(first["id"], 3, "{first}");
    trust.gate.as_ref().unwrap().notify_one();
    let ack = h.reply(2, &mut Vec::new()).await;
    assert_eq!(ack["result"]["kind"], "accepted", "{ack}");
}

/// `shutdown` 时还有写入在途：它的回复排在这条命令的 ACK 之后（同通道里的会话命令，protocol-v5.md §15.9）。
#[tokio::test]
async fn shutdown_waits_for_a_trust_write_in_flight() {
    let trust = held();
    let mut h = ready(trust.clone()).await;
    h.send(command(target(), payload())).await;
    // 先让写入真的进了入口，再发 shutdown
    h.send(request(3, "ping", json!({}))).await;
    h.reply(3, &mut Vec::new()).await;
    h.send(request(4, "shutdown", json!({}))).await;
    trust.gate.as_ref().unwrap().notify_one();
    let ack = h.next().await;
    assert_eq!(ack["id"], 2, "{ack}");
    assert_eq!(ack["result"]["kind"], "accepted", "{ack}");
    let closed = h.next().await;
    assert_eq!(closed["id"], 4, "{closed}");
}

#[tokio::test]
async fn a_trust_decision_reaches_the_entry_and_a_settled_record_is_a_noop() {
    let trust = answering(Ok(McpTrusted {
        changed: true,
        pending: Vec::new(),
    }));
    let result = run(Some(trust.clone()), command(target(), payload())).await;
    assert_eq!(result["kind"], "accepted", "{result}");
    assert_eq!(result["noop"], false);
    // 这个工作区没有活会话：没有效果帧
    assert_eq!(result["effects"], json!([]));
    assert_eq!(
        trust.decisions.lock_unpoisoned().clone(),
        [(
            KEY.to_owned(),
            McpDecision {
                server: "evil".to_owned(),
                digest: DIGEST.to_owned(),
                trusted: true,
            }
        )]
    );
    // 结果已是现状（重放）：noop
    let settled = answering(Ok(McpTrusted {
        changed: false,
        pending: Vec::new(),
    }));
    let again = run(Some(settled), command(target(), payload())).await;
    assert_eq!(
        (again["kind"].clone(), again["noop"].clone()),
        (json!("accepted"), json!(true))
    );
}

#[tokio::test]
async fn refusals_map_to_the_precondition_codes() {
    let changed = run(
        Some(answering(Err(McpTrustRefusal::DeclarationChanged))),
        command(target(), payload()),
    )
    .await;
    assert_eq!(
        changed["code"], "precondition.mcpDeclarationChanged",
        "{changed}"
    );
    let unavailable = run(
        Some(answering(Err(McpTrustRefusal::Unavailable(
            "corrupt".to_owned(),
        )))),
        command(target(), payload()),
    )
    .await;
    assert_eq!(
        unavailable["code"], "precondition.mcpTrustUnavailable",
        "{unavailable}"
    );
    assert_eq!(unavailable["message"], "corrupt");
}

#[tokio::test]
async fn the_command_is_checked_before_the_entry_sees_it() {
    let trust = answering(Ok(McpTrusted {
        changed: true,
        pending: Vec::new(),
    }));
    let missing = run(
        Some(trust.clone()),
        command(json!({"hostId": "local"}), payload()),
    )
    .await;
    assert_eq!(
        missing["code"], "precondition.workspaceRequired",
        "{missing}"
    );
    let relative = run(
        Some(trust.clone()),
        command(
            json!({"hostId": "local", "workspaceKey": "repo"}),
            payload(),
        ),
    )
    .await;
    assert_eq!(relative["code"], "precondition.invalid", "{relative}");
    // 参数严格：多出的字段、大写的 digest 都拒绝，并指出字段
    let mut extra = payload();
    extra["workspace"] = json!({"workspacePath": KEY});
    let unknown = run(Some(trust.clone()), command(target(), extra)).await;
    assert_eq!(unknown["code"], "precondition.invalidPayload", "{unknown}");
    assert_eq!(unknown["field"], "workspace");
    let mut upper = payload();
    upper["declarationDigest"] = json!(DIGEST.to_uppercase());
    let bad = run(Some(trust.clone()), command(target(), upper)).await;
    assert_eq!(bad["field"], "declarationDigest", "{bad}");
    assert!(trust.decisions.lock_unpoisoned().is_empty());
    // 进程级部件还没建好
    let early = run(None, command(target(), payload())).await;
    assert_eq!(early["code"], "precondition.storeUnavailable", "{early}");
}

#[test]
fn the_admission_is_null_without_pending_declarations() {
    assert_eq!(admission(&[]), Value::Null);
    let pending = |server: &str, digest: &str| McpPending {
        server: server.to_owned(),
        digest: digest.to_owned(),
    };
    let one = admission(&[pending("b", "2"), pending("a", "1")]);
    let other = admission(&[pending("a", "1"), pending("b", "2")]);
    assert_eq!(one["pendingCount"], 2);
    // 按名字排好再算：同一组声明同一个指纹
    assert_eq!(one, other);
    assert_eq!(one["bundleDigest"], sha256_hex(br#"[["a","1"],["b","2"]]"#));
}
