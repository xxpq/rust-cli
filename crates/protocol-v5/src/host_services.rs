//! CLI→Host 的 `host.*` 请求（docs/specs/rust-cli/protocol-v5.md §30、§37；docs/zcode-protocol-v5/21-request-auth.md §3.1–§3.3、§12.2）。
//!
//! ```text
//! 发请求的任务（取凭据、会话的通道、凭据的加解密）─ Event::Service{目标, 服务, 参数, reply} ─► 连接的 actor（不等）
//!                                                          ├ 没握手 / Host 没告知这个服务 / 会话不在这个进程里 → reply Err（不发）
//!                                                          ├ 会话 → 工作区；写出 `service` 请求（出站），reply Ok(等待)
//!                                                          └ 进程级（§74.1）：外壳不带 target，写出，reply Ok(等待)
//! 任务等「等待」（读循环把回复直接交给等待，不经 actor）─► 严格链路上按严格变体校验 → 读成生成的类型
//! ```
//!
//! actor 只写不等（§4.1）：回复的等待在发起的任务里；任务被丢掉（回合停了）就是放弃，迟到的回复由读循环丢掉
//! （`host.providers.requestAuth` 不可取消：Host 不等用户，21 §10 第 7 项）。

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use rust_cli_base::secret::RedactedString;
use rust_cli_kernel_api::clock::Clock;
use rust_cli_kernel_api::credential_cipher::{CipherFailure, CredentialCipher};
use rust_cli_kernel_api::env::EnvPath;
use rust_cli_kernel_api::model::ModelError;
use rust_cli_kernel_api::provider::{HeaderRequest, HeaderSource, SuppliedAuth};
use rust_cli_kernel_api::session_preferences::SessionPreferences;
use rust_cli_protocol_core::connection::Pending;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;

use crate::consts::REQUEST_AUTH_LIMIT;
use crate::generated::contract::SERVICE;
use crate::generated::core_v1::{ServiceEnvelope, ServiceEnvelopeTarget};
use crate::generated::session_v1::{
    ServiceHostCredentialsCipherParams, ServiceHostCredentialsCipherParamsOperation,
    ServiceHostCredentialsCipherResult, ServiceHostCredentialsCipherResultFailedReason,
    ServiceHostPreferencesGetParams, ServiceHostPreferencesGetResult,
    ServiceHostPreferencesGetResultToolSearchMode, ServiceHostProvidersRequestAuthParams,
    ServiceHostProvidersRequestAuthParamsDenied, ServiceHostProvidersRequestAuthParamsRejected,
    ServiceHostProvidersRequestAuthResult, ServiceHostProvidersRequestAuthResultHeaders,
    ServiceHostSessionExecutionDirectoriesParams, ServiceHostSessionExecutionDirectoriesResult,
};
use crate::server::{Actor, Event};
use crate::session::schemas;
use crate::session::verify::{Verify, VerifyAnswer};

/// 取账号凭据的服务（`HOST_SERVICES`）。
pub(crate) const REQUEST_AUTH: &str = "host.providers.requestAuth";
/// 会话现在的附加目录（`HOST_SERVICES`，§37）。
const SESSION_DIRECTORIES: &str = "host.session.executionDirectories";
/// App 的会话偏好（`HOST_SERVICES`，§48）。
const PREFERENCES: &str = "host.preferences.get";
/// 凭据值的加解密（`HOST_SERVICES`，进程级，§74）。
const CIPHER: &str = "host.credentials.cipher";
/// Host 报它的凭据库不可用（Main 的系统凭据库）：同 v4 的桥给 CLI 的受控文字。
const HOST_STORAGE_UNAVAILABLE: &str = "System credential storage is unavailable. Unlock the OS credential store and restart the application or CLI to retry.";
/// 取凭据的原因（`reason`）：rust 只发模型请求前的这一种；要求重新校验时由 Host 判定（21 §3.5，R2）。
const MODEL_REQUEST: &str = "model-request";
/// 账号访问事实的 `type`（v4 `ZCodeProviderAccountAccess`；同 rust v4 前端，`entitled` 恒为真：目录里只有授权了的账号模型）。
const ZHIPU_ACCOUNT: &str = "zhipu-account";

/// 给用户看的失败文字（模型请求以鉴权失败结束，前端照原文显示）。
const NO_SESSION: &str = "account credentials need the requesting session";
const LINK_CLOSED: &str = "the link to the host is closed";
const TIMED_OUT: &str = "Requesting the account credentials timed out";
const NOT_READY: &str = "the host link has not completed its handshake";
const NOT_OPEN: &str = "the requesting session is not open in this rust process";
/// 等用户完成官方版本的请求安全校验时到了截止时刻（21 §13.2.2）。
const VERIFY_TIMED_OUT: &str =
    "The request security check was not completed in time. Send your message again.";
