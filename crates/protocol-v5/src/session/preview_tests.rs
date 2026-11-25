//! 工具行运行中的输出预览（protocol-v5.md §19.4）：接得上时拼接、接不上时换成这一段、只留上限字节、5 / 100 行、估计行数。

use serde_json::json;

use super::*;
use crate::consts::{OUTPUT_PREVIEW_FULL_LINES, OUTPUT_PREVIEW_MAX_BYTES};

fn numbered(from: usize, to: usize) -> String {
    (from..=to).map(|n| format!("line-{n}\n")).collect()
}

#[test]
fn only_output_progress_counts() {
    let bash = json!({"type": "bash_progress", "output": "a\n", "totalBytes": 2, "taskId": "t"});
    assert_eq!(output_of(&bash), Some(("a\n", 2)));
    // 子代理的进度（`{agentId, event}`）不是输出
    assert_eq!(output_of(&json!({"agentId": "agent_1", "event": {}})), None);
    assert_eq!(output_of(&json!({"output": "a"})), None);
}

#[test]
fn contiguous_chunks_join_and_the_lines_are_counted() {
    let mut tail = Tail::default();
    let first = tail.push("a\nb\n", 4);
    assert_eq!(
        first,
        json!({"text": "a\nb\n", "fullText": "a\nb\n", "totalLines": 3, "totalBytes": 4, "linesEstimated": false})
    );
    let second = tail.push("c\n", 6);
    assert_eq!(second["fullText"], json!("a\nb\nc\n"));
    assert_eq!(second["totalLines"], json!(4));
    assert_eq!(second["linesEstimated"], json!(false));
}

#[test]
fn the_short_text_is_five_lines_and_the_full_text_a_hundred() {
    let mut tail = Tail::default();
    let output = numbered(1, 120);
    let preview = tail.push(&output, output.len() as u64);
    // 末尾的换行之后算一个空段（同 v4 的 CLI）：最后 5 段是 line-117 … line-120 与空段
    assert_eq!(preview["text"], json!(numbered(117, 120)));
    let full = preview["fullText"].as_str().unwrap();
    assert_eq!(full.split('\n').count(), OUTPUT_PREVIEW_FULL_LINES);
    assert!(full.starts_with("line-22\n"), "{full}");
}

/// 一段超过尾部上限只给了尾部、或补发跳过了中间的：`totalBytes` 接不上，缓冲换成这一段，不把不连续的输出拼在一起。
#[test]
fn a_gap_starts_over_from_the_new_chunk() {
    let mut tail = Tail::default();
    tail.push("old\n", 4);
    let preview = tail.push("new\n", 104);
    assert_eq!(preview["fullText"], json!("new\n"));
    assert_eq!(preview["totalBytes"], json!(104));
    assert_eq!(preview["linesEstimated"], json!(true));
    // 104 / 4 × 2 段
    assert_eq!(preview["totalLines"], json!(52));
}

#[test]
fn the_buffer_keeps_the_last_bytes_on_a_char_boundary() {
    let mut tail = Tail::default();
    let output = "é".repeat(OUTPUT_PREVIEW_MAX_BYTES);
    let preview = tail.push(&output, output.len() as u64);
    let full = preview["fullText"].as_str().unwrap();
    assert!(full.len() <= OUTPUT_PREVIEW_MAX_BYTES);
    assert!(full.chars().all(|c| c == 'é'));
    assert_eq!(preview["linesEstimated"], json!(true));
}
