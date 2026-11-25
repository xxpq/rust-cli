use serde_json::json;

use super::*;

/// 没超预算的原样；超了只带开头一半，结尾一半与全文字节数进 `truncated`（01 §19.5）。多字节字符跨在切点上时不切开。
#[test]
fn a_large_output_keeps_the_head_and_the_tail_on_char_boundaries() {
    let small = "x".repeat(TOOL_OUTPUT_PREVIEW_BYTES);
    assert_eq!(preview(&small), (json!({"text": small}), small.as_str()));
    let half = TOOL_OUTPUT_PREVIEW_BYTES / 2;
    // 「é」两个字节：放在切点前一个字节开始，开头只能停在它前面，结尾只能从它后面开始
    let text = format!(
        "{}é{}é{}",
        "a".repeat(half - 1),
        "m".repeat(100),
        "z".repeat(half - 1)
    );
    let (output, head) = preview(&text);
    assert_eq!(head, "a".repeat(half - 1));
    assert_eq!(output["text"], json!("a".repeat(half - 1)));
    assert_eq!(output["truncated"]["totalBytes"], json!(text.len()));
    assert_eq!(output["truncated"]["tail"], json!("z".repeat(half - 1)));
}
