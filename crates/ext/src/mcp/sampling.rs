//! `sampling/createMessage`（docs/specs/rust-cli/ext.md §3.14）：ZCode 产品层的功能（Node 版没有）。每个请求先让用户
//! 看到并同意（可以选「本会话一直允许这个服务器」），再用去处会话现在的主模型发一次旁路请求：不进对话、不写用量表。
//!
//! MCP `2026-07-28` 弃用了 sampling（SEP-2577），rmcp 把它的类型标成弃用；旧纪元的服务器仍在用，所以本模块整个放过这条警告。
#![expect(
    deprecated,
    reason = "MCP 弃用了 sampling（SEP-2577），旧纪元的服务器仍用它（ext.md §3.14）"
)]

use rust_cli_base::sync::LockUnpoisoned;
use rust_cli_base::units;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};

use rmcp::model::{
    ClientResult, CreateMessageRequestParams, CreateMessageResult, ErrorCode, ErrorData, Role,
    SamplingMessage, SamplingMessageContentBlock,
};
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::mcp_ask::{
    Sampling, SamplingAnswer, SamplingMessage as Shown, SamplingRole,
};
use rust_cli_kernel_api::message::{ContentBlock, ImageSource, StopReason};
use rust_cli_kernel_api::model::{ApiContent, ApiMessage, ApiRole, ModelRequest, SystemBlock};
use rust_cli_kernel_api::orchestration::InteractionRequest;
use rust_cli_kernel_api::side_request::{SideReply, SideSource};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::asks::{Asks, Caller, Reply, Stop, Stopped};

/// 实际给模型的输出上限：服务器要的与它中小的那个（同 WebFetch 旁路请求的上限）。
pub(super) const MAX_SAMPLING_TOKENS: u32 = 8_192;
/// 一个请求里全部文字（对话与系统提示）的字符上限（同 WebFetch 交给旁路模型的正文上限）。
pub(super) const MAX_SAMPLING_CHARS: usize = 100_000;
/// 同一个会话、同一个服务器在 [`SAMPLING_WINDOW_MS`] 里最多开始这么多个请求（含被拒绝的）。
pub(super) const MAX_SAMPLING_PER_MINUTE: usize = 10;
const SAMPLING_WINDOW_MS: u64 = units::MS_PER_MINUTE;
/// 用户拒绝（规范示例的错误码与文字）。
const REJECTED_CODE: ErrorCode = ErrorCode(-1);
const REJECTED: &str = "User rejected sampling request";
const TOO_LARGE: &str = "Sampling request is too large";
const TOO_MANY: &str = "Too many sampling requests";
const NO_ONE: &str = "No user can approve this sampling request";
const CANCELLED: &str = "Sampling request was cancelled";
/// 给人看的图片占位（发给模型的是原样的图片）。
const IMAGE: &str = "[image]";

/// 「本会话一直允许」与频率的记录（§3.14.1）：只在内存，会话释放时去掉。
#[derive(Default)]
pub(super) struct Grants(Mutex<GrantState>);

#[derive(Default)]
struct GrantState {
    allowed: HashSet<(SessionId, String)>,
    /// （会话，服务器）→ 最近开始的请求的时刻（ms），旧的在前。
    started: HashMap<(SessionId, String), VecDeque<u64>>,
}

impl Grants {
    fn allowed(&self, session: &SessionId, server: &str) -> bool {
        self.0
            .lock_unpoisoned()
            .allowed
            .contains(&(session.clone(), server.to_owned()))
    }

    fn allow(&self, session: &SessionId, server: &str) {
        self.0
            .lock_unpoisoned()
            .allowed
            .insert((session.clone(), server.to_owned()));
    }

    /// 这一次能不能开始：最近一个窗口里开始的不到上限时记下这一次。
    fn admit(&self, session: &SessionId, server: &str, now_ms: u64) -> bool {
        let mut state = self.0.lock_unpoisoned();
        let started = state
            .started
            .entry((session.clone(), server.to_owned()))
            .or_default();
        while started
            .front()
            .is_some_and(|at| now_ms.saturating_sub(*at) >= SAMPLING_WINDOW_MS)
        {
            started.pop_front();
        }
        if started.len() >= MAX_SAMPLING_PER_MINUTE {
            return false;
        }
        started.push_back(now_ms);
        true
    }