/// 带着校验的结果再取，Host 仍要用户（一次取凭据只开一次交互，21 §6 第 15 行）。
const STILL_NEEDS_USER: &str =
    "The request security check was not accepted. Send your message again.";
/// 交互没被应答就结束了：会话删除了，或到 Host 的链路断了。
const VERIFY_ENDED: &str = "The request security check ended before it was answered.";
/// 交互实体的 id 前缀（后面是随机的 64 位十六进制：跨进程不重复，窗口按 id 记进行中的校验）。
const VERIFICATION_ID_PREFIX: &str = "rv-";

/// actor 交回的：写出了的请求的等待与这条链路的读法（严格链路上按严格变体校验结果，03 §3.8）。
pub(crate) struct Sent {
    pending: Pending,
    strict: bool,
}

/// actor 交回的答复：写出了，或不能发的原因（给用户看的文字）。
pub(crate) type Reply = oneshot::Sender<Result<Sent, String>>;

/// 一个 `host.*` 请求属于谁：会话（外壳带会话所在的工作区与会话）或进程（外壳不带 target，18 §16.2.1）。
#[derive(Debug, Clone)]
pub(crate) enum ServiceTarget {
    Session(String),
    Process,
}

fn failed(message: impl Into<String>) -> ModelError {
    ModelError::Authentication {
        message: message.into(),
    }
}

/// 一次 `host.*` 请求没拿到结果的原因。
#[derive(Debug)]
pub(crate) enum Failure {
    /// 没发出去（没握手、Host 没告知、会话不在这个进程里）、链路断了，或 Host 的失败：文字原样。
    Failed(String),
    /// 回复不合契约（严格链路上按严格变体校验，或读不成生成的类型）：对端违约。
    Invalid(String),
}

/// 发一个 `host.*` 请求并等回复：经 actor 写出（actor 只写不等），等待在调用方的任务里；严格链路上先按严格变体校验，再读成生成的类型。
/// 丢掉等待即放弃，迟到的回复由读循环丢掉。
pub(crate) async fn request<T: DeserializeOwned>(
    events: &mpsc::UnboundedSender<Event>,
    target: ServiceTarget,
    service: &'static str,
    params: &impl Serialize,
) -> Result<T, Failure> {
    let params = serde_json::to_value(params).unwrap_or(Value::Null);
    let (reply, sent) = oneshot::channel();
    let _ = events.send(Event::Service {
        target,
        service,
        params,
        reply,
    });
    // actor 已经结束（链路断了）时 reply 被丢掉
    let Sent { pending, strict } = sent
        .await
        .map_err(|_| Failure::Failed(LINK_CLOSED.to_owned()))?
        .map_err(Failure::Failed)?;
    let result = pending
        .await
        .map_err(|error| Failure::Failed(error.message))?;
    if strict
        && let Some(schema) = schemas::service_result(service)
        && let Err(error) = schema.validate(&result, "")
    {
        return Err(Failure::Invalid(error));
    }
    serde_json::from_value(result).map_err(|error| Failure::Invalid(error.to_string()))
}

/// 会话现在的附加目录（`host.session.executionDirectories`，§37；session-dirs.md §12.4）：会话的通道冷打开之前问一次。Host 没告知、取不到时
/// 照没有附加目录打开（同以前）：附加目录只扩大工作目录的范围，没有它不是错误。
pub(crate) async fn session_directories(
    events: &mpsc::UnboundedSender<Event>,
    session: &str,
) -> Vec<EnvPath> {
    let params = ServiceHostSessionExecutionDirectoriesParams {
        session_id: session.to_owned(),
    };
    match request::<ServiceHostSessionExecutionDirectoriesResult>(
        events,
        ServiceTarget::Session(session.to_owned()),
        SESSION_DIRECTORIES,
        &params,
    )
    .await
    {
        Ok(result) => result
            .execution_directories
            .into_iter()
            .map(EnvPath::new)
            .collect(),
        Err(failure) => {
            tracing::info!(
                event = "protocol.service.session_directories_unavailable",
                session,
                failure = ?failure,
                "opening the session without the host's directories"
            );
            Vec::new()
        }
    }
}

