//! 握手的版本协商（docs/specs/rust-cli/ext.md §3.10.2）：发可用修订的第一项，接受服务器回的可用修订，别的断开；
//! 配置钉死新纪元时不连接。

use std::sync::Arc;

use rust_cli_base::TaskScope;
use rust_cli_kernel_api::env::WorkspaceRef;
use rust_cli_kernel_api::mcp::{McpConnectionId, McpError, McpTransportConfig, McpTransportEvent};

use super::*;
use crate::mcp::rpc::Refusal;
use crate::mcp::{McpProtocol, McpScope, McpServerOptions};
use crate::test_support::{FakeMcp, Handler, workspace};

/// `initialize` 回 `reply` 作 `protocolVersion`（None 时不带这个字段）的服务器。
fn answering(reply: Option<&'static str>) -> Arc<Handler> {
    Arc::new(move |method: &str, _: &Value| {
        (method == "initialize").then(|| {
            let mut result =
                json!({"capabilities": {}, "serverInfo": {"name": "fake", "version": "1"}});
            if let Some(version) = reply {
                result["protocolVersion"] = json!(version);
            }
            Ok(result)
        })
    })
}

fn server(transport: McpTransportConfig) -> McpServer {
    pinned(transport, McpProtocol::Initialize)
}

fn pinned(transport: McpTransportConfig, protocol: McpProtocol) -> McpServer {
    McpServer {
        name: "s".to_owned(),
        scope: McpScope::Project,
        transport,
        options: McpServerOptions {
            protocol,
            ..Default::default()
        },
        machine: Default::default(),
    }
}

fn stdio() -> McpTransportConfig {
    McpTransportConfig::stdio("node", Vec::new())
}

fn sse() -> McpTransportConfig {
    McpTransportConfig::sse("https://mcp.example.com/sse", Default::default())
}

fn http() -> McpTransportConfig {
    McpTransportConfig::http("https://mcp.example.com/mcp", Default::default())
}

/// 握手的结果、服务器收到的消息与连接次数。
async fn handshake(
    server: &McpServer,
    reply: Option<&'static str>,
) -> (Result<Session, String>, Vec<Value>, usize) {
    let fake = Arc::new(FakeMcp::default().with("s", answering(reply)));
    let local: Arc<dyn McpTransport> = fake.clone();
    let workspace = workspace();
    let pipe = Pipe {
        local: &local,
        machines: None,
        workspace: &workspace,
    };
    let scope = TaskScope::root("test", |_| {});
    let opened = refusal_text(
        open(
            &pipe,
            &scope.spawner(),
            server,
            Duration::from_secs(5),
            ("1", None),
        )
        .await,
    );
    let sent = fake
        .received()
        .into_iter()
        .map(|(_, message)| message)
        .collect();
    (opened, sent, fake.roots().len())
}

fn methods(sent: &[Value]) -> Vec<&str> {
    sent.iter()
        .filter_map(|message| message["method"].as_str())
        .collect()
}

/// 三种传输都发 2025-11-25；服务器回较低的修订时接受。streamable HTTP 以前只到 2025-06-18（传输没有 SEP-1699 的续读，
/// 回 2025-11-25 的服务器断开）；v5r-mcp2 线补上续读之后放开（ext.md §3.10.2、§3.10.6）。
#[tokio::test]
async fn offers_the_newest_revision_and_takes_an_older_reply() {
    for transport in [stdio(), sse(), http()] {
        for reply in ["2025-11-25", "2024-11-05"] {
            let (opened, sent, _) = handshake(&server(transport.clone()), Some(reply)).await;
            assert_eq!(
                opened.map(|session| session.protocol_version).ok(),
                Some(reply),
                "{transport:?}"
            );
            assert_eq!(
                sent[0]["params"]["protocolVersion"], "2025-11-25",
                "{transport:?}"
            );
            assert_eq!(methods(&sent), ["initialize", "notifications/initialized"]);
        }
    }
}

/// 回不认的修订（更新的、发布前的草稿、没有）时不发 `notifications/initialized`，断开。
#[tokio::test]
async fn an_unknown_reply_disconnects() {
    for (reply, shown) in [
        (Some("2026-07-28"), "2026-07-28"),
        (Some("2024-10-07"), "2024-10-07"),
        (None, "none"),
    ] {
        let (opened, sent, _) = handshake(&server(stdio()), reply).await;
        assert_eq!(
            opened.err(),
            Some(format!(
                "Server's protocol version is not supported: {shown}"
            ))
        );
        assert_eq!(methods(&sent), ["initialize"], "{reply:?}");
    }
}

