use std::time::Duration;

use rust_cli_base::TaskScope;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader, DuplexStream, duplex};
use tokio::sync::mpsc;

use super::*;
use crate::framing::{Dialect, Framing};

/// 一帧：`Content-Length` 头 + 正文。
fn frame(body: &str) -> Vec<u8> {
    format!("Content-Length: {}\r\n\r\n{body}", body.len()).into_bytes()
}

/// 读一帧（测试端的解析：只认 Content-Length）。
async fn read_frame(from: &mut DuplexStream) -> Value {
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        let mut byte = [0u8; 1];
        tokio::time::timeout(Duration::from_secs(5), from.read_exact(&mut byte))
            .await
            .unwrap()
            .unwrap();
        head.push(byte[0]);
    }
    let head = String::from_utf8(head).unwrap();
    let length: usize = head
        .lines()
        .find_map(|line| line.strip_prefix("Content-Length: "))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let mut body = vec![0u8; length];
    from.read_exact(&mut body).await.unwrap();
    serde_json::from_slice(&body).unwrap()
}

struct Harness {
    to_server: DuplexStream,
    from_server: DuplexStream,
    intake: mpsc::Receiver<Inbound>,
    peer: Peer,
    done: tokio::task::JoinHandle<std::io::Result<()>>,
    _scope: TaskScope,
}

fn v5_options() -> Options {
    Options {
        framing: Framing::ContentLength,
        dialect: Dialect::JsonRpc2,
        max_message_bytes: 256,
        ..Options::default()
    }
}

impl Harness {
    fn new(options: Options) -> Self {
        let scope = TaskScope::root("test", |_| {});
        let (to_server, server_in) = duplex(1 << 16);
        let (server_out, from_server) = duplex(1 << 16);
        let (peer, _writer) = start(server_out, &scope.spawner(), options);
        let (sender, intake) = mpsc::channel(16);
        let done = tokio::spawn(serve_in_order(
            BufReader::new(server_in),
            peer.clone(),
            sender,
            options,
        ));
        Self {
            to_server,
            from_server,
            intake,
            peer,
            done,
            _scope: scope,
        }
    }

    async fn send(&mut self, bytes: &[u8]) {
        self.to_server.write_all(bytes).await.unwrap();
    }

    async fn inbound(&mut self) -> Inbound {
        tokio::time::timeout(Duration::from_secs(5), self.intake.recv())
            .await
            .unwrap()
            .unwrap()
    }
}

