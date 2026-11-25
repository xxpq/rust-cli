//! streamable HTTP 与 SSE 传输（docs/specs/rust-cli/ext.md §3.2；MCP SDK `StreamableHTTPClientTransport`、
//! `SSEClientTransport`），请求经 `rust-cli-net` 的 [`Egress`]。
//!
//! - streamable HTTP：每条消息一个 POST；响应是 JSON 或 SSE 流（流在后台任务里读）；记下
//!   `mcp-session-id`；从 `initialize` 的回复记下协商到的修订，之后的请求都带 `mcp-protocol-version`（ext.md §3.10.5）；
//!   `notifications/initialized` 发出后开一个 GET SSE 流收服务器消息（405 时不开）；
//!   SSE 流在回复之前断开、给过事件 ID 时，按服务器的 `retry` 带 `Last-Event-ID` 再 GET 续读（SEP-1699，ext.md §3.10.6）；
//!   续不上时对等回复的请求送出 `ReplyLost`（同一节的“续不上时”）；对请求回 202、不认的内容类型、JSON 体里没有回复时
//!   同样送出 `ReplyLost`（同一节的“POST 的响应里没有回复”）；
//!   带会话 ID 的请求得到 404 时会话已过期，送出 `Closed{session_expired}`。
//! - SSE：GET 得到事件流，第一个 `endpoint` 事件给出 POST 地址；响应经事件流的 `message` 事件到达。
//!
//! 用 OAuth 的服务器（ext.md §3.12.3）：每个请求经 [`Bearer`] 带令牌，401 之后换一个、重发一次，换不了时是
//! `McpError::AuthRequired`（握手与 SSE 的 GET 时报给 ext，ext 记成 NeedsAuth）。
//!
//! 会话结束时应发 `DELETE`；net 还没有 DELETE，这里不发（ext.md §8）。
//!
//! 事件通道有界（审查 R1-18）：读响应流的任务在通道满时停读（背压到服务器）；`send` 自己收到的 JSON 响应与
//! 会话过期交给一个后台任务送出——`send` 可能正由事件的唯一读者调用（回答服务器的请求），在这里等通道就是
//! 自己等自己。

use rust_cli_base::sync::LockUnpoisoned;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rust_cli_base::OwnedTask;
use rust_cli_base::proto::http;
use rust_cli_base::secret::RedactedString;
use rust_cli_base::units;
use rust_cli_kernel_api::mcp::{McpError, McpTransportEvent};
use rust_cli_net::header::{ACCEPT, CONTENT_TYPE, EVENT_STREAM, JSON};
use rust_cli_net::sse::SseDecoder;
use rust_cli_net::{ERROR_BODY_LIMIT, Egress, HeaderValue, HttpRequest, HttpResponse};

pub(crate) use super::bearer::Bearer;
use serde_json::Value;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

/// Streamable HTTP 的会话头（MCP 规范；请求带上、响应里读，F55：以前写了两次）。
const SESSION_HEADER: &str = "mcp-session-id";
/// 协商到的修订的请求头（streamable HTTP，MCP `2025-06-18` 起 MUST；ext.md §3.10.5）。
const PROTOCOL_VERSION_HEADER: &str = "mcp-protocol-version";
/// 握手请求的方法名：传输从它的回复里取协商到的修订。
const INITIALIZE: &str = "initialize";
/// 握手完成的通知：发出之后开 GET 监听流。
const INITIALIZED: &str = "notifications/initialized";
/// 取消请求的通知：之后不再跟着读那个请求的流（ext.md §3.10.6）。
const CANCELLED: &str = "notifications/cancelled";
/// 续读时带上最后收到的事件 ID（SSE 规范的请求头；MCP 的续读一律是 GET）。
const LAST_EVENT_ID_HEADER: &str = "last-event-id";
/// 服务器给了事件 ID、没给 `retry` 时续读前等多久：SDK `StreamableHTTPClientTransport` 的初始值
/// （`initialReconnectionDelay`）；WHATWG 说由实现定（ext.md §3.10.6）。
const RECONNECTION_TIME: Duration = Duration::from_millis(1000);
/// 会话过期的原因文字（`Closed` 与发送的错误）。
const SESSION_EXPIRED: &str = "MCP session expired";
/// 等回复的流断了、没有事件 ID 可续时 `ReplyLost` 的原因（ext.md §3.10.6 的“续不上时”）。
const ENDED_WITHOUT_EVENT_ID: &str =
    "MCP response stream ended before the reply, with no event ID to resume from";
