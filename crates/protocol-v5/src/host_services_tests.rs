//! CLI→Host 的 `service` 请求与经 Host 取账号凭据（docs/specs/rust-cli/protocol-v5.md §30）：真实的 actor 与 Content-Length 分帧，
//! 测试扮演 Host（握手时告知服务、应答 `service`）。
use std::sync::Arc;

use rust_cli_base::models::GLM_5_3_ACCOUNT;
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{EnvironmentId, SessionId};
use rust_cli_kernel_api::model::{ModelError, ModelSelection};
use rust_cli_kernel_api::provider::{
    AccessDenial, HeaderRequest, RejectedCredential, SuppliedAuth,
};
use rust_cli_kernel_api::testing::{FixedClock, MemoryStore};
use serde_json::{Value, json};
use tokio::task::JoinHandle;

use crate::generated::contract::CONTRACT_DIGEST;
use crate::server::Workspaces;
use crate::test_link::Harness;
use crate::topics::session_topic;

const ROOT: &str = "/Users/dev/app";
const SESSION: &str = "sess_1";
const SCOPE: &str = "5f1d0a7a1d6ed0ef2c9d7b6f70ce2a6e4ac2fd0a7a1d6ed0ef2c9d7b6f70ce2a";

struct Local;

#[async_trait::async_trait]
impl Workspaces for Local {
    async fn open(&self, _key: &str) -> Result<crate::server::WorkspaceKernel, String> {
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

/// Host 拉起 rust 时的握手（严格链路）：告知这一代链路上应答的服务。
fn initialize(host_services: &[&str]) -> Value {
    json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
           "params": {"clientId": "host", "caps": {"core": [1], "agent": [1], "session": [1]},
                      "contract": CONTRACT_DIGEST, "hostServices": host_services}})
}

/// 握手、进程级部件建好，并订一个会话的 topic（会话进了 actor 的会话表；没有会话的内核让订阅失败，表里的工作区留着）。
async fn linked(host_services: &[&str]) -> Harness {
    let mut h = Harness::new();
    h.handle.ready(
        Arc::new(Local),
        Arc::new(MemoryStore::default()),
        Arc::new(rust_cli_kernel_api::testing::MemoryArtifacts::default()),
    );
    h.send(initialize(host_services)).await;
    assert!(h.next().await["result"].is_object());
    h.send(json!({"jsonrpc": "2.0", "id": 2, "method": "subscribe",
                  "params": {"sub": "s", "topic": session_topic(ROOT, SESSION)}}))
        .await;
    // ping 的回复到了，订阅一定已经处理过（actor 按到达顺序处理对端的消息）：会话进了会话表
    pinged(&mut h, 3).await;
    h
}

async fn pinged(h: &mut Harness, id: u64) {
    h.send(json!({"jsonrpc": "2.0", "id": id, "method": "ping"}))
        .await;
    loop {
        let next = h.next().await;
        assert_ne!(next["method"], "service");
        if next["id"] == id {
            return;
        }
    }
}

fn header_request(session: &str) -> HeaderRequest {
    HeaderRequest {
        selection: ModelSelection::new("account:zai-individual-coding-plan", GLM_5_3_ACCOUNT),
        account_type: "zai".into(),
        mode: "individual-coding-plan".into(),
        session: Some(SessionId::new(session)),
        root: None,
        rejected: None,
        denied: None,
        execution: None,
    }
}

fn ask(h: &Harness, request: HeaderRequest) -> JoinHandle<Result<SuppliedAuth, ModelError>> {
    let credentials = h.handle.credentials(Arc::new(FixedClock(1)));
    tokio::spawn(async move { credentials.request_auth(request).await })
}

/// 读到 rust 发来的 `service` 请求为止（中间的帧、订阅的回复跳过）。
async fn service_request(h: &mut Harness) -> Value {
    loop {
        let message = h.next().await;
        if message["method"] == "service" {
            return message;
        }
    }
}

async fn answer(h: &mut Harness, asked: &Value, result: Value) {
    h.send(json!({"jsonrpc": "2.0", "id": asked["id"], "result": result}))
        .await;
}

fn message(result: Result<SuppliedAuth, ModelError>) -> String {
    match result {
        Err(ModelError::Authentication { message }) => message,
        other => panic!("expected an authentication failure, got {other:?}"),
    }
}