/// 取不到偏好时用的（app-prefs.md §4.1）：契约 `HostPreferencesSchema` 的缺省（记忆关、ToolSearch 关），`prefs_tests.rs` 锁住两者一致。
/// 隐私上关是安全的一边，也是 v4 对旧 Host 的回退（app-prefs.md 待确认第 1 项）。
pub(crate) const CONTRACT_DEFAULT_PREFERENCES: SessionPreferences = SessionPreferences {
    auto_memory: false,
    tool_search: false,
};

/// App 的会话偏好（`host.preferences.get`，§48；app-prefs.md §4.1）：会话的通道建会话、冷打开之前问一次。Host 没告知、取不到时按
/// 契约的缺省。
pub(crate) async fn session_preferences(
    events: &mpsc::UnboundedSender<Event>,
    session: &str,
) -> SessionPreferences {
    match request::<ServiceHostPreferencesGetResult>(
        events,
        ServiceTarget::Session(session.to_owned()),
        PREFERENCES,
        &ServiceHostPreferencesGetParams {},
    )
    .await
    {
        Ok(answer) => preferences_of(&answer),
        Err(failure) => {
            tracing::info!(
                event = "protocol.service.session_preferences_unavailable",
                session,
                failure = ?failure,
                "opening the session with the contract's default preferences"
            );
            CONTRACT_DEFAULT_PREFERENCES
        }
    }
}

/// Host 的偏好 → 内核的会话偏好：内核只接记忆与 ToolSearch 两项（app-prefs.md §4.3）；`toolSearchMode` 的 `auto` 与 `local` 一样
/// （一律本地，D3），不认识的值是契约的降级值 `off`。
pub(crate) fn preferences_of(answer: &ServiceHostPreferencesGetResult) -> SessionPreferences {
    SessionPreferences {
        auto_memory: answer.memory_enabled,
        tool_search: matches!(
            answer.tool_search_mode,
            ServiceHostPreferencesGetResultToolSearchMode::Auto
                | ServiceHostPreferencesGetResultToolSearchMode::Local
        ),
    }
}

impl Actor {
    /// 一个 `service` 请求：查握手、Host 告知的服务与会话所在的工作区（进程级的不查会话），写出请求、交回等待；不能发时交回原因。只写不等。
    pub(crate) async fn service(
        &mut self,
        target: ServiceTarget,
        service: &'static str,
        params: Value,
        reply: Reply,
    ) {
        let sent = match self.service_target(&target, service) {
            Ok(target) => {
                let envelope = ServiceEnvelope {
                    r#type: service.to_owned(),
                    target,
                    params: Some(params),
                };
                let envelope = serde_json::to_value(envelope).unwrap_or(Value::Null);
                Ok(Sent {
                    pending: self.peer.send_request(SERVICE, &envelope).await,
                    strict: self.strict,
                })
            }
            Err(reason) => Err(reason),
        };
        let _ = reply.send(sent);
    }

    /// 能发吗：握手之后、Host 告知了这个服务；会话的服务还要会话在这个进程的会话表里（外壳的工作区按它定，01 §8）。
    /// 给出外壳的 target：进程级的没有（18 §16.2.1）。
    fn service_target(
        &self,
        target: &ServiceTarget,
        service: &str,
    ) -> Result<Option<ServiceEnvelopeTarget>, String> {
        if self.negotiated.is_none() {
            return Err(NOT_READY.to_owned());
        }
        if !self.host_services.iter().any(|known| known == service) {
            return Err(format!("this host does not provide {service}"));
        }
        let ServiceTarget::Session(session) = target else {
            return Ok(None);
        };
        let workspace_key = self
            .sessions
            .workspace_of(session)
            .map(str::to_owned)
            .ok_or_else(|| NOT_OPEN.to_owned())?;
        Ok(Some(ServiceEnvelopeTarget {
            workspace_key,
            session_id: Some(session.clone()),
        }))
    }
}

/// 经 Host 取账号凭据（kernel-api 的 [`HeaderSource`]，入口在 App 的组装里注入；21 §12.2）。需要用户时（`needsUser`，§35）由会话的
/// 投影开 `requestVerification` 交互，拿到应答再取一次。
pub(crate) struct HostCredentials {
    events: mpsc::UnboundedSender<Event>,
    clock: Arc<dyn Clock>,
}

/// 一次取凭据的截止时刻：任务的等待（单调时钟）与交互的 `expiresAt`（Unix 毫秒）同一个起点。
struct Deadline {
    at: Instant,
    expires_at: u64,
}

/// 等一段，到截止时刻还没完就是这次取凭据失败（文字给用户看）。丢掉等待即放弃（§30.0 第 4 项）。
async fn within<T>(
    deadline: &Deadline,
    expired: &str,
    step: impl std::future::Future<Output = Result<T, ModelError>>,
) -> Result<T, ModelError> {
    tokio::time::timeout_at(deadline.at, step)
        .await
        .unwrap_or_else(|_| Err(failed(expired)))
}