/// 续读的 GET 出错或不是 2xx 时 `ReplyLost` 的原因的开头（后接错误或 `HTTP <状态>: <响应体>`）。
const NOT_RESUMED: &str = "MCP response stream could not be resumed";
/// 对请求回 202 时 `ReplyLost` 的原因（ext.md §3.10.6 的“POST 的响应里没有回复”）。
const ACCEPTED_REQUEST: &str =
    "MCP server answered the request with 202 Accepted, which carries no reply";
/// 对请求回的内容类型既不是 JSON 也不是 SSE 时 `ReplyLost` 的原因的开头（后接内容类型）。
const UNSUPPORTED_CONTENT_TYPE: &str =
    "MCP server answered the request with an unsupported content type";
/// 响应没有内容类型时原因里写的内容类型。
const NO_CONTENT_TYPE: &str = "none";
/// JSON 响应体里没有这个请求的回复时 `ReplyLost` 的原因。
const JSON_WITHOUT_REPLY: &str = "MCP server's JSON response did not include the reply";
/// JSON 响应体的上限。
const BODY_LIMIT: usize = 64 * units::MIB;

type Events = super::EventSender;

/// 建连接要的：出口、服务器 URL、配置的 header 与（用 OAuth 时）令牌的来源。
pub(crate) struct Target {
    egress: Arc<Egress>,
    url: String,
    headers: BTreeMap<String, String>,
    bearer: Option<Bearer>,
}

impl Target {
    pub(crate) fn new(
        egress: Arc<Egress>,
        url: &str,
        headers: &BTreeMap<String, String>,
        bearer: Option<Bearer>,
    ) -> Self {
        Self {
            egress,
            url: url.to_owned(),
            headers: headers.clone(),
            bearer,
        }
    }
}

/// 发请求要的：出口、地址、配置的 header、令牌的来源、会话 ID、协商到的修订。连接与读 SSE 流的任务共用（`initialize` 的
/// 回复可能在流里到）；它不拥有读流的任务，任务与连接不互相持有。
struct Endpoint {
    egress: Arc<Egress>,
    headers: BTreeMap<String, String>,
    bearer: Option<Bearer>,
    /// streamable：服务器 URL；SSE：`endpoint` 事件给出的 POST 地址。
    url: String,
    session: Mutex<Option<String>>,
    revision: Mutex<Revision>,
}

/// 这条连接协商到的修订（streamable，ext.md §3.10.5）：发出的 `initialize` 的请求 ID，和它的回复里的 `protocolVersion`。
/// 服务器回的修订就是协商的结果：ext 不接受时不发 `initialized`、关闭连接，之后没有请求。
#[derive(Default)]
struct Revision {
    initialize: Option<Value>,
    negotiated: Option<String>,
}

impl Endpoint {
    fn new(target: Target, url: String) -> Self {
        Self {
            egress: target.egress,
            headers: target.headers,
            bearer: target.bearer,
            url,
            session: Mutex::default(),
            revision: Mutex::default(),
        }
    }

    /// 发一个请求：用 OAuth 时带令牌、401 之后换一个重发一次（[`Bearer`]）；网络错误是 `Connect`。
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, McpError> {
        match &self.bearer {
            Some(bearer) => bearer.send(&self.egress, request).await,
            None => self
                .egress
                .send(request)
                .await
                .map_err(|error| failed(error.to_string())),
        }
    }

    /// 加上配置的 header、会话 ID 与协商到的修订。
    fn request(&self, request: HttpRequest) -> HttpRequest {
        let mut request = with_headers(request, &self.headers);
        if let Some(session) = self.session.lock_unpoisoned().clone() {
            request = request.header(SESSION_HEADER, session);
        }
        if let Some(revision) = self.revision.lock_unpoisoned().negotiated.clone() {
            request = request.header(PROTOCOL_VERSION_HEADER, revision);
        }
        request
    }

    /// 会话过期：带会话 ID 的请求得到 404（streamable HTTP）。
    fn expired(&self, status: u16) -> bool {
        status == http::NOT_FOUND && self.session.lock_unpoisoned().is_some()
    }