#[tokio::test]
async fn account_credentials_come_from_the_host_through_a_service_request() {
    let mut h = linked(&["host.providers.requestAuth"]).await;
    let asking = ask(&h, header_request(SESSION));
    let asked = service_request(&mut h).await;
    // 外壳：服务名、工作区与会话；参数按契约（rust 只发 model-request）
    assert_eq!(asked["params"]["type"], "host.providers.requestAuth");
    assert_eq!(
        asked["params"]["target"],
        json!({"workspaceKey": ROOT, "sessionId": SESSION})
    );
    let params = &asked["params"]["params"];
    assert_eq!(params["provider"], "account:zai-individual-coding-plan");
    assert_eq!(params["modelId"], GLM_5_3_ACCOUNT);
    assert_eq!(params["sessionId"], SESSION);
    assert_eq!(params["reason"], "model-request");
    assert_eq!(
        params["accountAccess"],
        json!({"type": "zhipu-account", "accountType": "zai",
               "mode": "individual-coding-plan", "entitled": true})
    );
    assert!(params.get("rejected").is_none());
    answer(
        &mut h,
        &asked,
        json!({"kind": "headers", "headers": {"x-sign": "s"}, "apiKey": "sk-host", "accountScope": SCOPE}),
    )
    .await;
    let auth = asking.await.unwrap().unwrap();
    assert_eq!(auth.api_key.unwrap().expose(), "sk-host");
    assert_eq!(auth.headers[0].0, "x-sign");
    assert_eq!(auth.headers[0].1.expose(), "s");
    assert_eq!(auth.binding.as_deref(), Some(SCOPE));
    assert!(!auth.single_use);
}

/// 模型服务拒绝访问之后再问（21 §13.2.2）：拒绝的事实原样作为 `denied` 交给 Host，不带被拒的凭据。
#[tokio::test]
async fn an_access_denial_is_handed_to_the_host() {
    let mut h = linked(&["host.providers.requestAuth"]).await;
    let mut request = header_request(SESSION);
    request.denied = Some(AccessDenial {
        status: Some(403),
        code: Some("1311".to_owned()),
    });
    let asking = ask(&h, request);
    let asked = service_request(&mut h).await;
    let params = &asked["params"]["params"];
    assert_eq!(params["denied"], json!({"status": 403, "code": "1311"}));
    assert!(params.get("rejected").is_none());
    answer(
        &mut h,
        &asked,
        json!({"kind": "headers", "headers": {}, "apiKey": "sk-new"}),
    )
    .await;
    assert_eq!(
        asking.await.unwrap().unwrap().api_key.unwrap().expose(),
        "sk-new"
    );
}

/// 进程级 `agent` 的 `hostServices` 写握手里告知的值（以前恒为 `[]`）。
#[tokio::test]
async fn the_process_agent_shows_the_services_the_host_declared() {
    let mut h = linked(&["host.providers.requestAuth"]).await;
    h.send(json!({"jsonrpc": "2.0", "id": 4, "method": "subscribe",
                  "params": {"sub": "a", "topic": "agent"}}))
        .await;
    let snapshot = loop {
        let message = h.next().await;
        if message["method"] == "frame" && message["params"]["sub"] == "a" {
            break message;
        }
    };
    assert_eq!(
        snapshot["params"]["body"]["snapshot"]["state"]["hostServices"],
        json!(["host.providers.requestAuth"])
    );
}

/// 401 之后：被拒令牌的指纹与上次结果给的账号范围一起带上（Host 的规则：有指纹必须有范围）。
#[tokio::test]
async fn the_401_retry_carries_the_fingerprint_and_the_account_scope() {
    let mut h = linked(&["host.providers.requestAuth"]).await;
    let mut request = header_request(SESSION);
    request.rejected = Some(RejectedCredential {
        fingerprint: "a".repeat(64),
        binding: Some(SCOPE.to_owned()),
    });
    let asking = ask(&h, request);
    let asked = service_request(&mut h).await;
    assert_eq!(
        asked["params"]["params"]["rejected"],
        json!({"fingerprint": "a".repeat(64), "accountScope": SCOPE})
    );
    answer(
        &mut h,
        &asked,
        json!({"kind": "headers", "headers": {}, "apiKey": "sk-2", "accountScope": SCOPE}),
    )
    .await;
    assert_eq!(
        asking.await.unwrap().unwrap().api_key.unwrap().expose(),
        "sk-2"
    );
}

