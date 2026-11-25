//! WebSocket 服务端：与本 crate 的客户端握手、文本双向收发、ping 与关闭、按请求头拒绝、不是升级请求。

use std::io::{Read, Write};

use super::*;
use crate::websocket::{MAX_PAYLOAD, WsFrame};
use crate::{Egress, HeaderValue, NetworkPolicy};
use tokio_tungstenite::tungstenite::handshake::headers::MAX_HEADERS;

/// 客户端的一帧（FIN、加掩码，RFC 6455 §5.2）：测试里的原始 TCP 客户端用。
fn client_frame(opcode: u8, payload: &[u8], mask: [u8; 4]) -> Vec<u8> {
    let mut frame = vec![0x80 | opcode, 0x80 | u8::try_from(payload.len()).unwrap()];
    frame.extend_from_slice(&mask);
    frame.extend(
        payload
            .iter()
            .enumerate()
            .map(|(index, byte)| byte ^ mask[index % 4]),
    );
    frame
}

async fn egress() -> Egress {
    Egress::new(NetworkPolicy::default()).await.unwrap()
}

/// 在本机端口上监听，交出地址。
async fn listen() -> (WsListener, String) {
    let listener = WsListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    (listener, format!("ws://{address}/turns"))
}

fn bearer(token: &str) -> Vec<(String, HeaderValue)> {
    vec![(
        "authorization".to_owned(),
        HeaderValue::Secret(rust_cli_base::secret::RedactedString::new(format!(
            "Bearer {token}"
        ))),
    )]
}

#[tokio::test]
async fn an_admitted_client_exchanges_text_both_ways() {
    let (listener, url) = listen().await;
    let server = tokio::spawn(async move {
        let incoming = listener.accept().await.unwrap();
        let (mut socket, upgrade) = incoming
            .upgrade(MAX_PAYLOAD, |request| {
                match request.header("Authorization") {
                    Some("Bearer secret") => Ok(()),
                    _ => Err(http::UNAUTHORIZED),
                }
            })
            .await
            .unwrap();
        assert_eq!(upgrade.path, "/turns");
        assert_eq!(
            socket.next().await.unwrap(),
            WsFrame::Text("{\"type\":\"hello\"}".to_owned())
        );
        let (_, mut writer) = socket.split();
        writer.send_text("{\"type\":\"welcome\"}").await.unwrap();
        writer.close(crate::websocket::CLOSE_NORMAL).await;
    });
    let socket = egress()
        .await
        .websocket(&url, (&[], &bearer("secret")), MAX_PAYLOAD)
        .await
        .unwrap();
    let (mut reader, mut writer) = socket.split();
    writer.send_text("{\"type\":\"hello\"}").await.unwrap();
    assert_eq!(
        reader.next().await.unwrap(),
        WsFrame::Text("{\"type\":\"welcome\"}".to_owned())
    );
    let WsFrame::Close { code, .. } = reader.next().await.unwrap() else {
        panic!("expected a close");
    };
    assert_eq!(code, crate::websocket::CLOSE_NORMAL);
    server.await.unwrap();
}

/// 服务端发的帧不加掩码；收到 ping 时 `next` 回 pong（同客户端）。
#[tokio::test]
#[expect(
    clippy::disallowed_methods,
    reason = "测试里用阻塞的 TCP 客户端看原始帧"
)]
async fn server_frames_are_unmasked_and_pings_are_answered() {
    let (listener, url) = listen().await;
    let address = url
        .trim_start_matches("ws://")
        .trim_end_matches("/turns")
        .to_owned();
    let server = tokio::spawn(async move {
        let incoming = listener.accept().await.unwrap();
        let (mut socket, _) = incoming.upgrade(MAX_PAYLOAD, |_| Ok(())).await.unwrap();
        // 客户端先发 ping，再发一条文本：`next` 回了 pong，交出文本。
        assert_eq!(socket.next().await.unwrap(), WsFrame::Text("t".to_owned()));
        let (_, mut writer) = socket.split();
        writer.send_text("ok").await.unwrap();
    });
    let raw = tokio::task::spawn_blocking(move || {
        let mut stream = std::net::TcpStream::connect(address).unwrap();
        stream
            .write_all(
                b"GET /turns HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n",
            )
            .unwrap();
        let mut head = Vec::new();
        let mut byte = [0_u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).unwrap();
            head.push(byte[0]);
        }
        let head = String::from_utf8(head).unwrap();
        assert!(head.starts_with("HTTP/1.1 101"), "{head}");
        assert!(head.contains("Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo="), "{head}");
        stream
            .write_all(&client_frame(0x9, b"p", [1, 2, 3, 4]))
            .unwrap();
        stream
            .write_all(&client_frame(0x1, b"t", [5, 6, 7, 8]))
            .unwrap();
        let mut frames = Vec::new();
        for _ in 0..2 {
            let mut head = [0_u8; 2];
            stream.read_exact(&mut head).unwrap();
            assert_eq!(head[1] & 0x80, 0, "server frames carry no mask");
            let mut payload = vec![0; usize::from(head[1] & 0x7f)];
            stream.read_exact(&mut payload).unwrap();
            frames.push((head[0] & 0x0f, payload));
        }
        frames
    })
    .await
    .unwrap();
    assert_eq!(raw, [(0xa, b"p".to_vec()), (0x1, b"ok".to_vec())]);
    server.await.unwrap();
}

