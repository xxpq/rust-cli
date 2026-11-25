//! `mcp.oauth.*`（protocol-v5.md §74.2）：只在入口装了授权时声明与认；校验与拒绝码；ACK 与结果的形状；结局的查询等到授权结束（不挡链路）。
//! 授权本身（发现、注册、回调、重连）在 ext 与 platform 的测试里。
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures_util::FutureExt;
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{EnvironmentId, SessionId};
use rust_cli_kernel_api::manage::{McpOAuthServer, McpServerInfo, McpSignIn, McpSignInOutcome};
use rust_cli_kernel_api::session_api::{CreateSession, Kernel, KernelError, SessionHandle};
use rust_cli_kernel_api::store::{SessionFilter, SessionSummary, StoreQueries};
use rust_cli_kernel_api::testing::MemoryStore;
use serde_json::{Value, json};
use tokio::sync::oneshot;

use super::*;
use crate::generated::contract::CONTRACT_DIGEST;
use crate::mcp_trust::{McpDecision, McpPending, McpTrustRefusal, McpTrusted};
use crate::server::{WorkspaceKernel, Workspaces};
use crate::test_link::Harness;

#[cfg(not(windows))]
const KEY: &str = "/Users/dev/repo";
#[cfg(windows)]
const KEY: &str = r"C:\Users\dev\repo";
const URL: &str = "https://auth.example.com/authorize?state=s";

/// 内核的替身：一个用 OAuth 的服务器 `docs`（没存令牌、要授权）；`fail` 时开始授权失败；结局等测试给。
struct OAuthKernel {
    store: Arc<MemoryStore>,
    fail: bool,
    outcome: Mutex<Option<oneshot::Receiver<McpSignInOutcome>>>,
    signed_out: Mutex<Vec<String>>,
}

#[async_trait]
impl Kernel for OAuthKernel {
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

    async fn mcp_oauth_servers(&self) -> Vec<McpOAuthServer> {
        vec![McpOAuthServer {
            name: "docs".to_owned(),
            signed_in: false,
            needs_auth: true,
            pending_url: None,
        }]
    }

    async fn mcp_sign_in(&self, server: String) -> Result<McpSignIn, KernelError> {
        assert_eq!(server, "docs");
        if self.fail {
            return Err(KernelError::Failed {
                message: "Dynamic client registration failed: HTTP 400".to_owned(),
            });
        }
        Ok(McpSignIn {
            url: URL.to_owned(),
            done: async { Ok(None::<McpServerInfo>) }.boxed(),
        })
    }

    async fn mcp_sign_in_outcome(
        &self,
        _server: String,
        url: String,
    ) -> Result<McpSignInOutcome, KernelError> {
        assert_eq!(url, URL);
        let outcome = self.outcome.lock().unwrap().take();
        Ok(match outcome {
            Some(outcome) => outcome.await.unwrap_or(McpSignInOutcome::Ended),
            None => McpSignInOutcome::Ended,
        })
    }

    async fn mcp_sign_out(&self, server: String) -> Result<Option<McpServerInfo>, KernelError> {
        self.signed_out.lock().unwrap().push(server);
        Ok(None)
    }
}

struct Entry(Arc<OAuthKernel>);

#[async_trait]
impl Workspaces for Entry {
    async fn open(&self, _key: &str) -> Result<WorkspaceKernel, String> {
        let mut opened = crate::test_link::kernel();
        opened.kernel = self.0.clone();
        Ok(opened)
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
        Err(McpTrustRefusal::DeclarationChanged)
    }
}

fn initialize() -> Value {
    json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
           "params": {"clientId": "t", "caps": {"core": [1], "agent": [1], "mcp.oauth": [1]}, "contract": CONTRACT_DIGEST}})
}

fn target() -> Value {
    json!({"hostId": "local", "workspaceKey": KEY})
}

fn command(id: u64, kind: &str, payload: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "command", "params": {
        "commandId": format!("cmd-{id}"), "type": kind, "target": target(), "payload": payload,
    }})
}

fn query(id: u64, kind: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "query", "params": {
        "type": kind, "target": target(), "params": params,
    }})
}