/// 任务放弃时撤回交互（回合停了、到了截止时刻，§35.2）：交互还在就移除；已经应答过时 actor 什么也不做。
struct Withdraw {
    events: mpsc::UnboundedSender<Event>,
    session: String,
    id: String,
}

impl Drop for Withdraw {
    fn drop(&mut self) {
        let _ = self.events.send(Event::Withdraw {
            session: std::mem::take(&mut self.session),
            id: std::mem::take(&mut self.id),
        });
    }
}

impl HostCredentials {
    pub(crate) fn source(
        events: mpsc::UnboundedSender<Event>,
        clock: Arc<dyn Clock>,
    ) -> Arc<dyn HeaderSource> {
        Arc::new(Self { events, clock })
    }

    async fn ask(
        &self,
        request: HeaderRequest,
        deadline: &Deadline,
    ) -> Result<SuppliedAuth, ModelError> {
        // 子代理的请求属于启动它的顶层会话：会话表里只有顶层会话
        let session = request
            .root
            .as_ref()
            .or(request.session.as_ref())
            .ok_or_else(|| failed(NO_SESSION))?
            .as_str()
            .to_owned();
        let mut params = ServiceHostProvidersRequestAuthParams {
            provider: request.selection.provider.clone(),
            session_id: Some(session.clone()),
            turn_id: None,
            model_id: Some(request.selection.model.clone()),
            reason: Some(MODEL_REQUEST.to_owned()),
            account_access: Some(
                json!({"type": ZHIPU_ACCOUNT, "accountType": request.account_type,
                "mode": request.mode, "entitled": true}),
            ),
            verification_token: None,
            denied: request
                .denied
                .map(|denied| ServiceHostProvidersRequestAuthParamsDenied {
                    status: denied.status.map(i64::from),
                    code: denied.code,
                }),
            rejected: request.rejected.map(|rejected| {
                ServiceHostProvidersRequestAuthParamsRejected {
                    fingerprint: rejected.fingerprint,
                    account_scope: rejected.binding,
                }
            }),
            // 这一轮执行覆盖的凭据引用（§38）：Host 按它现拼这一轮的凭据
            execution: request
                .execution
                .as_deref()
                .and_then(crate::session::credential),
        };
        let challenge = match within(deadline, TIMED_OUT, self.call(&session, &params)).await? {
            ServiceHostProvidersRequestAuthResult::Headers(headers) => {
                return Ok(supplied(headers, false));
            }
            ServiceHostProvidersRequestAuthResult::NeedsUser(asked) => asked.challenge,
        };
        let token = within(
            deadline,
            VERIFY_TIMED_OUT,
            self.verify(&session, &request.selection.provider, challenge, deadline),
        )
        .await?;
        // 第二次：同样的请求带上校验的结果；拒绝访问的事实已经换成了这次校验（21 §13.2.2）
        params.verification_token = Some(token);
        params.denied = None;
        match within(deadline, TIMED_OUT, self.call(&session, &params)).await? {
            // 这份凭据带着用户刚完成的校验：用过一次即失效（模型层按厂商的规则解释它的空回复，provider-errors.md §3.7）
            ServiceHostProvidersRequestAuthResult::Headers(headers) => Ok(supplied(headers, true)),
            // 一次取凭据只开一次交互（21 §6 第 15 行）：带着结果还要用户，不再问
            ServiceHostProvidersRequestAuthResult::NeedsUser(_) => Err(failed(STILL_NEEDS_USER)),
        }
    }

    /// 一次 `host.providers.requestAuth`（[`request`]）：失败的文字给用户看。
    async fn call(
        &self,
        session: &str,
        params: &ServiceHostProvidersRequestAuthParams,
    ) -> Result<ServiceHostProvidersRequestAuthResult, ModelError> {
        request(
            &self.events,
            ServiceTarget::Session(session.to_owned()),
            REQUEST_AUTH,
            params,
        )
        .await
        .map_err(|failure| match failure {
            Failure::Failed(message) => failed(message),
            Failure::Invalid(error) => {
                failed(format!("invalid account credentials reply: {error}"))
            }
        })
    }

