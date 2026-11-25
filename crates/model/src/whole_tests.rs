//! 非流式请求的改写（provider-errors.md §3.6）：请求体只改给出的字段、键序不变；时限与回复类型换成非流式的。

use rust_cli_net::{HeaderValue, HttpRequest, header};
use serde_json::Value;

use super::*;

fn streaming(body: &str) -> HttpRequest {
    let mut request = HttpRequest::post("http://127.0.0.1/v1/chat/completions", body.to_owned())
        .header(header::ACCEPT, header::EVENT_STREAM)
        .header(header::CONTENT_TYPE, header::JSON);
    request.timeout = Some(rust_cli_net::timeout::MODEL);
    request
}

fn accept(request: &HttpRequest) -> Option<&str> {
    request
        .headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(header::ACCEPT))
        .and_then(|(_, value)| match value {
            HeaderValue::Plain(text) => Some(text.as_str()),
            HeaderValue::Secret(_) => None,
        })
}

#[test]
fn the_body_keeps_its_key_order_and_the_request_waits_for_the_whole_reply() {
    let request = streaming(r#"{"model":"m","messages":[],"stream":true,"max_tokens":8}"#);
    let whole = rewrite(&request, |body| {
        body.insert("stream".to_owned(), Value::Bool(false));
    })
    .unwrap();
    assert_eq!(
        std::str::from_utf8(&whole.body).unwrap(),
        r#"{"model":"m","messages":[],"stream":false,"max_tokens":8}"#
    );
    assert_eq!(whole.timeout, Some(rust_cli_net::timeout::MODEL_WHOLE));
    assert_eq!(accept(&whole), Some(header::JSON));
    assert_eq!(whole.url, request.url);
}

/// Provider 补丁把请求体换成了不是对象的值：改写不了，不改非流式。
#[test]
fn a_body_that_is_not_an_object_is_not_rewritten() {
    assert!(rewrite(&streaming("[1,2]"), |_| {}).is_none());
}
