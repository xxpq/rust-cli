//! 经 Host 取账号凭据（G8）：反向请求的参数、回复的换算与取消。
use std::sync::Arc;
use std::time::Duration;

use rust_cli_base::TaskScope;
use rust_cli_base::models::GLM_5_3_ACCOUNT;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::testing::FixedClock;
use rust_cli_protocol_core::connection::{self, Options, Reply, Service};
use rust_cli_protocol_core::rpc::RpcError;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, duplex};

use super::*;

struct Nothing;

#[async_trait]
impl Service for Nothing {
    async fn request(&self, method: &str, _params: Value) -> Result<Reply, RpcError> {
        Err(RpcError::method_not_found(method))
    }
}

fn request(session: Option<&str>) -> HeaderRequest {
    HeaderRequest {
        selection: ModelSelection::new("account:zai-individual-coding-plan", GLM_5_3_ACCOUNT),
        account_type: "zai".into(),
        mode: "individual-coding-plan".into(),
        session: session.map(SessionId::new),
        root: None,
        rejected: Some(RejectedCredential {
            fingerprint: "a".repeat(64),
            binding: None,
        }),
        denied: None,
        execution: None,
    }
}

/// 拒绝访问之后再问（platform.md §5.3）：v4 的 Host 没有这条路，直接失败、不发反向请求（调用方交出原来的拒绝，与以前相同）。
#[tokio::test]
async fn an_access_denial_is_not_asked_of_the_v4_host() {
    let scope = TaskScope::root("test", |_| {});
    let (agent_out, host_in) = duplex(1 << 16);
    let (peer, _writer) = connection::start(agent_out, &scope.spawner(), Options::default());
    let context = Arc::new(Context::new(peer, Arc::new(FixedClock(1))));
    context.route("session_1", "/w").unwrap();
    let source = HostHeaders::new(context);
    let denied = HeaderRequest {
        denied: Some(rust_cli_kernel_api::provider::AccessDenial {
            status: Some(403),
            code: None,
        }),
        ..request(Some("session_1"))
    };
    assert!(source.request_auth(denied).await.is_err());
    // 之后的第一条反向请求是这条链路上的第一个请求：拒绝访问那一次什么也没写
    let asking = tokio::spawn(async move { source.request_auth(request(Some("session_1"))).await });
    let line = BufReader::new(host_in)
        .lines()
        .next_line()
        .await
        .unwrap()
        .unwrap();
    let asked: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(asked["id"], "rs-1");
    asking.abort();
}

#[tokio::test]
async fn account_credentials_come_from_the_host_through_a_reverse_request() {
    let scope = TaskScope::root("test", |_| {});
    let (mut host_out, agent_in) = duplex(1 << 16);
    let (agent_out, host_in) = duplex(1 << 16);
    let (peer, _writer) = connection::start(agent_out, &scope.spawner(), Options::default());
    drop(scope.spawner().spawn(
        "serve",
        connection::serve(
            BufReader::new(agent_in),
            peer.clone(),
            Arc::new(Nothing),
            scope.spawner(),
            Options::default(),
        ),
    ));
    let context = Arc::new(Context::new(peer, Arc::new(FixedClock(1))));
    let source = HostHeaders::new(context.clone());
    // 会话不在这个进程里：不问 Host。
    assert!(
        source
            .request_auth(request(Some("session_x")))
            .await
            .is_err()
    );
    assert!(source.request_auth(request(None)).await.is_err());
    context.route("session_1", "/w").unwrap();
    let asking = tokio::spawn(async move { source.request_auth(request(Some("session_1"))).await });
    let mut lines = BufReader::new(host_in).lines();
    let line = tokio::time::timeout(Duration::from_secs(5), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let asked: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(asked["method"], REQUEST);
    let params = &asked["params"];
    assert_eq!(params["sessionId"], "session_1");
    assert_eq!(
        params["workspace"],
        json!({"workspacePath": "/w", "workspaceKey": "/w"})
    );
    assert_eq!(params["providerId"], "account:zai-individual-coding-plan");
    assert_eq!(params["accountAccess"]["mode"], "individual-coding-plan");
    assert_eq!(params["reason"], "model-request");
    assert_eq!(params["rejectedProjectTokenFingerprint"], "a".repeat(64));
    let reply = json!({"id": asked["id"], "result": {"headersApplied": true,
        "requestAuth": {"apiKey": "sk-host", "headers": {"x-sign": "s"}}}});
    host_out
        .write_all(format!("{reply}\n").as_bytes())
        .await
        .unwrap();
    let auth = asking.await.unwrap().unwrap();
    assert_eq!(auth.api_key.unwrap().expose(), "sk-host");
    assert_eq!(auth.headers[0].0, "x-sign");
}

/// 修 bug（docs/specs/rust-cli/platform.md §5.3）：401 之后只带被拒令牌的指纹、不带账号范围，Host 一律判
/// `project_token_scope_invalidated`（有指纹必须有范围），重取总是失败。Host 的应答带着 `requestAuth.accountScope`，现在它跟着凭据走
/// （`binding`），被拒再取时作为 `expectedAccountScope` 带回去。
#[tokio::test]
async fn the_401_retry_carries_the_account_scope_the_host_gave() {
    let scope = TaskScope::root("test", |_| {});
    let (mut host_out, agent_in) = duplex(1 << 16);
    let (agent_out, host_in) = duplex(1 << 16);
    let (peer, _writer) = connection::start(agent_out, &scope.spawner(), Options::default());
    drop(scope.spawner().spawn(
        "serve",
        connection::serve(
            BufReader::new(agent_in),
            peer.clone(),
            Arc::new(Nothing),
            scope.spawner(),
            Options::default(),
        ),
    ));
    let context = Arc::new(Context::new(peer, Arc::new(FixedClock(1))));
    context.route("session_1", "/w").unwrap();
    let source = Arc::new(HostHeaders::new(context));
    let mut lines = BufReader::new(host_in).lines();
    let account_scope = "b".repeat(64);
    let first = {
        let source = source.clone();
        tokio::spawn(async move {
            let mut first = request(Some("session_1"));
            first.rejected = None;
            source.request_auth(first).await
        })
    };
    let line = lines.next_line().await.unwrap().unwrap();
    let asked: Value = serde_json::from_str(&line).unwrap();
    assert!(asked["params"].get("expectedAccountScope").is_none());
    let reply = json!({"id": asked["id"], "result": {"headersApplied": true,
        "requestAuth": {"apiKey": "sk-1", "accountScope": account_scope}}});
    host_out
        .write_all(format!("{reply}\n").as_bytes())
        .await
        .unwrap();
    let auth = first.await.unwrap().unwrap();
    assert_eq!(auth.binding.as_deref(), Some(account_scope.as_str()));
    let retry = tokio::spawn(async move {
        let mut retry = request(Some("session_1"));
        retry.rejected = Some(RejectedCredential {
            fingerprint: "c".repeat(64),
            binding: auth.binding,
        });
        source.request_auth(retry).await
    });
    let line = lines.next_line().await.unwrap().unwrap();
    let asked: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(
        asked["params"]["rejectedProjectTokenFingerprint"],
        "c".repeat(64)
    );
    assert_eq!(asked["params"]["expectedAccountScope"], account_scope);
    let reply = json!({"id": asked["id"], "result": {"headersApplied": true,
        "requestAuth": {"apiKey": "sk-2", "accountScope": account_scope}}});
    host_out
        .write_all(format!("{reply}\n").as_bytes())
        .await
        .unwrap();
    assert_eq!(
        retry.await.unwrap().unwrap().api_key.unwrap().expose(),
        "sk-2"
    );
}
