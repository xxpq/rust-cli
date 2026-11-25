//! JS 的 `encodeURIComponent` / `decodeURIComponent`（docs/specs/rust-cli/mature-libraries.md §1.1 A1）。全工程只有这一份：
//! 账号键、工作区 topic、Artifact URI 与跨会话地址的解码都用它。百分号编码交给 `percent-encoding`，这里只定字符集与 JS 的
//! 解码规则。

use std::borrow::Cow;

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};

/// `encodeURIComponent` 要编码的 ASCII：字母数字与 `- _ . ! ~ * ' ( )` 以外的全部。非 ASCII 的字节库总是编码。
const URI_COMPONENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'!')
    .remove(b'~')
    .remove(b'*')
    .remove(b'\'')
    .remove(b'(')
    .remove(b')');

/// 一个百分号转义里 `%` 后面的十六进制位数。
const ESCAPE_DIGITS: usize = 2;

/// `encodeURIComponent`：其余字符按 UTF-8 写成大写的 `%XX`。
pub fn encode_uri_component(text: &str) -> String {
    utf8_percent_encode(text, URI_COMPONENT).to_string()
}

/// `decodeURIComponent`；JS 抛 URIError 的输入（坏的 `%`，或解出的字节不是 UTF-8）为 None。
pub fn decode_uri_component(text: &str) -> Option<String> {
    // 库遇到后面不是两位十六进制的 `%` 时原样留下（WHATWG URL 的做法），JS 抛错，所以先查一遍。逐位查十六进制：以前的几份
    // 手写解码用 `u8::from_str_radix` 读两位，它认开头的 `+`，`%+1` 被解成字节 1（JS 抛错，工作线 LIB-A 对拍查出）。
    let well_formed = text.match_indices('%').all(|(at, _)| {
        text.as_bytes()
            .get(at + 1..=at + ESCAPE_DIGITS)
            .is_some_and(|digits| digits.iter().all(u8::is_ascii_hexdigit))
    });
    if !well_formed {
        return None;
    }
    percent_decode_str(text)
        .decode_utf8()
        .ok()
        .map(Cow::into_owned)
}

#[cfg(test)]
#[path = "uri_tests.rs"]
mod tests;

#[cfg(test)]
#[cfg(feature = "parity")]
#[rustfmt::skip]
#[path = "uri_parity_tests.rs"]
mod parity_tests;