/// 子代理的请求带子会话，前端的会话表里没有它：按顶层会话找工作区，参数与外壳里都是顶层会话。
#[tokio::test]
async fn a_subagent_request_is_routed_by_its_root_session() {
    let mut h = linked(&["host.providers.requestAuth"]).await;
    let mut request = header_request("sess_child");
    request.root = Some(SessionId::new(SESSION));
    let asking = ask(&h, request);
    let asked = service_request(&mut h).await;
    assert_eq!(asked["params"]["target"]["sessionId"], SESSION);
    assert_eq!(asked["params"]["params"]["sessionId"], SESSION);
    answer(
        &mut h,
        &asked,
        json!({"kind": "headers", "headers": {}, "apiKey": "k"}),
    )
    .await;
    assert!(asking.await.unwrap().is_ok());
}

/// Host 没告知这个服务：不发请求，立即鉴权失败（文字给用户看）。之后链路照常（ping 是下一条回复）。
#[tokio::test]
async fn without_the_declared_service_nothing_is_sent() {
    let mut h = linked(&[]).await;
    let failed = message(ask(&h, header_request(SESSION)).await.unwrap());
    assert!(failed.contains("does not provide"), "{failed}");
    pinged(&mut h, 9).await;
}

/// 会话不在这个进程里（没订过、没发过命令）：不问 Host。
#[tokio::test]
async fn a_session_this_process_does_not_know_fails_without_asking() {
    let h = linked(&["host.providers.requestAuth"]).await;
    let failed = message(ask(&h, header_request("sess_elsewhere")).await.unwrap());
    assert!(failed.contains("not open"), "{failed}");
}

/// 需要用户完成官方版本的请求安全校验，但会话在会话表里却没有投影（这里的内核读不出会话）：开不了交互，立即鉴权失败（21 §13.4
/// 第 19 行）。R1 时这里一律报「rust 还不支持」；R2 起由会话的投影开交互（session/verify_tests.rs）。
#[tokio::test]
async fn needing_the_user_without_a_projection_fails_at_once() {
    let mut h = linked(&["host.providers.requestAuth"]).await;
    let asking = ask(&h, header_request(SESSION));
    let asked = service_request(&mut h).await;
    answer(&mut h, &asked, json!({"kind": "needsUser"})).await;
    let failed = message(asking.await.unwrap());
    assert!(failed.contains("not open"), "{failed}");
}

/// Host 的应答方失败（例如账号凭据不可用）：Host 的文字原样给用户看。
#[tokio::test]
async fn a_host_failure_is_shown_with_the_host_text() {
    let mut h = linked(&["host.providers.requestAuth"]).await;
    let asking = ask(&h, header_request(SESSION));
    let asked = service_request(&mut h).await;
    h.send(json!({"jsonrpc": "2.0", "id": asked["id"], "error": {
        "code": -31000, "message": "Account request credential is unavailable: zai",
        "data": {"code": "internal.failed", "message": "Account request credential is unavailable: zai", "retryable": false}}}))
        .await;
    assert_eq!(
        message(asking.await.unwrap()),
        "Account request credential is unavailable: zai"
    );
}

/// 严格链路上应答多一个键：对端违约，这次取凭据失败（不读成别的东西）。
#[tokio::test]
async fn a_reply_off_the_strict_schema_fails() {
    let mut h = linked(&["host.providers.requestAuth"]).await;
    let asking = ask(&h, header_request(SESSION));
    let asked = service_request(&mut h).await;
    answer(
        &mut h,
        &asked,
        json!({"kind": "headers", "headers": {}, "apiKey": "k", "extra": true}),
    )
    .await;
    let failed = message(asking.await.unwrap());
    assert!(failed.contains("invalid"), "{failed}");
}

/// 回合停了（取凭据的任务被丢掉）之后 Host 才回复：回复被丢掉，链路照常。
#[tokio::test]
async fn a_reply_after_the_caller_gave_up_is_dropped() {
    let mut h = linked(&["host.providers.requestAuth"]).await;
    let asking = ask(&h, header_request(SESSION));
    let asked = service_request(&mut h).await;
    asking.abort();
    assert!(asking.await.unwrap_err().is_cancelled());
    answer(
        &mut h,
        &asked,
        json!({"kind": "headers", "headers": {}, "apiKey": "late"}),
    )
    .await;
    pinged(&mut h, 9).await;
}

/// 在等回复时链路断开（Host 关掉输入、rust 换代之前）：等待以鉴权失败结束，不挂着。
#[tokio::test]
async fn a_link_that_closes_while_waiting_fails_the_request() {
    let mut h = linked(&["host.providers.requestAuth"]).await;
    let asking = ask(&h, header_request(SESSION));
    service_request(&mut h).await;
    h.close_input().await;
    let failed = message(asking.await.unwrap());
    assert!(failed.contains("closed"), "{failed}");
}