#[tokio::test]
async fn a_refused_client_sees_the_status() {
    let (listener, url) = listen().await;
    let server = tokio::spawn(async move {
        let incoming = listener.accept().await.unwrap();
        let refused = incoming
            .upgrade(MAX_PAYLOAD, |_| Err(http::UNAUTHORIZED))
            .await;
        assert!(matches!(
            refused,
            Err(WsError::Rejected { status: 401, .. })
        ));
    });
    let error = match egress()
        .await
        .websocket(&url, (&[], &bearer("wrong")), MAX_PAYLOAD)
        .await
    {
        Ok(_) => panic!("the upgrade should be refused"),
        Err(error) => error,
    };
    assert!(matches!(error, WsError::Rejected { status: 401, .. }));
    server.await.unwrap();
}

#[tokio::test]
#[expect(
    clippy::disallowed_methods,
    reason = "测试里用阻塞的 TCP 客户端发普通的 HTTP 请求"
)]
async fn a_plain_request_is_not_upgraded() {
    let (listener, url) = listen().await;
    let address = url
        .trim_start_matches("ws://")
        .trim_end_matches("/turns")
        .to_owned();
    let server = tokio::spawn(async move {
        let incoming = listener.accept().await.unwrap();
        assert!(incoming.upgrade(MAX_PAYLOAD, |_| Ok(())).await.is_err());
    });
    let response = tokio::task::spawn_blocking(move || {
        let mut stream = std::net::TcpStream::connect(address).unwrap();
        stream
            .write_all(b"GET /turns HTTP/1.1\r\nHost: x\r\n\r\n")
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    })
    .await
    .unwrap();
    assert!(response.starts_with("HTTP/1.1 400"), "{response}");
    server.await.unwrap();
}

/// 升级请求：请求行、`Host` 与缺省的四个升级头，再加 `extra` 里的头。
fn request(method: &str, version: &str, extra: &str) -> Vec<u8> {
    format!(
        "{method} /turns HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: {version}\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n{extra}\r\n"
    )
    .into_bytes()
}

/// 用原始的 TCP 客户端发 `bytes`，交出响应的状态码。服务端这一侧照常调 `upgrade`（都收）。
#[expect(
    clippy::disallowed_methods,
    reason = "测试里用阻塞的 TCP 客户端发原始请求"
)]
async fn status(bytes: Vec<u8>) -> u16 {
    let (listener, url) = listen().await;
    let address = url
        .trim_start_matches("ws://")
        .trim_end_matches("/turns")
        .to_owned();
    let server = tokio::spawn(async move {
        let incoming = listener.accept().await.unwrap();
        incoming.upgrade(MAX_PAYLOAD, |_| Ok(())).await.err()
    });
    let response = tokio::task::spawn_blocking(move || {
        let mut stream = std::net::TcpStream::connect(address).unwrap();
        stream.write_all(&bytes).unwrap();
        let mut head = Vec::new();
        let mut byte = [0_u8; 1];
        while !head.ends_with(b"\r\n") {
            stream.read_exact(&mut byte).unwrap();
            head.push(byte[0]);
        }
        String::from_utf8(head).unwrap()
    })
    .await
    .unwrap();
    server.abort();
    response.split(' ').nth(1).unwrap().parse().unwrap()
}

