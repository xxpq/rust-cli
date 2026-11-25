//! 非流式回退（G，docs/specs/rust-cli/provider-errors.md §3.6）：还没越过交出边界的流内失败与流式的 404 改用非流式请求重发
//! 一次，回复换成同样的事件交出；之后这次调用里的请求都是非流式。

#![allow(
    clippy::unwrap_used,
    reason = "辅助函数不在 #[test] 函数里，clippy 的测试放行不覆盖它；失败即测试失败"
)]

use futures_util::StreamExt;
use rust_cli_kernel_api::message::ContentBlock;
use rust_cli_kernel_api::model::{
    ModelError, ModelEvent, ModelNotice, ModelPort, ModelStreamItem, QuerySource, ThinkingConfig,
};
use rust_cli_kernel_api::side_request::SideSource;
use rust_cli_model::{ProviderModel, RouteOptions};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::common::{
    assemble, block_start, event, fast_retry, message_start, model_at, model_with, request,
    text_response, whole_message,
};
use super::fixture::{Captured, Fixture, Reply};

fn stream_reply(body: String) -> Reply {
    Reply::new(200)
        .header("content-type", "text/event-stream")
        .send(body)
}

/// `message_start` 与一个还没有正文的文本块，之后来一个错误事件（都在交出边界之前）。
fn early_error(kind: &str, message: &str) -> Reply {
    let mut body = message_start();
    body.push_str(&block_start(0, json!({"type": "text", "text": ""})));
    body.push_str(&event(
        json!({"type": "error", "error": {"type": kind, "message": message}}),
    ));
    stream_reply(body)
}

/// 非流式的回复：一条完整的消息。
fn whole(content: Value) -> Reply {
    Reply::new(200)
        .header("content-type", "application/json")
        .send(whole_message(content))
}

fn whole_text(text: &str) -> Reply {
    whole(json!([{"type": "text", "text": text}]))
}

fn status(code: u16, kind: &str, message: &str) -> Reply {
    Reply::new(code)
        .header("content-type", "application/json")
        .send(json!({"type": "error", "error": {"type": kind, "message": message}}).to_string())
}

fn body(request: &Captured) -> Value {
    serde_json::from_str(&request.body).unwrap()
}

/// 每个请求是不是流式的（`stream` 的值）。
fn streamed(server: &Fixture) -> Vec<bool> {
    server
        .requests()
        .iter()
        .map(|request| body(request)["stream"] == json!(true))
        .collect()
}

/// 带通知的流的形状：`retrying(<错误>, 0|wait)` / `resumed` / `event` / `error(<错误>)`。
async fn shape_of(
    model: &ProviderModel,
    source: QuerySource,
) -> (Vec<String>, Vec<Result<ModelEvent, ModelError>>) {
    let items: Vec<_> = model
        .stream_with_notices(request("hi", source), CancellationToken::new())
        .collect()
        .await;
    let mut events = Vec::new();
    let shape = items
        .into_iter()
        .map(|item| match item {
            Ok(ModelStreamItem::Notice(ModelNotice::Retrying {
                error, delay_ms, ..
            })) => {
                let delay = if delay_ms == 0 { "0" } else { "wait" };
                format!("retrying({error}, {delay})")
            }
            Ok(ModelStreamItem::Notice(notice)) => format!("{notice:?}").to_lowercase(),
            Ok(ModelStreamItem::Event(event)) => {
                events.push(Ok(event));
                "event".to_owned()
            }
            Err(error) => {
                let shown = format!("error({error})");
                events.push(Err(error));
                shown
            }
        })
        .collect();
    (shape, events)
}

/// 还没有正文时的服务端错误：不等待地改用非流式请求重发；非流式的回复换成同样的六个事件交出（以前用一次流式重发代替，
/// provider-errors.md D3 暂定 (b)）。
#[tokio::test]
async fn a_server_error_before_any_content_falls_back_to_a_whole_request() {
    let server = Fixture::start(vec![
        early_error("api_error", "Internal server error"),
        whole_text("ok"),
    ])
    .await;
    let model = model_at(server.url(), fast_retry(3)).await;
    let (shape, events) = shape_of(&model, QuerySource::MainThread).await;
    assert_eq!(
        shape,
        [
            "retrying(server error 500: Internal server error, 0)",
            "resumed",
            "event",
            "event",
            "event",
            "event",
            "event",
            "event",
        ]
    );
    assert_eq!(
        assemble(&events),
        [ContentBlock::Text {
            text: "ok".to_owned()
        }]
    );
    assert!(matches!(events.last(), Some(Ok(ModelEvent::MessageStop))));
    assert_eq!(streamed(&server), [true, false]);
    // 只有 `stream` 变了：两个请求的其余部分相同。
    let requests = server.requests();
    let (mut first, mut second) = (body(&requests[0]), body(&requests[1]));
    first["stream"] = Value::Null;
    second["stream"] = Value::Null;
    assert_eq!(first, second);
}

