//! 空回复（docs/specs/rust-cli/provider-errors.md §3.7，照 Node 的 `empty-completion-retry.ts`）：一个块都没有、结束原因不是
//! 正常结束与工具调用、用量全为零的回复作废，重试一次；第二次仍空时报错。

#![allow(
    clippy::unwrap_used,
    reason = "辅助函数不在 #[test] 函数里，clippy 的测试放行不覆盖它；失败即测试失败"
)]

use futures_util::StreamExt;
use rust_cli_base::models;
use rust_cli_kernel_api::model::{
    ModelError, ModelEvent, ModelNotice, ModelPort, ModelStreamItem, QuerySource,
};
use rust_cli_model::ProviderModel;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::common::{
    block_start, event, fast_retry, message_start, model_at, request, text_response,
};
use super::fixture::{Fixture, Reply};

const EMPTY: &str = "protocol error: Model returned no text, no tool calls, and no usage before completing the turn.";

fn stream_reply(body: String) -> Reply {
    Reply::new(200)
        .header("content-type", "text/event-stream")
        .send(body)
}

/// `message_start`，输入用量是 `input`、输出是 0。
fn start(input: u64) -> String {
    event(json!({"type": "message_start", "message": {
        "id": "msg_empty", "type": "message", "role": "assistant", "model": models::GLM_5_3,
        "content": [], "stop_reason": null, "stop_sequence": null,
        "usage": {"input_tokens": input, "cache_creation_input_tokens": 0,
                  "cache_read_input_tokens": 0, "output_tokens": 0}}}))
}

/// 一个块都没有的回复：结束原因是 `stop`，结束时的用量是（`input`，`output`）。
fn blockless(stop: serde_json::Value, input: u64, output: u64) -> String {
    let start = start(input);
    let delta = event(json!({"type": "message_delta",
        "delta": {"stop_reason": stop, "stop_sequence": null},
        "usage": {"input_tokens": input, "output_tokens": output}}));
    format!("{start}{delta}{}", event(json!({"type": "message_stop"})))
}

/// Node 的夹具里的空流：`stop_reason: "other"`，用量全为零。
fn empty() -> Reply {
    stream_reply(blockless(json!("other"), 0, 0))
}

fn ok() -> Reply {
    stream_reply(text_response(&["ok"]))
}

async fn shape_of(model: &ProviderModel) -> Vec<String> {
    let items: Vec<_> = model
        .stream_with_notices(
            request("hi", QuerySource::MainThread),
            CancellationToken::new(),
        )
        .collect()
        .await;
    items
        .iter()
        .map(|item| match item {
            Ok(ModelStreamItem::Notice(ModelNotice::Retrying {
                error, delay_ms, ..
            })) => {
                let delay = if *delay_ms == 0 { "0" } else { "wait" };
                format!("retrying({error}, {delay})")
            }
            Ok(ModelStreamItem::Notice(notice)) => format!("{notice:?}").to_lowercase(),
            Ok(ModelStreamItem::Event(ModelEvent::MessageDelta { stop_reason, .. })) => {
                format!("delta({stop_reason:?})")
            }
            Ok(ModelStreamItem::Event(_)) => "event".to_owned(),
            Err(error) => format!("error({error})"),
        })
        .collect()
}

/// 第一次空回复的事件不交出，退避后重试；第二次的回复照常交出。以前空回复按正常完成交出，回合静默结束、没有正文。
#[tokio::test]
async fn an_empty_reply_is_retried_once() {
    let server = Fixture::start(vec![empty(), ok()]).await;
    let model = model_at(server.url(), fast_retry(3)).await;
    let shape = shape_of(&model).await;
    assert_eq!(
        shape,
        [
            format!("retrying({EMPTY}, wait)"),
            "resumed".to_owned(),
            "event".to_owned(),
            "event".to_owned(),
            "event".to_owned(),
            "event".to_owned(),
            "delta(Some(EndTurn))".to_owned(),
            "event".to_owned(),
        ]
    );
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].body, requests[1].body);
}

/// 第二次仍空：不再重试，把空回复的错误交给调用方（Node 的 core 以同一句结束回合）。
#[tokio::test]
async fn a_second_empty_reply_ends_with_the_empty_reply_error() {
    let server = Fixture::start(vec![empty(), empty(), ok()]).await;
    let model = model_at(server.url(), fast_retry(3)).await;
    assert_eq!(
        shape_of(&model).await,
        [
            format!("retrying({EMPTY}, wait)"),
            "resumed".to_owned(),
            format!("error({EMPTY})"),
        ]
    );
    assert_eq!(server.requests().len(), 2);
}

/// 重试次数是 0（`ZCODE_MODEL_RETRY_MAX_RETRIES=0`）：空回复也不重试。
#[tokio::test]
async fn no_retries_left_means_no_empty_retry() {
    let server = Fixture::start(vec![empty(), ok()]).await;
    let model = model_at(server.url(), fast_retry(0)).await;
    assert_eq!(shape_of(&model).await, [format!("error({EMPTY})")]);
    assert_eq!(server.requests().len(), 1);
}

