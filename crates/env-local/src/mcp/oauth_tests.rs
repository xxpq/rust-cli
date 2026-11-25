//! 用 OAuth 的 HTTP / SSE 服务器（docs/specs/rust-cli/ext.md §3.12.3）：带 Bearer、401 之后换一个重发一次、换不了与重发仍 401
//! 时是 `AuthRequired`、403 的 insufficient_scope、配置了 Authorization 头时不用 OAuth。服务器是本地的假 MCP 服务器。

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rust_cli_base::secret::RedactedString;
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::EnvironmentId;
use rust_cli_kernel_api::mcp::{McpConnect, McpError, McpTransport, McpTransportConfig};
use rust_cli_kernel_api::mcp_auth::{McpAuthTarget, McpTokens};
use rust_cli_net::Egress;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::LocalMcpTransport;

/// 令牌端口的替身：`token` 给 `current`；`refreshed` 记下被拒的令牌，给 `fresh`（None 时“要授权”）。
struct Tokens {
    current: Option<&'static str>,
    fresh: Option<&'static str>,
    rejected: Mutex<Vec<Option<String>>>,
}

#[async_trait]
impl McpTokens for Tokens {
    async fn token(&self, target: &McpAuthTarget) -> Result<Option<RedactedString>, McpError> {
        assert_eq!(target.server, "docs");
        Ok(self.current.map(RedactedString::new))
    }

    async fn refreshed(
        &self,
        _target: &McpAuthTarget,
        rejected: Option<&RedactedString>,
    ) -> Result<RedactedString, McpError> {
        self.rejected
            .lock()
            .unwrap()
            .push(rejected.map(|token| token.expose().to_owned()));
        self.fresh
            .map(RedactedString::new)
            .ok_or_else(|| McpError::AuthRequired {
                message: "no refresh token".to_owned(),
                scope: None,
            })
    }
}

fn tokens(current: Option<&'static str>, fresh: Option<&'static str>) -> Arc<Tokens> {
    Arc::new(Tokens {
        current,
        fresh,
        rejected: Mutex::default(),
    })
}

/// 假 MCP 服务器：`Authorization` 等于 `Bearer <accept>` 的 POST 回 JSON 回复，其余回 `refusal`。记下每个请求的
/// `Authorization`（没有时 None）。GET（SSE 的事件流、监听流）同样检查。
struct Server {
    base: String,
    seen: Arc<Mutex<Vec<Option<String>>>>,
    task: tokio::task::JoinHandle<()>,
}

#[expect(clippy::disallowed_methods, reason = "测试搭本地 HTTP 夹具")]
async fn start_server(accept: &'static str, refusal: &'static str) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    let task = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = vec![0u8; 16 * 1024];
            let read = socket.read(&mut buffer).await.unwrap_or(0);
            let text = String::from_utf8_lossy(&buffer[..read]).into_owned();
            let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
            let authorization = head
                .lines()
                .filter_map(|line| line.split_once(':'))
                .find(|(name, _)| name.trim().eq_ignore_ascii_case("authorization"))
                .map(|(_, value)| value.trim().to_owned());
            let allowed = authorization.as_deref() == Some(&format!("Bearer {accept}"));
            log.lock().unwrap().push(authorization);
            let answer = if !allowed {
                refusal.to_owned()
            } else if head.starts_with("GET") {
                "HTTP/1.1 405 Method Not Allowed\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                    .to_owned()
            } else {
                let id =
                    serde_json::from_str::<Value>(body).map_or(Value::Null, |v| v["id"].clone());
                let reply = json!({"jsonrpc": "2.0", "id": id, "result": {}}).to_string();
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}",
                    reply.len()
                )
            };
            let _ = socket.write_all(answer.as_bytes()).await;
        }
    });
    Server { base, seen, task }
}

const UNAUTHORIZED: &str = "HTTP/1.1 401 Unauthorized\r\nwww-authenticate: Bearer resource_metadata=\"http://127.0.0.1/.well-known/oauth-protected-resource\"\r\ncontent-length: 0\r\nconnection: close\r\n\r\n";
const FORBIDDEN_SCOPE: &str = "HTTP/1.1 403 Forbidden\r\nwww-authenticate: Bearer error=\"insufficient_scope\", scope=\"files:read files:write\"\r\ncontent-length: 0\r\nconnection: close\r\n\r\n";

async fn local_transport(tokens: Option<Arc<Tokens>>) -> (LocalMcpTransport, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let egress = Arc::new(
        Egress::new(rust_cli_net::NetworkPolicy::default())
            .await
            .unwrap(),
    );
    let transport =
        LocalMcpTransport::new(dir.path().join("ledger"), Vec::new().into(), Some(egress));
    let transport = match tokens {
        Some(tokens) => transport.with_tokens(tokens),
        None => transport,
    };
    (transport, dir)
}

fn workspace(dir: &tempfile::TempDir) -> WorkspaceRef {
    WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new(dir.path().to_str().unwrap()),
    }
}

fn http(base: &str, headers: &[(&str, &str)]) -> McpConnect {
    McpConnect {
        server: "docs".to_owned(),
        transport: McpTransportConfig::http(
            format!("{base}/mcp"),
            headers
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect::<BTreeMap<_, _>>(),
        ),
    }
}

