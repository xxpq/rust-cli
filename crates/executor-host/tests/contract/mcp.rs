//! MCP 的管道（R5，remote-exec.md §22.4、§22.6）：本机的 `LocalMcpTransport` 与“env-remote + 内存 loopback +
//! exec-server + 同一个 `LocalMcpTransport`”结果相同——消息往返、关闭、服务器退出、起不来时的原文、streamable HTTP
//! 续不上时的 `ReplyLost`。

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use rust_cli_env_local::{LocalEnvironment, LocalMcpTransport};
use rust_cli_kernel_api::mcp::{McpConnect, McpTransportConfig, McpTransportEvent};
use serde_json::json;

use crate::contract;
use crate::support::{Kind, machine_with_mcp, workspace};

async fn setup(kind: Kind) -> (crate::support::Machine, tempfile::TempDir) {
    setup_with(kind, None).await
}

/// `egress` 给 HTTP 传输用（stdio 不要）。
async fn setup_with(
    kind: Kind,
    egress: Option<Arc<rust_cli_net::Egress>>,
) -> (crate::support::Machine, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let env = vec![("PATH".to_owned(), "/usr/bin:/bin".to_owned())];
    let mcp = Arc::new(LocalMcpTransport::new(
        dir.path().join("ledger"),
        env.into(),
        egress,
    ));
    let local = LocalEnvironment::new(None);
    (machine_with_mcp(kind, local, mcp).await, dir)
}

fn stdio(command: &str, args: &[&str]) -> McpConnect {
    McpConnect {
        server: "s".to_owned(),
        transport: McpTransportConfig::stdio(
            command.to_owned(),
            args.iter().map(|arg| (*arg).to_owned()).collect(),
        ),
    }
}

async fn next(events: &mut BoxStream<'static, McpTransportEvent>) -> Option<McpTransportEvent> {
    tokio::time::timeout(Duration::from_secs(10), events.next())
        .await
        .unwrap_or_else(|_| panic!("no event in time"))
}

async fn messages_round_trip_and_close(kind: Kind) {
    let (machine, dir) = setup(kind).await;
    let mcp = machine.mcp();
    let id = mcp
        .connect(&workspace(dir.path()), stdio("cat", &[]))
        .await
        .unwrap();
    let mut events = mcp.events(&id);
    let message = json!({"jsonrpc": "2.0", "id": 0, "method": "ping"});
    mcp.send(&id, message.clone()).await.unwrap();
    assert_eq!(
        next(&mut events).await,
        Some(McpTransportEvent::Message { message })
    );
    mcp.close(&id).await.unwrap();
    assert_eq!(
        next(&mut events).await,
        Some(McpTransportEvent::Closed {
            reason: None,
            session_expired: false
        })
    );
    assert_eq!(next(&mut events).await, None);
    assert!(mcp.send(&id, json!({})).await.is_err());
}

async fn a_server_that_exits_is_reported(kind: Kind) {
    let (machine, dir) = setup(kind).await;
    let mcp = machine.mcp();
    let script = r#"printf '{"hello":"world"}\n'"#;
    let id = mcp
        .connect(&workspace(dir.path()), stdio("/bin/sh", &["-c", script]))
        .await
        .unwrap();
    let mut events = mcp.events(&id);
    assert_eq!(
        next(&mut events).await,
        Some(McpTransportEvent::Message {
            message: json!({"hello": "world"})
        })
    );
    assert_eq!(
        next(&mut events).await,
        Some(McpTransportEvent::Closed {
            reason: Some("MCP server \"s\" process exited".to_owned()),
            session_expired: false
        })
    );
}

async fn a_server_that_cannot_start_keeps_its_message(kind: Kind) {
    let (machine, dir) = setup(kind).await;
    let error = machine
        .mcp()
        .connect(&workspace(dir.path()), stdio("/no/such/mcp-server", &[]))
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .starts_with("MCP server \"s\" failed to start: "),
        "{error}"
    );
}

/// 每个 POST 都回一个空的 SSE 流就断开的 streamable HTTP 服务器：回复没到、没有事件 ID，续不上（ext.md §3.10.6）。
fn cut_streams_server() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for socket in listener.incoming() {
            let Ok(mut socket) = socket else { return };
            // 读完请求（头与 content-length 的正文）再回，免得客户端还在写时连接就断了。
            let mut seen = Vec::new();
            let mut buffer = [0u8; 4096];
            while let Ok(read) = socket.read(&mut buffer) {
                if read == 0 {
                    break;
                }
                seen.extend_from_slice(&buffer[..read]);
                let text = String::from_utf8_lossy(&seen);
                let Some((head, body)) = text.split_once("\r\n\r\n") else {
                    continue;
                };
                let length = head
                    .lines()
                    .filter_map(|line| line.split_once(':'))
                    .find(|(name, _)| name.trim().eq_ignore_ascii_case("content-length"))
                    .and_then(|(_, value)| value.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                if body.len() >= length {
                    break;
                }
            }
            let _ = socket.write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
            );
        }
    });
    url
}

/// v5r-mcp3（ext.md §3.10.6 的“续不上时”，remote-exec.md §22.4）：执行器的传输与本机的是同一个；它给出的 `ReplyLost`
/// 经线上的 `reply_lost` 原样回到控制面（exec-client 在 hello 里报告了 `mcp_reply_lost`）。
async fn a_reply_that_cannot_arrive_is_reported(kind: Kind) {
    let egress = rust_cli_net::Egress::new(rust_cli_net::NetworkPolicy::default())
        .await
        .unwrap();
    let (machine, dir) = setup_with(kind, Some(Arc::new(egress))).await;
    let mcp = machine.mcp();
    let connect = McpConnect {
        server: "web".to_owned(),
        transport: McpTransportConfig::http(cut_streams_server(), BTreeMap::new()),
    };
    let id = mcp.connect(&workspace(dir.path()), connect).await.unwrap();
    let mut events = mcp.events(&id);
    let call = json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call"});
    mcp.send(&id, call).await.unwrap();
    assert_eq!(
        next(&mut events).await,
        Some(McpTransportEvent::ReplyLost {
            id: json!(3),
            reason: "MCP response stream ended before the reply, with no event ID to resume from"
                .to_owned()
        })
    );
}

contract!(
    messages_round_trip_and_close,
    a_server_that_exits_is_reported,
    a_server_that_cannot_start_keeps_its_message,
    a_reply_that_cannot_arrive_is_reported,
);
