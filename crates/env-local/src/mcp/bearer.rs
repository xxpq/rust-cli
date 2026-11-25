//! 用 OAuth 的服务器的请求（docs/specs/rust-cli/ext.md §3.12.3）：带 Bearer；401 之后经令牌端口换一个、重发一次；换不了、
//! 重发仍 401 时报“要授权”；403 带 `insufficient_scope` 时报“要更多权限”（带它要的 scope）。
//!
//! 令牌只经端口取，这里不存：事实源是令牌端口背后的凭据文件（与 Node 共用），每个请求取一次。

use std::sync::Arc;

use rust_cli_base::proto::http;
use rust_cli_base::secret::RedactedString;
use rust_cli_kernel_api::mcp::McpError;
use rust_cli_kernel_api::mcp_auth::{McpAuthTarget, McpTokens};
use rust_cli_net::header::{AUTHORIZATION, WWW_AUTHENTICATE, bearer};
use rust_cli_net::{Egress, HeaderValue, HttpRequest, HttpResponse};

/// 401 且令牌换不了时的原因（只进日志与状态；给模型的文字由 ext 写，ext.md §3.12.3）。
const UNAUTHORIZED: &str = "the MCP server needs authorization (HTTP 401)";
/// 403 要更多权限时的原因的开头（后接 scope）。
const INSUFFICIENT_SCOPE: &str = "the MCP server needs more permissions (HTTP 403)";
/// `WWW-Authenticate` 的方案（RFC 6750）。
const BEARER_SCHEME: &str = "bearer";
/// 403 时要更多权限的错误码（RFC 6750 §3.1）。
const INSUFFICIENT_SCOPE_ERROR: &str = "insufficient_scope";

/// 一个用 OAuth 的服务器的令牌来源。没有令牌端口时（执行器、不装它的组装）不带令牌，401 直接是“要授权”。
pub(crate) struct Bearer {
    target: McpAuthTarget,
    tokens: Option<Arc<dyn McpTokens>>,
}

impl Bearer {
    pub(crate) fn new(target: McpAuthTarget, tokens: Option<Arc<dyn McpTokens>>) -> Self {
        Self { target, tokens }
    }

    /// 发一个请求：带上现在的令牌；401 时换一个、重发一次。
    pub(crate) async fn send(
        &self,
        egress: &Egress,
        request: HttpRequest,
    ) -> Result<HttpResponse, McpError> {
        let token = match &self.tokens {
            Some(tokens) => tokens.token(&self.target).await?,
            None => None,
        };
        let response = send(egress, authorized(request.clone(), token.as_ref())).await?;
        if response.status() != http::UNAUTHORIZED {
            return scoped(response);
        }
        let Some(tokens) = &self.tokens else {
            return Err(unauthorized());
        };
        let fresh = tokens.refreshed(&self.target, token.as_ref()).await?;
        let retried = send(egress, authorized(request, Some(&fresh))).await?;
        if retried.status() == http::UNAUTHORIZED {
            return Err(unauthorized());
        }
        scoped(retried)
    }
}

async fn send(egress: &Egress, request: HttpRequest) -> Result<HttpResponse, McpError> {
    egress
        .send(request)
        .await
        .map_err(|error| McpError::Connect {
            message: error.to_string(),
        })
}

fn authorized(request: HttpRequest, token: Option<&RedactedString>) -> HttpRequest {
    match token {
        Some(token) => request.header(AUTHORIZATION, HeaderValue::Secret(bearer(token.expose()))),
        None => request,
    }
}

fn unauthorized() -> McpError {
    McpError::AuthRequired {
        message: UNAUTHORIZED.to_owned(),
        scope: None,
    }
}

/// 403 带 `insufficient_scope`：要更多权限（step-up）。别的响应原样交回。
fn scoped(response: HttpResponse) -> Result<HttpResponse, McpError> {
    if response.status() != http::FORBIDDEN {
        return Ok(response);
    }
    let challenge = response.headers().get(WWW_AUTHENTICATE).unwrap_or_default();
    if challenge_param(challenge, "error") != Some(INSUFFICIENT_SCOPE_ERROR) {
        return Ok(response);
    }
    let scope = challenge_param(challenge, "scope").map(str::to_owned);
    Err(McpError::AuthRequired {
        message: match &scope {
            Some(scope) => format!("{INSUFFICIENT_SCOPE} (scope: {scope})"),
            None => INSUFFICIENT_SCOPE.to_owned(),
        },
        scope,
    })
}

/// `WWW-Authenticate: Bearer k="v", k2=v2` 里的一个参数（TS SDK `extractFieldFromWwwAuth` 的读法：方案是 Bearer；值是引号里的
/// 一段，或到空白与逗号为止）。
pub(crate) fn challenge_param<'a>(challenge: &'a str, name: &str) -> Option<&'a str> {
    let (scheme, params) = challenge.trim_start().split_once(' ')?;
    if !scheme.eq_ignore_ascii_case(BEARER_SCHEME) {
        return None;
    }
    let mut rest = params;
    while let Some(at) = rest.find(name) {
        let after = &rest[at + name.len()..];
        // 参数名前面是开头、空白或逗号，后面紧跟 `=`。
        let boundary = rest[..at]
            .chars()
            .last()
            .is_none_or(|c| c.is_whitespace() || c == ',');
        if let (true, Some(value)) = (boundary, after.strip_prefix('=')) {
            let value = match value.strip_prefix('"') {
                Some(quoted) => &quoted[..quoted.find('"')?],
                None => value
                    .split(|c: char| c.is_whitespace() || c == ',')
                    .next()
                    .unwrap_or_default(),
            };
            return Some(value).filter(|value| !value.is_empty());
        }
        rest = after;
    }
    None
}

#[cfg(test)]
#[path = "bearer_tests.rs"]
mod tests;
