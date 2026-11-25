//! MCP OAuth 的整条链（ext.md §3.12，本地假授权服务器、临时目录、测试密钥，不连任何真实服务）：发现 → 注册 → 授权地址 →
//! 回调 → 换令牌 → 发布（Node 的形状）→ 取令牌 → 快过期时刷新 → 并发刷新只发一次 → `invalid_grant` → Sign out；没有令牌时
//! 不碰系统凭据库。

use std::sync::{Arc, Mutex};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rust_cli_kernel_api::mcp_auth::McpOAuth;
use rust_cli_net::{Egress, HttpRequest, NetworkPolicy};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use url::Url;

use super::*;
use crate::fake_server_tests::{Captured, Harness, NOW_MS, Reply, Server};

/// 授权服务器替身记着的：授权地址里的 challenge、允许的 refresh token、发出去的 access token 的序号。
#[derive(Default)]
struct AuthState {
    challenge: Option<String>,
    refresh: Option<String>,
    issued: usize,
}

fn form(body: &str) -> Vec<(String, String)> {
    url::form_urlencoded::parse(body.as_bytes())
        .into_owned()
        .collect()
}

fn field(pairs: &[(String, String)], name: &str) -> Option<String> {
    pairs
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.clone())
}

/// 受保护资源在 `/mcp`，授权服务器在 `/as`（元数据、注册、令牌）。
async fn auth_server(state: Arc<Mutex<AuthState>>) -> Server {
    let base: Arc<Mutex<String>> = Arc::default();
    let url = base.clone();
    let server = Server::start(move |request: &Captured| {
        let base = url.lock().unwrap().clone();
        match (request.method.as_str(), request.path.as_str()) {
            ("GET", "/.well-known/oauth-protected-resource/mcp") => Reply::json(
                200,
                &json!({"resource": format!("{base}/mcp"), "authorization_servers": [format!("{base}/as")], "scopes_supported": ["files:read"]}),
            ),
            ("GET", "/.well-known/oauth-authorization-server/as") => Reply::json(
                200,
                &json!({
                    "issuer": format!("{base}/as"),
                    "authorization_endpoint": format!("{base}/as/authorize"),
                    "token_endpoint": format!("{base}/as/token"),
                    "registration_endpoint": format!("{base}/as/register"),
                    "response_types_supported": ["code"],
                    "code_challenge_methods_supported": ["S256"],
                    "scopes_supported": ["files:read", "offline_access"],
                }),
            ),
            ("POST", "/as/register") => {
                let body = request.json();
                Reply::json(201, &json!({"client_id": "dyn-1", "redirect_uris": body["redirect_uris"]}))
            }
            ("POST", "/as/token") => {
                let pairs = form(&request.body);
                let mut state = state.lock().unwrap();
                let granted = match field(&pairs, "grant_type").as_deref() {
                    Some("authorization_code") => {
                        let verifier = field(&pairs, "code_verifier").unwrap_or_default();
                        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
                        field(&pairs, "code").as_deref() == Some("code-1")
                            && state.challenge.as_deref() == Some(challenge.as_str())
                    }
                    Some("refresh_token") => {
                        state.refresh.is_some() && field(&pairs, "refresh_token") == state.refresh
                    }
                    _ => false,
                };
                if !granted {
                    return Reply::json(400, &json!({"error": "invalid_grant"}));
                }
                state.issued += 1;
                state.refresh = Some("rt-1".to_owned());
                Reply::json(
                    200,
                    &json!({"access_token": format!("at-{}", state.issued), "token_type": "Bearer", "expires_in": 3600, "refresh_token": "rt-1"}),
                )
            }
            _ => Reply::json(404, &json!({})),
        }
    })
    .await;
    *base.lock().unwrap() = server.url();
    server
}

fn target(server: &Server) -> McpAuthTarget {
    McpAuthTarget {
        server: "docs".to_owned(),
        url: format!("{}/mcp", server.url()),
        oauth: McpOAuth::default(),
    }
}

