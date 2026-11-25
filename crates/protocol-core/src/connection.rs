//! 一条连接（docs/specs/rust-cli/app-protocol.md §3.3、§4.1）：读循环、请求派发、回复与通知的写者、
//! 本端发出的请求（反向请求）的关联。分帧（按 LF / Content-Length）与方言（带不带 `jsonrpc`）是 [`Options`] 的选项。
//!
//! - [`serve`]：读循环只做解析与派发：每个请求在自己的任务里处理，回复按完成顺序写出；读循环不等请求处理完。
//! - [`serve_in_order`]：读循环把请求与通知按到达顺序交给一个 owner（协议的语义依赖到达顺序时用），自己不处理。
//! - 写出经一个写者任务串行化，一行一个完整的 JSON；队列有界，满了时写的一方等（不丢）。
//! - 请求的回复可以带一个“回复之后”的动作（例如订阅之后发首帧）：动作在回复写进队列之后才开始，所以它写的东西排在回复
//!   之后。
//! - 本端发的请求用字符串 id `rs-<n>`，与对端的 id 空间分开；连接断开时等待中的请求全部失败。不靠超时同步状态。
//! - 发请求可以分两步（[`Peer::send_request`]）：写出之后就拿到等待（[`Pending`]），写的一方不必等回复（连接的 owner 只写，
//!   发起的任务等）；回复由读循环直接交给等待。

use rust_cli_base::sync::LockUnpoisoned;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use async_trait::async_trait;
use futures_util::future::BoxFuture;
use rust_cli_base::{Spawner, Task, units};
use serde_json::Value;
use tokio::io::{AsyncBufRead, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};

use crate::framing::{self, Dialect, Framing, Read};
use crate::rpc::{self, Incoming, RpcError};

/// 连接断开后发出的请求得到的错误码（实现自定义的范围）。
pub const CONNECTION_CLOSED: i64 = -32000;

/// 回复之后要做的事（排在回复之后写出）。
pub type After = Box<dyn FnOnce() -> BoxFuture<'static, ()> + Send>;

/// 一个请求的结果：回复的 `result`，以及回复写出之后的动作。
pub struct Reply {
    pub result: Value,
    pub then: Option<After>,
}

impl Reply {
    pub fn new(result: Value) -> Self {
        Self { result, then: None }
    }

    pub fn then(result: Value, after: After) -> Self {
        Self {
            result,
            then: Some(after),
        }
    }
}

impl From<Value> for Reply {
    fn from(result: Value) -> Self {
        Self::new(result)
    }
}

/// 协议的处理方。
#[async_trait]
pub trait Service: Send + Sync + 'static {
    async fn request(&self, method: &str, params: Value) -> Result<Reply, RpcError>;

    /// 对端的通知。缺省忽略（不认识的通知一律忽略）。
    async fn notification(&self, method: &str, params: Value) {
        let _ = (method, params);
    }
}

/// 缺省：一条消息的上限（字节）。
const DEFAULT_MAX_MESSAGE_BYTES: usize = 64 * units::MIB;
/// 缺省：写者队列的容量（条）。
const DEFAULT_QUEUE: usize = 1024;

/// 连接的参数。缺省是 v4 的写法：按 LF 分帧、没有 `jsonrpc` 字段。
#[derive(Debug, Clone, Copy)]
pub struct Options {
    /// 一条消息（一行或一帧的正文）的上限（字节）。按行分帧时超过的行丢弃并写诊断；按 Content-Length 分帧时读循环
    /// 以错误结束（帧边界已不可信）。
    pub max_message_bytes: usize,
    /// 写者队列的容量（条）。
    pub queue: usize,
    pub framing: Framing,
    pub dialect: Dialect,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            max_message_bytes: DEFAULT_MAX_MESSAGE_BYTES,
            queue: DEFAULT_QUEUE,
            framing: Framing::Lines,
            dialect: Dialect::Bare,
        }
    }
}

type Waiting = HashMap<String, oneshot::Sender<Result<Value, RpcError>>>;

