//! 每请求取端点、401 后续期凭据、连续过载后切 fallback 模型、重试通知。

#![allow(
    clippy::unwrap_used,
    reason = "辅助函数与替身不在 #[test] 函数里，clippy 的测试放行不覆盖它；失败即测试失败"
)]

use rust_cli_base::models;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures_util::StreamExt;
use rust_cli_base::secret::RedactedString;
use rust_cli_kernel_api::model::{
    ModelError, ModelNotice, ModelPort, ModelStreamItem, QuerySource,
};
use rust_cli_kernel_api::provider::{
    AccessDenial, Credential, EndpointAccess, EndpointQuery, ModelCapabilities, ModelEndpoint,
    ModelEndpoints, WireProtocol,
};
use rust_cli_model::{ProviderModel, RouteOptions};
use rust_cli_net::{Egress, NetworkPolicy};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::common::{clock, collect, fast_retry, model_at, model_with, request, text_response};
use super::fixture::{Fixture, Reply};

/// 账号模型的替身：每次取端点给出下一个令牌，并记下被拒的令牌。
struct Rotating {
    base_url: String,
    issued: Mutex<u32>,
    rejected: Mutex<Vec<String>>,
    /// 被拒的端点带回的绑定，与 `rejected` 一一对应。
    bindings: Mutex<Vec<Option<String>>>,
    /// 每次取端点时查询里的顶层会话。
    roots: Mutex<Vec<Option<String>>>,
}

#[async_trait]
impl ModelEndpoints for Rotating {
    async fn endpoint(&self, query: EndpointQuery<'_>) -> Result<ModelEndpoint, ModelError> {
        let EndpointQuery {
            selection,
            rejected,
            root,
            ..
        } = query;
        self.roots
            .lock()
            .unwrap()
            .push(root.map(|root| root.as_str().to_owned()));
        if let Some(endpoint) = rejected
            && let Credential::ApiKey(key) = &endpoint.credential
        {
            self.rejected.lock().unwrap().push(key.expose().to_owned());
            self.bindings.lock().unwrap().push(endpoint.binding.clone());
        }
        let mut issued = self.issued.lock().unwrap();
        *issued += 1;
        Ok(ModelEndpoint {
            protocol: WireProtocol::AnthropicMessages,
            base_url: self.base_url.clone(),
            model: format!("{}-api", selection.model),
            credential: Credential::ApiKey(RedactedString::new(format!("token-{issued}"))),
            headers: Vec::new(),
            body_patches: Vec::new(),
            capabilities: ModelCapabilities {
                supports_mid_conversation_system: true,
                supports_image: true,
                ..ModelCapabilities::default()
            },
            renewable: true,
            binding: Some(format!("scope-{issued}")),
            single_use: false,
            access: EndpointAccess::default(),
        })
    }
}

fn error(status: u16, kind: &str) -> Reply {
    Reply::new(status)
        .send(json!({"type": "error", "error": {"type": kind, "message": kind}}).to_string())
}

