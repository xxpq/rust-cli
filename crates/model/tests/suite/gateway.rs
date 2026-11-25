//! ZCode 套餐网关与智谱的码（docs/specs/rust-cli/provider-errors.md §5.3.1、§5.3.2，主会话审定 2026-11-06）：1006 登录失效、
//! 3008–3010 系统繁忙、错误体里的 `network_error` 码。夹具在 127.0.0.1，按只给测试的声明当成智谱的地址。

#![allow(
    clippy::unwrap_used,
    reason = "辅助函数与替身不在 #[test] 函数里，clippy 的测试放行不覆盖它；失败即测试失败"
)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures_util::StreamExt;
use rust_cli_base::secret::RedactedString;
use rust_cli_base::{hosts, providers};
use rust_cli_kernel_api::model::{
    ModelError, ModelNotice, ModelPort, ModelRequest, ModelStreamItem, QuerySource,
};
use rust_cli_kernel_api::provider::{
    Credential, EndpointAccess, EndpointQuery, ModelCapabilities, ModelEndpoint, ModelEndpoints,
    WireProtocol,
};
use rust_cli_kernel_api::side_request::SideSource;
use rust_cli_model::{ProviderModel, RetryPolicy, RouteOptions};
use rust_cli_net::{Egress, NetworkPolicy};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::common::{clock, fast_retry, model_with, request, text_response};
use super::fixture::{Fixture, Reply};

fn options(retry: RetryPolicy) -> RouteOptions {
    RouteOptions {
        retry,
        assume_base_url: Some(hosts::BIGMODEL_ANTHROPIC.to_owned()),
        ..RouteOptions::default()
    }
}

/// 固定端点（API key，不能续期）。
async fn fixed(server: &Fixture, retry: RetryPolicy) -> ProviderModel {
    model_with(server.url(), options(retry)).await
}

/// 账号端点的替身：每次取端点给出下一个令牌（能续期）。
struct Renewing {
    base_url: String,
    issued: Mutex<u32>,
}

#[async_trait]
impl ModelEndpoints for Renewing {
    async fn endpoint(&self, _query: EndpointQuery<'_>) -> Result<ModelEndpoint, ModelError> {
        let mut issued = self.issued.lock().unwrap();
        *issued += 1;
        Ok(ModelEndpoint {
            protocol: WireProtocol::AnthropicMessages,
            base_url: self.base_url.clone(),
            model: "glm".to_owned(),
            credential: Credential::ApiKey(RedactedString::new(format!("token-{issued}"))),
            headers: Vec::new(),
            body_patches: Vec::new(),
            capabilities: ModelCapabilities::default(),
            renewable: true,
            binding: None,
            single_use: false,
            access: EndpointAccess::default(),
        })
    }
}

async fn renewing(server: &Fixture) -> ProviderModel {
    let egress = Arc::new(Egress::new(NetworkPolicy::default()).await.unwrap());
    let endpoints = Arc::new(Renewing {
        base_url: server.url(),
        issued: Mutex::new(0),
    });
    ProviderModel::new(egress, endpoints, clock(), options(fast_retry(3)))
}

fn coded(status: u16, code: &str, message: &str) -> Reply {
    Reply::new(status)
        .header("content-type", "application/json")
        .send(json!({"error": {"code": code, "message": message}}).to_string())
}

/// HTTP 200 的流里不带事件名的错误块（智谱的码这样到达）。
fn coded_frame(code: &str, message: &str) -> Reply {
    let frame = json!({"error": {"code": code, "message": message}});
    Reply::new(200)
        .header("content-type", "text/event-stream")
        .send(format!("data: {frame}\n\n"))
}

fn ok() -> Reply {
    Reply::new(200)
        .header("content-type", "text/event-stream")
        .send(text_response(&["ok"]))
}

/// 一次调用的形状：重试通知写成 `retrying(<等待毫秒>)`，事件写成 `event`，错误写成 `error(<错误>)`。
async fn shape_of(model: &ProviderModel, request: ModelRequest) -> Vec<String> {
    let items: Vec<_> = model
        .stream_with_notices(request, CancellationToken::new())
        .collect()
        .await;
    items
        .iter()
        .filter_map(|item| match item {
            Ok(ModelStreamItem::Notice(ModelNotice::Retrying { delay_ms, .. })) => {
                Some(format!("retrying({delay_ms})"))
            }
            Ok(ModelStreamItem::Notice(_)) => None,
            Ok(ModelStreamItem::Event(_)) => Some("event".to_owned()),
            Err(error) => Some(format!("error({error})")),
        })
        .collect()
}

fn tokens(server: &Fixture) -> Vec<String> {
    server
        .requests()
        .iter()
        .map(|request| request.header("x-api-key").unwrap_or_default().to_owned())
        .collect()
}

/// 1006（登录状态失效）在不能续期的端点上：照 Node 不重试，直接结束。以前按 401 退避重试到上限。
#[tokio::test]
async fn login_expired_on_a_fixed_endpoint_ends_at_once() {
    let server = Fixture::start(vec![coded(401, "1006", "登录状态已失效"), ok()]).await;
    let model = fixed(&server, fast_retry(3)).await;
    let shape = shape_of(&model, request("hi", QuerySource::MainThread)).await;
    assert_eq!(shape, ["error(authentication failed: 登录状态已失效)"]);
    assert_eq!(server.requests().len(), 1);
}

