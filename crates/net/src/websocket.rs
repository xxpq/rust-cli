//! WebSocket 客户端（Monitor 的 ws 来源，docs/specs/rust-cli/tools-more.md §19.7；远端执行的 ws 传输，
//! remote-exec.md §29.4）。
//!
//! 升级请求经出口的同一个 HTTP 客户端发出（代理、不走代理的规则与根证书同其他请求）：`ws://` 换成 `http://`、
//! `wss://` 换成 `https://`，响应必须是 101 且 `Sec-WebSocket-Accept` 对得上。升级后的连接交给 `tokio-tungstenite`
//! 读写帧（README §3.2.1）：分片拼起来、ping 由库回 pong、对方的 close 由库回 close；文本帧与二进制帧交给调用方。
//! 单帧（或拼起来的消息）超过这条连接的上限时报错。客户端发的帧加掩码，服务端（[`crate::ws_server`]，serve 用）不加，
//! 都由库按角色定。
//!
//! 读是 cancel-safe 的（读到一半的帧留在库的缓冲里），放进 `select!` 不会丢帧。要同时收发时用 [`WebSocket::split`]
//! 拆成两半，各交给一个任务；拆开后读的一侧收到 ping 时，库在下一次读之前把 pong 发出去（写不动就留到之后，读不等写）。

use bytes::Bytes;
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{Sink, SinkExt, Stream, StreamExt};
use reqwest::header::HeaderMap;
use rust_cli_base::proto::http;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::error::{CapacityError, ProtocolError};
use tokio_tungstenite::tungstenite::handshake::client::generate_key;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::frame::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::{Role, WebSocketConfig};
use tokio_tungstenite::tungstenite::{Error, Message};

use crate::http::header_pair;
use crate::{ERROR_BODY_LIMIT, HeaderValue, HttpResponse, NetError};

/// Monitor 的单条上限：一帧（或一条分片的消息）最多这么多字节。别的调用方按自己的契约给（[`Egress::websocket`]）。
///
/// [`Egress::websocket`]: crate::Egress::websocket
pub const MAX_PAYLOAD: u64 = 1_048_576;

/// 关闭码（RFC 6455 §7.4.1）：正常关闭。
pub const CLOSE_NORMAL: u16 = 1000;
/// 关闭码：close 帧里没有状态码（只在本地报告，不上线）。
pub const CLOSE_NO_STATUS: u16 = 1005;
/// 关闭码：没有 close 帧就断开（只在本地报告，不上线）。
pub const CLOSE_ABNORMAL: u16 = 1006;