/// 没有人贴回调地址（本机的浏览器回调）。
fn no_paste() -> tokio::sync::mpsc::Receiver<rust_cli_kernel_api::mcp_auth::PastedCallback> {
    tokio::sync::mpsc::channel(1).1
}

/// 浏览器：打开回调地址，交回状态与正文。
async fn visit(url: &str) -> (u16, String) {
    let egress = Egress::new(NetworkPolicy::default()).await.unwrap();
    let response = egress.send(HttpRequest::get(url)).await.unwrap();
    let status = response.status();
    (status, response.text(4096).await.unwrap())
}

fn query(url: &Url, name: &str) -> Option<String> {
    url.query_pairs()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.into_owned())
}

/// 走完一次授权：给出授权地址的参数。
async fn sign_in(harness: &Harness, server: &Server, state: &Arc<Mutex<AuthState>>) -> Url {
    let client = harness.platform.mcp_oauth();
    let pending = client.begin(&target(server), None).await.unwrap();
    let url = Url::parse(pending.url()).unwrap();
    state.lock().unwrap().challenge = query(&url, "code_challenge");
    let redirect = query(&url, "redirect_uri").unwrap();
    let finish = tokio::spawn(pending.finish(no_paste()));
    // 陌生的 state 不打断这次授权
    let (status, _) = visit(&format!("{redirect}?code=code-1&state=someone-else")).await;
    assert_eq!(status, 400);
    let (status, text) = visit(&format!(
        "{redirect}?code=code-1&state={}",
        query(&url, "state").unwrap()
    ))
    .await;
    assert_eq!(
        (status, text.as_str()),
        (
            200,
            "Authorization successful! You may close this window and return to the CLI."
        )
    );
    finish.await.unwrap().unwrap();
    url
}

/// 凭据文件里这个服务器的一个键（JSON）。
async fn stored(harness: &Harness, server: &Server, name: &str) -> Option<Value> {
    let keys = Keys::of(&target(server));
    harness
        .credentials()
        .await
        .get(&keys.key(name))
        .unwrap()
        .map(|value| serde_json::from_str(value.expose()).unwrap())
}