    /// 需要用户完成官方版本的请求安全校验（§35.2）：交给 actor 开交互，等应答（任务里等，actor 不等）。放弃时守卫撤回交互。
    async fn verify(
        &self,
        session: &str,
        provider: &str,
        challenge: Option<Value>,
        deadline: &Deadline,
    ) -> Result<String, ModelError> {
        let id = format!(
            "{VERIFICATION_ID_PREFIX}{:016x}",
            rust_cli_base::random::u64()
        );
        let (answer, answered) = oneshot::channel();
        let _withdraw = Withdraw {
            events: self.events.clone(),
            session: session.to_owned(),
            id: id.clone(),
        };
        let _ = self.events.send(Event::Verify(Box::new(Verify {
            session: session.to_owned(),
            id,
            provider: provider.to_owned(),
            challenge: challenge.unwrap_or(Value::Null),
            expires_at: deadline.expires_at,
            answer,
        })));
        match answered.await {
            Ok(Ok(VerifyAnswer::Token(token))) => Ok(token),
            // 校验没做成：说明给用户看（v4 的 headersApplied: false + errorMessage）
            Ok(Ok(VerifyAnswer::Failure(message))) | Ok(Err(message)) => Err(failed(message)),
            // 应答口被丢掉：会话删除了，或链路断了
            Err(_) => Err(failed(VERIFY_ENDED)),
        }
    }
}

fn supplied(
    headers: ServiceHostProvidersRequestAuthResultHeaders,
    single_use: bool,
) -> SuppliedAuth {
    SuppliedAuth {
        api_key: headers.api_key.map(RedactedString::new),
        headers: secret_headers(headers.headers),
        binding: headers.account_scope,
        single_use,
    }
}

fn secret_headers(headers: BTreeMap<String, String>) -> Vec<(String, RedactedString)> {
    headers
        .into_iter()
        .map(|(name, value)| (name, RedactedString::new(value)))
        .collect()
}

#[async_trait]
impl HeaderSource for HostCredentials {
    async fn request_auth(&self, request: HeaderRequest) -> Result<SuppliedAuth, ModelError> {
        // 端口的总时限（T2）：一个截止时刻管三段（第一次取、等用户、第二次取）；丢掉等待即放弃，迟到的回复由读循环丢掉
        let limit_ms = u64::try_from(REQUEST_AUTH_LIMIT.as_millis()).unwrap_or(u64::MAX);
        let deadline = Deadline {
            at: Instant::now() + REQUEST_AUTH_LIMIT,
            expires_at: self.clock.now_ms().saturating_add(limit_ms),
        };
        self.ask(request, &deadline).await
    }
}

/// 经 Host 加解密凭据的值（kernel-api 的 [`CredentialCipher`]，§74；入口在 App 的组装里交给 MCP OAuth 的凭据库）：每个值一次
/// `host.credentials.cipher`（进程级）。没发出去、链路断了、回复不合契约都是不可用（存储错误，不是登录失效）。
pub(crate) struct HostCipher {
    events: mpsc::UnboundedSender<Event>,
}

impl HostCipher {
    pub(crate) fn port(events: mpsc::UnboundedSender<Event>) -> Arc<dyn CredentialCipher> {
        Arc::new(Self { events })
    }

    async fn call(
        &self,
        operation: ServiceHostCredentialsCipherParamsOperation,
        value: &str,
    ) -> Result<String, CipherFailure> {
        let params = ServiceHostCredentialsCipherParams {
            operation,
            value: value.to_owned(),
        };
        match request(&self.events, ServiceTarget::Process, CIPHER, &params).await {
            Ok(ServiceHostCredentialsCipherResult::Done(done)) => Ok(done.value),
            Ok(ServiceHostCredentialsCipherResult::Failed(failed)) => Err(match failed.reason {
                ServiceHostCredentialsCipherResultFailedReason::Decrypt => CipherFailure::Decrypt,
                _ => CipherFailure::Unavailable(HOST_STORAGE_UNAVAILABLE.to_owned()),
            }),
            Err(Failure::Failed(message)) => Err(CipherFailure::Unavailable(message)),
            Err(Failure::Invalid(error)) => Err(CipherFailure::Unavailable(format!(
                "invalid credential cipher reply: {error}"
            ))),
        }
    }
}

#[async_trait]
impl CredentialCipher for HostCipher {
    async fn encrypt(&self, plain: &RedactedString) -> Result<String, CipherFailure> {
        self.call(
            ServiceHostCredentialsCipherParamsOperation::Encrypt,
            plain.expose(),
        )
        .await
    }

    async fn decrypt(&self, sealed: &str) -> Result<RedactedString, CipherFailure> {
        self.call(ServiceHostCredentialsCipherParamsOperation::Decrypt, sealed)
            .await
            .map(RedactedString::new)
    }
}

#[cfg(test)]
#[path = "host_services_tests.rs"]
mod tests;
