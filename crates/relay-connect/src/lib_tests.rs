//! 执行中转的拨号规矩：拨号带上票、账号与子协议；哪些状态码是终态；409 先拉起沙盒再拨一次，只一次；`/up` 的头与
//! 请求体照契约。
//!
//! 服务端是本进程的一个线程，只听 127.0.0.1，按顺序回给定的响应；回完就关掉监听，多出来的连接会被拒绝。

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;

use rust_cli_net::NetworkPolicy;

use super::*;

/// 服务端对一次请求的回答。
enum Reply {
    /// 回这个状态码与响应体，然后关掉连接。
    Status(u16, &'static str),
    /// 回 101，再发一个二进制帧 `ready`，等客户端断开。
    Upgrade,
}

/// 收到的一次请求。
struct Seen {
    /// 请求行与请求头。
    head: String,
    body: String,
}

#[expect(
    clippy::disallowed_methods,
    reason = "测试的本地 HTTP 与 WebSocket 服务"
)]
fn serve(replies: Vec<Reply>) -> (String, mpsc::Receiver<Seen>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (seen, received) = mpsc::channel();
    std::thread::spawn(move || {
        for reply in replies {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_request(&mut stream);
            let key = header(&request.head, "sec-websocket-key");
            let _ = seen.send(request);
            match reply {
                Reply::Status(status, body) => {
                    let response = format!(
                        "HTTP/1.1 {status} X\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    stream.write_all(response.as_bytes()).unwrap();
                }
                Reply::Upgrade => {
                    let accept = rust_cli_net::websocket::accept_for_tests(&key.unwrap());
                    let response = format!(
                        "HTTP/1.1 101 Switching Protocols\r\nupgrade: websocket\r\nconnection: Upgrade\r\nsec-websocket-accept: {accept}\r\n\r\n"
                    );
                    stream.write_all(response.as_bytes()).unwrap();
                    stream.write_all(&[0x82, 5]).unwrap();
                    stream.write_all(b"ready").unwrap();
                    let mut rest = Vec::new();
                    let _ = stream.read_to_end(&mut rest);
                }
            }
        }
    });
    (format!("127.0.0.1:{port}"), received)
}

fn read_request(stream: &mut TcpStream) -> Seen {
    let mut head = Vec::new();
    let mut byte = [0_u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).unwrap();
        head.push(byte[0]);
    }
    let head = String::from_utf8(head).unwrap();
    let length = header(&head, "content-length").map_or(0, |value| value.parse().unwrap());
    let mut body = vec![0_u8; length];
    stream.read_exact(&mut body).unwrap();
    Seen {
        head,
        body: String::from_utf8(body).unwrap(),
    }
}

fn header(head: &str, name: &str) -> Option<String> {
    head.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.eq_ignore_ascii_case(name)
            .then(|| value.trim().to_owned())
    })
}

async fn connector(host: &str, up: bool) -> Relay {
    let egress = Arc::new(Egress::new(NetworkPolicy::default()).await.unwrap());
    Relay::new(
        egress,
        RelayTarget {
            url: format!("ws://{host}/api/exec/v1/chats/chat-x/connect"),
            token: RedactedString::new("the-ticket"),
            user: "someone".to_owned(),
            up_url: up.then(|| format!("http://{host}/api/workspaces/chat-x/up")),
        },
    )
}

/// 拨号带上票、账号与子协议；连上之后就是一条能收帧的 Link。
#[tokio::test]
async fn a_dial_carries_the_ticket_the_user_and_the_subprotocol() {
    let (host, seen) = serve(vec![Reply::Upgrade]);
    let link = connector(&host, false).await.connect().await.unwrap();
    let (_sink, mut source) = link.split();
    assert_eq!(source.recv().await.unwrap().as_deref(), Some(&b"ready"[..]));
    let head = seen.recv().unwrap().head;
    assert!(
        head.starts_with("GET /api/exec/v1/chats/chat-x/connect "),
        "{head}"
    );
    assert_eq!(
        header(&head, "authorization").as_deref(),
        Some("Bearer the-ticket")
    );
    assert_eq!(header(&head, "x-user-id").as_deref(), Some("someone"));
    assert_eq!(
        header(&head, "sec-websocket-protocol").as_deref(),
        Some(SUBPROTOCOL)
    );
}

