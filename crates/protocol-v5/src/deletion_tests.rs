//! 归档会话的永久删除（protocol-v5.md §65）：预检与执行的外壳校验、结果的形状、别的工作区是前置条件的拒绝、删了之后会话列表移除这些会话、
//! 执行不挡链路。内核的语义在 runtime 的 `deletion_tests.rs`，真进程在一致性场景 `session-permanent-delete`。
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rust_cli_base::sync::LockUnpoisoned;
use rust_cli_kernel_api::deletion::{
    DeletionBlock, DeletionOutcome, DeletionPreview, DeletionResult, RetainedArtifact,
    TitledSession,
};
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{EnvironmentId, SessionId};
use rust_cli_kernel_api::session_api::{CreateSession, Kernel, KernelError, SessionHandle};
use rust_cli_kernel_api::store::{
    SessionFilter, SessionKind, SessionMeta, SessionStore, SessionSummary, StoreQueries,
};
use rust_cli_kernel_api::testing::MemoryStore;
use serde_json::{Value, json};
use tokio::sync::Notify;

use crate::mcp_trust::{McpDecision, McpPending, McpTrustRefusal, McpTrusted};
use crate::server::{WorkspaceKernel, Workspaces};
use crate::test_link::{Harness, initialize};

#[cfg(not(windows))]
const KEY: &str = "/Users/dev/repo";
#[cfg(windows)]
const KEY: &str = r"C:\Users\dev\repo";

fn workspace(key: &str) -> WorkspaceRef {
    WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new(key),
    }
}

fn titled(id: &str) -> TitledSession {
    TitledSession {
        session: SessionId::new(id),
        title: format!("{id} title"),
    }
}

/// 内核的替身：预检回给定的阻止，执行回给定的结局；记下执行收到的 revision。有闸门时执行先等它。
struct Deleting {
    store: Arc<MemoryStore>,
    blocked: Option<DeletionBlock>,
    outcome: DeletionOutcome,
    revisions: Mutex<Vec<String>>,
    gate: Option<Notify>,
}

impl Deleting {
    fn preview(&self) -> DeletionPreview {
        DeletionPreview {
            sessions: vec![SessionId::new("beta"), SessionId::new("child")],
            forks: vec![titled("fork")],
            retained: vec![RetainedArtifact {
                session: SessionId::new("beta"),
                artifact: "shared".to_owned(),
                file_name: "shared.png".to_owned(),
                referrers: vec![titled("fork")],
            }],
            deleted_files: 2,
            blocked: self.blocked,
            incomplete: false,
            completed: false,
            revision: "rev-1".to_owned(),
        }
    }
}

#[async_trait]
impl Kernel for Deleting {
    async fn create_session(&self, _request: CreateSession) -> Result<SessionId, KernelError> {
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

    async fn deletion_preview(
        &self,
        _root: &SessionId,
        _workspace: &WorkspaceRef,
    ) -> Result<DeletionPreview, KernelError> {
        Ok(self.preview())
    }

    async fn delete_permanently(
        &self,
        _root: &SessionId,
        _workspace: &WorkspaceRef,
        revision: &str,
        operation: &str,
    ) -> Result<DeletionResult, KernelError> {
        if let Some(gate) = &self.gate {
            gate.notified().await;
        }
        self.revisions
            .lock_unpoisoned()
            .push(format!("{revision} {operation}"));
        Ok(DeletionResult {
            outcome: self.outcome.clone(),
            sessions: self.preview().sessions,
            retained: 1,
        })
    }
}

struct Entry(Arc<Deleting>);

#[async_trait]
impl Workspaces for Entry {
    async fn open(&self, _key: &str) -> Result<WorkspaceKernel, String> {
        Ok(WorkspaceKernel {
            kernel: self.0.clone(),
            selection: rust_cli_kernel_api::testing::selection(),
            mode: rust_cli_kernel_api::permission::PermissionMode::Default,
        })
    }

    fn release(&self, _key: &str) -> bool {
        false
    }

    fn workspace(&self, key: &str) -> WorkspaceRef {
        workspace(key)
    }

    async fn mcp_pending(&self, _key: &str) -> Vec<McpPending> {
        Vec::new()
    }

    async fn trust_mcp(
        &self,
        _key: &str,
        _decision: McpDecision,
    ) -> Result<McpTrusted, McpTrustRefusal> {
        Err(McpTrustRefusal::Unavailable("unused".to_owned()))
    }
}

fn deleting(
    blocked: Option<DeletionBlock>,
    outcome: DeletionOutcome,
    gated: bool,
) -> Arc<Deleting> {
    Arc::new(Deleting {
        store: Arc::new(MemoryStore::default()),
        blocked,
        outcome,
        revisions: Mutex::default(),
        gate: gated.then(Notify::new),
    })
}

fn target() -> Value {
    json!({"hostId": "local", "workspaceKey": KEY})
}

fn preview_query(id: u64, target: Value, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "query", "params": {
        "type": "sessionData.deletionPreview", "target": target, "params": params,
    }})
}

