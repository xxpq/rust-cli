//! OAuth 的 HTTP（docs/specs/rust-cli/ext.md §3.12.12）：`oauth2` 的请求经它的 `AsyncHttpClient` 接口交给 net 的 [`Egress`]
//! （代理、根证书与模型请求相同，不另起 HTTP 栈）；发现与注册的几个 JSON 请求也经这里。
use std::error::Error;
use std::fmt;

use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use oauth2::http;
use rust_cli_base::proto::http::header::AUTHORIZATION;
use rust_cli_base::proto::mime::JSON;
use rust_cli_base::secret::RedactedString;
use rust_cli_base::units;
use rust_cli_net::header::{ACCEPT, CONTENT_TYPE};
use rust_cli_net::{Egress, HeaderValue, HttpRequest, HttpResponse};
use serde_json::Value;

/// 授权服务器的回复最多读这么多（元数据、注册与令牌的 JSON 都很小；同 MCP 官方 SDK 的上限）。
const BODY_LIMIT: usize = units::MIB;
/// 发现时带的协议修订头（TS SDK `MCP-Protocol-Version`；本客户端的修订，ext.md §3.10.2）。
const PROTOCOL_VERSION_HEADER: &str = "mcp-protocol-version";
pub(super) const PROTOCOL_VERSION: &str = "2025-11-25";

/// 请求没发出去或回复读不出（网络错误，不是授权服务器的回答）。
#[derive(Debug)]
pub(super) struct HttpFailure(pub String);

impl fmt::Display for HttpFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Error for HttpFailure {}

/// `oauth2` 的 HTTP 客户端：每个请求经出口发出，回复整个读进内存（有上限）。
pub(super) struct EgressClient<'a>(pub &'a Egress);

impl<'c> oauth2::AsyncHttpClient<'c> for EgressClient<'_> {
    type Error = HttpFailure;
    type Future = BoxFuture<'c, Result<oauth2::HttpResponse, HttpFailure>>;

    fn call(&'c self, request: oauth2::HttpRequest) -> Self::Future {
        async move {
            let request = outgoing(request)?;
            let response = self
                .0
                .send(request)
                .await
                .map_err(|error| HttpFailure(error.to_string()))?;
            incoming(response).await
        }
        .boxed()
    }
}

/// `http::Request` → net 的请求。`oauth2` 只发 GET 与 POST；`Authorization`（client 的 Basic 认证）是秘密值。
fn outgoing(request: oauth2::HttpRequest) -> Result<HttpRequest, HttpFailure> {
    let (parts, body) = request.into_parts();
    let url = parts.uri.to_string();
    let mut out = match parts.method {
        http::Method::GET => HttpRequest::get(url),
        http::Method::POST => HttpRequest::post(url, body),
        other => {
            return Err(HttpFailure(format!(
                "unsupported OAuth request method {other}"
            )));
        }
    };
    for (name, value) in &parts.headers {
        let value = value
            .to_str()
            .map_err(|_| HttpFailure(format!("header {name} is not text")))?;
        let value = if name.as_str() == AUTHORIZATION {
            HeaderValue::Secret(RedactedString::new(value))
        } else {
            HeaderValue::Plain(value.to_owned())
        };
        out = out.header(name.as_str(), value);
    }
    Ok(out)
}

async fn incoming(mut response: HttpResponse) -> Result<oauth2::HttpResponse, HttpFailure> {
    let mut builder = http::Response::builder().status(response.status());
    for (name, value) in response.headers().pairs() {
        builder = builder.header(name, value);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| HttpFailure(error.to_string()))?
    {
        if body.len() + chunk.len() > BODY_LIMIT {
            return Err(HttpFailure(format!(
                "the OAuth response is larger than {BODY_LIMIT} bytes"
            )));
        }
        body.extend_from_slice(&chunk);
    }
    builder
        .body(body)
        .map_err(|error| HttpFailure(error.to_string()))
}

/// 一个 JSON 请求的结果：状态与正文（不是 JSON 时 None）。
pub(super) struct JsonReply {
    pub status: u16,
    pub body: Option<Value>,
    pub text: String,
}

impl JsonReply {
    pub(super) fn ok(&self) -> bool {
        rust_cli_base::proto::http::SUCCESS.contains(&self.status)
    }
}

/// GET 一个元数据文档（TS SDK 的发现：带 `MCP-Protocol-Version` 与 `Accept: application/json`）。
pub(super) async fn get_json(egress: &Egress, url: &str) -> Result<JsonReply, HttpFailure> {
    let request = HttpRequest::get(url)
        .header(PROTOCOL_VERSION_HEADER, PROTOCOL_VERSION)
        .header(ACCEPT, JSON);
    reply(egress, request).await
}

/// POST 一个 JSON 正文（动态注册，RFC 7591）。
pub(super) async fn post_json(
    egress: &Egress,
    url: &str,
    body: &Value,
) -> Result<JsonReply, HttpFailure> {
    let request = HttpRequest::post(url, body.to_string()).header(CONTENT_TYPE, JSON);
    reply(egress, request).await
}

async fn reply(egress: &Egress, request: HttpRequest) -> Result<JsonReply, HttpFailure> {
    let response = egress
        .send(request)
        .await
        .map_err(|error| HttpFailure(error.to_string()))?;
    let status = response.status();
    let text = response
        .text(BODY_LIMIT)
        .await
        .map_err(|error| HttpFailure(error.to_string()))?;
    Ok(JsonReply {
        status,
        body: serde_json::from_str(&text).ok(),
        text,
    })
}
