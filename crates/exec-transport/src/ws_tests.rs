//! WebSocket 上的连接：一条消息一帧、读的时候来了 ping 回 pong（net 用的库回）、关闭码进错误、文本消息报错、`close` 发 close 帧。
//!
//! 服务端是本进程的一个线程，只听 127.0.0.1。

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

use rust_cli_net::NetworkPolicy;

use super::*;

const PROTOCOL: &str = "zcode-exec.v1";

/// 读请求头、回 101，再照剧本收发帧。请求头经通道交回。
#[expect(clippy::disallowed_methods, reason = "测试的本地 WebSocket 服务")]
fn serve(
    script: impl FnOnce(&mut TcpStream) + Send + 'static,
) -> (String, std::sync::mpsc::Receiver<String>) {
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
        let head = String::from_utf8(head).unwrap();
        let key = head
            .lines()
            .find_map(|line| line.strip_prefix("sec-websocket-key: "))
            .unwrap()
            .to_owned();
        heads.send(head).unwrap();
        let response = format!(
            "HTTP/1.1 101 Switching Protocols\r\nupgrade: websocket\r\nconnection: Upgrade\r\nsec-websocket-accept: {}\r\n\r\n",
            rust_cli_net::websocket::accept_for_tests(&key)
        );
        stream.write_all(response.as_bytes()).unwrap();
        script(&mut stream);
    });
    (format!("ws://127.0.0.1:{port}/connect"), received)
}

/// 服务端的帧（不加掩码，内容不超过 125 字节）。
fn server_frame(opcode: u8, payload: &[u8]) -> Vec<u8> {
    let mut frame = vec![0x80 | opcode, u8::try_from(payload.len()).unwrap()];
    frame.extend_from_slice(payload);
    frame
}

/// 读客户端的一帧（去掉掩码，内容不超过 125 字节）：（opcode、内容）。
fn client_read(stream: &mut TcpStream) -> (u8, Vec<u8>) {
    let mut head = [0_u8; 2];
    stream.read_exact(&mut head).unwrap();
    let mut mask = [0_u8; 4];
    stream.read_exact(&mut mask).unwrap();
    let mut payload = vec![0_u8; usize::from(head[1] & 0x7f)];
    stream.read_exact(&mut payload).unwrap();
    for (index, byte) in payload.iter_mut().enumerate() {
        *byte ^= mask[index % 4];
    }
    (head[0] & 0x0f, payload)
}

async fn link(url: &str, headers: &[(String, HeaderValue)]) -> Box<dyn Link> {
    let egress = Egress::new(NetworkPolicy::default()).await.unwrap();
    dial(&egress, url, &[PROTOCOL.to_owned()], headers)
        .await
        .unwrap()
}

/// 一帧就是一条二进制消息，前面没有 u32 长度；请求头与子协议都发出去了。
#[tokio::test]
async fn a_frame_is_one_binary_message_each_way() {
    let (url, heads) = serve(|stream| {
        assert_eq!(client_read(stream), (0x2, b"up-frame".to_vec()));
        stream.write_all(&server_frame(0x2, b"down-frame")).unwrap();
    });
    let headers = [("x-user-id".to_owned(), HeaderValue::from("someone"))];
    let (mut sink, mut source) = link(&url, &headers).await.split();
    sink.send(Bytes::from_static(b"up-frame")).await.unwrap();
    assert_eq!(
        source.recv().await.unwrap(),
        Some(Bytes::from_static(b"down-frame"))
    );
    let head = heads.recv().unwrap();
    assert!(
        head.contains(&format!("sec-websocket-protocol: {PROTOCOL}")),
        "{head}"
    );
    assert!(head.contains("x-user-id: someone"), "{head}");
}