/// `initialize` 的回复续不上（`ReplyLost`）时，握手立即以传输给的原因失败（v5r-mcp3 线，ext.md §3.10.6 的“续不上时”）。
/// rmcp 的传输接口没有这种事件（§3.11 的 G5）：适配器不处理时握手等到它的期限，这里失败。
#[tokio::test(start_paused = true)]
async fn a_lost_initialize_reply_fails_the_handshake_with_its_reason() {
    let reason = "MCP response stream ended before the reply, with no event ID to resume from";
    let fake = Arc::new(
        FakeMcp::default()
            .with("s", answering(Some("2025-11-25")))
            .losing("initialize", reason),
    );
    let local: Arc<dyn McpTransport> = fake.clone();
    let workspace = workspace();
    let pipe = Pipe {
        local: &local,
        machines: None,
        workspace: &workspace,
    };
    let scope = TaskScope::root("test", |_| {});
    let opened = open(
        &pipe,
        &scope.spawner(),
        &server(http()),
        Duration::from_secs(100),
        ("1", None),
    )
    .await;
    assert_eq!(refusal_text(opened).err().as_deref(), Some(reason));
    let sent: Vec<Value> = fake
        .received()
        .into_iter()
        .map(|(_, message)| message)
        .collect();
    assert_eq!(methods(&sent), ["initialize"]);
}

/// `initialize` 的回复只要修订对就连上，少了 `serverInfo`、`capabilities` 也行（以前就这样）；能力与 instructions 从原样的
/// 回复里取。rmcp 按类型解析 `InitializeResult` 时少了它们就握手失败（§3.11 的 G10），这里失败。
#[tokio::test]
async fn a_reply_with_only_a_revision_still_connects() {
    let fake = Arc::new(FakeMcp::default().with(
        "s",
        Arc::new(|method: &str, _: &Value| {
            (method == "initialize").then(|| {
                Ok(json!({"protocolVersion": "2025-06-18", "instructions": "Use it.", "capabilities": {"tools": {"listChanged": "yes"}}}))
            })
        }),
    ));
    let local: Arc<dyn McpTransport> = fake.clone();
    let workspace = workspace();
    let pipe = Pipe {
        local: &local,
        machines: None,
        workspace: &workspace,
    };
    let scope = TaskScope::root("test", |_| {});
    let session = open(
        &pipe,
        &scope.spawner(),
        &server(stdio()),
        Duration::from_secs(5),
        ("1", None),
    )
    .await
    .unwrap();
    assert_eq!(session.protocol_version, "2025-06-18");
    assert_eq!(session.instructions.as_deref(), Some("Use it."));
    assert_eq!(
        session.capabilities,
        json!({"tools": {"listChanged": "yes"}})
    );
}

/// `protocolVersion: "2026-07-28"` 钉死新纪元、不回退：本客户端还不会，不连接（以前警告后按旧握手连接）。
#[tokio::test]
async fn a_pinned_modern_revision_does_not_connect() {
    for transport in [stdio(), sse(), http()] {
        let (opened, sent, connects) =
            handshake(&pinned(transport, McpProtocol::Modern), Some("2025-11-25")).await;
        assert_eq!(
            opened.err().as_deref(),
            Some(
                "protocolVersion \"2026-07-28\" requires the 2026-07-28 protocol revision, which this client does not support yet"
            )
        );
        assert!(sent.is_empty());
        assert_eq!(connects, 0);
    }
}

/// 替身传输加两样：`connect` 先等 `delay`（真实的 stdio 起进程要几毫秒），每次 `close` 计数。
struct Recording {
    fake: Arc<FakeMcp>,
    delay: Duration,
    closes: tokio::sync::watch::Sender<usize>,
}

#[async_trait::async_trait]
impl McpTransport for Recording {
    async fn connect(
        &self,
        workspace: &WorkspaceRef,
        request: McpConnect,
    ) -> Result<McpConnectionId, McpError> {
        tokio::time::sleep(self.delay).await;
        self.fake.connect(workspace, request).await
    }

    async fn send(&self, connection: &McpConnectionId, message: Value) -> Result<(), McpError> {
        self.fake.send(connection, message).await
    }

    fn events(
        &self,
        connection: &McpConnectionId,
    ) -> futures_util::stream::BoxStream<'static, McpTransportEvent> {
        self.fake.events(connection)
    }

    async fn close(&self, connection: &McpConnectionId) -> Result<(), McpError> {
        self.closes.send_modify(|closes| *closes += 1);
        self.fake.close(connection).await
    }
}

/// 一个服务器 `s`（`initialize` 回 `reply`，None 时不回答）的管道，连接要花 `delay`。
fn recording(reply: Option<&'static str>, delay: Duration) -> (Arc<FakeMcp>, Arc<Recording>) {
    let handler = match reply {
        Some(_) => answering(reply),
        None => crate::test_support::held(|_, _| None),
    };
    let fake = Arc::new(FakeMcp::default().with("s", handler));
    let transport = Arc::new(Recording {
        fake: fake.clone(),
        delay,
        closes: tokio::sync::watch::channel(0).0,
    });
    (fake, transport)
}