/// 再连也一样的状态码是 `Rejected`（exec-client 不再重连），说明里带着中转的原因；不会去拉起沙盒。
#[tokio::test]
async fn statuses_that_will_not_change_stop_reconnecting() {
    for status in TERMINAL {
        let (host, seen) = serve(vec![Reply::Status(status, "chat_not_found")]);
        let error = connector(&host, true).await.connect().await.err().unwrap();
        assert!(
            matches!(&error, LinkError::Rejected(text) if text.contains(&format!("{status}: chat_not_found"))),
            "{status}: {error:?}"
        );
        assert_eq!(seen.iter().count(), 1, "{status}");
    }
}

/// 其余的拒绝是 `Connect`：exec-client 按自己的退避重连。
#[tokio::test]
async fn other_refusals_are_retried_by_the_client() {
    for status in [http::INTERNAL_SERVER_ERROR, 502, 503] {
        let (host, _seen) = serve(vec![Reply::Status(status, "chat_initializing")]);
        let error = connector(&host, true).await.connect().await.err().unwrap();
        assert!(
            matches!(&error, LinkError::Connect(text) if text.contains("chat_initializing")),
            "{status}: {error:?}"
        );
    }
}

/// 沙盒停了（409），没配 `up_url`：不是终态，交给 exec-client 重连；不发 `/up`。
#[tokio::test]
async fn a_stopped_sandbox_without_an_up_url_is_retried() {
    let (host, seen) = serve(vec![Reply::Status(http::CONFLICT, "needs_up")]);
    let error = connector(&host, false).await.connect().await.err().unwrap();
    assert!(matches!(error, LinkError::Connect(_)), "{error:?}");
    assert_eq!(seen.iter().count(), 1);
}

/// 沙盒停了（409）：`POST /up` 拉起，再拨一次就连上。`/up` 带账号，请求体照契约原文。
#[tokio::test]
async fn a_stopped_sandbox_is_brought_up_and_dialed_again() {
    let (host, seen) = serve(vec![
        Reply::Status(http::CONFLICT, "needs_up"),
        Reply::Status(http::OK, "{}"),
        Reply::Upgrade,
    ]);
    let link = connector(&host, true).await.connect().await.unwrap();
    let (_sink, mut source) = link.split();
    assert_eq!(source.recv().await.unwrap().as_deref(), Some(&b"ready"[..]));
    let _first = seen.recv().unwrap();
    let up = seen.recv().unwrap();
    assert!(
        up.head.starts_with("POST /api/workspaces/chat-x/up "),
        "{}",
        up.head
    );
    assert_eq!(header(&up.head, "x-user-id").as_deref(), Some("someone"));
    assert_eq!(
        header(&up.head, "content-type").as_deref(),
        Some(mime::JSON)
    );
    let body: serde_json::Value = serde_json::from_str(&up.body).unwrap();
    assert_eq!(
        body,
        serde_json::json!({ "user_id": "someone", "flags": ["general_agent"] })
    );
}

/// 拉起之后还是 409：照常交出去，不再拉第二次。
#[tokio::test]
async fn the_sandbox_is_brought_up_once_per_connect() {
    let (host, seen) = serve(vec![
        Reply::Status(http::CONFLICT, "needs_up"),
        Reply::Status(http::OK, "{}"),
        Reply::Status(http::CONFLICT, "needs_up"),
    ]);
    let error = connector(&host, true).await.connect().await.err().unwrap();
    assert!(matches!(error, LinkError::Connect(_)), "{error:?}");
    assert_eq!(seen.iter().count(), 3, "dial, up, dial");
}

/// `/up` 自己失败：报出来，带中转的原因，不再拨号。
#[tokio::test]
async fn a_failing_up_call_is_reported() {
    let (host, seen) = serve(vec![
        Reply::Status(http::CONFLICT, "needs_up"),
        Reply::Status(http::INTERNAL_SERVER_ERROR, "boom"),
    ]);
    let error = connector(&host, true).await.connect().await.err().unwrap();
    assert!(
        matches!(&error, LinkError::Connect(text) if text.contains("bringing the sandbox up answered 500: boom")),
        "{error:?}"
    );
    assert_eq!(seen.iter().count(), 2);
}
