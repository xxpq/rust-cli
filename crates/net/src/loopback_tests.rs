use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use super::*;

/// 发一段原始的请求，读到连接关掉。
#[expect(clippy::disallowed_methods, reason = "测试用原始 TCP 客户端发请求")]
async fn exchange(port: u16, raw: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    stream.write_all(raw.as_bytes()).await.unwrap();
    let mut reply = String::new();
    stream.read_to_string(&mut reply).await.unwrap();
    reply
}

#[tokio::test]
async fn a_callback_request_is_handed_over_with_its_query_and_answered_in_plain_text() {
    let listener = LoopbackListener::bind().await.unwrap();
    assert_ne!(listener.port(), 0);
    let port = listener.port();
    let client = tokio::spawn(async move {
        exchange(
            port,
            "GET /oauth/callback/mcp/docs?code=abc&state=s1 HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
        )
        .await
    });
    let request = listener.accept().await.unwrap();
    assert_eq!(request.target, "/oauth/callback/mcp/docs?code=abc&state=s1");
    request.respond(200, "Authorization successful!").await;
    let reply = client.await.unwrap();
    assert!(reply.starts_with("HTTP/1.1 200 OK\r\n"), "{reply}");
    assert!(reply.contains("Content-Type: text/plain; charset=utf-8\r\n"));
    assert!(
        reply.ends_with("\r\n\r\nAuthorization successful!"),
        "{reply}"
    );
}

#[tokio::test]
async fn unreadable_requests_are_refused_and_the_listener_keeps_waiting() {
    let listener = LoopbackListener::bind().await.unwrap();
    let port = listener.port();
    let refused = tokio::spawn(async move {
        let not_http = exchange(port, "garbage\r\n\r\n").await;
        let post = exchange(port, "POST /cb HTTP/1.1\r\nHost: x\r\n\r\n").await;
        let good = exchange(port, "GET /cb?state=s HTTP/1.1\r\nHost: x\r\n\r\n").await;
        (not_http, post, good)
    });
    let request = listener.accept().await.unwrap();
    assert_eq!(request.target, "/cb?state=s");
    request.respond(400, "Authorization failed.").await;
    let (not_http, post, good) = refused.await.unwrap();
    assert!(not_http.starts_with("HTTP/1.1 400 "), "{not_http}");
    assert!(post.starts_with("HTTP/1.1 405 "), "{post}");
    assert!(good.starts_with("HTTP/1.1 400 "), "{good}");
}