/// 读到的一帧。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WsFrame {
    Text(String),
    /// 二进制帧的内容。
    Binary(Bytes),
    /// 关闭：close 帧的代码与原因；没有 close 帧就断开时是 [`CLOSE_ABNORMAL`]、原因为空。
    Close {
        code: u16,
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WsError {
    /// 升级请求的响应不是 101。`body` 是响应体的开头（最多 [`ERROR_BODY_LIMIT`]）：服务端常把原因写在这里。
    ///
    /// [`ERROR_BODY_LIMIT`]: crate::ERROR_BODY_LIMIT
    #[error("Unexpected server response: {status}")]
    Rejected { status: u16, body: String },
    #[error(transparent)]
    Net(#[from] NetError),
    /// 握手或帧不合协议。
    #[error("{0}")]
    Protocol(String),
    /// 一帧或一条消息超过这条连接的上限（字节数）。
    #[error("frame of {0} bytes exceeds the limit")]
    TooBig(u64),
}

/// 一条连接底下的字节流：客户端是升级后的 HTTP 连接，服务端是接受的 TCP 连接。
pub(crate) trait ByteStream:
    tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin
{
}

impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin> ByteStream for T {}

pub(crate) type Io = Box<dyn ByteStream>;

/// 一条打开的连接。
pub struct WebSocket {
    inner: WebSocketStream<Io>,
    /// 已经发过（或回过）close 帧。
    closing: bool,
}

/// 读的一侧。
pub struct WsReader {
    inner: SplitStream<WebSocketStream<Io>>,
}

/// 写的一侧。
pub struct WsWriter {
    inner: SplitSink<WebSocketStream<Io>, Message>,
    /// 已经发过（或回过）close 帧。
    closing: bool,
}

/// `ws:` / `wss:` 换成 HTTP 的写法。
fn http_url(url: &str) -> Result<String, WsError> {
    let mut parsed =
        reqwest::Url::parse(url).map_err(|error| WsError::Protocol(error.to_string()))?;
    let scheme = match parsed.scheme() {
        "ws" | "http" => "http",
        "wss" | "https" => "https",
        other => {
            return Err(WsError::Protocol(format!(
                "unsupported socket base scheme {other}:"
            )));
        }
    };
    parsed
        .set_scheme(scheme)
        .map_err(|()| WsError::Protocol("invalid URL".to_owned()))?;
    Ok(parsed.to_string())
}

/// 测试的服务端回握手用（env-local、exec-transport、relay-connect 的 ws 测试）。
#[doc(hidden)]
pub fn accept_for_tests(key: &str) -> String {
    derive_accept_key(key.as_bytes())
}

/// 发升级请求、核对响应（`Egress::websocket` 调用）。`extra` 是这一次连接自己的请求头（秘密值用
/// [`HeaderValue::Secret`]），同名时盖过出口的策略头。
pub(crate) async fn connect(
    client: &reqwest::Client,
    base: HeaderMap,
    (url, protocols, extra): (&str, &[String], &[(String, HeaderValue)]),
    max_payload: u64,
) -> Result<WebSocket, WsError> {
    let target = http_url(url)?;
    let key = generate_key();
    let mut headers = base;
    let mut set = |name: &str, value: &HeaderValue| -> Result<(), WsError> {
        let (name, value) = header_pair(name, value).map_err(WsError::Protocol)?;
        headers.insert(name, value);
        Ok(())
    };
    for (name, value) in extra {
        set(name, value)?;
    }
    set("connection", &HeaderValue::from("Upgrade"))?;
    set("upgrade", &HeaderValue::from("websocket"))?;
    set("sec-websocket-version", &HeaderValue::from("13"))?;
    set("sec-websocket-key", &HeaderValue::from(key.clone()))?;
    if !protocols.is_empty() {
        set(
            "sec-websocket-protocol",
            &HeaderValue::from(protocols.join(", ")),
        )?;
    }
    let response = client
        .get(&target)
        .headers(headers)
        .send()
        .await
        .map_err(|error| NetError::from_reqwest(&error))?;
    let status = response.status().as_u16();
    if status != http::SWITCHING_PROTOCOLS {
        let body = HttpResponse::new(response)
            .text(ERROR_BODY_LIMIT)
            .await
            .unwrap_or_default();
        return Err(WsError::Rejected { status, body });
    }
    let accept = response
        .headers()
        .get("sec-websocket-accept")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if accept != derive_accept_key(key.as_bytes()) {
        return Err(WsError::Protocol(
            "Invalid Sec-WebSocket-Accept header".to_owned(),
        ));
    }
    let upgraded = response
        .upgrade()
        .await
        .map_err(|error| NetError::from_reqwest(&error))?;
    Ok(WebSocket::over(Box::new(upgraded), Vec::new(), max_payload, Role::Client).await)
}

impl WebSocket {
    /// 握手已经完成的字节流上的一条连接，这一端是 `role`。`early` 是读握手时多读的字节（对方紧接着发的帧），先于 `io` 读。
    pub(crate) async fn over(io: Io, early: Vec<u8>, max_payload: u64, role: Role) -> Self {
        let limit = Some(usize::try_from(max_payload).unwrap_or(usize::MAX));
        let config = WebSocketConfig::default()
            .max_message_size(limit)
            .max_frame_size(limit);
        Self {
            inner: WebSocketStream::from_partially_read(io, early, role, Some(config)).await,
            closing: false,
        }
    }

    /// 拆成读写两半，各交给一个任务。
    pub fn split(self) -> (WsReader, WsWriter) {
        let (sink, stream) = self.inner.split();
        (
            WsReader { inner: stream },
            WsWriter {
                inner: sink,
                closing: self.closing,
            },
        )
    }

    /// 下一条消息。对方的 close 由库排好回应，这里把它发出去再交出 close。
    pub async fn next(&mut self) -> Result<WsFrame, WsError> {
        let frame = next_frame(&mut self.inner).await?;
        if matches!(frame, WsFrame::Close { .. }) {
            close(&mut self.inner, &mut self.closing, CLOSE_NORMAL).await;
        }
        Ok(frame)
    }

    /// 发 close 帧（正常关闭 [`CLOSE_NORMAL`]）。
    pub async fn close(&mut self) {
        close(&mut self.inner, &mut self.closing, CLOSE_NORMAL).await;
    }
}

impl WsReader {
    /// 下一条消息。不回应任何东西（这一侧不能写）：ping 的 pong 由库发，对方的 close 由调用方经写的一侧回。
    pub async fn next(&mut self) -> Result<WsFrame, WsError> {
        next_frame(&mut self.inner).await
    }
}

impl WsWriter {
    /// 发一条二进制消息。
    pub async fn send_binary(&mut self, payload: &[u8]) -> Result<(), WsError> {
        self.send(Message::Binary(Bytes::copy_from_slice(payload)))
            .await
    }

    /// 发一条文本消息。
    pub async fn send_text(&mut self, text: &str) -> Result<(), WsError> {
        self.send(Message::text(text)).await
    }

    async fn send(&mut self, message: Message) -> Result<(), WsError> {
        self.inner
            .send(message)
            .await
            .map_err(|error| failure(&error))
    }

    /// 发 close 帧，只发一次。回应对方的 close 时库带上对方的代码与原因；只在本地报告的代码（[`CLOSE_NO_STATUS`]、
    /// [`CLOSE_ABNORMAL`]）不上线，发一个不带代码的 close 帧。
    pub async fn close(&mut self, code: u16) {
        close(&mut self.inner, &mut self.closing, code).await;
    }
}

/// 读到下一条文本、二进制或 close（ping、pong 跳过：pong 由库回）。
async fn next_frame(
    stream: &mut (impl Stream<Item = Result<Message, Error>> + Unpin),
) -> Result<WsFrame, WsError> {
    loop {
        let message = match stream.next().await {
            Some(Ok(message)) => message,
            // 没有 close 帧就断开：读到流的末尾（库报 `ResetWithoutClosingHandshake`），或关闭之后、出错之后流已结束。
            None | Some(Err(Error::Protocol(ProtocolError::ResetWithoutClosingHandshake))) => {
                return Ok(WsFrame::Close {
                    code: CLOSE_ABNORMAL,
                    reason: String::new(),
                });
            }
            Some(Err(error)) => return Err(failure(&error)),
        };
        return Ok(match message {
            Message::Text(text) => WsFrame::Text(text.as_str().to_owned()),
            Message::Binary(payload) => WsFrame::Binary(payload),
            Message::Close(Some(frame)) => WsFrame::Close {
                code: frame.code.into(),
                reason: frame.reason.as_str().to_owned(),
            },
            Message::Close(None) => WsFrame::Close {
                code: CLOSE_NO_STATUS,
                reason: String::new(),
            },
            Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => continue,
        });
    }
}

/// 发 close 帧（只发一次）。对方先关时库已排好回应，再写 close 会被拒（`SendAfterClosing`）：`flush` 把排好的回应发出去。
/// 发不出去不再处理：连接本来就要关。
async fn close(
    sink: &mut (impl Sink<Message, Error = Error> + Unpin),
    closing: &mut bool,
    code: u16,
) {
    if std::mem::replace(closing, true) {
        return;
    }
    let frame = match code {
        CLOSE_NO_STATUS | CLOSE_ABNORMAL => None,
        _ => Some(CloseFrame {
            code: code.into(),
            reason: "".into(),
        }),
    };
    let _ = sink.send(Message::Close(frame)).await;
    let _ = sink.flush().await;
}

/// 库的错误换成本模块的：太大是 [`WsError::TooBig`]，IO 是 [`NetError::Io`]，别的是协议错误。
fn failure(error: &Error) -> WsError {
    match error {
        Error::Capacity(CapacityError::MessageTooLong { size, .. }) => {
            WsError::TooBig(*size as u64)
        }
        Error::Io(error) => WsError::Net(NetError::Io(error.to_string())),
        other => WsError::Protocol(other.to_string()),
    }
}

#[cfg(test)]
#[path = "websocket_tests.rs"]
mod tests;