async fn ready(
    fail: bool,
) -> (
    Harness,
    Arc<OAuthKernel>,
    Value,
    Option<oneshot::Sender<McpSignInOutcome>>,
) {
    let (settle, outcome) = oneshot::channel();
    let kernel = Arc::new(OAuthKernel {
        store: Arc::default(),
        fail,
        outcome: Mutex::new(Some(outcome)),
        signed_out: Mutex::default(),
    });
    let mut h = Harness::with_mcp_oauth();
    h.handle.ready(
        Arc::new(Entry(kernel.clone())),
        Arc::new(MemoryStore::default()),
        Arc::new(rust_cli_kernel_api::testing::MemoryArtifacts::default()),
    );
    h.send(initialize()).await;
    let handshake = h.next().await;
    (h, kernel, handshake, Some(settle))
}

#[tokio::test]
async fn the_capability_is_declared_only_when_the_entry_signs_in() {
    let (_h, _, handshake, _) = ready(false).await;
    assert_eq!(handshake["result"]["caps"]["mcp.oauth"], 1, "{handshake}");

    let mut plain = Harness::new();
    plain.send(initialize()).await;
    let handshake = plain.next().await;
    assert!(
        handshake["result"]["caps"].get("mcp.oauth").is_none(),
        "{handshake}"
    );
    plain.send(query(2, SERVERS, json!({}))).await;
    let reply = plain.reply(2, &mut Vec::new()).await;
    assert_eq!(
        reply["error"]["data"]["code"], "query.unsupported",
        "{reply}"
    );
}

#[tokio::test]
async fn servers_are_the_kernels_oauth_servers() {
    let (mut h, ..) = ready(false).await;
    h.send(query(2, SERVERS, json!({}))).await;
    let reply = h.reply(2, &mut Vec::new()).await;
    assert_eq!(
        reply["result"],
        json!({"kind": "ok", "data": {"servers": [{"name": "docs", "signedIn": false, "needsAuth": true}]}}),
        "{reply}"
    );
}

#[tokio::test]
async fn sign_in_answers_with_the_authorization_url_and_the_outcome_waits_for_the_end() {
    let (mut h, _, _, settle) = ready(false).await;
    h.send(command(2, SIGN_IN, json!({"serverName": "docs"})))
        .await;
    let reply = h.reply(2, &mut Vec::new()).await;
    assert_eq!(reply["result"]["kind"], "accepted", "{reply}");
    assert_eq!(reply["result"]["result"], json!({"authorizationUrl": URL}));

    h.send(query(
        3,
        SIGN_IN_OUTCOME,
        json!({"serverName": "docs", "authorizationUrl": URL}),
    ))
    .await;
    // 结局还没到：链路照常（ping 先回）
    h.send(json!({"jsonrpc": "2.0", "id": 4, "method": "ping"}))
        .await;
    let first = h.next().await;
    assert_eq!(first["id"], 4, "{first}");
    settle
        .unwrap()
        .send(McpSignInOutcome::Failed("access_denied".to_owned()))
        .unwrap();
    let reply = h.reply(3, &mut Vec::new()).await;
    assert_eq!(
        reply["result"]["data"],
        json!({"outcome": "failed", "message": "access_denied"}),
        "{reply}"
    );
}

#[tokio::test]
async fn unknown_servers_and_failed_starts_have_their_own_codes() {
    let (mut h, ..) = ready(false).await;
    h.send(command(2, SIGN_IN, json!({"serverName": "nope"})))
        .await;
    let reply = h.reply(2, &mut Vec::new()).await;
    assert_eq!(
        reply["result"]["code"], "precondition.mcpOAuthUnavailable",
        "{reply}"
    );

    let (mut h, ..) = ready(true).await;
    h.send(command(2, SIGN_IN, json!({"serverName": "docs"})))
        .await;
    let reply = h.reply(2, &mut Vec::new()).await;
    assert_eq!(
        reply["result"]["code"], "precondition.mcpOAuthFailed",
        "{reply}"
    );
    assert_eq!(
        reply["result"]["message"],
        "Dynamic client registration failed: HTTP 400"
    );

    h.send(command(3, SIGN_IN, json!({}))).await;
    let reply = h.reply(3, &mut Vec::new()).await;
    assert_eq!(
        reply["result"]["code"], "precondition.invalidPayload",
        "{reply}"
    );
}

#[tokio::test]
async fn signing_out_a_server_without_tokens_is_a_noop() {
    let (mut h, kernel, ..) = ready(false).await;
    h.send(command(2, SIGN_OUT, json!({"serverName": "docs"})))
        .await;
    let reply = h.reply(2, &mut Vec::new()).await;
    assert_eq!(reply["result"]["kind"], "accepted", "{reply}");
    assert_eq!(reply["result"]["noop"], true);
    assert_eq!(*kernel.signed_out.lock().unwrap(), ["docs"]);
}
