//! WebSocket 服务端（serve 的入口，docs/specs/rust-cli/multi-tenant.md §4.1）：监听、读升级请求、由调用方按请求头决定收
//! 不收，收下时回 101，之后的读写同客户端（[`WebSocket`]，帧由 `tokio-tungstenite` 读写），只是服务端发的帧不加掩码、
//! 收的帧必须有掩码（RFC 6455 §5.1，库按角色定）。
//!
//! 只认最小的升级请求：`GET`、`Upgrade: websocket`、`Connection` 含 `upgrade`、`Sec-WebSocket-Version: 13`、
//! `Sec-WebSocket-Key`。不协商子协议与扩展。请求头整体超过 [`MAX_REQUEST_BYTES`]、或多于 [`MAX_HEADERS`] 个时回 431。
//!
//! 请求行与请求头由 `httparse`（库的服务端握手内部用的解析器）解析，状态码自己定（README §3.2.1）：库的服务端握手遇到不合格
//! 的请求时不回状态码就断开，serve 要回 400 / 405 / 426 / 431。
use std::net::SocketAddr;

use rust_cli_base::proto::http;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::handshake::headers::MAX_HEADERS;
use tokio_tungstenite::tungstenite::protocol::Role;

use crate::websocket::{WebSocket, WsError};

/// 升级请求的请求行与请求头加起来最多这么多字节。
pub const MAX_REQUEST_BYTES: usize = 16 * 1024;

/// 一个监听中的端口。
pub struct WsListener {
    inner: TcpListener,
}

impl WsListener {
    /// 在 `address`（`host:port`，端口 0 由系统分配）上监听。
    #[expect(
        clippy::disallowed_methods,
        reason = "net 是套接字的唯一入口（README §4）：serve 在这里监听"
    )]
    pub async fn bind(address: &str) -> std::io::Result<Self> {
        Ok(Self {
            inner: TcpListener::bind(address).await?,
        })
    }

    /// 实际监听的地址。
    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.inner.local_addr()
    }

    /// 下一条进来的 TCP 连接（还没读升级请求）。
    pub async fn accept(&self) -> std::io::Result<Incoming> {
        let (stream, peer) = self.inner.accept().await?;
        Ok(Incoming { stream, peer })
    }
}

/// 进来的一条连接，还没握手。
pub struct Incoming {
    stream: TcpStream,
    peer: SocketAddr,
}

/// 升级请求：路径与请求头（名字是小写）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upgrade {
    pub path: String,
    pub headers: Vec<(String, String)>,
}

impl Upgrade {
    /// 某个请求头的值（名字不分大小写；同名多个时取第一个）。
    pub fn header(&self, name: &str) -> Option<&str> {
        let name = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| value.as_str())
    }
}

impl Incoming {
    pub fn peer(&self) -> SocketAddr {
        self.peer
    }

    /// 读升级请求；`admit` 看请求决定收不收：`Err(状态码)` 时回那个状态码并关掉连接。收下时回 101，交出连接。
    pub async fn upgrade(
        mut self,
        max_payload: u64,
        admit: impl FnOnce(&Upgrade) -> Result<(), u16>,
    ) -> Result<(WebSocket, Upgrade), WsError> {
        let (upgrade, early) = match read_request(&mut self.stream).await {
            Ok(read) => read,
            Err(status) => {
                refuse(&mut self.stream, status).await;
                return Err(WsError::Protocol(format!(
                    "refused the upgrade request with {status}"
                )));
            }
        };
        let key = match validate(&upgrade) {
            Ok(key) => key.to_owned(),
            Err(status) => {
                refuse(&mut self.stream, status).await;
                return Err(WsError::Protocol(format!(
                    "refused the upgrade request with {status}"
                )));
            }
        };
        if let Err(status) = admit(&upgrade) {
            refuse(&mut self.stream, status).await;
            return Err(WsError::Rejected {
                status,
                body: String::new(),
            });
        }
        let response = format!(
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n",
            derive_accept_key(key.as_bytes())
        );
        self.stream
            .write_all(response.as_bytes())
            .await
            .map_err(|error| WsError::Protocol(format!("writing the upgrade response: {error}")))?;
        Ok((
            WebSocket::over(Box::new(self.stream), early, max_payload, Role::Server).await,
            upgrade,
        ))
    }
}

