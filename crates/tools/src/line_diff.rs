//! 改动片段（docs/specs/rust-cli/tools-files.md §4.1）：jsdiff 9 `structuredPatch(old, new, {context: 8})` 的每个 hunk 只要新文件里的行：
//! 去掉删除行，从 hunk 在旧文件里的起始行开始编号。diff 本身在 base 的 `line_diff`（session 的回合文件 diff 也用它）。

use rust_cli_base::line_diff::{self, NO_NEWLINE_MARKER};

use crate::common::utf16_len;
use crate::read_support::number_lines;

/// 片段里每个 hunk 前后的上下文行数。
const CONTEXT: usize = 8;
/// 一个文件的片段最多这么多 UTF-16 单元，超过时在这之前的最后一个换行处截断。
const MAX_SNIPPET: usize = 8192;

/// 改动片段：每个 hunk 只留新文件里的行，从 hunk 在旧文件里的起始行编号，hunk 之间是 `\n...\n`；太长时截断。
/// 算不出 diff（超过编辑距离上限）或没有可显示的行时为空。
pub(crate) fn snippet(old: &str, new: &str) -> String {
    let Some(hunks) = line_diff::structured_patch(old, new, CONTEXT) else {
        return String::new();
    };
    let text = hunks
        .iter()
        .map(|hunk| {
            let content = hunk
                .lines
                .iter()
                .filter(|line| !line.starts_with('-') && line.as_str() != NO_NEWLINE_MARKER)
                .map(|line| &line[1..])
                .collect::<Vec<_>>()
                .join("\n");
            if content.is_empty() {
                String::new()
            } else {
                number_lines(&content, hunk.old_start as u64)
            }
        })
        .collect::<Vec<_>>()
        .join("\n...\n");
    truncate(text)
}

/// 超过 [`MAX_SNIPPET`] 个 UTF-16 单元时：在这之前（含）的最后一个换行处截断（没有换行时按长度截），接上没显示的行数。
fn truncate(text: String) -> String {
    if utf16_len(&text) <= MAX_SNIPPET {
        return text;
    }
    let limit = byte_at_utf16(&text, MAX_SNIPPET);
    // 从第 8192 个单元往前找换行（含第 8192 个单元本身）。
    let search_end = (limit + 1).min(text.len());
    let search_end = (search_end..=text.len())
        .find(|end| text.is_char_boundary(*end))
        .unwrap_or(text.len());
    let kept = match text[..search_end].rfind('\n') {
        Some(at) if at > 0 => &text[..at],
        _ => &text[..limit],
    };
    let hidden = text[(kept.len() + 1).min(text.len())..]
        .matches('\n')
        .count()
        + 1;
    format!("{kept}\n\n... [{hidden} lines truncated] ...")
}

/// 第 `units` 个 UTF-16 单元处的字节位置（落在一个字符中间时取它之前）。
fn byte_at_utf16(text: &str, units: usize) -> usize {
    let mut counted = 0;
    for (index, ch) in text.char_indices() {
        if counted + ch.len_utf16() > units {
            return index;
        }
        counted += ch.len_utf16();
    }
    text.len()
}

#[cfg(test)]
#[path = "line_diff_tests.rs"]
mod tests;
