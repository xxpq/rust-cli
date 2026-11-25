//! 发行版的请求处理（`RequestGuard`，docs/specs/rust-cli/model.md §5.4）：每次物理发送之前改写请求、非 2xx 之后立即再发、
//! 改写失败时不发、换了凭据时按新端点重开。替身只做中性的事：每次发送加一个计数头。

#![allow(
    clippy::unwrap_used,
    reason = "辅助函数与替身不在 #[test] 函数里，clippy 的测试放行不覆盖它；失败即测试失败"
)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures_util::StreamExt;
use rust_cli_base::secret::RedactedString;
use rust_cli_kernel_api::model::{
    ModelError, ModelNotice, ModelPort, ModelStreamItem, QuerySource,
};
use rust_cli_kernel_api::provider::{
    Credential, EndpointAccess, EndpointQuery, ModelCapabilities, ModelEndpoint, ModelEndpoints,
    WireProtocol,
};
use rust_cli_model::{GuardedRequest, ProviderModel, RequestGuard, RouteOptions};
use rust_cli_net::{Egress, HeaderValue, HttpRequest, NetworkPolicy};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::common::{clock, fast_retry, request, text_response};
use super::fixture::{Fixture, Reply};

/// 计数头：替身每次发送写一次，值是这个逻辑请求里第几次发送。
const SEND: &str = "x-guard-send";
/// 替身认的“请立即再发”的正文。
const AGAIN: &str = "again";

/// 记下每次打开时端点的凭据；`fail` 时改写失败。
#[derive(Default)]
struct Counting {
    opened: Mutex<Vec<String>>,
    fail: bool,
}

struct Counter {
    sends: u32,
    fail: bool,
}

impl RequestGuard for Counting {
    fn open(&self, endpoint: &ModelEndpoint) -> Option<Box<dyn GuardedRequest>> {
        let key = match &endpoint.credential {
            Credential::ApiKey(key) => key.expose().to_owned(),
            Credential::None => String::new(),
        };
        self.opened.lock().unwrap().push(key);
        Some(Box::new(Counter {
            sends: 0,
            fail: self.fail,
        }))
    }
}

#[async_trait]
impl GuardedRequest for Counter {
    async fn prepare(&mut self, request: &mut HttpRequest) -> Result<(), ModelError> {
        if self.fail {
            return Err(ModelError::Authentication {
                message: "the guard refused".to_owned(),
            });
        }
        self.sends += 1;
        request
            .headers
            .push((SEND.to_owned(), HeaderValue::Plain(self.sends.to_string())));
        Ok(())
    }

    fn resend(&mut self, status: u16, body: &str) -> bool {
        status == 401 && body.contains(AGAIN)
    }
}

/// 每次取端点给下一个令牌的可续期端点（账号模型的样子）。
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
            model: "m".to_owned(),
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

async fn model(
    endpoints: Arc<dyn ModelEndpoints>,
    guard: Arc<Counting>,
    max_retries: u32,
) -> ProviderModel {
    let egress = Arc::new(Egress::new(NetworkPolicy::default()).await.unwrap());
    ProviderModel::new(
        egress,
        endpoints,
        clock(),
        RouteOptions {
            retry: fast_retry(max_retries),
            guard: Some(guard),
            ..RouteOptions::default()
        },
    )
}

fn renewing(base_url: String) -> Arc<Renewing> {
    Arc::new(Renewing {
        base_url,
        issued: Mutex::new(0),
    })
}

fn error(status: u16, message: &str) -> Reply {
    Reply::new(status).send(
        json!({"type": "error", "error": {"type": "authentication_error", "message": message}})
            .to_string(),
    )
}

async fn items(model: &ProviderModel) -> Vec<Result<ModelStreamItem, ModelError>> {
    model
        .stream_with_notices(
            request("hi", QuerySource::MainThread),
            CancellationToken::new(),
        )
        .collect()
        .await
}

#[tokio::test]
async fn the_guard_rewrites_every_send_including_retries() {
    let server = Fixture::start(vec![
        error(500, "api_error"),
        Reply::new(200).send(text_response(&["ok"])),
    ])
    .await;
    let guard = Arc::new(Counting::default());
    let model = model(renewing(server.url()), guard.clone(), 2).await;
    let items = items(&model).await;
    assert!(items.iter().all(Result::is_ok), "{items:?}");
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].header(SEND), Some("1"));
    assert_eq!(requests[1].header(SEND), Some("2"));
    // 一个逻辑请求开一次；普通重试不重开。
    assert_eq!(*guard.opened.lock().unwrap(), ["token-1"]);
}

#[tokio::test]
async fn a_resend_asked_by_the_guard_is_immediate_and_outside_the_retry_budget() {
    let server = Fixture::start(vec![
        error(401, AGAIN),
        Reply::new(200).send(text_response(&["ok"])),
    ])
    .await;
    let guard = Arc::new(Counting::default());
    // 重试次数是 0：再发不占它。
    let model = model(renewing(server.url()), guard.clone(), 0).await;
    let items = items(&model).await;
    assert!(items.iter().all(Result::is_ok), "{items:?}");
    // 没有重试通知：这次再发不是重试。
    assert!(
        !items.iter().any(|item| matches!(
            item,
            Ok(ModelStreamItem::Notice(ModelNotice::Retrying { .. }))
        )),
        "{items:?}"
    );
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].header(SEND), Some("2"));
    // 同一个凭据：没有续期，没有重开。
    assert_eq!(requests[1].header("x-api-key"), Some("token-1"));
    assert_eq!(*guard.opened.lock().unwrap(), ["token-1"]);
}

#[tokio::test]
async fn a_guard_that_fails_sends_nothing() {
    let server = Fixture::start(vec![Reply::new(200).send(text_response(&["ok"]))]).await;
    let guard = Arc::new(Counting {
        fail: true,
        ..Counting::default()
    });
    let model = model(renewing(server.url()), guard, 3).await;
    let items = items(&model).await;
    assert!(
        matches!(
            items.as_slice(),
            [Err(ModelError::Authentication { message })] if message == "the guard refused"
        ),
        "{items:?}"
    );
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn a_renewed_credential_reopens_the_guard() {
    let server = Fixture::start(vec![
        error(401, "authentication_error"),
        Reply::new(200).send(text_response(&["ok"])),
    ])
    .await;
    let guard = Arc::new(Counting::default());
    let model = model(renewing(server.url()), guard.clone(), 2).await;
    let items = items(&model).await;
    assert!(items.iter().all(Result::is_ok), "{items:?}");
    let requests = server.requests();
    assert_eq!(requests[1].header("x-api-key"), Some("token-2"));
    // 新凭据新开：计数从 1 重新开始。
    assert_eq!(requests[1].header(SEND), Some("1"));
    assert_eq!(*guard.opened.lock().unwrap(), ["token-1", "token-2"]);
}
