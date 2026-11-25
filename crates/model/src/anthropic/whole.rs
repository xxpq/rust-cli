//! Anthropic Messages 的非流式回退（docs/specs/rust-cli/provider-errors.md §3.6）：请求改成 `stream: false` 并压低输出上限；
//! 回复的一条消息换成同一次流式响应会产生的事件，交给同一个解码器。

use serde_json::{Map, Value, json};

use crate::whole::push_event;

/// 非流式请求的输出上限：整条回复要在一次请求的时限里到齐，上限压到 64000（provider-errors.md §3.6）。
const MAX_TOKENS: u64 = 64_000;

/// 改写请求体：`stream: false`；`max_tokens` 压到 [`MAX_TOKENS`]；`thinking.type` 是 `enabled` 时预算压到上限减一（预算必须
/// 小于 `max_tokens`）。键序不变（`stream` 原来就在最后）。
pub(super) fn edit(body: &mut Map<String, Value>) {
    body.insert("stream".to_owned(), Value::Bool(false));
    let Some(max_tokens) = body.get("max_tokens").and_then(Value::as_u64) else {
        return;
    };
    let cap = max_tokens.min(MAX_TOKENS);
    body.insert("max_tokens".to_owned(), cap.into());
    if let Some(thinking) = body.get_mut("thinking").and_then(Value::as_object_mut)
        && thinking.get("type").and_then(Value::as_str) == Some("enabled")
        && let Some(budget) = thinking.get("budget_tokens").and_then(Value::as_u64)
    {
        thinking.insert(
            "budget_tokens".to_owned(),
            budget.min(cap.saturating_sub(1)).into(),
        );
    }
}

/// 一条消息换成事件：开始（不带内容）、每块的开始 / 增量 / 结束、结束原因与用量、结束。块的开始事件带着整块：解码器对文字、
/// thinking、tool_use 不看开始里的内容、只看增量（同 SDK），对服务端块只看开始；增量按块的类型补出。
/// 不是一条消息（错误形状等）时原样作为一个不带事件名的块：错误形状的由解码器按流里的错误块分类，别的它不认，流按截断结束。
pub(super) fn replay(mut message: Value) -> String {
    let mut out = String::new();
    if message.get("type").and_then(Value::as_str) != Some("message") {
        push_event(&mut out, None, &message);
        return out;
    }
    let content = take(&mut message, "content");
    let stop_reason = take(&mut message, "stop_reason");
    let stop_sequence = take(&mut message, "stop_sequence");
    let usage = message.get("usage").cloned().unwrap_or_default();
    message["content"] = json!([]);
    push_event(
        &mut out,
        Some("message_start"),
        &json!({"type": "message_start", "message": message}),
    );
    for (index, block) in content.as_array().into_iter().flatten().enumerate() {
        push_event(
            &mut out,
            Some("content_block_start"),
            &json!({"type": "content_block_start", "index": index, "content_block": block}),
        );
        for delta in deltas(block) {
            push_event(
                &mut out,
                Some("content_block_delta"),
                &json!({"type": "content_block_delta", "index": index, "delta": delta}),
            );
        }
        push_event(
            &mut out,
            Some("content_block_stop"),
            &json!({"type": "content_block_stop", "index": index}),
        );
    }
    push_event(
        &mut out,
        Some("message_delta"),
        &json!({"type": "message_delta",
            "delta": {"stop_reason": stop_reason, "stop_sequence": stop_sequence},
            "usage": usage}),
    );
    push_event(
        &mut out,
        Some("message_stop"),
        &json!({"type": "message_stop"}),
    );
    out
}

fn take(message: &mut Value, field: &str) -> Value {
    message.get_mut(field).map(Value::take).unwrap_or_default()
}

/// 一块的增量：文字、thinking 与签名、tool_use 的输入（整段 JSON）；别的块没有增量。
fn deltas(block: &Value) -> Vec<Value> {
    let text = |field: &str| {
        block
            .get(field)
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
    };
    match block.get("type").and_then(Value::as_str) {
        Some("text") => text("text")
            .map(|text| json!({"type": "text_delta", "text": text}))
            .into_iter()
            .collect(),
        Some("thinking") => [
            text("thinking")
                .map(|thinking| json!({"type": "thinking_delta", "thinking": thinking})),
            text("signature")
                .map(|signature| json!({"type": "signature_delta", "signature": signature})),
        ]
        .into_iter()
        .flatten()
        .collect(),
        Some("tool_use") => block
            .get("input")
            .map(|input| json!({"type": "input_json_delta", "partial_json": input.to_string()}))
            .into_iter()
            .collect(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
#[path = "whole_tests.rs"]
mod tests;