    pub(super) fn released(&self, session: &SessionId) {
        let mut state = self.0.lock_unpoisoned();
        state.allowed.retain(|(owner, _)| owner != session);
        state.started.retain(|(owner, _), _| owner != session);
    }
}

pub(super) async fn serve(asks: &Arc<Asks>, params: Option<Value>, stop: Stop) -> Option<Reply> {
    let request =
        match serde_json::from_value::<CreateMessageRequestParams>(params.unwrap_or(Value::Null)) {
            Ok(request) => request,
            Err(error) => {
                return Some(error_reply(
                    ErrorCode::INVALID_PARAMS,
                    format!("Invalid params: {error}"),
                ));
            }
        };
    let (shown, messages) = match read(&request) {
        Ok(read) => read,
        Err(message) => return Some(error_reply(ErrorCode::INVALID_PARAMS, message)),
    };
    let Some(caller) = asks.caller() else {
        return Some(error_reply(ErrorCode::INTERNAL_ERROR, NO_ONE.to_owned()));
    };
    let ports = &asks.ports;
    if !ports
        .grants
        .admit(caller.top(), &asks.server, ports.clock.now_ms())
    {
        return Some(error_reply(ErrorCode::INTERNAL_ERROR, TOO_MANY.to_owned()));
    }
    let stop = stop.on(&caller);
    let sampling = Sampling {
        server: asks.server.clone(),
        messages: shown,
        system: request.system_prompt.clone(),
        max_tokens: request.max_tokens.min(MAX_SAMPLING_TOKENS),
    };
    match sample(asks, &caller, (sampling, messages, &request), &stop).await {
        Ok(reply) => Some(reply),
        Err(Stopped::Gone) => None,
        Err(Stopped::Called) => Some(error_reply(ErrorCode::INTERNAL_ERROR, CANCELLED.to_owned())),
    }
}

async fn sample(
    asks: &Asks,
    caller: &Caller,
    (sampling, messages, request): (Sampling, Vec<ApiMessage>, &CreateMessageRequestParams),
    stop: &Stop,
) -> Result<Reply, Stopped> {
    let ports = &asks.ports;
    let server = &asks.server;
    if !ports.grants.allowed(caller.top(), server) {
        let ask = InteractionRequest::McpSampling {
            tool_use: caller.tool_use.clone(),
            sampling: Box::new(sampling.clone()),
            asked_by: None,
        };
        let answer = asks
            .ask(caller, ask, stop)
            .await?
            .and_then(|answer| SamplingAnswer::try_from(answer).ok())
            .unwrap_or(SamplingAnswer::SamplingDenied);
        match answer {
            SamplingAnswer::SamplingAllowed { session } => {
                if session {
                    ports.grants.allow(caller.top(), server);
                }
            }
            SamplingAnswer::SamplingDenied => {
                return Ok(error_reply(REJECTED_CODE, REJECTED.to_owned()));
            }
        }
    }
    let selection = caller
        .session_ops
        .view()
        .await
        .and_then(|view| view.selection)
        .unwrap_or_else(|| ports.default_model.current());
    let system = sampling
        .system
        .iter()
        .map(|text| SystemBlock {
            text: text.clone(),
            cache_control: None,
        })
        .collect();
    let mut model_request = ModelRequest::side(
        selection.clone(),
        SideSource::McpSampling,
        system,
        messages,
        sampling.max_tokens,
        caller.requester(),
    );
    model_request.temperature = request.temperature;
    model_request.stop_sequences = request.stop_sequences.clone().unwrap_or_default();
    let cancel = CancellationToken::new();
    let _cancel_on_return = cancel.clone().drop_guard();
    let reply = stop
        .until(SideReply::collect(
            ports.model.stream(model_request, cancel),
        ))
        .await?;
    let reply = match reply {
        Ok(reply) => reply,
        Err(error) => return Ok(error_reply(ErrorCode::INTERNAL_ERROR, error.to_string())),
    };
    tracing::info!(
        event = "mcp.sampling",
        server = %server,
        model = %selection.model,
        input_tokens = reply.usage.input_tokens,
        output_tokens = reply.usage.output_tokens,
        "MCP sampling request answered"
    );
    let mut result = CreateMessageResult::new(
        SamplingMessage::assistant_text(reply.text()),
        selection.model,
    );
    if let Some(reason) = reply.stop_reason.and_then(stop_reason) {
        result = result.with_stop_reason(reason);
    }
    Ok(Reply::Result(ClientResult::CreateMessageResult(Box::new(
        result,
    ))))
}

