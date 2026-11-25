//! OpenAI 两种协议的非流式回退（docs/specs/rust-cli/provider-errors.md §3.6），按各自协议的非流式写法：请求改成
//! `stream: false`（Chat 另去掉只给流式的 `stream_options`）；回复换成流式会产生的事件，交给同一个解码器。

use serde_json::{Map, Value, json};

use crate::whole::push_event;

/// Chat Completions 流的结束标记。
const DONE: &str = "data: [DONE]\n\n";

/// Chat：`stream: false`，去掉 `stream_options`（非流式请求带它时端点报参数错误）。
pub(super) fn chat_edit(body: &mut Map<String, Value>) {
    body.insert("stream".to_owned(), Value::Bool(false));
    body.shift_remove("stream_options");
}

/// Responses：`stream: false`。
pub(super) fn responses_edit(body: &mut Map<String, Value>) {
    body.insert("stream".to_owned(), Value::Bool(false));
}

/// Chat 的回复换成一个 chunk：每个 choice 的 `message` 就是流式时累计的 `delta`（正文、推理、工具调用），`finish_reason`
/// 与 `usage` 同名；之后是 `[DONE]`。错误形状的正文照样是一个 chunk，解码器按 chunk 里的 `error` 分类。
pub(super) fn chat_replay(mut response: Value) -> String {
    for choice in response
        .get_mut("choices")
        .and_then(Value::as_array_mut)
        .into_iter()
        .flatten()
    {
        if let Some(choice) = choice.as_object_mut()
            && let Some(message) = choice.shift_remove("message")
        {
            choice.insert("delta".to_owned(), message);
        }
    }
    let mut out = String::new();
    push_event(&mut out, None, &response);
    out.push_str(DONE);
    out
}

/// Responses 的回复换成事件：`response.created`、每个输出项的 `added` 与 `done`（解码器在 `done` 时从完整的项补出正文、推理与
/// 函数参数）、按 `status` 的结束事件。没有 `output` 的正文（错误形状）按流里的 `error` 事件。
pub(super) fn responses_replay(response: Value) -> String {
    let mut out = String::new();
    let Some(output) = response.get("output").and_then(Value::as_array) else {
        let error = response.get("error").cloned().unwrap_or(response);
        push_event(&mut out, None, &json!({"type": "error", "error": error}));
        return out;
    };
    push_event(
        &mut out,
        None,
        &json!({"type": "response.created", "response": response}),
    );
    for (index, item) in output.iter().enumerate() {
        for kind in ["response.output_item.added", "response.output_item.done"] {
            push_event(
                &mut out,
                None,
                &json!({"type": kind, "output_index": index, "item": item}),
            );
        }
    }
    let end = match response.get("status").and_then(Value::as_str) {
        Some("failed") => "response.failed",
        Some("incomplete") => "response.incomplete",
        _ => "response.completed",
    };
    push_event(&mut out, None, &json!({"type": end, "response": response}));
    out
}

#[cfg(test)]
#[path = "whole_tests.rs"]
mod tests;