struct PeerInner {
    out: mpsc::Sender<String>,
    dialect: Dialect,
    waiting: Mutex<Waiting>,
    next: AtomicU64,
    closed: AtomicBool,
}

/// 往对端写东西的句柄：通知、回复与本端发的请求。可克隆。
#[derive(Clone)]
pub struct Peer(Arc<PeerInner>);

impl Peer {
    /// 写一条消息（完整的 JSON 文字，不带分帧）。连接已断开时返回假。
    pub async fn write(&self, line: String) -> bool {
        self.0.out.send(line).await.is_ok()
    }

    pub async fn notify(&self, method: &str, params: &Value) -> bool {
        self.write(rpc::notification(self.0.dialect, method, params))
            .await
    }

    /// 一条 params 已经编码成 JSON 文字的通知：不再解析、重编（调用方量字节时已经编过一次的大消息）。
    pub async fn notify_text(&self, method: &str, params: &str) -> bool {
        self.write(rpc::notification_text(self.0.dialect, method, params))
            .await
    }

    /// 回复对端的请求。
    pub async fn respond(&self, id: &Value, result: &Result<Value, RpcError>) -> bool {
        self.write(rpc::response(self.0.dialect, id, result)).await
    }

    /// 不等队列的通知（在不能 await 的地方用，例如取消时的 Drop）：队列满或已断开时丢弃并返回假。
    pub fn try_notify(&self, method: &str, params: &Value) -> bool {
        self.0
            .out
            .try_send(rpc::notification(self.0.dialect, method, params))
            .is_ok()
    }

    /// 发一个请求并等回复。连接断开时（之前或等待中）返回 [`CONNECTION_CLOSED`]。
    pub async fn request(&self, method: &str, params: &Value) -> Result<Value, RpcError> {
        self.send_request(method, params).await.await
    }

    /// 写出一个请求，返回它的等待（不等回复）。连接已经断开、或写不进去时，等待立即以 [`CONNECTION_CLOSED`] 结束。
    pub async fn send_request(&self, method: &str, params: &Value) -> Pending {
        if self.0.closed.load(Ordering::Acquire) {
            return Pending::closed();
        }
        let id = format!("rs-{}", self.0.next.fetch_add(1, Ordering::Relaxed) + 1);
        let (sender, receiver) = oneshot::channel();
        self.waiting().insert(id.clone(), sender);
        // 等待表登记之后再检查一次：读循环在两者之间结束时也不会漏掉。
        if self.0.closed.load(Ordering::Acquire) {
            self.waiting().remove(&id);
            return Pending::closed();
        }
        let forget = Forget {
            peer: self.clone(),
            id: id.clone(),
        };
        if !self
            .write(rpc::request(
                self.0.dialect,
                &Value::String(id),
                method,
                params,
            ))
            .await
        {
            return Pending::closed();
        }
        Pending(Wait::Open {
            receiver,
            _forget: forget,
        })
    }

    /// 连接已经断开（读到结束或写失败）。
    pub fn is_closed(&self) -> bool {
        self.0.closed.load(Ordering::Acquire)
    }

    fn waiting(&self) -> std::sync::MutexGuard<'_, Waiting> {
        self.0.waiting.lock_unpoisoned()
    }

    fn resolve(&self, id: &Value, result: Result<Value, RpcError>) {
        let Some(key) = id.as_str() else {
            tracing::warn!(
                event = "protocol.response.unknown",
                "a response with a non-string id"
            );
            return;
        };
        match self.waiting().remove(key) {
            Some(sender) => {
                let _ = sender.send(result);
            }
            // debug 不是 warn：调用方放弃了等待（例如回合停了）之后回复才到，是正常的结局（不可取消的请求被放弃时就是这样），
            // 不是对端的错。
            None => tracing::debug!(
                event = "protocol.response.unknown",
                id = key,
                "a response to no waiting request"
            ),
        }
    }

    fn close(&self) {
        self.0.closed.store(true, Ordering::Release);
        let waiting: Vec<_> = self.waiting().drain().collect();
        for (_, sender) in waiting {
            let _ = sender.send(Err(connection_closed()));
        }
    }
}