fn execute(id: u64, target: Value, payload: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "command", "params": {
        "commandId": format!("c{id}"), "type": "sessionData.deletionExecute", "target": target, "payload": payload,
    }})
}

async fn ready(kernel: Option<Arc<Deleting>>) -> Harness {
    let mut h = Harness::new();
    if let Some(kernel) = kernel {
        h.handle.ready(
            Arc::new(Entry(kernel.clone())),
            kernel.store.clone(),
            Arc::new(rust_cli_kernel_api::testing::MemoryArtifacts::default()),
        );
    }
    h.send(initialize()).await;
    h.next().await;
    h
}

async fn ask(kernel: Option<Arc<Deleting>>, message: Value) -> Value {
    let mut h = ready(kernel).await;
    h.send(message).await;
    h.reply(2, &mut Vec::new()).await
}

/// 预检的结果照契约（02 §2.6）：范围、forks、保留的附件、文件数；`incomplete` 恒为 false。
#[tokio::test]
async fn the_preview_has_the_contract_shape() {
    let kernel = deleting(None, DeletionOutcome::Deleted, false);
    let reply = ask(
        Some(kernel),
        preview_query(2, target(), json!({"sessionId": "beta"})),
    )
    .await;
    assert_eq!(reply["result"]["kind"], "ok", "{reply}");
    assert_eq!(
        reply["result"]["data"],
        json!({
            "sessionId": "beta",
            "revision": "rev-1",
            "sessionIds": ["beta", "child"],
            "forks": [{"sessionId": "fork", "title": "fork title"}],
            "retainedArtifacts": [{"sessionId": "beta", "artifactId": "shared", "fileName": "shared.png",
                                   "referrers": [{"sessionId": "fork", "title": "fork title"}]}],
            "deletedFiles": 2,
            "incomplete": false,
            "completed": false,
        })
    );
    let running = deleting(
        Some(DeletionBlock::Running),
        DeletionOutcome::Deleted,
        false,
    );
    let reply = ask(
        Some(running),
        preview_query(2, target(), json!({"sessionId": "beta"})),
    )
    .await;
    assert_eq!(
        reply["result"]["data"]["blockedReason"], "running",
        "{reply}"
    );
    // 没有根也没有凭据（同 Node 的 not-found）
    let missing = deleting(
        Some(DeletionBlock::NotFound),
        DeletionOutcome::Deleted,
        false,
    );
    let reply = ask(
        Some(missing),
        preview_query(2, target(), json!({"sessionId": "beta"})),
    )
    .await;
    assert_eq!(
        reply["result"]["data"]["blockedReason"], "not-found",
        "{reply}"
    );
}

/// 根在别的工作区：预检与执行都是前置条件的拒绝（会话的路由粘住），什么都不删。
#[tokio::test]
async fn another_workspace_is_a_precondition() {
    let kernel = deleting(
        Some(DeletionBlock::OtherWorkspace),
        DeletionOutcome::Blocked(DeletionBlock::OtherWorkspace),
        false,
    );
    let reply = ask(
        Some(kernel.clone()),
        preview_query(2, target(), json!({"sessionId": "beta"})),
    )
    .await;
    assert_eq!(
        reply["error"]["data"]["code"], "precondition.workspaceMismatch",
        "{reply}"
    );
    let reply = ask(
        Some(kernel),
        execute(
            2,
            target(),
            json!({"sessionId": "beta", "revision": "rev-1"}),
        ),
    )
    .await;
    assert_eq!(reply["result"]["kind"], "rejected", "{reply}");
    assert_eq!(reply["result"]["code"], "precondition.workspaceMismatch");
}

