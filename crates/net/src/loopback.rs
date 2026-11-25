//! 本机回调的监听（MCP OAuth 的 redirect_uri，docs/specs/rust-cli/ext.md §3.12.6）：只在 127.0.0.1 上、端口由系统分配，
//! 每条连接读一个 GET 请求、回一段纯文本、关掉。请求行与请求头的读法同 [`crate::ws_server`]（`httparse`）。
//!
//! 只有 net 能开端口（README §4）；判断请求是不是这次授权的回调（路径、state）是调用方的事。
use std::io;

use rust_cli_base::proto::http;
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};

use crate::ws_server::read_request;

/// 只监听本机回环（RFC 8252 §7.3：回环地址用 IP 字面量）。
const LOOPBACK_HOST: &str = "127.0.0.1";
/// 端口由系统分配。
const ANY_PORT: u16 = 0;

/// 一个监听中的回调端口。放下它就关掉端口。
pub struct LoopbackListener {
    inner: TcpListener,
    port: u16,
}

impl LoopbackListener {
    pub async fn bind() -> io::Result<Self> {
        Self::bind_port(ANY_PORT).await
    }

    /// 在给定的端口上监听（MCP OAuth 配了固定的回调端口，ext.md §3.12.15）；端口被占时报错。
    #[expect(
        clippy::disallowed_methods,
        reason = "net 是套接字的唯一入口（README §4）：OAuth 的本机回调在这里监听"
    )]
    pub async fn bind_port(port: u16) -> io::Result<Self> {
        let inner = TcpListener::bind((LOOPBACK_HOST, port)).await?;
        let port = inner.local_addr()?.port();
        Ok(Self { inner, port })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// 下一个读得出的 GET 请求。读不出的连接（不合 HTTP、太大、不是 GET）回对应的状态码、关掉，接着等下一条。
    pub async fn accept(&self) -> io::Result<LoopbackRequest> {
        loop {
            let (mut stream, _) = self.inner.accept().await?;
            match read_request(&mut stream).await {
                Ok((head, _)) => {
                    return Ok(LoopbackRequest {
                        stream,
                        target: head.path,
                    });
                }
                Err(status) => write_text(&mut stream, status, "").await,
            }
        }
    }
}

/// 一个回调请求：请求目标（路径加查询串，原样）。回答之后连接关掉。
pub struct LoopbackRequest {
    stream: TcpStream,
    pub target: String,
}

impl LoopbackRequest {
    /// 回 `status` 与一段纯文本（UTF-8）。写失败不再处理：浏览器那边已经走了。
    pub async fn respond(mut self, status: u16, text: &str) {
        write_text(&mut self.stream, status, text).await;
    }
}

async fn write_text(stream: &mut TcpStream, status: u16, text: &str) {
    let response = format!(
        "HTTP/1.1 {status} {}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
        http::reason_phrase(status),
        text.len(),
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

#[cfg(test)]
#[path = "loopback_tests.rs"]
mod tests;
