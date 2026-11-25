//! WebSocket 客户端：握手、文本与二进制帧、分片、ping、关闭、太大的帧、升级被拒、拆成两半（本地的 WebSocket 服务）。
//! 帧由库读写；这里的服务端照 RFC 6455 手工写帧、读帧，看线上的字节。

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};

use super::*;
use crate::{Egress, NetworkPolicy};

/// 读请求头，回 101（`accept` 为 false 时回 403），然后照剧本写帧、读客户端发来的帧。
#[expect(clippy::disallowed_methods, reason = "测试的本地 WebSocket 服务")]
fn serve(script: impl FnOnce(&mut TcpStream) + Send + 'static, accept: bool) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut head = Vec::new();
        let mut byte = [0_u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).unwrap();
            head.push(byte[0]);
        }
        let head = String::from_utf8(head).unwrap();
        let key = head
            .lines()
            .find_map(|line| line.strip_prefix("sec-websocket-key: "))
            .unwrap()
            .to_owned();
        assert!(head.contains("sec-websocket-protocol: v1, v2"), "{head}");
        if !accept {
            stream
                .write_all(b"HTTP/1.1 403 Forbidden\r\ncontent-length: 0\r\n\r\n")
                .unwrap();
            return;
        }
        let response = format!(
            "HTTP/1.1 101 Switching Protocols\r\nupgrade: websocket\r\nconnection: Upgrade\r\nsec-websocket-accept: {}\r\n\r\n",
            accept_for_tests(&key)
        );
        stream.write_all(response.as_bytes()).unwrap();
        script(&mut stream);
    });
    format!("ws://127.0.0.1:{port}/feed")
}

/// 服务端的帧（不加掩码）。
fn server_frame(fin: bool, opcode: u8, payload: &[u8]) -> Vec<u8> {
    let mut frame = vec![if fin { 0x80 } else { 0 } | opcode];
    if payload.len() < 126 {
        frame.push(u8::try_from(payload.len()).unwrap());
    } else {
        frame.push(127);
        frame.extend_from_slice(&(payload.len() as u64).to_be_bytes());
    }
    frame.extend_from_slice(payload);
    frame
}

/// 读客户端的一帧（去掉掩码）：（opcode、内容）。
fn client_read(stream: &mut TcpStream) -> (u8, Vec<u8>) {
    let mut head = [0_u8; 2];
    stream.read_exact(&mut head).unwrap();
    assert_eq!(head[1] & 0x80, 0x80, "client frames are masked");
    let length = usize::from(head[1] & 0x7f);
    let mut mask = [0_u8; 4];
    stream.read_exact(&mut mask).unwrap();
    let mut payload = vec![0_u8; length];
    stream.read_exact(&mut payload).unwrap();
    for (index, byte) in payload.iter_mut().enumerate() {
        *byte ^= mask[index % 4];
    }
    (head[0] & 0x0f, payload)
}

async fn egress() -> Egress {
    Egress::new(NetworkPolicy::default()).await.unwrap()
}

fn protocols() -> Vec<String> {
    vec!["v1".to_owned(), "v2".to_owned()]
}

#[test]
fn the_accept_key_matches_the_rfc_example() {
    // RFC 6455 §1.3 的例子。
    assert_eq!(
        accept_for_tests("dGhlIHNhbXBsZSBub25jZQ=="),
        "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
    );
}

