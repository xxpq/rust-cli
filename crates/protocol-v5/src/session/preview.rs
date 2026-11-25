//! 工具行运行中的输出预览（protocol-v5.md §19.4；02 §5 的 `ToolProgress → entity.put`）：内核每段输出一条进度，带这一段的尾部与
//! 到这一段为止的总字节数。这里按总字节数把接得上的尾部拼成一个有上限的缓冲，算出 v4 `executionOutputPreviewSchema` 形状的
//! `outputPreview`（`text` 最后 5 行、`fullText` 最后 100 行，照 v4 的 CLI）。缓冲只在投影的内存里，随投影建、随投影丢。

use serde_json::{Value, json};

use crate::consts::{
    OUTPUT_PREVIEW_FULL_LINES, OUTPUT_PREVIEW_MAX_BYTES, OUTPUT_PREVIEW_SHORT_LINES,
};

/// 进度里的输出与总字节数（Bash 的 `bash_progress`）。
const OUTPUT: &str = "output";
const TOTAL_BYTES: &str = "totalBytes";

/// 带输出的工具进度：`(这一段的尾部, 到这一段为止的总字节数)`。别的进度（子代理的 `{agentId, event}`）不是输出。
pub(crate) fn output_of(data: &Value) -> Option<(&str, u64)> {
    Some((data[OUTPUT].as_str()?, data[TOTAL_BYTES].as_u64()?))
}

/// 一个工具的尾部缓冲。
#[derive(Debug, Default)]
pub(crate) struct Tail {
    text: String,
    /// 缓冲末尾对应的总字节数。
    end: u64,
    /// 上一次给出的总行数（估计时不往回退，同 v4 的 CLI）。
    lines: u64,
}

impl Tail {
    /// 接上（或换成）这一段，给出工具行的 `outputPreview`。
    pub(crate) fn push(&mut self, output: &str, total: u64) -> Value {
        // 接得上：这一段的新字节数不超过 `output` 的字节数（`output` 是有损解码，可能多出几个字节）。接不上（中间有缺：一段只给了尾部，
        // 或补发跳过了中间的进度）就从这一段重新开始，不把不连续的输出拼在一起。只看总字节数，不看时间。
        if total.saturating_sub(self.end) > output.len() as u64 {
            self.text.clear();
        }
        self.text.push_str(output);
        if let Some(mut cut) = self.text.len().checked_sub(OUTPUT_PREVIEW_MAX_BYTES) {
            while !self.text.is_char_boundary(cut) {
                cut += 1;
            }
            self.text.drain(..cut);
        }
        self.end = total;
        let bytes = self.text.len() as u64;
        let lines = segments(&self.text);
        let estimated = bytes > 0 && bytes < total;
        // 缓冲不是全部输出时按字节比例估计总行数（同 v4 的 CLI 的 `buildBashOutputPreview`），四舍五入
        self.lines = match bytes {
            0 => self.lines,
            _ if estimated => self
                .lines
                .max((total.saturating_mul(lines) + bytes / 2) / bytes),
            _ => lines,
        };
        json!({
            "text": last_segments(&self.text, OUTPUT_PREVIEW_SHORT_LINES),
            "fullText": last_segments(&self.text, OUTPUT_PREVIEW_FULL_LINES),
            "totalLines": self.lines,
            "totalBytes": total,
            "linesEstimated": estimated,
        })
    }
}

/// 按 `\n` 分的段数（末尾的换行之后算一个空段，同 v4 的 CLI）；空文本没有段。
fn segments(text: &str) -> u64 {
    if text.is_empty() {
        0
    } else {
        text.matches('\n').count() as u64 + 1
    }
}

/// 最后 `count` 段。
fn last_segments(text: &str, count: usize) -> &str {
    match text.rmatch_indices('\n').nth(count - 1) {
        Some((at, _)) => &text[at + 1..],
        None => text,
    }
}

#[cfg(test)]
#[path = "preview_tests.rs"]
mod tests;