#[tokio::test]
async fn a_rejected_account_credential_is_renewed_once_per_failure() {
    let server = Fixture::start(vec![
        error(401, "authentication_error"),
        Reply::new(200).send(text_response(&["ok"])),
    ])
    .await;
    let endpoints = Arc::new(rotating(server.url()));
    let egress = Arc::new(Egress::new(NetworkPolicy::default()).await.unwrap());
    let model = ProviderModel::new(
        egress,
        endpoints.clone(),
        clock(),
        RouteOptions {
            retry: fast_retry(2),
            ..RouteOptions::default()
        },
    );
    let events = collect(&model, request("hi", QuerySource::MainThread)).await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");
    let requests = server.requests();
    assert_eq!(requests[0].header("x-api-key"), Some("token-1"));
    assert_eq!(requests[1].header("x-api-key"), Some("token-2"));
    // 请求体按端点给的模型 ID 编码，续期不重新编码。
    assert!(
        requests[1]
            .body
            .starts_with(&format!(r#"{{"model":"{}-api","#, models::GLM_5_3))
    );
    assert_eq!(*endpoints.rejected.lock().unwrap(), ["token-1"]);
}

/// 凭据来自来源的账号模型的替身（docs/specs/rust-cli/model.md §5.2「拒绝访问之后问来源」）：每次取端点给下一个令牌，记下查询里
/// 拒绝访问的事实；`refuse` 时对带着它的查询回失败（来源认不出这次拒绝）。
struct Asking {
    base_url: String,
    issued: Mutex<u32>,
    denials: Mutex<Vec<AccessDenial>>,
    refuse: bool,
    /// 端点的协议与「凭据带着一次性校验」（空回复的厂商规则看这两样）。
    protocol: WireProtocol,
    single_use: bool,
}

/// 来源认不出时给的文字：调用方不该把它交给用户。
const NOT_RECOVERABLE: &str = "the source cannot help with this denial";

#[async_trait]
impl ModelEndpoints for Asking {
    async fn endpoint(&self, query: EndpointQuery<'_>) -> Result<ModelEndpoint, ModelError> {
        if let Some(denied) = query.denied {
            self.denials.lock().unwrap().push(denied.clone());
            if self.refuse {
                return Err(ModelError::Authentication {
                    message: NOT_RECOVERABLE.to_owned(),
                });
            }
        }
        let mut issued = self.issued.lock().unwrap();
        *issued += 1;
        Ok(ModelEndpoint {
            protocol: self.protocol,
            base_url: self.base_url.clone(),
            model: query.selection.model.clone(),
            credential: Credential::ApiKey(RedactedString::new(format!("token-{issued}"))),
            headers: Vec::new(),
            body_patches: Vec::new(),
            capabilities: ModelCapabilities::default(),
            renewable: true,
            binding: None,
            single_use: self.single_use,
            access: EndpointAccess::default(),
        })
    }
}

async fn asking_model(base_url: String, refuse: bool) -> (ProviderModel, Arc<Asking>) {
    let endpoints = Asking {
        base_url,
        issued: Mutex::new(0),
        denials: Mutex::new(Vec::new()),
        refuse,
        protocol: WireProtocol::AnthropicMessages,
        single_use: false,
    };
    asking_with(endpoints, RouteOptions::default()).await
}

async fn asking_with(endpoints: Asking, options: RouteOptions) -> (ProviderModel, Arc<Asking>) {
    let endpoints = Arc::new(endpoints);
    let egress = Arc::new(Egress::new(NetworkPolicy::default()).await.unwrap());
    let model = ProviderModel::new(
        egress,
        endpoints.clone(),
        clock(),
        RouteOptions {
            retry: fast_retry(2),
            ..options
        },
    );
    (model, endpoints)
}

/// OpenAI Chat 的流：一个块之后 `[DONE]`。
fn chat_stream(chunk: serde_json::Value) -> Reply {
    Reply::new(200)
        .header("content-type", "text/event-stream")
        .send(format!("data: {chunk}\n\ndata: [DONE]\n\n"))
}

/// 智谱的 OpenAI Chat 端点上带着一次性校验的请求（夹具在 127.0.0.1，按只给测试的声明当成智谱的地址查厂商）。
async fn checked_chat(base_url: String, single_use: bool) -> (ProviderModel, Arc<Asking>) {
    let endpoints = Asking {
        base_url,
        issued: Mutex::new(0),
        denials: Mutex::new(Vec::new()),
        refuse: false,
        protocol: WireProtocol::OpenAiChatCompletions,
        single_use,
    };
    let options = RouteOptions {
        assume_base_url: Some(rust_cli_base::hosts::BIGMODEL_OPENAI.to_owned()),
        ..RouteOptions::default()
    };
    asking_with(endpoints, options).await
}

/// 修 bug（照 Node，provider-errors.md §3.7）：带着一次性校验的请求得到空回复，是校验失效（3007），走「拒绝访问之后问来源」
/// 重新校验一次；以前按空回复重试，同一个用过的校验再发一次。没带校验的空回复照旧重试一次（Q20），不问来源。
#[tokio::test]
async fn an_empty_reply_to_a_single_use_check_asks_for_a_new_check() {
    let empty = json!({"id": "c1", "model": models::GLM_5_3,
        "choices": [{"index": 0, "delta": {}, "finish_reason": "other"}]});
    let ok = json!({"id": "c2", "model": models::GLM_5_3,
        "choices": [{"index": 0, "delta": {"content": "ok"}, "finish_reason": "stop"}]});
    let server = Fixture::start(vec![chat_stream(empty.clone()), chat_stream(ok.clone())]).await;
    let (model, endpoints) = checked_chat(server.url(), true).await;
    let events = collect(&model, request("hi", QuerySource::MainThread)).await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");
    assert_eq!(
        *endpoints.denials.lock().unwrap(),
        [AccessDenial {
            status: Some(200),
            code: Some("3007".to_owned()),
        }]
    );
    assert_eq!(
        server.requests()[1].header("authorization"),
        Some("Bearer token-2")
    );

    // 只重新校验一次：新的校验换来的还是空回复时，交出拒绝访问
    let server = Fixture::start(vec![chat_stream(empty.clone()), chat_stream(empty.clone())]).await;
    let (model, endpoints) = checked_chat(server.url(), true).await;
    let events = collect(&model, request("hi", QuerySource::MainThread)).await;
    assert!(
        matches!(events.last(), Some(Err(ModelError::Authentication { .. }))),
        "{events:?}"
    );
    assert_eq!(endpoints.denials.lock().unwrap().len(), 1);
    assert_eq!(server.requests().len(), 2);

    let server = Fixture::start(vec![chat_stream(empty), chat_stream(ok)]).await;
    let (model, endpoints) = checked_chat(server.url(), false).await;
    let events = collect(&model, request("hi", QuerySource::MainThread)).await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");
    assert!(endpoints.denials.lock().unwrap().is_empty());
    assert_eq!(server.requests().len(), 2);
}

/// 拒绝访问（不是凭据被拒）：带上事实问来源一次，拿到新的凭据立即重发。
#[tokio::test]
async fn an_access_denial_asks_the_source_once_and_resends_with_its_credential() {
    let server = Fixture::start(vec![
        error(403, "permission_error"),
        Reply::new(200).send(text_response(&["ok"])),
    ])
    .await;
    let (model, endpoints) = asking_model(server.url(), false).await;
    let events = collect(&model, request("hi", QuerySource::MainThread)).await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");
    let requests = server.requests();
    assert_eq!(requests[1].header("x-api-key"), Some("token-2"));
    assert_eq!(
        *endpoints.denials.lock().unwrap(),
        [AccessDenial {
            status: Some(403),
            code: None,
        }]
    );
}

/// 来源认不出这次拒绝：交出原来的拒绝，不重发。
#[tokio::test]
async fn a_source_that_cannot_help_leaves_the_original_denial() {
    let server = Fixture::start(vec![error(403, "permission_error")]).await;
    let (model, endpoints) = asking_model(server.url(), true).await;
    let events = collect(&model, request("hi", QuerySource::MainThread)).await;
    assert!(
        matches!(events.as_slice(), [Err(ModelError::Authentication { message })]
            if message.contains("permission_error") && !message.contains(NOT_RECOVERABLE)),
        "{events:?}"
    );
    assert_eq!(server.requests().len(), 1);
    assert_eq!(endpoints.denials.lock().unwrap().len(), 1);
}

/// 一个请求只问一次：换来的凭据又被拒绝访问，交出这一次的拒绝。
#[tokio::test]
async fn a_second_denial_in_the_same_request_is_not_asked_again() {
    let server = Fixture::start(vec![
        error(403, "permission_error"),
        error(403, "permission_error"),
    ])
    .await;
    let (model, endpoints) = asking_model(server.url(), false).await;
    let events = collect(&model, request("hi", QuerySource::MainThread)).await;
    assert!(
        matches!(events.as_slice(), [Err(ModelError::Authentication { .. })]),
        "{events:?}"
    );
    assert_eq!(server.requests().len(), 2);
    assert_eq!(endpoints.denials.lock().unwrap().len(), 1);
}

fn rotating(base_url: String) -> Rotating {
    Rotating {
        base_url,
        issued: Mutex::new(0),
        rejected: Mutex::new(Vec::new()),
        bindings: Mutex::new(Vec::new()),
        roots: Mutex::new(Vec::new()),
    }
}

/// 来源给的绑定跟着凭据换（docs/specs/rust-cli/platform.md §5.3）：第二次 401 带回的是第二次的绑定；
/// 请求的顶层会话（子代理的请求属于启动它的会话）每次取端点都交给端点。
#[tokio::test]
async fn the_binding_is_renewed_with_the_credential_and_the_root_is_passed_on() {
    let server = Fixture::start(vec![
        error(401, "authentication_error"),
        error(401, "authentication_error"),
        Reply::new(200).send(text_response(&["ok"])),
    ])
    .await;
    let endpoints = Arc::new(rotating(server.url()));
    let egress = Arc::new(Egress::new(NetworkPolicy::default()).await.unwrap());
    let model = ProviderModel::new(
        egress,
        endpoints.clone(),
        clock(),
        RouteOptions {
            retry: fast_retry(3),
            ..RouteOptions::default()
        },
    );
    let mut asked = request("hi", QuerySource::MainThread);
    asked.options.root = Some(rust_cli_kernel_api::ids::SessionId::new("sess_root"));
    let events = collect(&model, asked).await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");
    assert_eq!(*endpoints.rejected.lock().unwrap(), ["token-1", "token-2"]);
    assert_eq!(
        *endpoints.bindings.lock().unwrap(),
        [Some("scope-1".to_owned()), Some("scope-2".to_owned())]
    );
    assert!(
        endpoints
            .roots
            .lock()
            .unwrap()
            .iter()
            .all(|root| root.as_deref() == Some("sess_root"))
    );
}

#[tokio::test]
async fn repeated_overload_switches_to_the_fallback_model() {
    let replies = (0..3).map(|_| error(529, "overloaded_error")).collect();
    let server = Fixture::start(replies).await;
    let model = model_at(server.url(), fast_retry(10)).await;
    let mut with_fallback = request("hi", QuerySource::MainThread);
    with_fallback.options.fallback_model = Some(models::GLM_5_3_FLASH.to_owned());
    let events = collect(&model, with_fallback).await;
    assert_eq!(
        events,
        vec![Err(ModelError::FallbackTriggered {
            original_model: models::GLM_5_3.to_owned(),
            fallback_model: models::GLM_5_3_FLASH.to_owned(),
        })]
    );
    assert_eq!(server.requests().len(), 3);

    // 没有 fallback 时照常按重试上限重试。
    let replies = (0..4).map(|_| error(529, "overloaded_error")).collect();
    let server = Fixture::start(replies).await;
    let model = model_at(server.url(), fast_retry(3)).await;
    let events = collect(&model, request("hi", QuerySource::MainThread)).await;
    assert!(matches!(
        events.as_slice(),
        [Err(ModelError::Overloaded { .. })]
    ));
    assert_eq!(server.requests().len(), 4);
}

/// F10：opus 系列的主模型没有 fallback 时，第 3 次过载后不再重试，报 `Repeated 529 Overloaded
/// errors`（连续过载次数达到上限）；其他系列照常按重试上限重试（上一个测试）。
#[tokio::test]
async fn opus_models_stop_after_three_overloads_without_a_fallback() {
    let replies = (0..4).map(|_| error(529, "overloaded_error")).collect();
    let server = Fixture::start(replies).await;
    let model = model_at(server.url(), fast_retry(10)).await;
    let mut opus = request("hi", QuerySource::MainThread);
    opus.selection.model = models::OPUS_5_5.to_owned();
    let events = collect(&model, opus).await;
    assert_eq!(
        events,
        vec![Err(ModelError::Overloaded {
            message: "Repeated 529 Overloaded errors".to_owned(),
        })]
    );
    assert_eq!(server.requests().len(), 3);
}

/// 设了 `IS_SANDBOX` 时不报 `Repeated 529 Overloaded errors`，照常按重试上限重试
/// （docs/specs/rust-cli/cli-args.md §9）。
#[tokio::test]
async fn a_sandbox_keeps_retrying_overloaded_opus_requests() {
    let replies = (0..4).map(|_| error(529, "overloaded_error")).collect();
    let server = Fixture::start(replies).await;
    let model = model_with(
        server.url(),
        RouteOptions {
            retry: fast_retry(3),
            sandbox: true,
            ..RouteOptions::default()
        },
    )
    .await;
    let mut opus = request("hi", QuerySource::MainThread);
    opus.selection.model = models::OPUS_5_5.to_owned();
    let events = collect(&model, opus).await;
    assert!(matches!(
        events.as_slice(),
        [Err(ModelError::Overloaded { message })] if message != "Repeated 529 Overloaded errors"
    ));
    assert_eq!(server.requests().len(), 4);
}

#[tokio::test]
async fn retries_are_reported_as_notices() {
    let server = Fixture::start(vec![
        error(500, "api_error"),
        Reply::new(200).send(text_response(&["ok"])),
    ])
    .await;
    let model = model_at(server.url(), fast_retry(3)).await;
    let items: Vec<_> = model
        .stream_with_notices(
            request("hi", QuerySource::MainThread),
            CancellationToken::new(),
        )
        .collect()
        .await;
    match items.first() {
        Some(Ok(ModelStreamItem::Notice(ModelNotice::Retrying {
            attempt: 1,
            max_retries: 3,
            error: ModelError::Server { status: 500, .. },
            ..
        }))) => {}
        other => panic!("expected a retry notice, got {other:?}"),
    }
    // 重发拿到响应头时清掉重试通知（L35，stall-visibility.md §2.4）。
    assert!(matches!(
        items.get(1),
        Some(Ok(ModelStreamItem::Notice(ModelNotice::Resumed)))
    ));
    assert!(
        items[2..]
            .iter()
            .all(|item| matches!(item, Ok(ModelStreamItem::Event(_))))
    );
    // `stream` 不带通知。
    let server = Fixture::start(vec![
        error(500, "api_error"),
        Reply::new(200).send(text_response(&["ok"])),
    ])
    .await;
    let model = model_at(server.url(), fast_retry(3)).await;
    let events = collect(&model, request("hi", QuerySource::MainThread)).await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");
}