    /// 服务器发来的消息是 `initialize` 的回复时记下修订（只认第一个）。先于把它送进事件通道调用：ext 收到回复之后才发
    /// `initialized`，所以之后的请求一定带上。服务器自己的请求有它自己的 ID 空间（可能也是 0），只认没有 `method` 的回复。
    fn observe(&self, message: &Value) {
        let mut revision = self.revision.lock_unpoisoned();
        if revision.initialize.is_none() || reply_id(message) != revision.initialize.as_ref() {
            return;
        }
        revision.initialize = None;
        revision.negotiated = message
            .pointer("/result/protocolVersion")
            .and_then(Value::as_str)
            .map(str::to_owned);
    }
}

/// 一条 HTTP 连接（streamable 或 SSE）。
pub(crate) struct HttpLink {
    endpoint: Arc<Endpoint>,
    streamable: bool,
    events: Events,
    /// 后台任务（读流、送出事件），带它在等回复的请求 ID（只有 POST 的响应流有）：连接释放时全部中止；请求被取消时
    /// 中止它的流（不再读、不再续读）。
    readers: Mutex<Vec<(Option<Value>, OwnedTask<()>)>>,
    closed: CancellationToken,
}

fn failed(message: String) -> McpError {
    McpError::Connect { message }
}

/// 加上配置给的 header。
///
/// 缺陷修复 R2-13：值原来按普通值交给 net；配置里的 header 常是 Authorization 或 API key，现在一律是秘密值
/// （发送时标为敏感，任何 Debug 输出都看不到原值）。
fn with_headers(request: HttpRequest, headers: &BTreeMap<String, String>) -> HttpRequest {
    headers.iter().fold(request, |request, (name, value)| {
        request.header(
            name.as_str(),
            HeaderValue::Secret(RedactedString::new(value.as_str())),
        )
    })
}

fn messages(value: Value) -> Vec<Value> {
    match value {
        Value::Array(items) => items,
        other => vec![other],
    }
}

/// 回复（有 `id`、没有 `method`）的 ID；请求与通知是 None。
fn reply_id(message: &Value) -> Option<&Value> {
    message
        .get("id")
        .filter(|_| message.get("method").is_none())
}

/// 读一个 SSE 流到结束，把 `message`（或没有名字的）事件里的消息先交给 `seen`、再送进事件通道；`endpoint` 事件交给
/// `endpoint`。解码器由调用方给：续读换连接时接着用（last event ID 与 `retry` 属于事件源）。
async fn read_sse(
    mut response: HttpResponse,
    decoder: &mut SseDecoder,
    events: &Events,
    mut endpoint: Option<oneshot::Sender<String>>,
    mut seen: impl FnMut(&Value),
) -> Result<(), String> {
    loop {
        let chunk = match response.chunk().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => return Ok(()),
            Err(error) => return Err(error.to_string()),
        };
        let found = decoder.push(&chunk).map_err(|error| error.to_string())?;
        for event in found {
            match event.event.as_deref() {
                Some("endpoint") => {
                    if let Some(sender) = endpoint.take() {
                        let _ = sender.send(event.data);
                    }
                }
                // 续读的 priming 事件（只有 ID 与空的 data）不是消息。
                None | Some("message") if event.data.is_empty() => {}
                None | Some("message") => match serde_json::from_str::<Value>(&event.data) {
                    Ok(value) => {
                        for message in messages(value) {
                            seen(&message);
                            // 通道满时等（背压）；读者已经释放事件流时不再读这个流。
                            if events
                                .send(McpTransportEvent::Message { message })
                                .await
                                .is_err()
                            {
                                return Ok(());
                            }
                        }
                    }
                    Err(error) => tracing::debug!(%error, "ignored a non-JSON MCP SSE event"),
                },
                Some(other) => tracing::debug!(event = other, "ignored an MCP SSE event"),
            }
        }
    }
}