/// 服务器的对话 → 给人看的与发给模型的。音频、`tool_use` / `tool_result` 块与 `tools` 不支持（没声明 `sampling.tools`）；全部文字
/// 超过 [`MAX_SAMPLING_CHARS`] 时太大。
fn read(request: &CreateMessageRequestParams) -> Result<(Vec<Shown>, Vec<ApiMessage>), String> {
    if request
        .tools
        .as_ref()
        .is_some_and(|tools| !tools.is_empty())
    {
        return Err(unsupported("tools"));
    }
    let mut chars = request
        .system_prompt
        .as_deref()
        .map_or(0, |text| text.chars().count());
    let mut shown = Vec::new();
    let mut messages = Vec::new();
    for message in &request.messages {
        let (role, api_role) = match message.role {
            Role::User => (SamplingRole::User, ApiRole::User),
            Role::Assistant => (SamplingRole::Assistant, ApiRole::Assistant),
        };
        let mut texts = Vec::new();
        let mut content = Vec::new();
        for block in message.content.iter() {
            let block = match block {
                SamplingMessageContentBlock::Text(text) => {
                    chars += text.text.chars().count();
                    texts.push(text.text.clone());
                    ContentBlock::Text {
                        text: text.text.clone(),
                    }
                }
                SamplingMessageContentBlock::Image(image) => {
                    texts.push(IMAGE.to_owned());
                    ContentBlock::Image {
                        source: ImageSource::Base64 {
                            media_type: image.mime_type.clone(),
                            data: image.data.clone(),
                        },
                        origin: None,
                    }
                }
                SamplingMessageContentBlock::Audio(_) => return Err(unsupported("audio")),
                SamplingMessageContentBlock::ToolUse(_) => return Err(unsupported("tool_use")),
                SamplingMessageContentBlock::ToolResult(_) => {
                    return Err(unsupported("tool_result"));
                }
                // rmcp 的类型标了 `non_exhaustive`：钉死的 3.9.0 没有别的变体（ext.md §3.11.1）。
                _ => return Err(unsupported("unknown")),
            };
            content.push(ApiContent {
                block,
                cache_control: None,
            });
        }
        shown.push(Shown {
            role,
            text: texts.join("\n"),
        });
        messages.push(ApiMessage {
            role: api_role,
            content,
            output_config: None,
        });
    }
    if chars > MAX_SAMPLING_CHARS {
        return Err(TOO_LARGE.to_owned());
    }
    Ok((shown, messages))
}

fn unsupported(kind: &str) -> String {
    format!("Unsupported sampling content: {kind}")
}

/// 模型的停止原因 → MCP 的写法；别的不写。
fn stop_reason(reason: StopReason) -> Option<&'static str> {
    match reason {
        StopReason::EndTurn => Some(CreateMessageResult::STOP_REASON_END_TURN),
        StopReason::MaxTokens => Some(CreateMessageResult::STOP_REASON_END_MAX_TOKEN),
        StopReason::StopSequence => Some(CreateMessageResult::STOP_REASON_END_SEQUENCE),
        _ => None,
    }
}

fn error_reply(code: ErrorCode, message: String) -> Reply {
    Reply::Error(ErrorData::new(code, message, None))
}