#[tokio::test]
async fn content_length_frames_are_read_in_arrival_order() {
    let mut harness = Harness::new(v5_options());
    // 两帧粘在一起写，第三帧分两段写；头名不分大小写，可以有别的头
    let mut bytes = frame(r#"{"jsonrpc":"2.0","id":1,"method":"hello","params":{"a":1}}"#);
    bytes.extend(frame(
        r#"{"jsonrpc":"2.0","method":"credit","params":{"sub":"s"}}"#,
    ));
    harness.send(&bytes).await;
    let body = r#"{"jsonrpc":"2.0","id":"x","method":"ping"}"#;
    let third = format!(
        "content-length: {}\r\nContent-Type: application/vscode-jsonrpc; charset=utf-8\r\n\r\n{body}",
        body.len()
    );
    let (a, b) = third.as_bytes().split_at(10);
    harness.send(a).await;
    harness.send(b).await;
    assert_eq!(
        harness.inbound().await,
        Inbound::Request {
            id: json!(1),
            method: "hello".to_owned(),
            params: json!({"a": 1})
        }
    );
    assert_eq!(
        harness.inbound().await,
        Inbound::Notification {
            method: "credit".to_owned(),
            params: json!({"sub": "s"})
        }
    );
    assert_eq!(
        harness.inbound().await,
        Inbound::Request {
            id: json!("x"),
            method: "ping".to_owned(),
            params: Value::Null
        }
    );
}

#[tokio::test]
async fn the_json_rpc_2_dialect_writes_the_version_and_content_length() {
    let mut harness = Harness::new(v5_options());
    harness.peer.notify("frame", &json!({"sub": "s"})).await;
    assert_eq!(
        read_frame(&mut harness.from_server).await,
        json!({"jsonrpc": "2.0", "method": "frame", "params": {"sub": "s"}})
    );
    harness
        .peer
        .respond(&json!(7), &Ok(json!({"ok": true})))
        .await;
    assert_eq!(
        read_frame(&mut harness.from_server).await,
        json!({"jsonrpc": "2.0", "id": 7, "result": {"ok": true}})
    );
}

/// 读失败即链路断开（01 §3.2，16 §10 审定第 7 条）：正文不是 JSON 说明对端写来的内容这一端读不了，
/// 按序投递已不成立，与坏头同样结束读循环。以前回 id 为 null 的 -32700 再接着读：正文坏了的那条若是回复，在途请求永远
/// 等不到；若是通知（credit），状态静默少一份。
#[tokio::test]
async fn a_body_that_is_not_json_ends_the_read_loop() {
    let mut harness = Harness::new(v5_options());
    harness.send(&frame("not json")).await;
    let result = tokio::time::timeout(Duration::from_secs(5), harness.done)
        .await
        .unwrap()
        .unwrap();
    assert!(result.is_err());
    assert!(harness.peer.is_closed());
}

/// 是 JSON、但不是请求 / 通知 / 回复：内容解不出是会话层的事（01 §3.2），认不出 id 时回 id 为 null 的 -32600，接着读。
#[tokio::test]
async fn json_that_is_not_a_message_gets_an_invalid_request_and_reading_goes_on() {
    let mut harness = Harness::new(v5_options());
    harness.send(&frame(r#"{"jsonrpc":"2.0"}"#)).await;
    let reply = read_frame(&mut harness.from_server).await;
    assert_eq!(reply["jsonrpc"], json!("2.0"));
    assert_eq!(reply["id"], Value::Null);
    assert_eq!(reply["error"]["code"], json!(-32600));
    harness
        .send(&frame(r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#))
        .await;
    assert!(matches!(harness.inbound().await, Inbound::Request { .. }));
}

#[tokio::test]
async fn a_broken_header_ends_the_read_loop_with_an_error() {
    let mut harness = Harness::new(v5_options());
    harness.send(b"Content-Length: abc\r\n\r\n{}").await;
    let result = tokio::time::timeout(Duration::from_secs(5), harness.done)
        .await
        .unwrap()
        .unwrap();
    assert!(result.is_err());
    assert!(harness.peer.is_closed());
}

#[tokio::test]
async fn a_frame_over_the_limit_ends_the_read_loop_with_an_error() {
    let mut harness = Harness::new(v5_options());
    harness
        .send(format!("Content-Length: {}\r\n\r\n", 10_000).as_bytes())
        .await;
    let result = tokio::time::timeout(Duration::from_secs(5), harness.done)
        .await
        .unwrap()
        .unwrap();
    let error = result.unwrap_err();
    assert!(error.to_string().contains("10000"), "{error}");
}

#[tokio::test]
async fn a_header_without_content_length_ends_the_read_loop() {
    let mut harness = Harness::new(v5_options());
    harness.send(b"X-Other: 1\r\n\r\n{}").await;
    let result = tokio::time::timeout(Duration::from_secs(5), harness.done)
        .await
        .unwrap()
        .unwrap();
    assert!(result.is_err());
}

#[tokio::test]
async fn responses_resolve_outbound_requests_and_are_not_handed_over() {
    let mut harness = Harness::new(v5_options());
    let peer = harness.peer.clone();
    let waiting = tokio::spawn(async move { peer.request("host.ask", &json!({})).await });
    let asked = read_frame(&mut harness.from_server).await;
    assert_eq!(asked["jsonrpc"], json!("2.0"));
    assert_eq!(asked["method"], json!("host.ask"));
    let id = asked["id"].as_str().unwrap().to_owned();
    harness
        .send(&frame(&format!(
            r#"{{"jsonrpc":"2.0","id":"{id}","result":{{"answer":1}}}}"#
        )))
        .await;
    let answer = tokio::time::timeout(Duration::from_secs(5), waiting)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(answer.unwrap(), json!({"answer": 1}));
    harness
        .send(&frame(r#"{"jsonrpc":"2.0","method":"after"}"#))
        .await;
    assert_eq!(
        harness.inbound().await,
        Inbound::Notification {
            method: "after".to_owned(),
            params: Value::Null
        }
    );
}

#[tokio::test]
async fn reading_goes_on_after_the_owner_stops_taking_messages() {
    let mut harness = Harness::new(v5_options());
    harness.intake.close();
    harness
        .send(&frame(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#))
        .await;
    harness.to_server.shutdown().await.unwrap();
    let result = tokio::time::timeout(Duration::from_secs(5), harness.done)
        .await
        .unwrap()
        .unwrap();
    assert!(result.is_ok());
}

#[tokio::test]
async fn the_line_framing_with_the_bare_dialect_is_unchanged() {
    // v4 的写法：按 LF 分帧、没有 jsonrpc 字段；坏行认不出 id 时不回复
    let mut harness = Harness::new(Options::default());
    harness
        .send(b"garbage\n{\"id\":3,\"method\":\"echo\"}\n")
        .await;
    assert_eq!(
        harness.inbound().await,
        Inbound::Request {
            id: json!(3),
            method: "echo".to_owned(),
            params: Value::Null
        }
    );
    harness.peer.notify("n", &json!(1)).await;
    let mut line = String::new();
    let mut reader = BufReader::new(&mut harness.from_server);
    tokio::io::AsyncBufReadExt::read_line(&mut reader, &mut line)
        .await
        .unwrap();
    assert_eq!(line, "{\"method\":\"n\",\"params\":1}\n");
}

/// 写者在全部 Peer 句柄都丢掉之后写完队列并结束（以前写者自己拿着一个 Peer，永远等不到队列关闭，进程退出时要等到作用域的
/// 收尾上限才被中止）。
#[tokio::test]
async fn the_writer_drains_and_ends_once_every_peer_is_dropped() {
    let scope = TaskScope::root("test", |_| {});
    let (server_out, mut from_server) = duplex(1 << 16);
    let (peer, writer) = start(server_out, &scope.spawner(), v5_options());
    peer.notify("last", &json!({})).await;
    drop(peer);
    tokio::time::timeout(Duration::from_secs(5), writer)
        .await
        .expect("the writer did not end")
        .unwrap();
    assert_eq!(read_frame(&mut from_server).await["method"], json!("last"));
}