/// 跟着读 streamable HTTP 的一个 SSE 流（POST 的响应流或 GET 监听流；SEP-1699，ext.md §3.10.6）。流在 `awaiting` 的回复
/// 之前结束、且给过事件 ID 时，等服务器给的 `retry`（没给时 [`RECONNECTION_TIME`]），带 `Last-Event-ID` 再 GET 接着读；
/// 续读的流又空着断开（服务器在轮询）时照样再续读。回复到了、读者释放了事件流时停；没有事件 ID、续读的 GET 失败时停，
/// 并对 `awaiting` 送出 `ReplyLost`（监听流没有等回复的请求，只记 debug）；连接关闭、请求被取消时任务被中止。不另设重试
/// 次数：等多久、续到什么时候都由服务器定。
///
/// 缺陷修复（v5r-mcp2 线）：以前流读完就结束，服务器在回复之前断开时请求一直等到期限（工具调用缺省约 27.8 小时），
/// 所以 streamable HTTP 只协商到 `2025-06-18`。
///
/// 缺陷修复（v5r-mcp3 线，ext.md §3.10.6 的“续不上时”）：续不上时以前只停止跟读，请求照样等到期限。这里明确知道回复
/// 不会来了，是真实的传输错误：经端口的 `ReplyLost` 告诉 ext，请求立即失败。不合成服务器的错误回复（那是伪造服务器的
/// 消息）。`ReplyLost` 与这个流的消息走同一个通道、排在它们后面，回复先到时不送。
async fn follow(
    endpoint: Arc<Endpoint>,
    events: Events,
    mut response: HttpResponse,
    awaiting: Option<Value>,
) {
    let mut decoder = SseDecoder::new();
    let mut answered = false;
    let lost = loop {
        let seen = |message: &Value| {
            endpoint.observe(message);
            answered |= awaiting.is_some() && reply_id(message) == awaiting.as_ref();
        };
        if let Err(error) = read_sse(response, &mut decoder, &events, None, seen).await {
            tracing::debug!(error, "MCP stream broke");
        }
        if answered || events.is_closed() {
            return;
        }
        let Some(last) = decoder.last_event_id().map(str::to_owned) else {
            break ENDED_WITHOUT_EVENT_ID.to_owned();
        };
        tokio::time::sleep(decoder.retry().unwrap_or(RECONNECTION_TIME)).await;
        let request = endpoint.request(
            HttpRequest::get(&endpoint.url)
                .header(ACCEPT, EVENT_STREAM)
                .header(LAST_EVENT_ID_HEADER, last),
        );
        response = match endpoint.send(request).await {
            Ok(resumed) if resumed.is_success() => resumed,
            Ok(refused) if endpoint.expired(refused.status()) => {
                let _ = events
                    .send(McpTransportEvent::Closed {
                        reason: Some(SESSION_EXPIRED.to_owned()),
                        session_expired: true,
                    })
                    .await;
                return;
            }
            // WHATWG 的事件源在非 200 时也不再连。原因同 POST 出错时的写法（状态与响应体）。
            Ok(refused) => {
                let status = refused.status();
                let body = refused.text(ERROR_BODY_LIMIT).await.unwrap_or_default();
                break format!("{NOT_RESUMED}: HTTP {status}: {body}");
            }
            Err(error) => break format!("{NOT_RESUMED}: {error}"),
        };
        decoder.restart();
    };
    match awaiting {
        Some(id) => {
            let _ = events
                .send(McpTransportEvent::ReplyLost { id, reason: lost })
                .await;
        }
        None => tracing::debug!(reason = lost, "MCP listener stream stopped"),
    }
}

impl HttpLink {
    /// 登记一个后台任务；`request` 是它在等回复的请求（POST 的响应流）。
    fn keep(&self, request: Option<Value>, task: OwnedTask<()>) {
        let mut readers = self.readers.lock_unpoisoned();
        readers.retain(|(_, reader)| !reader.is_finished());
        readers.push((request, task));
    }

    /// 在后台按顺序送出这些事件（通道满时等，不卡住 `send` 的调用方）。
    fn deliver(&self, batch: Vec<McpTransportEvent>) {
        let events = self.events.clone();
        self.keep(
            None,
            rust_cli_base::owned(async move {
                for event in batch {
                    if events.send(event).await.is_err() {
                        return;
                    }
                }
            }),
        );
    }

    /// 请求 `request` 的回复不会来了（POST 的响应里没有它）：在后台送出 `ReplyLost`。不是请求（通知、对服务器请求的
    /// 回答）时什么也不做：对它们回 202 是规范要求的。
    fn lose(&self, request: Option<Value>, reason: String) {
        if let Some(id) = request {
            self.deliver(vec![McpTransportEvent::ReplyLost { id, reason }]);
        }
    }