/// 改非流式之后这次调用里的请求都是非流式：非流式请求的 500 按响应头之前的规则退避重试，重发的仍是非流式。
#[tokio::test]
async fn after_the_fallback_every_retry_is_a_whole_request() {
    let server = Fixture::start(vec![
        early_error("api_error", "Internal server error"),
        status(500, "api_error", "still broken"),
        whole_text("ok"),
    ])
    .await;
    let model = model_at(server.url(), fast_retry(3)).await;
    let (shape, _) = shape_of(&model, QuerySource::MainThread).await;
    assert_eq!(
        shape[..4],
        [
            "retrying(server error 500: Internal server error, 0)",
            "retrying(server error 500: still broken, wait)",
            "resumed",
            "event",
        ],
        "{shape:?}"
    );
    assert_eq!(streamed(&server), [true, false, false]);
}

/// 流式请求回 404：改非流式重发一次（端点可能只是没有流式接口）。
#[tokio::test]
async fn a_streaming_404_falls_back_to_a_whole_request() {
    let server = Fixture::start(vec![
        status(404, "not_found_error", "Streaming is not supported"),
        whole_text("ok"),
    ])
    .await;
    let model = model_at(server.url(), fast_retry(3)).await;
    let (shape, events) = shape_of(&model, QuerySource::Compact).await;
    assert_eq!(
        shape[..2],
        [
            "retrying(invalid request: Streaming is not supported, 0)",
            "resumed"
        ],
        "{shape:?}"
    );
    assert_eq!(
        assemble(&events),
        [ContentBlock::Text {
            text: "ok".to_owned()
        }]
    );
    assert_eq!(streamed(&server), [true, false]);
}

/// 非流式请求也回 404：不再改（G 只一次），把错误交给调用方。
#[tokio::test]
async fn a_404_on_the_whole_request_ends_the_call() {
    let server = Fixture::start(vec![
        status(404, "not_found_error", "no such model"),
        status(404, "not_found_error", "no such model"),
        whole_text("never"),
    ])
    .await;
    let model = model_at(server.url(), fast_retry(3)).await;
    let (shape, _) = shape_of(&model, QuerySource::MainThread).await;
    assert_eq!(
        shape,
        [
            "retrying(invalid request: no such model, 0)",
            "error(invalid request: no such model)"
        ]
    );
    assert_eq!(streamed(&server), [true, false]);
}

/// `ZCODE_DISABLE_NONSTREAMING_FALLBACK`（入口的开关）：不改非流式，错误交给调用方。
#[tokio::test]
async fn the_fallback_can_be_turned_off() {
    let server = Fixture::start(vec![
        early_error("api_error", "Internal server error"),
        whole_text("never"),
    ])
    .await;
    let model = model_with(
        server.url(),
        RouteOptions {
            retry: fast_retry(3),
            disable_nonstreaming_fallback: true,
            ..RouteOptions::default()
        },
    )
    .await;
    let (shape, _) = shape_of(&model, QuerySource::MainThread).await;
    assert_eq!(shape, ["error(server error 500: Internal server error)"]);
    assert_eq!(server.requests().len(), 1);
}

/// 后台来源的流内过载：不流式重试，直接改非流式（provider-errors.md D19 改）。以前直接失败。
#[tokio::test]
async fn a_background_overload_in_the_stream_falls_back() {
    let server = Fixture::start(vec![
        early_error("overloaded_error", "Overloaded"),
        whole_text("a title"),
    ])
    .await;
    let model = model_at(server.url(), fast_retry(3)).await;
    let (shape, _) = shape_of(&model, QuerySource::Side(SideSource::SessionTitle)).await;
    assert_eq!(
        shape[..2],
        ["retrying(model overloaded: Overloaded, 0)", "resumed"],
        "{shape:?}"
    );
    assert_eq!(streamed(&server), [true, false]);
}