#[tokio::test(flavor = "current_thread")]
async fn a_sign_in_discovers_registers_authorizes_and_publishes_tokens_in_node_s_shape() {
    let state = Arc::new(Mutex::new(AuthState::default()));
    let server = auth_server(state.clone()).await;
    let harness = Harness::new(&server.url()).await;
    let url = sign_in(&harness, &server, &state).await;

    // 授权地址：PKCE S256、state、scope 带上授权服务器提供的 offline_access 与 prompt=consent、resource
    assert_eq!(url.path(), "/as/authorize");
    assert_eq!(query(&url, "response_type").as_deref(), Some("code"));
    assert_eq!(query(&url, "client_id").as_deref(), Some("dyn-1"));
    assert_eq!(
        query(&url, "code_challenge_method").as_deref(),
        Some("S256")
    );
    assert_eq!(
        query(&url, "scope").as_deref(),
        Some("files:read offline_access")
    );
    assert_eq!(query(&url, "prompt").as_deref(), Some("consent"));
    assert_eq!(
        query(&url, "resource"),
        Some(format!("{}/mcp", server.url()))
    );
    let redirect = Url::parse(&query(&url, "redirect_uri").unwrap()).unwrap();
    assert_eq!(redirect.host_str(), Some("127.0.0.1"));
    assert_eq!(redirect.path(), "/oauth/callback/mcp/docs");

    // 注册：公开的本机 client、这次的回调地址、同一个 scope
    let requests = server.requests();
    let register = requests
        .iter()
        .find(|r| r.path == "/as/register")
        .unwrap()
        .json();
    assert_eq!(register["client_name"], "ZCode docs");
    assert_eq!(register["token_endpoint_auth_method"], "none");
    assert_eq!(
        register["grant_types"],
        json!(["authorization_code", "refresh_token"])
    );
    assert_eq!(register["redirect_uris"], json!([redirect.as_str()]));
    assert_eq!(register["scope"], "files:read offline_access");
    // 换令牌：code、verifier、redirect_uri、resource、client_id（公开 client 在正文里）
    let exchange = form(
        &requests
            .iter()
            .find(|r| r.path == "/as/token")
            .unwrap()
            .body,
    );
    assert_eq!(
        field(&exchange, "redirect_uri").as_deref(),
        Some(redirect.as_str())
    );
    assert_eq!(
        field(&exchange, "resource"),
        Some(format!("{}/mcp", server.url()))
    );
    assert_eq!(field(&exchange, "client_id").as_deref(), Some("dyn-1"));

    // 发布：canonical v2 与两份镜像、发现的结果
    let canonical = stored(&harness, &server, keys::CANONICAL).await.unwrap();
    assert_eq!(canonical["version"], 2);
    assert_eq!(canonical["client_information"]["client_id"], "dyn-1");
    assert_eq!(canonical["tokens"]["access_token"], "at-1");
    assert_eq!(canonical["tokens"]["refresh_token"], "rt-1");
    assert_eq!(canonical["obtained_at"], NOW_MS);
    assert_eq!(canonical["expires_at"], NOW_MS + 3_600_000);
    assert_eq!(canonical["issuer"], format!("{}/as", server.url()));
    assert_eq!(canonical["generation"].as_str().unwrap().len(), 32);
    let state_param = query(&url, "state").unwrap();
    let published_by: String = Sha256::digest(state_param.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(canonical["published_by"], published_by);
    assert_eq!(
        stored(&harness, &server, keys::LEGACY_TOKENS)
            .await
            .unwrap(),
        canonical["tokens"]
    );
    assert_eq!(
        stored(&harness, &server, keys::LEGACY_CLIENT)
            .await
            .unwrap(),
        canonical["client_information"]
    );
    assert_eq!(
        stored(&harness, &server, keys::DISCOVERY).await.unwrap()["authorizationServerUrl"],
        format!("{}/as", server.url())
    );

    let client = harness.platform.mcp_oauth();
    let token = client.token(&target(&server)).await.unwrap().unwrap();
    assert_eq!(token.expose(), "at-1");
    assert!(client.signed_in(&target(&server)).await);
}

#[tokio::test(flavor = "current_thread")]
async fn tokens_are_refreshed_when_they_are_about_to_expire_and_once_for_concurrent_401s() {
    let state = Arc::new(Mutex::new(AuthState::default()));
    let server = auth_server(state.clone()).await;
    let harness = Harness::new(&server.url()).await;
    sign_in(&harness, &server, &state).await;
    let client = harness.platform.mcp_oauth();
    let target = target(&server);
    let discoveries = server.count("/.well-known/");

    // 离过期还远：照用
    harness.advance(3_600_000 - EXPIRY_SKEW_MS - 1);
    assert_eq!(
        client.token(&target).await.unwrap().unwrap().expose(),
        "at-1"
    );
    // 进了 300 s 的余量：先刷新（发现用缓存，不再取）
    harness.advance(2);
    assert_eq!(
        client.token(&target).await.unwrap().unwrap().expose(),
        "at-2"
    );
    assert_eq!(server.count("/.well-known/"), discoveries);
    let refresh = form(&server.requests().last().unwrap().body);
    assert_eq!(
        field(&refresh, "grant_type").as_deref(),
        Some("refresh_token")
    );
    assert_eq!(field(&refresh, "refresh_token").as_deref(), Some("rt-1"));
    assert_eq!(
        field(&refresh, "resource"),
        Some(format!("{}/mcp", server.url()))
    );
    assert_eq!(
        stored(&harness, &server, keys::CANONICAL).await.unwrap()["published_by"],
        format!("refresh:{}", Keys::of(&target).prefix())
    );

    // 两个请求同时被拒（都拿着 at-2）：锁里重读，第二个直接用第一个换来的
    let rejected = RedactedString::new("at-2");
    let before = server.count("/as/token");
    let (first, second) = tokio::join!(
        client.refreshed(&target, Some(&rejected)),
        client.refreshed(&target, Some(&rejected))
    );
    assert_eq!(first.unwrap().expose(), "at-3");
    assert_eq!(second.unwrap().expose(), "at-3");
    assert_eq!(server.count("/as/token"), before + 1);
}

#[tokio::test(flavor = "current_thread")]
async fn a_rejected_refresh_token_drops_the_tokens_and_needs_authorization_and_sign_out_forgets_the_rest()
 {
    let state = Arc::new(Mutex::new(AuthState::default()));
    let server = auth_server(state.clone()).await;
    let harness = Harness::new(&server.url()).await;
    sign_in(&harness, &server, &state).await;
    let client = harness.platform.mcp_oauth();
    let target = target(&server);
    state.lock().unwrap().refresh = None;

    let error = client
        .refreshed(&target, Some(&RedactedString::new("at-1")))
        .await
        .unwrap_err();
    assert!(matches!(error, McpError::AuthRequired { .. }), "{error:?}");
    assert_eq!(stored(&harness, &server, keys::CANONICAL).await, None);
    assert_eq!(stored(&harness, &server, keys::LEGACY_TOKENS).await, None);
    // client 留着作再授权的种子（Node 的规则）
    assert!(
        stored(&harness, &server, keys::LEGACY_CLIENT)
            .await
            .is_some()
    );
    assert_eq!(client.token(&target).await.unwrap(), None);
    assert!(!client.signed_in(&target).await);

    client.sign_out(&target).await.unwrap();
    assert_eq!(stored(&harness, &server, keys::LEGACY_CLIENT).await, None);
    assert_eq!(stored(&harness, &server, keys::DISCOVERY).await, None);
}

#[tokio::test]
async fn without_stored_tokens_the_system_key_store_is_not_touched() {
    use crate::credentials::testing::MemoryKeyStore;
    let dir = tempfile::tempdir().unwrap();
    let file = rust_cli_paths::StoragePaths::resolve(&Default::default(), dir.path(), dir.path())
        .credentials();
    // 文件里有别的 `enc:v2:os` 值（账号登录写的）：读它要钥匙串
    crate::test_support::write(
        file.as_path(),
        json!({"oauth:zai:access_token": "enc:v2:os:000000000000000000000000:AAAA.BBBB.CCCC"})
            .to_string(),
    )
    .await;
    let keys = Arc::new(MemoryKeyStore::default());
    // 没有受管密钥：v2 的值要系统凭据库里的密钥
    let secret = crate::CredentialSecret {
        explicit: None,
        ..crate::fake_server_tests::secret()
    };
    let store = CredentialStore::new(file, &secret, keys.clone());
    let clock = Arc::new(rust_cli_kernel_api::testing::ManualClock::new(NOW_MS));
    let egress = Arc::new(Egress::new(NetworkPolicy::default()).await.unwrap());
    let client = McpOAuthClient::new(store, egress, clock);
    let target = McpAuthTarget {
        server: "docs".to_owned(),
        url: "https://mcp.example.com/mcp".to_owned(),
        oauth: McpOAuth::default(),
    };
    assert_eq!(client.token(&target).await.unwrap(), None);
    assert!(!client.signed_in(&target).await);
    assert_eq!(keys.reads(), 0);
}

/// 配置的 `authServerMetadataUrl` 与 `callbackPort`（ext.md §3.12.15）：只取配置的元数据地址（不做受保护资源与按路径的发现）、不带
/// resource、没配 scope 时用元数据的 scope；回调地址是 `http://localhost:<端口>/callback`，在这个端口上收回调。
#[tokio::test(flavor = "current_thread")]
async fn configured_metadata_url_and_callback_port_shape_the_sign_in() {
    let state = Arc::new(Mutex::new(AuthState::default()));
    let server = auth_server(state.clone()).await;
    let harness = Harness::new(&server.url()).await;
    // 一个空着的端口（先占再放）
    let port = rust_cli_net::loopback::LoopbackListener::bind()
        .await
        .unwrap()
        .port();
    let target = McpAuthTarget {
        oauth: McpOAuth {
            metadata_url: Some(format!(
                "{}/.well-known/oauth-authorization-server/as",
                server.url()
            )),
            callback_port: Some(port),
            ..McpOAuth::default()
        },
        ..target(&server)
    };
    let client = harness.platform.mcp_oauth();
    let pending = client.begin(&target, None).await.unwrap();
    let url = Url::parse(pending.url()).unwrap();
    assert_eq!(
        query(&url, "redirect_uri"),
        Some(format!("http://localhost:{port}/callback"))
    );
    assert_eq!(query(&url, "resource"), None);
    assert_eq!(
        query(&url, "scope").as_deref(),
        Some("files:read offline_access")
    );
    // 只取了配置的元数据地址
    assert_eq!(server.count("/.well-known/oauth-protected-resource"), 0);
    state.lock().unwrap().challenge = query(&url, "code_challenge");
    let finish = tokio::spawn(pending.finish(no_paste()));
    let (status, _) = visit(&format!(
        "http://127.0.0.1:{port}/callback?code=code-1&state={}",
        query(&url, "state").unwrap()
    ))
    .await;
    assert_eq!(status, 200);
    finish.await.unwrap().unwrap();
    let exchange = form(
        &server
            .requests()
            .iter()
            .find(|r| r.path == "/as/token")
            .unwrap()
            .body,
    );
    assert_eq!(
        field(&exchange, "redirect_uri"),
        Some(format!("http://localhost:{port}/callback"))
    );
    assert_eq!(field(&exchange, "resource"), None);
    assert_eq!(
        client.token(&target).await.unwrap().unwrap().expose(),
        "at-1"
    );
}

/// 用户贴回来的回调地址（授权伪工具 complete_authentication，ext.md §3.12.16）：别的 state 不收、不打断；这一次的收下、同回调换令牌。
#[tokio::test(flavor = "current_thread")]
async fn a_pasted_callback_url_completes_the_sign_in() {
    let state = Arc::new(Mutex::new(AuthState::default()));
    let server = auth_server(state.clone()).await;
    let harness = Harness::new(&server.url()).await;
    let client = harness.platform.mcp_oauth();
    let pending = client.begin(&target(&server), None).await.unwrap();
    let url = Url::parse(pending.url()).unwrap();
    state.lock().unwrap().challenge = query(&url, "code_challenge");
    let redirect = query(&url, "redirect_uri").unwrap();
    let (paste, pasted) = tokio::sync::mpsc::channel(1);
    let finish = tokio::spawn(pending.finish(pasted));
    let submit = |text: String| {
        let paste = paste.clone();
        async move {
            let (accepted, answer) = tokio::sync::oneshot::channel();
            paste
                .send(rust_cli_kernel_api::mcp_auth::PastedCallback {
                    url: text,
                    accepted,
                })
                .await
                .unwrap();
            answer.await.unwrap()
        }
    };
    assert!(!submit(format!("{redirect}?code=code-1&state=someone-else")).await);
    assert!(
        submit(format!(
            "{redirect}?code=code-1&state={}",
            query(&url, "state").unwrap()
        ))
        .await
    );
    finish.await.unwrap().unwrap();
    assert_eq!(
        client
            .token(&target(&server))
            .await
            .unwrap()
            .unwrap()
            .expose(),
        "at-1"
    );
}