#[tokio::test]
async fn frames_are_read_and_pings_answered() {
    let url = serve(
        |stream| {
            stream.write_all(&server_frame(true, 0x1, b"one")).unwrap();
            stream.write_all(&server_frame(false, 0x1, b"tw")).unwrap();
            stream.write_all(&server_frame(true, 0x9, b"hb")).unwrap();
            stream.write_all(&server_frame(true, 0x0, b"o")).unwrap();
            assert_eq!(client_read(stream), (0xa, b"hb".to_vec()));
            stream
                .write_all(&server_frame(true, 0x2, &[0, 1, 2]))
                .unwrap();
            let mut close = 1001_u16.to_be_bytes().to_vec();
            close.extend_from_slice(b"going away");
            stream.write_all(&server_frame(true, 0x8, &close)).unwrap();
            // 回应带上对方的代码与原因（库的做法；以前自写的编解码只回代码，README §3.2.1）。
            assert_eq!(client_read(stream), (0x8, close));
        },
        true,
    );
    let mut socket = egress()
        .await
        .websocket(&url, (&protocols(), &[]), MAX_PAYLOAD)
        .await
        .unwrap();
    assert_eq!(
        socket.next().await.unwrap(),
        WsFrame::Text("one".to_owned())
    );
    assert_eq!(
        socket.next().await.unwrap(),
        WsFrame::Text("two".to_owned())
    );
    assert_eq!(
        socket.next().await.unwrap(),
        WsFrame::Binary(Bytes::from_static(&[0, 1, 2]))
    );
    assert_eq!(
        socket.next().await.unwrap(),
        WsFrame::Close {
            code: 1001,
            reason: "going away".to_owned()
        }
    );
}

#[tokio::test]
async fn a_dropped_connection_closes_with_1006_and_big_frames_fail() {
    let url = serve(
        |stream| stream.shutdown(std::net::Shutdown::Both).unwrap(),
        true,
    );
    let mut socket = egress()
        .await
        .websocket(&url, (&protocols(), &[]), MAX_PAYLOAD)
        .await
        .unwrap();
    assert_eq!(
        socket.next().await.unwrap(),
        WsFrame::Close {
            code: 1006,
            reason: String::new()
        }
    );
    let url = serve(
        |stream| {
            let big = vec![b'x'; usize::try_from(MAX_PAYLOAD).unwrap() + 1];
            let _ = stream.write_all(&server_frame(true, 0x1, &big));
        },
        true,
    );
    let mut socket = egress()
        .await
        .websocket(&url, (&protocols(), &[]), MAX_PAYLOAD)
        .await
        .unwrap();
    assert_eq!(
        socket.next().await.unwrap_err(),
        WsError::TooBig(MAX_PAYLOAD + 1)
    );
}

#[tokio::test]
async fn a_rejected_upgrade_reports_the_status() {
    let url = serve(|_| {}, false);
    let error = egress()
        .await
        .websocket(&url, (&protocols(), &[]), MAX_PAYLOAD)
        .await
        .err()
        .unwrap();
    assert_eq!(
        error,
        WsError::Rejected {
            status: 403,
            body: String::new()
        }
    );
    let error = egress()
        .await
        .websocket("ftp://e.example.com/", (&[], &[]), MAX_PAYLOAD)
        .await
        .err()
        .unwrap();
    assert!(matches!(error, WsError::Protocol(_)));
}

/// 读到请求头就回一个固定的响应，请求头经通道交回（看这一次连接自己的请求头带上没有）。
#[expect(clippy::disallowed_methods, reason = "测试的本地 HTTP 服务")]
fn serve_once(response: &'static str) -> (String, std::sync::mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (heads, received) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut head = Vec::new();
        let mut byte = [0_u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).unwrap();
            head.push(byte[0]);
        }
        heads.send(String::from_utf8(head).unwrap()).unwrap();
        stream.write_all(response.as_bytes()).unwrap();
    });
    (format!("ws://127.0.0.1:{port}/connect"), received)
}

/// 被拒时带回响应体（服务端把原因写在这里）；这一次连接的请求头都发出去了，秘密值也照发。
#[tokio::test]
async fn a_rejected_upgrade_keeps_the_body_and_extra_headers_are_sent() {
    let (url, heads) =
        serve_once("HTTP/1.1 404 Not Found\r\ncontent-length: 14\r\n\r\nchat_not_found");
    let headers = [
        ("x-user-id".to_owned(), HeaderValue::from("someone")),
        (
            "authorization".to_owned(),
            HeaderValue::Secret(rust_cli_base::secret::RedactedString::new("Bearer t")),
        ),
    ];
    let error = egress()
        .await
        .websocket(&url, (&protocols(), &headers), MAX_PAYLOAD)
        .await
        .err()
        .unwrap();
    assert_eq!(
        error,
        WsError::Rejected {
            status: 404,
            body: "chat_not_found".to_owned()
        }
    );
    let head = heads.recv().unwrap();
    assert!(head.contains("x-user-id: someone"), "{head}");
    assert!(head.contains("authorization: Bearer t"), "{head}");
}

