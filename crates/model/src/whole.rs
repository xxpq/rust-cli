//! 非流式回退（G，docs/specs/rust-cli/provider-errors.md §3.6）三种协议共用的部分：把已编码的流式请求改写成非流式请求，
//! 与把换算出的事件写成 SSE 文本。各协议改什么字段、回复怎么换成事件写在各自的模块里。
//!
//! 回复换成 SSE 文本交给同一个解码器（而不是另写一份解码）：结束原因与错误块照常经厂商的分类器，交出边界与空回复的判定
//! 也只有一处（`transport::read`）。

use rust_cli_base::js::json::stringify;
use rust_cli_net::{Bytes, HttpRequest, header};
use serde_json::{Map, Value};

/// 非流式请求要求的回复类型（流式请求的 OpenAI 写的是 `text/event-stream`）。
const ACCEPT: &str = header::JSON;

/// 改写已编码的流式请求：请求体解析一次、交给 `edit` 改字段（键序不变），再按 `JSON.stringify` 写回；时限换成非流式的。
/// 请求体不是 JSON 对象（Provider 补丁把它整个换掉了）时为 None：这次不改非流式。
pub(crate) fn rewrite(
    request: &HttpRequest,
    edit: impl FnOnce(&mut Map<String, Value>),
) -> Option<HttpRequest> {
    let Ok(Value::Object(mut body)) = serde_json::from_slice::<Value>(&request.body) else {
        return None;
    };
    edit(&mut body);
    let mut whole = request.clone();
    whole.body = Bytes::from(stringify(&Value::Object(body)));
    whole.timeout = Some(rust_cli_net::timeout::MODEL_WHOLE);
    for (name, value) in &mut whole.headers {
        if name.eq_ignore_ascii_case(header::ACCEPT) {
            *value = ACCEPT.into();
        }
    }
    Some(whole)
}

/// 追加一个 SSE 事件：`name` 为 None 时不带事件名（OpenAI 的写法）。`data` 写成一行 JSON。
pub(crate) fn push_event(out: &mut String, name: Option<&str>, data: &Value) {
    if let Some(name) = name {
        out.push_str("event: ");
        out.push_str(name);
        out.push('\n');
    }
    out.push_str("data: ");
    out.push_str(&data.to_string());
    out.push_str("\n\n");
}

#[cfg(test)]
#[path = "whole_tests.rs"]
mod tests;