/// 1006 在能续期的端点上：续期一次再发；续期之后仍是 1006 时结束。以前每次都续期、重试到上限。
#[tokio::test]
async fn login_expired_on_a_renewable_endpoint_is_renewed_once() {
    let server = Fixture::start(vec![
        coded(401, "1006", "登录状态已失效"),
        coded(401, "1006", "登录状态已失效"),
        ok(),
    ])
    .await;
    let model = renewing(&server).await;
    let shape = shape_of(&model, request("hi", QuerySource::MainThread)).await;
    assert_eq!(
        shape.last().unwrap(),
        "error(authentication failed: 登录状态已失效)"
    );
    assert_eq!(tokens(&server), ["token-1", "token-2"]);
}

/// 1006 多在 HTTP 200 的流里到达：同样续期一次再发。以前流里的凭据被拒直接结束，不续期。
#[tokio::test]
async fn login_expired_in_the_stream_is_renewed_once() {
    let server = Fixture::start(vec![coded_frame("1006", "登录状态已失效"), ok()]).await;
    let model = renewing(&server).await;
    let shape = shape_of(&model, request("hi", QuerySource::MainThread)).await;
    assert_eq!(shape.last().unwrap(), "event", "{shape:?}");
    assert_eq!(tokens(&server), ["token-1", "token-2"]);
}

/// 错误体里的码是 `network_error` / `network_error_retryable`：网络失败，照常重试（响应头之前与流里都是）。以前按 HTTP 状态
/// 归参数错误，或在流里当作不认识的块。
#[tokio::test]
async fn a_network_error_code_is_retried() {
    let server = Fixture::start(vec![coded(400, "network_error", "upstream reset"), ok()]).await;
    let shape = shape_of(
        &fixed(&server, fast_retry(3)).await,
        request("hi", QuerySource::MainThread),
    )
    .await;
    assert_eq!(shape.last().unwrap(), "event", "{shape:?}");
    assert_eq!(server.requests().len(), 2);

    let server = Fixture::start(vec![coded_frame("NETWORK_ERROR_RETRYABLE", "reset"), ok()]).await;
    let shape = shape_of(
        &fixed(&server, fast_retry(3)).await,
        request("hi", QuerySource::MainThread),
    )
    .await;
    assert_eq!(shape.last().unwrap(), "event", "{shape:?}");
    assert_eq!(server.requests().len(), 2);
}

fn start_plan(source: QuerySource, prior_turns: bool) -> ModelRequest {
    let mut request = request("hi", source);
    request.selection.provider = providers::BIGMODEL_START_PLAN.to_owned();
    request.options.prior_turns = prior_turns;
    request
}

/// 3008–3010（系统繁忙）在普通请求上不重试：会话的第一轮、标题请求、不是 Start Plan 的账号（照 Node：适配器层
/// `retryable: false`）。以前一律重试 2 次。
#[tokio::test]
async fn system_busy_is_not_retried_on_ordinary_requests() {
    let mut coding_plan = start_plan(QuerySource::MainThread, true);
    coding_plan.selection.provider = "account:bigmodel-individual-coding-plan".to_owned();
    for request in [
        start_plan(QuerySource::MainThread, false),
        start_plan(QuerySource::Side(SideSource::SessionTitle), true),
        start_plan(QuerySource::Compact, true),
        coding_plan,
    ] {
        let server = Fixture::start(vec![coded(429, "3008", "当前系统繁忙"), ok()]).await;
        let shape = shape_of(&fixed(&server, fast_retry(3)).await, request).await;
        assert_eq!(shape, ["error(rate limited: 当前系统繁忙)"]);
        assert_eq!(server.requests().len(), 1);
    }
}

/// Start Plan 主回合的准入（会话已经完成过回合）：固定等 1 s、2 s 重发，最多 2 次；不受重试次数的上限限制（Node 的准入
/// 重试在 core，不在适配器的预算里）。
#[tokio::test]
async fn a_start_plan_admission_after_an_earlier_turn_retries_twice() {
    let server = Fixture::start(vec![
        coded(429, "3008", "当前系统繁忙"),
        coded_frame("3010", "当前系统繁忙"),
        ok(),
    ])
    .await;
    let model = fixed(&server, fast_retry(0)).await;
    let shape = shape_of(&model, start_plan(QuerySource::MainThread, true)).await;
    assert_eq!(
        shape[..2],
        ["retrying(1000)", "retrying(2000)"],
        "{shape:?}"
    );
    assert_eq!(shape.last().unwrap(), "event", "{shape:?}");
    assert_eq!(server.requests().len(), 3);

    let busy = || coded(429, "3009", "当前系统繁忙");
    let server = Fixture::start(vec![busy(), busy(), busy(), ok()]).await;
    let model = fixed(&server, fast_retry(3)).await;
    let shape = shape_of(&model, start_plan(QuerySource::MainThread, true)).await;
    assert_eq!(
        shape,
        [
            "retrying(1000)",
            "retrying(2000)",
            "error(rate limited: 当前系统繁忙)"
        ]
    );
    assert_eq!(server.requests().len(), 3);
}
