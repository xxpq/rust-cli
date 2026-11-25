use serde_json::json;

use super::*;

#[test]
fn the_dialects_write_the_version_only_for_json_rpc_2() {
    let fields = || vec![("id", json!(1)), ("result", json!({}))];
    assert_eq!(Dialect::Bare.message(fields()), r#"{"id":1,"result":{}}"#);
    assert_eq!(
        Dialect::JsonRpc2.message(fields()),
        r#"{"jsonrpc":"2.0","id":1,"result":{}}"#
    );
}

#[test]
fn content_length_counts_utf8_bytes() {
    let bytes = encode(Framing::ContentLength, r#"{"a":"中"}"#);
    let text = String::from_utf8(bytes).unwrap();
    assert_eq!(text, "Content-Length: 11\r\n\r\n{\"a\":\"中\"}");
    assert_eq!(encode(Framing::Lines, "{}"), b"{}\n");
}

#[tokio::test]
async fn a_frame_that_ends_early_is_broken() {
    let mut input: &[u8] = b"Content-Length: 10\r\n\r\n{}";
    let error = read(&mut input, Framing::ContentLength, 100)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("frame body"), "{error}");
}

#[tokio::test]
async fn an_empty_input_is_the_end() {
    let mut input: &[u8] = b"";
    assert_eq!(
        read(&mut input, Framing::ContentLength, 100).await.unwrap(),
        None
    );
}
