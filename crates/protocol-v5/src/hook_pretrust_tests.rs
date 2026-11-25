//! `hooks.trustGrant`（protocol-v5.md §59.2）：命令的校验与拒绝码、ACK 的形状（否定结论是业务结果）、写入不挡链路、与 `mcp.trust.set`
//! 共用一个按顺序的工作任务。写记录与重读 hooks 由真实进程测（一致性场景 `workspace-hook-pretrust`）。
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rust_cli_base::sync::LockUnpoisoned;
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::EnvironmentId;
use rust_cli_kernel_api::testing::MemoryStore;
use serde_json::{Value, json};
use tokio::sync::Notify;

use super::*;
use crate::mcp_trust::{McpDecision, McpPending, McpTrustRefusal, McpTrusted};
use crate::server::{WorkspaceKernel, Workspaces};
use crate::test_link::{Harness, initialize};

const BUNDLE: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
#[cfg(not(windows))]
const KEY: &str = "/Users/dev/repo";
#[cfg(windows)]
const KEY: &str = r"C:\Users\dev\repo";

/// 入口的替身：`pretrust_hooks` 回事先给的结果，记下收到的请求（按到达入口的顺序，两种信任写入记在一起）。有闸门时先等它打开。
struct Pretrust {
    answer: Result<(), HookPretrustRefusal>,
    seen: Mutex<Vec<String>>,
    gate: Option<Notify>,
}

#[async_trait]
impl Workspaces for Pretrust {
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
        _key: &str,
        decision: McpDecision,
    ) -> Result<McpTrusted, McpTrustRefusal> {
        self.seen
            .lock_unpoisoned()
            .push(format!("mcp {}", decision.server));
        Ok(McpTrusted {
            changed: true,
            pending: Vec::new(),
        })
    }

    async fn pretrust_hooks(
        &self,
        key: &str,
        request: HookPretrust,
    ) -> Result<(), HookPretrustRefusal> {
        if let Some(gate) = &self.gate {
            gate.notified().await;
        }
        self.seen.lock_unpoisoned().push(format!(
            "hooks {key} {} {}",
            &request.bundle[..1],
            &request.digest[..1]
        ));
        self.answer.clone()
    }
}

fn pretrust(answer: Result<(), HookPretrustRefusal>, gated: bool) -> Arc<Pretrust> {
    Arc::new(Pretrust {
        answer,
        seen: Mutex::default(),
        gate: gated.then(Notify::new),
    })
}

fn command(id: u64, target: Value, payload: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "command", "params": {
        "commandId": format!("c{id}"), "type": "hooks.trustGrant", "target": target, "payload": payload,
    }})
}

fn target() -> Value {
    json!({"hostId": "local", "workspaceKey": KEY})
}

fn payload() -> Value {
    json!({"bundleDigest": BUNDLE, "hookDeclarationDigest": DIGEST})
}