    /// 收服务器主动发来的消息（streamable 的 GET 流）。405 表示服务器不提供，静默跳过。
    async fn open_listener(&self) {
        let endpoint = &self.endpoint;
        let request =
            endpoint.request(HttpRequest::get(&endpoint.url).header(ACCEPT, EVENT_STREAM));
        match endpoint.send(request).await {
            Ok(response) if response.is_success() => {
                let task = follow(endpoint.clone(), self.events.clone(), response, None);
                self.keep(None, rust_cli_base::owned(task));
            }
            Ok(response) => {
                tracing::debug!(
                    status = response.status(),
                    "MCP server has no listener stream"
                );
            }
            Err(error) => tracing::debug!(%error, "MCP listener stream failed"),
        }
    }

    pub(crate) async fn send(&self, message: Value) -> Result<(), McpError> {
        if self.closed.is_cancelled() {
            return Err(McpError::Closed {
                message: "connection closed".to_owned(),
            });
        }
        let method = message.get("method").and_then(Value::as_str);
        let initialized = method == Some(INITIALIZED);
        let endpoint = &self.endpoint;
        if self.streamable && method == Some(INITIALIZE) {
            endpoint.revision.lock_unpoisoned().initialize = message.get("id").cloned();
        }
        if let Some(cancelled) = message
            .pointer("/params/requestId")
            .filter(|_| method == Some(CANCELLED))
        {
            // 取消的请求不再要回复：中止它的流，不再读、不再续读（ext.md §3.10.6；有了续读，不停的话服务器断流时会一直
            // 续读下去）。
            self.readers
                .lock_unpoisoned()
                .retain(|(request, _)| request.as_ref() != Some(cancelled));
        }
        let request = endpoint.request(
            HttpRequest::post(&endpoint.url, message.to_string())
                .header(CONTENT_TYPE, JSON)
                .header(ACCEPT, "application/json, text/event-stream"),
        );
        let response = endpoint.send(request).await?;
        let status = response.status();
        if let Some(session) = response.headers().get(SESSION_HEADER) {
            *endpoint.session.lock_unpoisoned() = Some(session.to_owned());
        }
        if self.streamable && endpoint.expired(status) {
            self.deliver(vec![McpTransportEvent::Closed {
                reason: Some(SESSION_EXPIRED.to_owned()),
                session_expired: true,
            }]);
            return Err(McpError::Closed {
                message: SESSION_EXPIRED.to_owned(),
            });
        }
        if !response.is_success() {
            let body = response.text(ERROR_BODY_LIMIT).await.unwrap_or_default();
            return Err(failed(format!("HTTP {status}: {body}")));
        }
        if initialized && self.streamable {
            self.open_listener().await;
        }
        if !self.streamable {
            return Ok(());
        }
        // 本客户端发出的请求（有 `method` 与 `id`）。规范要它的回复在这个 POST 的响应里：JSON 体，或 SSE 流与它的续读
        // （监听流上 MUST NOT 发回复）。
        //
        // 缺陷修复（v5r-mcp4 线，ext.md §3.10.6 的“POST 的响应里没有回复”）：对请求回 202、内容类型既不是 JSON 也不是
        // SSE、JSON 体里没有回复时，以前什么也不送，请求等到它自己的期限（工具调用缺省约 27.8 小时）。这里已经确定回复
        // 不会来了，同“续不上时”送 `ReplyLost`。
        let request = message.get("id").filter(|_| method.is_some()).cloned();
        if status == http::ACCEPTED {
            self.lose(request, ACCEPTED_REQUEST.to_owned());
            return Ok(());
        }
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .unwrap_or_default()
            .to_ascii_lowercase();
        if content_type.starts_with(EVENT_STREAM) {
            let task = follow(
                endpoint.clone(),
                self.events.clone(),
                response,
                request.clone(),
            );
            self.keep(request, rust_cli_base::owned(task));
        } else if content_type.starts_with(JSON) {
            let body = response
                .text(BODY_LIMIT)
                .await
                .map_err(|error| failed(error.to_string()))?;
            let value: Value = serde_json::from_str(&body)
                .map_err(|error| failed(format!("invalid JSON from the MCP server: {error}")))?;
            let messages = messages(value);
            for message in &messages {
                endpoint.observe(message);
            }
            let lost =
                request.filter(|id| !messages.iter().any(|message| reply_id(message) == Some(id)));
            let mut batch: Vec<_> = messages
                .into_iter()
                .map(|message| McpTransportEvent::Message { message })
                .collect();
            // 排在 JSON 体里的消息后面（同一个后台任务、同一个通道）。
            batch.extend(lost.map(|id| McpTransportEvent::ReplyLost {
                id,
                reason: JSON_WITHOUT_REPLY.to_owned(),
            }));
            self.deliver(batch);
        } else {
            let shown = if content_type.is_empty() {
                NO_CONTENT_TYPE
            } else {
                content_type.as_str()
            };
            self.lose(request, format!("{UNSUPPORTED_CONTENT_TYPE}: {shown}"));
        }
        Ok(())
    }

