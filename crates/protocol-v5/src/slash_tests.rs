//! `slash.commands`（protocol-v5.md §60）：内核给的用户命令去掉 `hidden`、按内核的顺序；`argumentHint` 原样；进程级部件建好之前、
//! 没有工作区时回错误；查询在任务里，actor 不等它（`ping` 照常先回）。
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rust_cli_base::sync::LockUnpoisoned;
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{EnvironmentId, SessionId};
use rust_cli_kernel_api::session_api::{CreateSession, Kernel, KernelError, SessionHandle};
use rust_cli_kernel_api::store::{SessionFilter, SessionSummary, StoreQueries};
use rust_cli_kernel_api::testing::{MemoryArtifacts, MemoryStore, selection};
use rust_cli_kernel_api::user_command::{UserCommand, UserCommandKind};
use serde_json::{Value, json};
use tokio::sync::Notify;

use crate::mcp_trust::{McpDecision, McpPending, McpTrustRefusal, McpTrusted};
use crate::server::{WorkspaceKernel, Workspaces};
use crate::test_link::{Harness, initialize};

#[cfg(not(windows))]
const KEY: &str = "/Users/dev/repo";
#[cfg(windows)]
const KEY: &str = r"C:\Users\dev\repo";

/// 只答 `user_commands` 的内核：记下问到的工作区与附加目录。
#[derive(Default)]
struct Commands {
    asked: Mutex<Vec<(WorkspaceRef, Vec<EnvPath>)>>,
}

#[async_trait]
impl Kernel for Commands {
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
        Arc::new(MemoryStore::default())
    }

    async fn user_commands(
        &self,
        workspace: &WorkspaceRef,
        additional: &[EnvPath],
    ) -> Vec<UserCommand> {
        self.asked
            .lock_unpoisoned()
            .push((workspace.clone(), additional.to_vec()));
        let command = |name: &str, hint: Option<&str>, hidden: bool| UserCommand {
            name: name.to_owned(),
            description: format!("{name} description"),
            argument_hint: hint.map(str::to_owned),
            hidden,
            kind: UserCommandKind::Command,
        };
        vec![
            UserCommand {
                kind: UserCommandKind::Skill,
                ..command("review", None, false)
            },
            command("frontend:component", Some("<name>"), false),
            command("inner", None, true),
            command("init", None, false),
        ]
    }
}

/// 工作区端口：`open` 给出上面的内核；有闸门时先等它打开（替身里的“正在组装”）。
struct Entry {
    kernel: Arc<Commands>,
    gate: Option<Notify>,
}

#[async_trait]
impl Workspaces for Entry {
    async fn open(&self, _key: &str) -> Result<WorkspaceKernel, String> {
        if let Some(gate) = &self.gate {
            gate.notified().await;
        }
        Ok(WorkspaceKernel {
            kernel: self.kernel.clone(),
            selection: selection(),
            mode: rust_cli_kernel_api::permission::PermissionMode::Default,
        })
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
        _decision: McpDecision,
    ) -> Result<McpTrusted, McpTrustRefusal> {
        Err(McpTrustRefusal::Unavailable("not used".to_owned()))
    }
}

fn query(id: u64, target: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "query", "params": {
        "type": "slash.commands", "target": target, "params": {},
    }})
}

fn target() -> Value {
    json!({"hostId": "local", "workspaceKey": KEY})
}

async fn harness(entry: Option<Arc<Entry>>) -> Harness {
    let mut h = Harness::new();
    if let Some(entry) = entry {
        h.handle.ready(
            entry,
            Arc::new(MemoryStore::default()),
            Arc::new(MemoryArtifacts::default()),
        );
    }
    h.send(initialize()).await;
    h.next().await;
    h
}

fn entry(gate: Option<Notify>) -> Arc<Entry> {
    Arc::new(Entry {
        kernel: Arc::default(),
        gate,
    })
}

/// 每条带来源 `kind`（protocol-v5.md §62.3）：skill 与命令按内核给的分开。
#[tokio::test]
async fn the_kernels_commands_come_back_without_the_hidden_ones() {
    let entry = entry(None);
    let mut h = harness(Some(entry.clone())).await;
    h.send(query(2, target())).await;
    let reply = h.reply(2, &mut Vec::new()).await;
    assert_eq!(
        reply["result"],
        json!({"kind": "ok", "data": {"commands": [
            {"name": "review", "description": "review description", "kind": "skill"},
            {"name": "frontend:component", "description": "frontend:component description", "argumentHint": "<name>", "kind": "command"},
            {"name": "init", "description": "init description", "kind": "command"},
        ]}}),
        "{reply}"
    );
    // 工作区的命令，不带附加目录
    let asked = entry.kernel.asked.lock_unpoisoned().clone();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].0.root.as_str(), KEY);
    assert!(asked[0].1.is_empty());
}

/// 组装在任务里：组装没完时 `ping` 先得到回复（actor 不等，§4.1）。
#[tokio::test]
async fn the_query_does_not_hold_the_link_while_the_workspace_opens() {
    let entry = entry(Some(Notify::new()));
    let mut h = harness(Some(entry.clone())).await;
    h.send(query(2, target())).await;
    h.send(json!({"jsonrpc": "2.0", "id": 3, "method": "ping", "params": {}}))
        .await;
    let first = h.next().await;
    assert_eq!(first["id"], 3, "{first}");
    entry.gate.as_ref().unwrap().notify_one();
    let reply = h.reply(2, &mut Vec::new()).await;
    assert_eq!(reply["result"]["kind"], "ok", "{reply}");
}

#[tokio::test]
async fn before_the_process_parts_are_ready_the_query_is_refused() {
    let mut h = harness(None).await;
    h.send(query(2, target())).await;
    let reply = h.reply(2, &mut Vec::new()).await;
    assert_eq!(
        reply["error"]["data"]["code"], "precondition.storeUnavailable",
        "{reply}"
    );
}

#[tokio::test]
async fn a_query_needs_a_local_workspace() {
    let mut h = harness(Some(entry(None))).await;
    h.send(query(2, json!({"hostId": "local"}))).await;
    let missing = h.reply(2, &mut Vec::new()).await;
    assert_eq!(
        missing["error"]["data"]["code"], "precondition.workspaceRequired",
        "{missing}"
    );
    h.send(query(
        3,
        json!({"hostId": "local", "workspaceKey": "relative/dir"}),
    ))
    .await;
    let relative = h.reply(3, &mut Vec::new()).await;
    assert_eq!(
        relative["error"]["data"]["code"], "precondition.invalid",
        "{relative}"
    );
}