/// 非流式请求的输出上限压到 64000，`thinking.type: enabled` 的预算压到上限减一。
/// 按官方端点的方案发（兼容端点的方案去掉 `thinking`）。
#[tokio::test]
async fn the_whole_request_caps_the_output_and_the_thinking_budget() {
    let server = Fixture::start(vec![
        early_error("api_error", "Internal server error"),
        whole_text("ok"),
    ])
    .await;
    let model = model_with(
        server.url(),
        RouteOptions {
            retry: fast_retry(3),
            assume_first_party: true,
            ..RouteOptions::default()
        },
    )
    .await;
    let mut big = request("hi", QuerySource::MainThread);
    big.max_tokens = 128_000;
    big.thinking = Some(ThinkingConfig::Enabled {
        budget_tokens: 100_000,
    });
    let _: Vec<_> = model.stream(big, CancellationToken::new()).collect().await;
    let requests = server.requests();
    let (first, second) = (body(&requests[0]), body(&requests[1]));
    assert_eq!(first["max_tokens"], 128_000);
    assert_eq!(first["thinking"]["budget_tokens"], 100_000);
    assert_eq!(second["max_tokens"], 64_000);
    assert_eq!(second["thinking"]["budget_tokens"], 63_999);
}

/// 非流式的回复里有 thinking 与 tool_use：换成事件后与流式时拼出同样的块。
#[tokio::test]
async fn a_whole_reply_with_thinking_and_a_tool_call_assembles_the_same_blocks() {
    let server = Fixture::start(vec![
        early_error("api_error", "Internal server error"),
        whole(json!([
            {"type": "thinking", "thinking": "plan", "signature": "sig_1"},
            {"type": "text", "text": "Reading."},
            {"type": "tool_use", "id": "toolu_1", "name": "Read", "input": {"file_path": "/tmp/a"}}
        ])),
    ])
    .await;
    let model = model_at(server.url(), fast_retry(3)).await;
    let (_, events) = shape_of(&model, QuerySource::MainThread).await;
    assert_eq!(
        assemble(&events),
        [
            ContentBlock::Thinking {
                thinking: "plan".to_owned(),
                signature: Some("sig_1".to_owned()),
            },
            ContentBlock::Text {
                text: "Reading.".to_owned()
            },
            ContentBlock::ToolUse {
                id: rust_cli_kernel_api::ids::ToolUseId::new("toolu_1".to_owned()),
                name: "Read".to_owned(),
                input: json!({"file_path": "/tmp/a"}),
            },
        ]
    );
}

/// 越过交出边界之后的失败照旧交给调用方，不改非流式（P2 是 query 的部分收尾）。
#[tokio::test]
async fn a_failure_after_content_does_not_fall_back() {
    let mut body = super::common::started_text("partial");
    body.push_str(&event(
        json!({"type": "error", "error": {"type": "api_error", "message": "boom"}}),
    ));
    let server = Fixture::start(vec![
        stream_reply(body),
        stream_reply(text_response(&["x"])),
    ])
    .await;
    let model = model_at(server.url(), fast_retry(3)).await;
    let (shape, _) = shape_of(&model, QuerySource::MainThread).await;
    assert_eq!(
        shape.last().map(String::as_str),
        Some("error(server error 500: boom)")
    );
    assert_eq!(server.requests().len(), 1);
}

/// 已取消的调用不发非流式请求。
#[tokio::test]
async fn a_cancelled_call_does_not_fall_back() {
    let server = Fixture::start(vec![
        early_error("api_error", "Internal server error"),
        whole_text("never"),
    ])
    .await;
    let model = model_at(server.url(), fast_retry(3)).await;
    let cancel = CancellationToken::new();
    let mut stream =
        model.stream_with_notices(request("hi", QuerySource::MainThread), cancel.clone());
    // 第一项是 G 之前的重试通知：此时取消，之后只剩取消的错误。
    let first = stream.next().await;
    assert!(
        matches!(
            first,
            Some(Ok(ModelStreamItem::Notice(ModelNotice::Retrying { .. })))
        ),
        "{first:?}"
    );
    cancel.cancel();
    let rest: Vec<_> = stream.collect().await;
    assert_eq!(rest, vec![Err(ModelError::Cancelled)]);
    assert_eq!(server.requests().len(), 1);
}
