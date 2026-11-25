//! 这一轮执行覆盖的单次尝试与退回（docs/specs/rust-cli/model.md §5.2）：带退回规则的
//! 请求首次失败就按规则判，命中交出 `ExecutionFellBack`；凭据引用每次取端点都带上。

#![allow(
    clippy::unwrap_used,
    reason = "辅助函数不在 #[test] 函数里，clippy 的测试放行不覆盖它；失败即测试失败"
)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rust_cli_base::hosts;
use rust_cli_base::models::GLM_5_3_ACCOUNT;
use rust_cli_base::secret::RedactedString;
use rust_cli_kernel_api::model::{ModelError, ModelSelection, QuerySource, RequestOptions};
use rust_cli_kernel_api::provider::{
    Credential, EndpointAccess, EndpointQuery, ModelCapabilities, ModelEndpoint, ModelEndpoints,
    WireProtocol,
};
use rust_cli_kernel_api::turn_execution::{ExecutionFallback, FallbackRule};
use rust_cli_model::{ProviderModel, RouteOptions};
use rust_cli_net::{Egress, NetworkPolicy};
use serde_json::json;

use super::common::{clock, collect, fast_retry, request, text_response};
use super::fixture::{Fixture, Reply};

const CARD: &str = r#"{"kind":"highspeedCard","cardId":"hsc_1"}"#;

/// 取端点时记下引用的端点来源：`url` 为 None 时选择不可用（`InvalidRequest`，同平台层对目录里没有的选择）。
struct Recorded {
    url: Option<String>,
    renewable: bool,
    seen: Mutex<Vec<(Option<String>, bool)>>,
}

#[async_trait]
impl ModelEndpoints for Recorded {
    async fn endpoint(&self, query: EndpointQuery<'_>) -> Result<ModelEndpoint, ModelError> {
        self.seen
            .lock()
            .unwrap()
            .push((query.execution.map(str::to_owned), query.rejected.is_some()));
        let Some(base_url) = &self.url else {
            return Err(ModelError::InvalidRequest {
                message: "not in the catalog".to_owned(),
            });
        };
        Ok(ModelEndpoint {
            protocol: WireProtocol::AnthropicMessages,
            base_url: base_url.clone(),
            model: GLM_5_3_ACCOUNT.to_owned(),
            credential: Credential::ApiKey(RedactedString::new("k")),
            headers: Vec::new(),
            body_patches: Vec::new(),
            capabilities: ModelCapabilities::default(),
            renewable: self.renewable,
            binding: None,
            single_use: false,
            access: EndpointAccess::default(),
        })
    }
}

fn recorded(url: Option<String>, renewable: bool) -> Arc<Recorded> {
    Arc::new(Recorded {
        url,
        renewable,
        seen: Mutex::default(),
    })
}

/// 夹具在本机：按智谱的端点查厂商（D13），业务码才读得出
async fn model(endpoints: Arc<Recorded>) -> ProviderModel {
    let egress = Arc::new(Egress::new(NetworkPolicy::default()).await.unwrap());
    ProviderModel::new(
        egress,
        endpoints,
        clock(),
        RouteOptions {
            retry: fast_retry(3),
            assume_base_url: Some(hosts::BIGMODEL_ANTHROPIC.to_owned()),
            ..RouteOptions::default()
        },
    )
}

fn rules() -> ExecutionFallback {
    ExecutionFallback {
        rules: vec![
            FallbackRule {
                reason: "highspeed_card_expired".to_owned(),
                provider_code: Some("3402".to_owned()),
            },
            FallbackRule {
                reason: "highspeed_request_failed".to_owned(),
                provider_code: None,
            },
        ],
        unavailable: "highspeed_selection_unavailable".to_owned(),
    }
}

fn executing(fallback: Option<ExecutionFallback>) -> rust_cli_kernel_api::model::ModelRequest {
    let mut request = request("hi", QuerySource::MainThread);
    request.selection = ModelSelection::new("account:zai-highspeed-card", GLM_5_3_ACCOUNT);
    request.options = RequestOptions {
        execution: Some(CARD.to_owned()),
        fallback,
        ..RequestOptions::default()
    };
    request
}

fn coded(status: u16, code: &str) -> Reply {
    Reply::new(status)
        .header("content-type", "application/json")
        .send(json!({"error": {"code": code, "message": "card"}}).to_string())
}

fn fell_back(events: &[Result<rust_cli_kernel_api::model::ModelEvent, ModelError>]) -> &str {
    match events {
        [Err(ModelError::ExecutionFellBack { reason, .. })] => reason,
        other => panic!("not a fallback: {other:?}"),
    }
}

/// 加速服务说卡已失效（3402）：第一次就退回，不重试。
#[tokio::test]
async fn a_card_rejection_falls_back_on_the_first_attempt() {
    let server = Fixture::start(vec![
        coded(400, "3402"),
        Reply::new(200).send(text_response(&["ok"])),
    ])
    .await;
    let endpoints = recorded(Some(server.url()), true);
    let events = collect(&model(endpoints.clone()).await, executing(Some(rules()))).await;
    assert_eq!(fell_back(&events), "highspeed_card_expired");
    assert_eq!(server.requests().len(), 1);
    assert_eq!(
        endpoints.seen.lock().unwrap().clone(),
        [(Some(CARD.to_owned()), false)]
    );
}

/// 别的失败（这里是本来会重试的 5xx）走不带码的那条规则，同样只试一次。
#[tokio::test]
async fn any_other_failure_falls_back_without_retrying() {
    let server = Fixture::start(vec![
        Reply::new(503).send("busy"),
        Reply::new(200).send(text_response(&["ok"])),
    ])
    .await;
    let events = collect(
        &model(recorded(Some(server.url()), true)).await,
        executing(Some(rules())),
    )
    .await;
    assert_eq!(fell_back(&events), "highspeed_request_failed");
    assert_eq!(server.requests().len(), 1);
}

/// O1 收回了加速 provider 的权益（目录里没有这个选择）：取端点就失败，原因是 `unavailable`。
#[tokio::test]
async fn unavailable_selection_falls_back() {
    let events = collect(&model(recorded(None, true)).await, executing(Some(rules()))).await;
    assert_eq!(fell_back(&events), "highspeed_selection_unavailable");
}

/// 没有退回规则的执行覆盖（子代理）照常重试与续期，续期时同样带引用。
#[tokio::test]
async fn without_rules_the_request_retries_and_renews_with_the_reference() {
    let server = Fixture::start(vec![
        Reply::new(401)
            .header("content-type", "application/json")
            .send(json!({"type": "error", "error": {"type": "authentication_error", "message": "expired"}}).to_string()),
        Reply::new(200).send(text_response(&["ok"])),
    ])
    .await;
    let endpoints = recorded(Some(server.url()), true);
    let events = collect(&model(endpoints.clone()).await, executing(None)).await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");
    assert_eq!(server.requests().len(), 2);
    assert_eq!(
        endpoints.seen.lock().unwrap().clone(),
        [
            (Some(CARD.to_owned()), false),
            (Some(CARD.to_owned()), true)
        ]
    );
}
