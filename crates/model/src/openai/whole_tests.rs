//! OpenAI 两种协议的非流式回退（provider-errors.md §3.6）：请求体的改写；回复换成事件、交给解码器后与等价的流相同。

use std::collections::VecDeque;

use rust_cli_base::models;
use rust_cli_kernel_api::model::ModelEvent;
use rust_cli_net::sse::{SseDecoder, SseEvent};
use serde_json::{Map, Value, json};

use super::*;
use crate::errors::{Class, Vendor, Verdict};
use crate::openai::chat_stream::ChatStream;
use crate::openai::responses_stream::ResponsesStream;
use crate::protocol::StreamDecoder;

fn object(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        other => panic!("{other}"),
    }
}

#[test]
fn chat_drops_the_stream_options_and_keeps_the_order() {
    let mut body = object(json!({"model": "m", "messages": [], "stream": true,
        "stream_options": {"include_usage": true}, "max_tokens": 8}));
    chat_edit(&mut body);
    assert_eq!(
        Value::Object(body).to_string(),
        r#"{"model":"m","messages":[],"stream":false,"max_tokens":8}"#
    );
    let mut body = object(json!({"model": "m", "stream": true, "store": false}));
    responses_edit(&mut body);
    assert_eq!(
        Value::Object(body).to_string(),
        r#"{"model":"m","stream":false,"store":false}"#
    );
}

fn decode(
    decoder: &mut dyn StreamDecoder,
    events: Vec<SseEvent>,
) -> Result<Vec<ModelEvent>, Verdict> {
    let mut out = VecDeque::new();
    for event in events {
        decoder.decode(event, &mut out)?;
    }
    assert!(decoder.finished(), "{out:?}");
    Ok(out.into_iter().collect())
}

fn parse(text: &str) -> Vec<SseEvent> {
    SseDecoder::new().push(text.as_bytes()).unwrap()
}

fn data(value: Value) -> SseEvent {
    SseEvent {
        event: None,
        data: value.to_string(),
    }
}

/// Chat 的一条回复（推理、正文、没有 `index` 的工具调用）与同样内容的流解出相同的事件。
#[test]
fn a_chat_completion_decodes_like_its_stream() {
    let usage = json!({"prompt_tokens": 120, "completion_tokens": 30,
        "prompt_tokens_details": {"cached_tokens": 100}});
    let call = json!({"id": "call_a", "type": "function",
        "function": {"name": "Read", "arguments": "{\"file_path\":\"a\"}"}});
    let completion = json!({"id": "chatcmpl-1", "model": models::GLM_5, "choices": [{"index": 0,
        "message": {"role": "assistant", "reasoning_content": "think", "content": "Let me look.",
                    "tool_calls": [call]},
        "finish_reason": "tool_calls"}], "usage": usage});
    let whole = decode(
        &mut ChatStream::new(Vendor::Generic),
        parse(&chat_replay(completion)),
    )
    .unwrap();
    let chunk = |delta: Value| {
        data(
            json!({"id": "chatcmpl-1", "model": models::GLM_5, "choices": [{"index": 0, "delta": delta}]}),
        )
    };
    let mut indexed = call;
    indexed["index"] = json!(0);
    let streamed = decode(
        &mut ChatStream::new(Vendor::Generic),
        vec![
            chunk(json!({"role": "assistant", "reasoning_content": "think"})),
            chunk(json!({"content": "Let me look."})),
            chunk(json!({"tool_calls": [indexed]})),
            data(json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]})),
            data(json!({"choices": [], "usage": usage})),
            SseEvent {
                event: None,
                data: "[DONE]".to_owned(),
            },
        ],
    )
    .unwrap();
    assert_eq!(whole, streamed);
}

/// Chat 的错误形状的正文（HTTP 200 带 `error`）：按 chunk 里的错误分类。
#[test]
fn a_chat_error_body_is_classified_like_a_chunk_error() {
    let body = json!({"error": {"code": "insufficient_quota", "message": "quota"}});
    let error = decode(
        &mut ChatStream::new(Vendor::Generic),
        parse(&chat_replay(body)),
    )
    .unwrap_err();
    assert_eq!(error.class, Class::QuotaExhausted);
}

/// Responses 的一个回复（推理、消息、函数调用）与同样内容的流解出相同的事件。
#[test]
fn a_responses_reply_decodes_like_its_stream() {
    let reasoning =
        json!({"type": "reasoning", "summary": [{"type": "summary_text", "text": "plan"}]});
    let message = json!({"type": "message", "content": [{"type": "output_text", "text": "Hi."}]});
    let call = json!({"type": "function_call", "call_id": "call_1", "name": "Read",
        "arguments": "{\"file_path\":\"a\"}"});
    let response = json!({"id": "resp_1", "object": "response", "status": "completed",
        "model": models::GPT_5_4, "output": [reasoning, message, call],
        "usage": {"input_tokens": 10, "output_tokens": 5, "input_tokens_details": {"cached_tokens": 2}}});
    let whole = decode(
        &mut ResponsesStream::new(Vendor::Generic),
        parse(&responses_replay(response.clone())),
    )
    .unwrap();
    let event = |value: Value| data(value);
    let streamed = decode(
        &mut ResponsesStream::new(Vendor::Generic),
        vec![
            event(json!({"type": "response.created", "response": {"id": "resp_1", "model": models::GPT_5_4}})),
            event(json!({"type": "response.output_item.added", "output_index": 0,
                "item": {"id": "rs_1", "type": "reasoning", "summary": []}})),
            event(json!({"type": "response.reasoning_summary_text.delta", "output_index": 0, "item_id": "rs_1", "delta": "plan"})),
            event(json!({"type": "response.output_item.done", "output_index": 0, "item": reasoning})),
            event(json!({"type": "response.output_item.added", "output_index": 1,
                "item": {"id": "msg_1", "type": "message", "content": []}})),
            event(json!({"type": "response.output_text.delta", "output_index": 1, "item_id": "msg_1", "delta": "Hi."})),
            event(json!({"type": "response.output_item.done", "output_index": 1, "item": message})),
            event(json!({"type": "response.output_item.added", "output_index": 2,
                "item": {"type": "function_call", "call_id": "call_1", "name": "Read", "arguments": ""}})),
            event(json!({"type": "response.output_item.done", "output_index": 2, "item": call})),
            event(json!({"type": "response.completed", "response": response})),
        ],
    )
    .unwrap();
    assert_eq!(whole, streamed);
}