fn connection_closed() -> RpcError {
    RpcError::new(CONNECTION_CLOSED, "the connection is closed")
}

/// 一个已经写出的请求的回复（[`Peer::send_request`]）。丢弃它就是放弃等待：从等待表里拿走，之后到的回复被读循环丢掉。
pub struct Pending(Wait);

enum Wait {
    /// 没写出去（连接已经断开）。
    Closed,
    Open {
        receiver: oneshot::Receiver<Result<Value, RpcError>>,
        _forget: Forget,
    },
}

impl Pending {
    fn closed() -> Self {
        Self(Wait::Closed)
    }
}

impl Future for Pending {
    type Output = Result<Value, RpcError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match &mut self.get_mut().0 {
            Wait::Closed => Poll::Ready(Err(connection_closed())),
            // 发送端被丢掉（连接关闭时清空了等待表）也是连接断开
            Wait::Open { receiver, .. } => Pin::new(receiver)
                .poll(cx)
                .map(|result| result.unwrap_or_else(|_| Err(connection_closed()))),
        }
    }
}

/// 请求的 future 被丢弃（调用方取消）时从等待表里拿走。
struct Forget {
    peer: Peer,
    id: String,
}

impl Drop for Forget {
    fn drop(&mut self) {
        self.peer.waiting().remove(&self.id);
    }
}

/// 起写者任务：按 [`Options::framing`] 一条一条写，写完一条就 flush。写失败时连接算断开。
pub fn start<W>(writer: W, spawner: &Spawner, options: Options) -> (Peer, Task<()>)
where
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (out, mut lines) = mpsc::channel::<String>(options.queue.max(1));
    let peer = Peer(Arc::new(PeerInner {
        out,
        dialect: options.dialect,
        waiting: Mutex::new(HashMap::new()),
        next: AtomicU64::new(0),
        closed: AtomicBool::new(false),
    }));
    // 修复：写者以前拿着一个 Peer（为了写失败时标记断开），队列的发送端永远有一个活着，写者等不到队列关闭，进程退出时要等到
    // 作用域的收尾上限才被中止。改成弱引用：全部 Peer 丢掉后写者写完队列就结束。
    let closing = Arc::downgrade(&peer.0);
    let framing = options.framing;
    let task = spawner.spawn("protocol-writer", async move {
        let mut writer = writer;
        while let Some(message) = lines.recv().await {
            let bytes = framing::encode(framing, &message);
            let written = async {
                writer.write_all(&bytes).await?;
                writer.flush().await
            };
            if let Err(error) = written.await {
                tracing::warn!(event = "protocol.write.failed", error = %error, "writing to the peer failed");
                if let Some(inner) = closing.upgrade() {
                    Peer(inner).close();
                }
                break;
            }
        }
    });
    (peer, task)
}

/// 读一条消息并解析；`None` 是读到结束。超过上限的行（按行分帧）丢弃、写诊断；坏消息按方言回复或写诊断。
async fn next_message<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    peer: &Peer,
    options: Options,
) -> std::io::Result<Option<Incoming>> {
    loop {
        let text = match framing::read(reader, options.framing, options.max_message_bytes).await? {
            Some(Read::Text(text)) => text,
            Some(Read::TooLong) => {
                tracing::warn!(
                    event = "protocol.line.too_long",
                    limit = options.max_message_bytes,
                    "an incoming line exceeded the limit and was dropped"
                );
                continue;
            }
            None => return Ok(None),
        };
        if text.trim().is_empty() {
            continue;
        }
        match rpc::parse(&text) {
            Ok(incoming) => return Ok(Some(incoming)),
            // 修改原因（16 §10 审定第 7 条，读失败即链路断开）：以前回 id 为 null 的 -32700 再接着读；正文坏了的那条若是
            // 回复或通知，在途请求与信用都静默少了一份，只有关链路能让在途的东西按断开结算。
            Err(malformed)
                if malformed.error.code == rpc::codes::PARSE_ERROR
                    && options.dialect.closes_on_unreadable() =>
            {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "an incoming message is not JSON: {}",
                        malformed.error.message
                    ),
                ));
            }
            Err(malformed) => match malformed.id {
                Some(id) => {
                    peer.respond(&id, &Err(malformed.error)).await;
                }
                // JSON-RPC 2.0：认不出 id 的坏消息回 id 为 null 的错误（v4 的对端不认这种回复，只写诊断）
                None if options.dialect.answers_unknown_ids() => {
                    peer.respond(&Value::Null, &Err(malformed.error)).await;
                }
                None => tracing::warn!(
                    event = "protocol.line.malformed",
                    error = %malformed.error,
                    "an incoming line could not be parsed"
                ),
            },
        }
    }
}