/// 上限按连接给：同一个 9 字节的帧，上限 8 时报 `TooBig`。
#[tokio::test]
async fn the_limit_is_per_connection() {
    let url = serve(
        |stream| {
            let _ = stream.write_all(&server_frame(true, 0x2, b"123456789"));
        },
        true,
    );
    let mut socket = egress()
        .await
        .websocket(&url, (&protocols(), &[]), 8)
        .await
        .unwrap();
    assert_eq!(socket.next().await.unwrap_err(), WsError::TooBig(9));
}

/// 拆开后：读的一侧收到 ping 时库回 pong（夹在两片之间也不丢已收到的那一片）；写的一侧发数据、回 close。
#[tokio::test]
async fn a_split_socket_answers_pings_while_reading() {
    let url = serve(
        |stream| {
            stream.write_all(&server_frame(false, 0x2, b"tw")).unwrap();
            stream.write_all(&server_frame(true, 0x9, b"hb")).unwrap();
            assert_eq!(client_read(stream), (0xa, b"hb".to_vec()));
            stream.write_all(&server_frame(true, 0x0, b"o")).unwrap();
            assert_eq!(client_read(stream), (0x2, b"up".to_vec()));
            stream
                .write_all(&server_frame(true, 0x8, &4001_u16.to_be_bytes()))
                .unwrap();
            assert_eq!(client_read(stream), (0x8, 4001_u16.to_be_bytes().to_vec()));
        },
        true,
    );
    let socket = egress()
        .await
        .websocket(&url, (&protocols(), &[]), MAX_PAYLOAD)
        .await
        .unwrap();
    let (mut reader, mut writer) = socket.split();
    assert_eq!(
        reader.next().await.unwrap(),
        WsFrame::Binary(Bytes::from_static(b"two"))
    );
    writer.send_binary(b"up").await.unwrap();
    let WsFrame::Close { code, .. } = reader.next().await.unwrap() else {
        panic!("expected a close");
    };
    assert_eq!(code, 4001);
    writer.close(code).await;
}

/// 读是 cancel-safe 的：一帧分两次到达，读在 `select!` 里被另一个分支反复打断，也读出完整的这一帧，后面的帧不错位。
/// 以前自写的读帧用 `read_exact`，被打断时丢掉读到一半的字节，之后整条流错位（只能把读放进独占的任务）。
#[tokio::test]
async fn reading_survives_being_cancelled_mid_frame() {
    let url = serve(
        |stream| {
            let frame = server_frame(true, 0x1, b"hello world");
            stream.write_all(&frame[..5]).unwrap();
            stream.flush().unwrap();
            std::thread::sleep(std::time::Duration::from_millis(100));
            stream.write_all(&frame[5..]).unwrap();
            stream.write_all(&server_frame(true, 0x1, b"next")).unwrap();
            let mut rest = Vec::new();
            let _ = stream.read_to_end(&mut rest);
        },
        true,
    );
    let mut socket = egress()
        .await
        .websocket(&url, (&protocols(), &[]), MAX_PAYLOAD)
        .await
        .unwrap();
    let mut ticks = tokio::time::interval(std::time::Duration::from_millis(5));
    let mut frames = Vec::new();
    let mut interrupted = 0;
    // 读错位时会一直等字节（以前的实现就是这样卡住）：给整个读一个上限，让测试失败而不是挂住。
    let reading = async {
        while frames.len() < 2 {
            tokio::select! {
                frame = socket.next() => frames.push(frame.unwrap()),
                _ = ticks.tick() => interrupted += 1,
            }
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(5), reading)
        .await
        .expect("an interrupted read lost part of a frame");
    assert!(interrupted > 1, "the read was not interrupted");
    assert_eq!(
        frames,
        [
            WsFrame::Text("hello world".to_owned()),
            WsFrame::Text("next".to_owned())
        ]
    );
}
