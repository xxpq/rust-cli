//! JS 的 `encodeURIComponent` / `decodeURIComponent`：保留字符、UTF-8、JS 抛错的输入。逐条对拍在 `uri_parity_tests.rs`。

use super::*;

#[test]
fn reserved_characters_stay_and_the_rest_is_utf8_escaped() {
    assert_eq!(
        encode_uri_component("a b/é!*'()~._-"),
        "a%20b%2F%C3%A9!*'()~._-"
    );
    assert_eq!(encode_uri_component("中"), "%E4%B8%AD");
}

#[test]
fn decoding_refuses_what_javascript_throws_on() {
    assert_eq!(
        decode_uri_component("a%20b%2F%C3%A9").as_deref(),
        Some("a b/é")
    );
    for bad in ["%", "%2", "%zz", "%+1", "%E4%B8", "%C0%80"] {
        assert_eq!(decode_uri_component(bad), None, "{bad}");
    }
}