/// 读请求行与请求头，到空行为止：交出请求与读多了的字节（客户端紧接着发的第一帧，交给帧的读者）。连接在读完之前结束、
/// 不合 HTTP、太大或头太多时给出要回的状态码。
pub(crate) async fn read_request(
    stream: &mut (impl AsyncRead + Unpin),
) -> Result<(Upgrade, Vec<u8>), u16> {
    let mut buffer = Vec::with_capacity(MAX_REQUEST_BYTES);
    loop {
        let read = stream
            .read_buf(&mut buffer)
            .await
            .map_err(|_| http::BAD_REQUEST)?;
        if read == 0 {
            return Err(http::BAD_REQUEST);
        }
        // httparse 不留状态：每读一次从头解一遍（请求头最多 16 KiB）。
        let mut headers = [httparse::EMPTY_HEADER; MAX_HEADERS];
        let mut request = httparse::Request::new(&mut headers);
        let length = match request.parse(&buffer) {
            Ok(httparse::Status::Complete(length)) => length,
            Ok(httparse::Status::Partial) if buffer.len() > MAX_REQUEST_BYTES => {
                return Err(http::REQUEST_HEADER_FIELDS_TOO_LARGE);
            }
            Ok(httparse::Status::Partial) => continue,
            Err(httparse::Error::TooManyHeaders) => {
                return Err(http::REQUEST_HEADER_FIELDS_TOO_LARGE);
            }
            Err(_) => return Err(http::BAD_REQUEST),
        };
        if length > MAX_REQUEST_BYTES {
            return Err(http::REQUEST_HEADER_FIELDS_TOO_LARGE);
        }
        let upgrade = upgrade_of(&request)?;
        return Ok((upgrade, buffer.split_off(length)));
    }
}

/// 解完的请求 → [`Upgrade`]：只收 `GET`；名字转小写、值去首尾空白；值不是 UTF-8 时 400。
fn upgrade_of(request: &httparse::Request) -> Result<Upgrade, u16> {
    let (Some(method), Some(path)) = (request.method, request.path) else {
        return Err(http::BAD_REQUEST);
    };
    if method != "GET" {
        return Err(http::METHOD_NOT_ALLOWED);
    }
    let headers = request
        .headers
        .iter()
        .map(|header| {
            let value = std::str::from_utf8(header.value).map_err(|_| http::BAD_REQUEST)?;
            Ok((header.name.to_ascii_lowercase(), value.trim().to_owned()))
        })
        .collect::<Result<_, u16>>()?;
    Ok(Upgrade {
        path: path.to_owned(),
        headers,
    })
}

/// 核对升级请求的必要头，给出 `Sec-WebSocket-Key`。
fn validate(upgrade: &Upgrade) -> Result<&str, u16> {
    let has = |name: &str, token: &str| {
        upgrade.header(name).is_some_and(|value| {
            value
                .split(',')
                .any(|part| part.trim().eq_ignore_ascii_case(token))
        })
    };
    if !has("upgrade", "websocket") || !has("connection", "upgrade") {
        return Err(http::BAD_REQUEST);
    }
    if upgrade.header("sec-websocket-version") != Some("13") {
        return Err(http::UPGRADE_REQUIRED);
    }
    upgrade
        .header("sec-websocket-key")
        .filter(|key| !key.is_empty())
        .ok_or(http::BAD_REQUEST)
}

/// 回一个不升级的响应，关掉连接（写失败不再处理：连接本来就要关）。
async fn refuse(stream: &mut TcpStream, status: u16) {
    let response = format!(
        "HTTP/1.1 {status} {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        http::reason_phrase(status)
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

#[cfg(test)]
#[path = "ws_server_tests.rs"]
mod tests;