async fn open_on(
    transport: &Arc<Recording>,
    scope: &TaskScope,
    timeout: Duration,
) -> Result<Session, String> {
    let local: Arc<dyn McpTransport> = transport.clone();
    let workspace = workspace();
    let pipe = Pipe {
        local: &local,
        machines: None,
        workspace: &workspace,
    };
    refusal_text(
        open(
            &pipe,
            &scope.spawner(),
            &server(stdio()),
            timeout,
            ("1", None),
        )
        .await,
    )
}

/// 握手没成的原因的文字（要授权的单列，这里的用例都不是）。
fn refusal_text(opened: Result<Session, Refusal>) -> Result<Session, String> {
    opened.map_err(|refusal| match refusal {
        Refusal::Failed(reason) => reason,
        Refusal::AuthRequired { .. } => "needs authorization".to_owned(),
    })
}

/// 等这个传输关闭一次连接；期限只用来让测试失败而不是挂住（测试用暂停的时钟，空闲时直接跳到期限）。
async fn closed(transport: &Recording) -> bool {
    let mut closes = transport.closes.subscribe();
    tokio::time::timeout(
        Duration::from_secs(60),
        closes.wait_for(|closes| *closes > 0),
    )
    .await
    .is_ok()
}

/// 缺陷（v5r-mcp2 线，ext.md §3.10.7）：握手到期时原来不关连接，stdio 服务器的进程留到 rust 退出。整个握手的期限从调
/// `open` 起算、比 `initialize` 自己的早到（差 `connect` 的耗时），到期时丢弃握手的 future，出错分支里的关闭走不到。
#[tokio::test(start_paused = true)]
async fn a_handshake_that_runs_out_of_time_closes_its_connection() {
    let (_fake, transport) = recording(None, Duration::from_millis(10));
    let scope = TaskScope::root("test", |_| {});
    let opened = open_on(&transport, &scope, Duration::from_millis(100)).await;
    assert_eq!(
        opened.err().as_deref(),
        Some("MCP server \"s\" connection timed out after 100ms")
    );
    assert!(closed(&transport).await);
}

/// 握手中连接任务被取消（`/mcp reconnect`、停用、会话释放；这里用外面的期限丢弃 `open`）：原来不关连接（§3.10.7）。
#[tokio::test(start_paused = true)]
async fn a_dropped_handshake_closes_its_connection() {
    let (fake, transport) = recording(None, Duration::ZERO);
    let scope = TaskScope::root("test", |_| {});
    let cut = Duration::from_millis(50);
    let opened =
        tokio::time::timeout(cut, open_on(&transport, &scope, Duration::from_secs(5))).await;
    assert!(opened.is_err());
    assert_eq!(fake.received()[0].1["method"], "initialize");
    assert!(closed(&transport).await);
}

/// 握手成功、发布之前连接任务被取消（`serve` 拉清单时）：会话随 future 一起丢弃，原来不关连接（§3.10.7）。
#[tokio::test(start_paused = true)]
async fn a_session_dropped_before_it_is_published_closes_its_connection() {
    let (_fake, transport) = recording(Some("2025-11-25"), Duration::ZERO);
    let scope = TaskScope::root("test", |_| {});
    let session = open_on(&transport, &scope, Duration::from_secs(5)).await;
    assert!(session.is_ok());
    assert_eq!(*transport.closes.borrow(), 0);
    drop(session);
    assert!(closed(&transport).await);
}

/// 缺陷（v5r-mcp2 线，ext.md §3.10.8）：`initialize` 到期时原来可能发 `notifications/cancelled`，规范不许客户端取消
/// `initialize`。连接与发送不花时间时（这个替身；真实的传输里只有它们在同一个计时器刻度内做完才会这样），`initialize`
/// 自己的期限与整个握手的同时到，tokio 先轮询里面的 future，于是对请求 0 发了取消。现在 `initialize` 没有自己的期限，
/// 到期只关闭连接。
#[tokio::test(start_paused = true)]
async fn initialize_is_never_cancelled() {
    let (fake, transport) = recording(None, Duration::ZERO);
    let scope = TaskScope::root("test", |_| {});
    let opened = open_on(&transport, &scope, Duration::from_millis(100)).await;
    assert_eq!(
        opened.err().as_deref(),
        Some("MCP server \"s\" connection timed out after 100ms")
    );
    assert!(closed(&transport).await);
    let sent: Vec<Value> = fake
        .received()
        .into_iter()
        .map(|(_, message)| message)
        .collect();
    assert_eq!(methods(&sent), ["initialize"]);
}
