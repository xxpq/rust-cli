//! Anthropic 的非流式回退（provider-errors.md §3.6）：请求体的改写；回复换成事件、交给解码器后与流式时相同。

use std::collections::VecDeque;

use rust_cli_base::models;
use rust_cli_kernel_api::message::{ContentBlock, StopReason, Usage};
use rust_cli_kernel_api::model::{BlockDelta, ModelEvent};
use rust_cli_net::sse::SseDecoder;
use serde_json::{Map, Value, json};

use super::*;
use crate::anthropic::stream::AnthropicStream;
use crate::errors::{Class, Vendor, Verdict};
use crate::protocol::StreamDecoder;

fn object(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        other => panic!("{other}"),
    }
}

/// 上限压到 64000，`enabled` 的预算压到上限减一；`adaptive` 不动。
#[test]
fn the_output_cap_and_the_thinking_budget_are_lowered() {
    let mut body = object(json!({"model": "m", "max_tokens": 128_000,
        "thinking": {"type": "enabled", "budget_tokens": 100_000}, "stream": true}));
    edit(&mut body);
    assert_eq!(
        Value::Object(body).to_string(),
        r#"{"model":"m","max_tokens":64000,"thinking":{"type":"enabled","budget_tokens":63999},"stream":false}"#
    );
    let mut small = object(json!({"max_tokens": 1024,
        "thinking": {"type": "enabled", "budget_tokens": 512}, "stream": true}));
    edit(&mut small);
    assert_eq!(small["max_tokens"], 1024);
    assert_eq!(small["thinking"]["budget_tokens"], 512);
    let mut adaptive =
        object(json!({"max_tokens": 128_000, "thinking": {"type": "adaptive"}, "stream": true}));
    edit(&mut adaptive);
    assert_eq!(adaptive["thinking"], json!({"type": "adaptive"}));
}

fn decode(text: &str, vendor: Vendor) -> Result<Vec<ModelEvent>, Verdict> {
    let mut sse = SseDecoder::new();
    let mut decoder = AnthropicStream::new(vendor);
    let mut out = VecDeque::new();
    for event in sse.push(text.as_bytes()).unwrap() {
        decoder.decode(event, &mut out)?;
    }
    assert!(decoder.finished(), "{out:?}");
    Ok(out.into_iter().collect())
}

/// 一条带 thinking、文字、工具调用与服务端块的消息：块的顺序、内容、结束原因与用量都与流式时相同。
#[test]
fn a_message_becomes_the_events_of_the_same_stream() {
    let message = json!({
        "id": "msg_1", "type": "message", "role": "assistant", "model": models::GLM_5_3,
        "content": [
            {"type": "thinking", "thinking": "plan", "signature": "sig"},
            {"type": "text", "text": "Hi."},
            {"type": "tool_use", "id": "toolu_1", "name": "Read", "input": {"file_path": "/a"}},
        ],
        "stop_reason": "tool_use", "stop_sequence": null,
        "usage": {"input_tokens": 20, "cache_creation_input_tokens": 0,
                  "cache_read_input_tokens": 4, "output_tokens": 7}});
    let events = decode(&replay(message), Vendor::Generic).unwrap();
    let usage = Usage {
        input_tokens: 20,
        output_tokens: 7,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 4,
    };
    assert_eq!(
        events,
        [
            ModelEvent::MessageStart {
                id: "msg_1".to_owned(),
                model: models::GLM_5_3.to_owned(),
                usage,
            },
            ModelEvent::BlockStart {
                index: 0,
                block: ContentBlock::Thinking {
                    thinking: String::new(),
                    signature: Some(String::new()),
                },
            },
            ModelEvent::BlockDelta {
                index: 0,
                delta: BlockDelta::Thinking("plan".to_owned()),
            },
            ModelEvent::BlockDelta {
                index: 0,
                delta: BlockDelta::Signature("sig".to_owned()),
            },
            ModelEvent::BlockStop { index: 0 },
            ModelEvent::BlockStart {
                index: 1,
                block: ContentBlock::Text {
                    text: String::new()
                },
            },
            ModelEvent::BlockDelta {
                index: 1,
                delta: BlockDelta::Text("Hi.".to_owned()),
            },
            ModelEvent::BlockStop { index: 1 },
            ModelEvent::BlockStart {
                index: 2,
                block: ContentBlock::ToolUse {
                    id: rust_cli_kernel_api::ids::ToolUseId::new("toolu_1".to_owned()),
                    name: "Read".to_owned(),
                    input: json!({}),
                },
            },
            ModelEvent::BlockDelta {
                index: 2,
                delta: BlockDelta::InputJson(r#"{"file_path":"/a"}"#.to_owned()),
            },
            ModelEvent::BlockStop { index: 2 },
            ModelEvent::MessageDelta {
                stop_reason: Some(StopReason::ToolUse),
                usage,
            },
            ModelEvent::MessageStop,
        ]
    );
}

/// 结束原因照常经分类器换算：`refusal` 是内容拦截（同流式）。
#[test]
fn the_stop_reason_goes_through_the_classifier() {
    let message = json!({"id": "m", "type": "message", "role": "assistant", "model": models::GLM_5_3,
        "content": [], "stop_reason": "refusal", "stop_sequence": null,
        "usage": {"input_tokens": 1, "output_tokens": 0}});
    let events = decode(&replay(message), Vendor::Generic).unwrap();
    assert!(events.contains(&ModelEvent::MessageDelta {
        stop_reason: Some(StopReason::Refusal),
        usage: Usage {
            input_tokens: 1,
            ..Usage::default()
        },
    }));
}

/// 不是一条消息、是错误形状的正文（HTTP 200 带错误）：按流里的错误块分类。
#[test]
fn an_error_shaped_body_is_classified_like_a_stream_error() {
    let mut sse = SseDecoder::new();
    let mut decoder = AnthropicStream::new(Vendor::Generic);
    let body =
        json!({"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}});
    let mut out = VecDeque::new();
    let verdicts: Vec<_> = sse
        .push(replay(body).as_bytes())
        .unwrap()
        .into_iter()
        .filter_map(|event| decoder.decode(event, &mut out).err())
        .collect();
    assert_eq!(verdicts.len(), 1);
    assert_eq!(verdicts[0].class, Class::Overloaded);
}