/// 外壳的拒绝：没有工作区、相对路径、参数不严格、根 id 不是安全的文件名段、进程级部件还没建好。内核都没看到。
#[tokio::test]
async fn the_envelope_is_checked_before_the_kernel_sees_it() {
    let kernel = deleting(None, DeletionOutcome::Deleted, false);
    let payload = json!({"sessionId": "beta", "revision": "rev-1"});
    let missing = ask(
        Some(kernel.clone()),
        execute(2, json!({"hostId": "local"}), payload.clone()),
    )
    .await;
    assert_eq!(
        missing["result"]["code"], "precondition.workspaceRequired",
        "{missing}"
    );
    let relative = ask(
        Some(kernel.clone()),
        execute(
            2,
            json!({"hostId": "local", "workspaceKey": "repo"}),
            payload.clone(),
        ),
    )
    .await;
    assert_eq!(
        relative["result"]["code"], "precondition.invalid",
        "{relative}"
    );
    let mut extra = payload.clone();
    extra["operationId"] = json!("op");
    let unknown = ask(Some(kernel.clone()), execute(2, target(), extra)).await;
    assert_eq!(
        unknown["result"]["code"], "precondition.invalidPayload",
        "{unknown}"
    );
    let unsafe_id = ask(
        Some(kernel.clone()),
        preview_query(2, target(), json!({"sessionId": "../x"})),
    )
    .await;
    assert_eq!(
        unsafe_id["error"]["data"]["code"], "precondition.invalidPayload",
        "{unsafe_id}"
    );
    assert!(kernel.revisions.lock_unpoisoned().is_empty());
    let early = ask(None, execute(2, target(), payload)).await;
    assert_eq!(
        early["result"]["code"], "precondition.storeUnavailable",
        "{early}"
    );
}

/// 删了：会话列表的 feed 移除这些会话（效果在 ACK 之前），结果照契约。执行等内核时链路照常（`ping` 先回）。
#[tokio::test]
async fn a_deletion_removes_the_sessions_from_the_feed_before_the_ack() {
    let kernel = deleting(None, DeletionOutcome::Deleted, true);
    for id in ["beta", "child"] {
        kernel
            .store
            .create(
                SessionMeta {
                    id: SessionId::new(id),
                    kind: SessionKind::Interactive,
                    parent: None,
                    workspace: workspace(KEY),
                    title: id.to_owned(),
                    trace: rust_cli_kernel_api::ids::TraceId::new(id),
                    created_at_ms: 1,
                },
                rust_cli_kernel_api::testing::no_facts(),
            )
            .await
            .unwrap();
    }
    let mut h = ready(Some(kernel.clone())).await;
    h.send(json!({"jsonrpc": "2.0", "id": 9, "method": "subscribe",
                  "params": {"sub": "f", "topic": crate::topics::workspace_topic(KEY, crate::topics::WORKSPACE_SESSIONS)}}))
        .await;
    let mut frames = Vec::new();
    h.reply(9, &mut frames).await;
    h.send(execute(
        2,
        target(),
        json!({"sessionId": "beta", "revision": "rev-1"}),
    ))
    .await;
    h.send(json!({"jsonrpc": "2.0", "id": 3, "method": "ping", "params": {}}))
        .await;
    let pong = h.reply(3, &mut frames).await;
    assert_eq!(pong["result"], json!({}));
    kernel.gate.as_ref().unwrap().notify_one();
    let mut frames = Vec::new();
    let reply = h.reply(2, &mut frames).await;
    assert_eq!(reply["result"]["kind"], "accepted", "{reply}");
    assert_eq!(
        reply["result"]["result"],
        json!({"sessionId": "beta", "status": "deleted", "sessionIds": ["beta", "child"], "retainedArtifacts": 1})
    );
    let removed: Vec<Value> = frames
        .iter()
        .filter(|frame| frame["method"] == "frame")
        .flat_map(|frame| {
            frame["params"]["body"]["deltas"]
                .as_array()
                .cloned()
                .unwrap_or_default()
        })
        .filter(|delta| delta["op"] == "entity.remove")
        .map(|delta| delta["id"].clone())
        .collect();
    assert_eq!(removed, [json!("beta"), json!("child")]);
    // 凭据的 operation id 是这条命令的 commandId（02 §2.6）
    assert_eq!(kernel.revisions.lock_unpoisoned().clone(), ["rev-1 c2"]);
}

/// 阻止与删到一半是业务结果（accepted），带原因；会话列表不动。
#[tokio::test]
async fn blocked_and_incomplete_are_business_results() {
    let blocked = deleting(
        None,
        DeletionOutcome::Blocked(DeletionBlock::Changed),
        false,
    );
    let reply = ask(
        Some(blocked),
        execute(2, target(), json!({"sessionId": "beta", "revision": "old"})),
    )
    .await;
    assert_eq!(reply["result"]["result"]["status"], "blocked", "{reply}");
    assert_eq!(reply["result"]["result"]["reason"], "changed");
    let incomplete = deleting(
        None,
        DeletionOutcome::Incomplete("EACCES".to_owned()),
        false,
    );
    let reply = ask(
        Some(incomplete),
        execute(
            2,
            target(),
            json!({"sessionId": "beta", "revision": "rev-1"}),
        ),
    )
    .await;
    assert_eq!(reply["result"]["result"]["status"], "incomplete", "{reply}");
    assert_eq!(reply["result"]["result"]["reason"], "EACCES");
}