fn request(id: u64, method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

async fn ready(entry: Option<Arc<Pretrust>>) -> Harness {
    let mut h = Harness::new();
    if let Some(entry) = entry {
        h.handle.ready(
            entry,
            Arc::new(MemoryStore::default()),
            Arc::new(rust_cli_kernel_api::testing::MemoryArtifacts::default()),
        );
    }
    h.send(initialize()).await;
    h.next().await;
    h
}

async fn run(entry: Option<Arc<Pretrust>>, message: Value) -> Value {
    let mut h = ready(entry).await;
    h.send(message).await;
    h.reply(2, &mut Vec::new()).await["result"].clone()
}

#[tokio::test]
async fn an_accepted_grant_is_acked_with_the_business_result() {
    let entry = pretrust(Ok(()), false);
    let result = run(Some(entry.clone()), command(2, target(), payload())).await;
    assert_eq!(result["kind"], "accepted", "{result}");
    assert_eq!(result["noop"], false);
    assert_eq!(result["effects"], json!([]));
    assert_eq!(result["result"], json!({"accepted": true}));
    assert_eq!(
        entry.seen.lock_unpoisoned().clone(),
        [format!("hooks {KEY} b a")]
    );
}

/// 否定结论是业务结果（契约 `workspace-hooks.ts`）：`accepted`，结果里 `accepted: false` 与公开的原因码，什么都没写（noop）。
#[tokio::test]
async fn a_refused_grant_is_accepted_with_the_reason_code() {
    let entry = pretrust(Err(HookPretrustRefusal::WorkspaceHooksBundleChanged), false);
    let result = run(Some(entry), command(2, target(), payload())).await;
    assert_eq!(result["kind"], "accepted", "{result}");
    assert_eq!(result["noop"], true);
    assert_eq!(
        result["result"],
        json!({"accepted": false, "reasonCode": "workspace_hooks_bundle_changed"})
    );
}

#[tokio::test]
async fn the_command_is_checked_before_the_entry_sees_it() {
    let entry = pretrust(Ok(()), false);
    let missing = run(
        Some(entry.clone()),
        command(2, json!({"hostId": "local"}), payload()),
    )
    .await;
    assert_eq!(
        missing["code"], "precondition.workspaceRequired",
        "{missing}"
    );
    let relative = run(
        Some(entry.clone()),
        command(
            2,
            json!({"hostId": "local", "workspaceKey": "repo"}),
            payload(),
        ),
    )
    .await;
    assert_eq!(relative["code"], "precondition.invalid", "{relative}");
    let mut upper = payload();
    upper["hookDeclarationDigest"] = json!(DIGEST.to_uppercase());
    let bad = run(Some(entry.clone()), command(2, target(), upper)).await;
    assert_eq!(bad["code"], "precondition.invalidPayload", "{bad}");
    assert_eq!(bad["field"], "hookDeclarationDigest");
    let mut extra = payload();
    extra["workspace"] = json!({"workspacePath": KEY});
    let unknown = run(Some(entry.clone()), command(2, target(), extra)).await;
    assert_eq!(unknown["code"], "precondition.invalidPayload", "{unknown}");
    assert!(entry.seen.lock_unpoisoned().is_empty());
    // 进程级部件还没建好
    let early = run(None, command(2, target(), payload())).await;
    assert_eq!(early["code"], "precondition.storeUnavailable", "{early}");
}

/// 写入在等锁时 actor 照常处理别的请求；排在它后面的 `mcp.trust.set` 等它做完才进入口（同一个按顺序的工作任务，17 §19.6）。
#[tokio::test]
async fn a_held_grant_does_not_hold_the_link_and_later_trust_writes_wait_in_order() {
    let entry = pretrust(Ok(()), true);
    let mut h = ready(Some(entry.clone())).await;
    h.send(command(2, target(), payload())).await;
    h.send(
        json!({"jsonrpc": "2.0", "id": 3, "method": "command", "params": {
            "commandId": "c3", "type": "mcp.trust.set", "target": target(),
            "payload": {"serverName": "srv", "declarationDigest": DIGEST, "trusted": true},
        }}),
    )
    .await;
    h.send(request(4, "ping", json!({}))).await;
    let first = h.next().await;
    assert_eq!(first["id"], 4, "{first}");
    assert!(entry.seen.lock_unpoisoned().is_empty());
    entry.gate.as_ref().unwrap().notify_one();
    let grant = h.reply(2, &mut Vec::new()).await;
    assert_eq!(grant["result"]["result"]["accepted"], true, "{grant}");
    let mcp = h.reply(3, &mut Vec::new()).await;
    assert_eq!(mcp["result"]["kind"], "accepted", "{mcp}");
    assert_eq!(
        entry.seen.lock_unpoisoned().clone(),
        [format!("hooks {KEY} b a"), "mcp srv".to_owned()]
    );
}

#[tokio::test]
async fn shutdown_waits_for_a_grant_in_flight() {
    let entry = pretrust(Ok(()), true);
    let mut h = ready(Some(entry.clone())).await;
    h.send(command(2, target(), payload())).await;
    h.send(request(3, "ping", json!({}))).await;
    h.reply(3, &mut Vec::new()).await;
    h.send(request(4, "shutdown", json!({}))).await;
    entry.gate.as_ref().unwrap().notify_one();
    let ack = h.next().await;
    assert_eq!(ack["id"], 2, "{ack}");
    let closed = h.next().await;
    assert_eq!(closed["id"], 4, "{closed}");
}