/// serve 回的状态码（multi-tenant.md §4.1）：不是 GET 405，版本不是 13 426，缺 key 400，请求头多于库的上限 431。
#[tokio::test]
async fn bad_upgrade_requests_get_their_status() {
    assert_eq!(
        status(request("POST", "13", "")).await,
        http::METHOD_NOT_ALLOWED
    );
    assert_eq!(
        status(request("GET", "8", "")).await,
        http::UPGRADE_REQUIRED
    );
    assert_eq!(
        status(b"GET /turns HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\n\r\n".to_vec()).await,
        http::BAD_REQUEST
    );
    let many: String = (0..MAX_HEADERS)
        .map(|index| format!("x-{index}: v\r\n"))
        .collect();
    assert_eq!(
        status(request("GET", "13", &many)).await,
        http::REQUEST_HEADER_FIELDS_TOO_LARGE
    );
}

/// 客户端把请求头与第一帧放在同一次写里：读请求头时多读的字节就是这一帧，升级之后照常读出。
#[tokio::test]
#[expect(
    clippy::disallowed_methods,
    reason = "测试里用阻塞的 TCP 客户端把请求头与第一帧一起写出"
)]
async fn the_first_frame_may_come_with_the_request() {
    let (listener, url) = listen().await;
    let address = url
        .trim_start_matches("ws://")
        .trim_end_matches("/turns")
        .to_owned();
    let server = tokio::spawn(async move {
        let incoming = listener.accept().await.unwrap();
        let (mut socket, _) = incoming.upgrade(MAX_PAYLOAD, |_| Ok(())).await.unwrap();
        socket.next().await.unwrap()
    });
    tokio::task::spawn_blocking(move || {
        let mut stream = std::net::TcpStream::connect(address).unwrap();
        let mut bytes = request("GET", "13", "");
        bytes.extend(client_frame(0x1, b"early", [9, 8, 7, 6]));
        stream.write_all(&bytes).unwrap();
        let mut head = Vec::new();
        let mut byte = [0_u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).unwrap();
            head.push(byte[0]);
        }
        assert!(head.starts_with(b"HTTP/1.1 101"));
    })
    .await
    .unwrap();
    assert_eq!(server.await.unwrap(), WsFrame::Text("early".to_owned()));
}

/// 只读请求头这一步（不经 TCP）：太大与头太多 431，不合 HTTP 400，只有 LF 的换行照收，读多了的字节原样交出。
#[tokio::test]
async fn reading_the_request_limits_its_size_and_keeps_the_early_bytes() {
    let read = |bytes: Vec<u8>| async move { read_request(&mut bytes.as_slice()).await };
    let mut long = request("GET", "13", "");
    long.splice(
        long.len() - 2..,
        format!("x-long: {}\r\n\r\n", "a".repeat(MAX_REQUEST_BYTES)).into_bytes(),
    );
    assert_eq!(
        read(long).await.err(),
        Some(http::REQUEST_HEADER_FIELDS_TOO_LARGE)
    );
    let endless = format!(
        "GET /turns HTTP/1.1\r\nx-long: {}",
        "a".repeat(MAX_REQUEST_BYTES)
    );
    assert_eq!(
        read(endless.into_bytes()).await.err(),
        Some(http::REQUEST_HEADER_FIELDS_TOO_LARGE)
    );
    assert_eq!(
        read(b"GET /turns HTTP/1.1\r\nHost".to_vec()).await.err(),
        Some(http::BAD_REQUEST)
    );
    assert_eq!(
        read(b"GET /turns HTTP/1.1\r\nno colon\r\n\r\n".to_vec())
            .await
            .err(),
        Some(http::BAD_REQUEST)
    );
    // 语法跟着库（RFC 9112）：请求行要有版本，名字与冒号之间不能有空白；请求行之前的空行跳过。
    for bad in [
        &b"GET /turns\r\n\r\n"[..],
        b"GET /turns HTTP/1.1\r\nHost : x\r\n\r\n",
    ] {
        assert_eq!(read(bad.to_vec()).await.err(), Some(http::BAD_REQUEST));
    }
    assert!(
        read(b"\r\nGET /turns HTTP/1.1\r\n\r\n".to_vec())
            .await
            .is_ok()
    );
    let (upgrade, early) =
        read(b"GET /turns?x=1 HTTP/1.1\nUpgrade:  websocket \n\n\x81\x00".to_vec())
            .await
            .unwrap();
    assert_eq!(upgrade.path, "/turns?x=1");
    assert_eq!(upgrade.header("UPGRADE"), Some("websocket"));
    assert_eq!(early, b"\x81\x00");
}