/// 连上并发 `initialize`，交回发送的结果。
async fn initialize(
    transport: &LocalMcpTransport,
    dir: &tempfile::TempDir,
    connect: McpConnect,
) -> Result<(), McpError> {
    let id = transport.connect(&workspace(dir), connect).await?;
    transport
        .send(
            &id,
            json!({"jsonrpc": "2.0", "id": 0, "method": "initialize"}),
        )
        .await
}

fn bearers(seen: &Mutex<Vec<Option<String>>>) -> Vec<Option<String>> {
    seen.lock().unwrap().clone()
}

#[tokio::test]
async fn a_request_carries_the_stored_token_as_a_bearer() {
    let server = start_server("t1", UNAUTHORIZED).await;
    let (transport, dir) = local_transport(Some(tokens(Some("t1"), None))).await;
    initialize(&transport, &dir, http(&server.base, &[]))
        .await
        .unwrap();
    assert_eq!(bearers(&server.seen), [Some("Bearer t1".to_owned())]);
    server.task.abort();
}

#[tokio::test]
async fn a_401_gets_a_fresh_token_and_the_request_is_sent_once_more() {
    let server = start_server("new", UNAUTHORIZED).await;
    let port = tokens(Some("old"), Some("new"));
    let (transport, dir) = local_transport(Some(port.clone())).await;
    initialize(&transport, &dir, http(&server.base, &[]))
        .await
        .unwrap();
    assert_eq!(
        bearers(&server.seen),
        [Some("Bearer old".to_owned()), Some("Bearer new".to_owned())]
    );
    assert_eq!(*port.rejected.lock().unwrap(), [Some("old".to_owned())]);
    server.task.abort();
}

#[tokio::test]
async fn a_401_after_the_fresh_token_or_without_one_needs_authorization() {
    // 换来的令牌也被拒：只重发一次
    let server = start_server("never", UNAUTHORIZED).await;
    let (transport, dir) = local_transport(Some(tokens(Some("old"), Some("new")))).await;
    let error = initialize(&transport, &dir, http(&server.base, &[]))
        .await
        .unwrap_err();
    assert!(
        matches!(error, McpError::AuthRequired { scope: None, .. }),
        "{error:?}"
    );
    assert_eq!(bearers(&server.seen).len(), 2);
    server.task.abort();
    // 没有存过令牌、也换不来：不带 Authorization 发一次，“要授权”
    let server = start_server("never", UNAUTHORIZED).await;
    let port = tokens(None, None);
    let (transport, dir) = local_transport(Some(port.clone())).await;
    let error = initialize(&transport, &dir, http(&server.base, &[]))
        .await
        .unwrap_err();
    assert!(matches!(error, McpError::AuthRequired { .. }), "{error:?}");
    assert_eq!(bearers(&server.seen), [None]);
    assert_eq!(*port.rejected.lock().unwrap(), [None]);
    server.task.abort();
}

#[tokio::test]
async fn without_a_token_port_a_401_needs_authorization_at_once() {
    let server = start_server("never", UNAUTHORIZED).await;
    let (transport, dir) = local_transport(None).await;
    let error = initialize(&transport, &dir, http(&server.base, &[]))
        .await
        .unwrap_err();
    assert!(matches!(error, McpError::AuthRequired { .. }), "{error:?}");
    assert_eq!(bearers(&server.seen), [None]);
    server.task.abort();
}

#[tokio::test]
async fn a_403_for_insufficient_scope_reports_the_scope_it_needs() {
    let server = start_server("never", FORBIDDEN_SCOPE).await;
    let (transport, dir) = local_transport(Some(tokens(Some("t1"), None))).await;
    let error = initialize(&transport, &dir, http(&server.base, &[]))
        .await
        .unwrap_err();
    let McpError::AuthRequired { scope, .. } = error else {
        panic!("{error:?}");
    };
    assert_eq!(scope.as_deref(), Some("files:read files:write"));
    server.task.abort();
}

#[tokio::test]
async fn a_configured_authorization_header_is_sent_as_is_and_a_401_stays_a_connection_failure() {
    let server = start_server("never", UNAUTHORIZED).await;
    let port = tokens(Some("t1"), Some("t2"));
    let (transport, dir) = local_transport(Some(port.clone())).await;
    let error = initialize(
        &transport,
        &dir,
        http(&server.base, &[("Authorization", "Bearer static")]),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(&error, McpError::Connect { message } if message.starts_with("HTTP 401")),
        "{error:?}"
    );
    assert_eq!(bearers(&server.seen), [Some("Bearer static".to_owned())]);
    assert!(port.rejected.lock().unwrap().is_empty());
    server.task.abort();
}

#[tokio::test]
async fn the_sse_stream_request_needs_authorization_too() {
    let server = start_server("never", UNAUTHORIZED).await;
    let (transport, dir) = local_transport(None).await;
    let connect = McpConnect {
        server: "docs".to_owned(),
        transport: McpTransportConfig::sse(format!("{}/sse", server.base), BTreeMap::new()),
    };
    let error = transport
        .connect(&workspace(&dir), connect)
        .await
        .unwrap_err();
    assert!(matches!(error, McpError::AuthRequired { .. }), "{error:?}");
    server.task.abort();
}