/// 不算空回复的：有原因的结束（拒答、窗口用满、正常结束）、用量不为零、有块（哪怕是空的文字块）。都照常交出，只发一次请求。
#[tokio::test]
async fn replies_with_a_reason_usage_or_a_block_are_not_empty() {
    let mut empty_text_block = start(0);
    empty_text_block.push_str(&block_start(0, json!({"type": "text", "text": ""})));
    empty_text_block.push_str(&event(json!({"type": "content_block_stop", "index": 0})));
    empty_text_block.push_str(&event(json!({"type": "message_delta",
        "delta": {"stop_reason": "other", "stop_sequence": null}, "usage": {"output_tokens": 0}})));
    empty_text_block.push_str(&event(json!({"type": "message_stop"})));
    let cases = [
        (blockless(json!("refusal"), 0, 0), "delta(Some(Refusal))"),
        (
            blockless(json!("model_context_window_exceeded"), 0, 0),
            "delta(Some(ModelContextWindowExceeded))",
        ),
        (blockless(json!("end_turn"), 0, 0), "delta(Some(EndTurn))"),
        (blockless(json!("other"), 80, 5), "delta(Some(Unknown))"),
        (empty_text_block, "delta(Some(Unknown))"),
    ];
    for (body, delta) in cases {
        let server = Fixture::start(vec![stream_reply(body), ok()]).await;
        let model = model_at(server.url(), fast_retry(3)).await;
        let shape = shape_of(&model).await;
        assert!(shape.iter().any(|item| item == delta), "{delta}: {shape:?}");
        assert!(
            !shape.iter().any(|item| item.starts_with("retrying")),
            "{shape:?}"
        );
        assert_eq!(server.requests().len(), 1, "{delta}");
    }
}

/// 没有结束原因（只有开始与结束）、用量为零：也是空回复（Node 归一成 `unknown`）。
#[tokio::test]
async fn a_reply_without_a_stop_reason_and_usage_is_empty() {
    let bare = format!("{}{}", start(0), event(json!({"type": "message_stop"})));
    let server = Fixture::start(vec![stream_reply(bare), ok()]).await;
    let model = model_at(server.url(), fast_retry(3)).await;
    let shape = shape_of(&model).await;
    assert_eq!(shape[0], format!("retrying({EMPTY}, wait)"), "{shape:?}");
    assert_eq!(server.requests().len(), 2);
}

/// 改非流式之后的回复是空的：同样重试一次，重发的仍是非流式（不再走 G）。
#[tokio::test]
async fn an_empty_whole_reply_is_retried_as_a_whole_request() {
    let early = {
        let mut body = message_start();
        body.push_str(&event(
            json!({"type": "error", "error": {"type": "api_error", "message": "Internal server error"}}),
        ));
        stream_reply(body)
    };
    let whole = |content: serde_json::Value, stop: &str, output: u64| {
        let message = json!({
            "id": "msg_whole", "type": "message", "role": "assistant", "model": models::GLM_5_3,
            "content": content, "stop_reason": stop, "stop_sequence": null,
            "usage": {"input_tokens": 0, "output_tokens": output}});
        Reply::new(200)
            .header("content-type", "application/json")
            .send(message.to_string())
    };
    let server = Fixture::start(vec![
        early,
        whole(json!([]), "other", 0),
        whole(json!([{"type": "text", "text": "ok"}]), "end_turn", 3),
    ])
    .await;
    let model = model_at(server.url(), fast_retry(3)).await;
    let shape = shape_of(&model).await;
    // 非流式的回复到了（`resumed`），是空的：退避后再发一次非流式请求。
    assert_eq!(
        shape[..4],
        [
            "retrying(server error 500: Internal server error, 0)".to_owned(),
            "resumed".to_owned(),
            format!("retrying({EMPTY}, wait)"),
            "resumed".to_owned(),
        ],
        "{shape:?}"
    );
    let streamed: Vec<bool> = server
        .requests()
        .iter()
        .map(|request| request.body.contains("\"stream\":true"))
        .collect();
    assert_eq!(streamed, [true, false, false]);
}

/// 空回复的判定不改调用方拿到的错误类型：交给调用方时是 `Protocol`。
#[tokio::test]
async fn the_plain_stream_reports_the_empty_reply_as_a_protocol_error() {
    let server = Fixture::start(vec![empty(), empty()]).await;
    let model = model_at(server.url(), fast_retry(1)).await;
    let events: Vec<_> = model
        .stream(
            request("hi", QuerySource::MainThread),
            CancellationToken::new(),
        )
        .collect()
        .await;
    assert!(
        matches!(events.as_slice(), [Err(ModelError::Protocol { .. })]),
        "{events:?}"
    );
}