    pub(crate) async fn close(&self) {
        self.closed.cancel();
        self.readers.lock_unpoisoned().clear();
    }
}

/// 连接任务：等关闭，送出 `Closed`。
async fn until_closed(closed: CancellationToken, events: Events) {
    closed.cancelled().await;
    let _ = events
        .send(McpTransportEvent::Closed {
            reason: None,
            session_expired: false,
        })
        .await;
}

/// streamable HTTP：建连不发请求（第一条消息是 `initialize`）。
pub(crate) fn streamable(target: Target, events: Events) -> (Arc<HttpLink>, OwnedTask<()>) {
    let closed = CancellationToken::new();
    let url = target.url.clone();
    let link = Arc::new(HttpLink {
        endpoint: Arc::new(Endpoint::new(target, url)),
        streamable: true,
        events: events.clone(),
        readers: Mutex::default(),
        closed: closed.clone(),
    });
    (link, rust_cli_base::owned(until_closed(closed, events)))
}

/// SSE：GET 事件流，等 `endpoint` 事件；连接任务读这个流直到结束。
pub(crate) async fn sse(
    target: Target,
    events: Events,
) -> Result<(Arc<HttpLink>, OwnedTask<()>), McpError> {
    let url = target.url.clone();
    let request =
        with_headers(HttpRequest::get(&url), &target.headers).header(ACCEPT, EVENT_STREAM);
    // 事件流的 GET 与之后的 POST 用同一个令牌来源（401 时同样换一个重发一次）。
    let opening = Endpoint::new(target, url.clone());
    let response = opening.send(request).await?;
    if !response.is_success() {
        let status = response.status();
        let body = response.text(ERROR_BODY_LIMIT).await.unwrap_or_default();
        return Err(failed(format!("HTTP {status}: {body}")));
    }
    let (endpoint, found) = oneshot::channel();
    let closed = CancellationToken::new();
    let stream_events = events.clone();
    let stop = closed.clone();
    let task = rust_cli_base::owned(async move {
        let mut decoder = SseDecoder::new();
        let reason = tokio::select! {
            read = read_sse(response, &mut decoder, &stream_events, Some(endpoint), |_| {}) => {
                Some(read.err().unwrap_or_else(|| "MCP SSE stream ended".to_owned()))
            }
            () = stop.cancelled() => None,
        };
        let _ = stream_events
            .send(McpTransportEvent::Closed {
                reason,
                session_expired: false,
            })
            .await;
    });
    let endpoint = found
        .await
        .map_err(|_| failed("the SSE stream ended before the endpoint event".to_owned()))?;
    let base = url::Url::parse(&url)
        .map_err(|error| failed(format!("invalid MCP endpoint {endpoint}: {error}")))?;
    let post_url = base
        .join(&endpoint)
        .map_err(|error| failed(format!("invalid MCP endpoint {endpoint}: {error}")))?;
    // 缺陷修复 R2-5：`endpoint` 原来可以指向别的 origin，配置的 header（常有 Authorization）会跟着 POST 过去。
    // 按 MCP SDK `SSEClientTransport`：origin 不同时连接失败（文字相同），事件流随连接任务一起结束。
    if post_url.origin() != base.origin() {
        return Err(failed(format!(
            "Endpoint origin does not match connection origin: {}",
            post_url.origin().ascii_serialization()
        )));
    }
    let link = Arc::new(HttpLink {
        endpoint: Arc::new(Endpoint {
            url: post_url.to_string(),
            ..opening
        }),
        streamable: false,
        events,
        readers: Mutex::default(),
        closed,
    });
    Ok((link, task))
}

#[cfg(test)]
#[path = "http_tests.rs"]
mod tests;