/// 读的时候来了 ping：库在读的时候回 pong，读的一侧接着交出后面的帧。
#[tokio::test]
async fn a_ping_while_reading_is_answered() {
    let (url, _heads) = serve(|stream| {
        stream.write_all(&server_frame(0x9, b"hb")).unwrap();
        assert_eq!(client_read(stream), (0xa, b"hb".to_vec()));
        stream.write_all(&server_frame(0x2, b"after")).unwrap();
    });
    let (_sink, mut source) = link(&url, &[]).await.split();
    assert_eq!(
        source.recv().await.unwrap(),
        Some(Bytes::from_static(b"after"))
    );
}

/// 对方关闭：回一个同样代码与原因的 close 帧（库的做法；以前自写的编解码只回代码）；4001 这样的代码进错误的文字，1000 是
/// 正常关闭（`Ok(None)`）。
#[tokio::test]
async fn a_close_is_answered_and_its_code_reported() {
    for (code, expected) in [
        (
            4001_u16,
            Err(LinkError::Io(
                "websocket closed with code 4001: idle".to_owned(),
            )),
        ),
        (1000, Ok(None)),
    ] {
        let (url, _heads) = serve(move |stream| {
            let mut close = code.to_be_bytes().to_vec();
            if code != 1000 {
                close.extend_from_slice(b"idle");
            }
            stream.write_all(&server_frame(0x8, &close)).unwrap();
            assert_eq!(client_read(stream), (0x8, close));
        });
        let (_sink, mut source) = link(&url, &[]).await.split();
        assert_eq!(source.recv().await, expected);
    }
}

/// 执行连接上只有二进制消息：文本消息是错误。
#[tokio::test]
async fn a_text_message_is_an_error() {
    let (url, _heads) = serve(|stream| {
        stream.write_all(&server_frame(0x1, b"hello")).unwrap();
    });
    let (_sink, mut source) = link(&url, &[]).await.split();
    let error = source.recv().await.unwrap_err();
    assert!(
        matches!(&error, LinkError::Io(text) if text.contains("text message")),
        "{error:?}"
    );
}

/// `close` 发一个正常关闭的 close 帧，等它发完才返回；之后再发是 `Closed`。
#[tokio::test]
async fn closing_the_sink_sends_a_close_frame() {
    let (url, _heads) = serve(|stream| {
        assert_eq!(client_read(stream), (0x8, 1000_u16.to_be_bytes().to_vec()));
    });
    let (mut sink, _source) = link(&url, &[]).await.split();
    sink.close().await.unwrap();
    assert_eq!(
        sink.send(Bytes::from_static(b"late")).await,
        Err(LinkError::Closed)
    );
}

/// 修复（F142，评审 P2，remote-exec.md §29.5）：写的一侧堵在 TCP 上时（对方不读），读的一侧照样交出后面的帧。以前回
/// pong 要排进与数据帧共用、容量 1 的队列：写的一侧堵住时队列是满的，读到 ping 就停住，要等心跳判死才重连。
#[tokio::test]
async fn reading_goes_on_while_writing_is_stuck() {
    let (go, wait_go) = std::sync::mpsc::channel::<()>();
    let (done, wait_done) = std::sync::mpsc::channel::<()>();
    let (url, _heads) = serve(move |stream| {
        // 不读客户端发来的东西：客户端的写会堵在 TCP 上。
        wait_go.recv().unwrap();
        stream.write_all(&server_frame(0x9, b"hb")).unwrap();
        stream.write_all(&server_frame(0x2, b"after")).unwrap();
        let _ = wait_done.recv();
    });
    let (mut sink, mut source) = link(&url, &[]).await.split();
    let flooding = rust_cli_base::owned(async move {
        let chunk = Bytes::from(vec![0_u8; 1 << 20]);
        while sink.send(chunk.clone()).await.is_ok() {}
    });
    // 本机回环的缓冲几毫秒就满了；之后写的一侧发不动，发送队列也是满的。
    tokio::time::sleep(Duration::from_millis(300)).await;
    go.send(()).unwrap();
    let got = tokio::time::timeout(Duration::from_secs(5), source.recv()).await;
    let _ = done.send(());
    assert_eq!(
        got.expect("reading does not wait for writing").unwrap(),
        Some(Bytes::from_static(b"after"))
    );
    drop(flooding);
}