/// 这一轮执行覆盖的凭据引用（protocol-v5.md §38）：读回成契约的判别联合，作为 `execution` 交给 Host；没有引用的请求不带它（上面几条）。
#[tokio::test]
async fn the_execution_reference_is_handed_to_the_host() {
    let mut h = linked(&["host.providers.requestAuth"]).await;
    let mut request = header_request(SESSION);
    request.execution = Some(json!({"kind": "highspeedCard", "cardId": "hsc_1"}).to_string());
    let asking = ask(&h, request);
    let asked = service_request(&mut h).await;
    assert_eq!(
        asked["params"]["params"]["execution"],
        json!({"kind": "highspeedCard", "cardId": "hsc_1"})
    );
    answer(
        &mut h,
        &asked,
        json!({"kind": "headers", "headers": {"x-card": "c"}, "apiKey": "sk-card"}),
    )
    .await;
    assert_eq!(
        asking.await.unwrap().unwrap().api_key.unwrap().expose(),
        "sk-card"
    );
}

/// 只握手、进程级部件建好，不开任何会话：进程级的服务不要会话（§74.1）。
async fn handshaken(host_services: &[&str]) -> Harness {
    let mut h = Harness::new();
    h.handle.ready(
        Arc::new(Local),
        Arc::new(MemoryStore::default()),
        Arc::new(rust_cli_kernel_api::testing::MemoryArtifacts::default()),
    );
    h.send(initialize(host_services)).await;
    assert!(h.next().await["result"].is_object());
    h
}

const CIPHER: &str = "host.credentials.cipher";

/// 凭据的加解密经 Host（§74、18 §16.2）：进程级的请求，外壳不带 target，没开会话也能发；结果是 Host 给的值。
#[tokio::test]
async fn credential_values_are_sealed_by_the_host_without_a_target() {
    let mut h = handshaken(&[CIPHER]).await;
    let cipher = h.handle.cipher();
    let sealing = tokio::spawn(async move {
        cipher
            .encrypt(&rust_cli_base::secret::RedactedString::new("secret"))
            .await
    });
    let asked = service_request(&mut h).await;
    assert_eq!(asked["params"]["type"], CIPHER);
    assert!(asked["params"].get("target").is_none());
    assert_eq!(
        asked["params"]["params"],
        json!({"operation": "encrypt", "value": "secret"})
    );
    answer(
        &mut h,
        &asked,
        json!({"kind": "done", "value": "enc:v2:os:sealed"}),
    )
    .await;
    assert_eq!(sealing.await.unwrap().unwrap(), "enc:v2:os:sealed");
}

/// Host 的两种否定结论：解不开是坏值；凭据库不可用是存储错误（文字是 v4 桥的受控文字）。
#[tokio::test]
async fn the_hosts_cipher_failures_keep_their_kind() {
    use rust_cli_kernel_api::credential_cipher::CipherFailure;
    let mut h = handshaken(&[CIPHER]).await;
    let cipher = h.handle.cipher();
    let opening = {
        let cipher = cipher.clone();
        tokio::spawn(async move { cipher.decrypt("enc:v2:os:bad").await })
    };
    let asked = service_request(&mut h).await;
    assert_eq!(
        asked["params"]["params"],
        json!({"operation": "decrypt", "value": "enc:v2:os:bad"})
    );
    answer(
        &mut h,
        &asked,
        json!({"kind": "failed", "reason": "decrypt"}),
    )
    .await;
    assert_eq!(opening.await.unwrap().unwrap_err(), CipherFailure::Decrypt);

    let opening = tokio::spawn(async move { cipher.decrypt("enc:v2:os:x").await });
    let asked = service_request(&mut h).await;
    answer(
        &mut h,
        &asked,
        json!({"kind": "failed", "reason": "unavailable"}),
    )
    .await;
    assert!(matches!(
        opening.await.unwrap().unwrap_err(),
        CipherFailure::Unavailable(message) if message.starts_with("System credential storage is unavailable")
    ));
}

/// Host 没告知这个服务（旧的 Host）：不发，报不可用。
#[tokio::test]
async fn an_undeclared_cipher_service_is_unavailable_without_a_request() {
    use rust_cli_kernel_api::credential_cipher::CipherFailure;
    let mut h = handshaken(&[]).await;
    let failure = h.handle.cipher().decrypt("enc:v2:os:x").await.unwrap_err();
    assert_eq!(
        failure,
        CipherFailure::Unavailable(format!("this host does not provide {CIPHER}"))
    );
    pinged(&mut h, 9).await;
}