/// 读循环：一直读到对端关掉输入。结束时连接算断开，等待中的反向请求全部失败。
pub async fn serve<R>(
    mut reader: R,
    peer: Peer,
    service: Arc<dyn Service>,
    spawner: Spawner,
    options: Options,
) -> std::io::Result<()>
where
    R: AsyncBufRead + Unpin,
{
    let result = loop {
        let incoming = match next_message(&mut reader, &peer, options).await {
            Ok(Some(incoming)) => incoming,
            Ok(None) => break Ok(()),
            Err(error) => break Err(error),
        };
        match incoming {
            Incoming::Request { id, method, params } => {
                let (peer, service) = (peer.clone(), service.clone());
                drop(spawner.spawn("protocol-request", async move {
                    let reply = service.request(&method, params).await;
                    let (result, then) = match reply {
                        Ok(Reply { result, then }) => (Ok(result), then),
                        Err(error) => (Err(error), None),
                    };
                    if !peer.respond(&id, &result).await {
                        return;
                    }
                    if let Some(then) = then {
                        then().await;
                    }
                }));
            }
            Incoming::Notification { method, params } => {
                let service = service.clone();
                drop(spawner.spawn("protocol-notification", async move {
                    service.notification(&method, params).await;
                }));
            }
            Incoming::Response { id, result } => peer.resolve(&id, result),
        }
    };
    peer.close();
    result
}

/// 交给 owner 的一条消息（对端的回复不在这里：读循环直接交给本端请求的等待表）。
#[derive(Debug, Clone, PartialEq)]
pub enum Inbound {
    Request {
        id: Value,
        method: String,
        params: Value,
    },
    Notification {
        method: String,
        params: Value,
    },
}

/// 按到达顺序的读循环：请求与通知按到达的顺序送进 `intake`（有界：owner 处理不过来时读循环等，不丢）。owner 不再收
/// （`intake` 关了）之后照常读到结束、丢掉消息（例如对端说了再见）。结束时连接算断开。
pub async fn serve_in_order<R>(
    mut reader: R,
    peer: Peer,
    intake: mpsc::Sender<Inbound>,
    options: Options,
) -> std::io::Result<()>
where
    R: AsyncBufRead + Unpin,
{
    let result = loop {
        let incoming = match next_message(&mut reader, &peer, options).await {
            Ok(Some(incoming)) => incoming,
            Ok(None) => break Ok(()),
            Err(error) => break Err(error),
        };
        let inbound = match incoming {
            Incoming::Response { id, result } => {
                peer.resolve(&id, result);
                continue;
            }
            Incoming::Request { id, method, params } => Inbound::Request { id, method, params },
            Incoming::Notification { method, params } => Inbound::Notification { method, params },
        };
        if intake.send(inbound).await.is_err() {
            tracing::debug!(
                event = "protocol.inbound.dropped",
                "the owner stopped taking messages; dropped one"
            );
        }
    };
    peer.close();
    result
}

#[cfg(test)]
#[path = "connection_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "connection_frames_tests.rs"]
mod frames_tests;
