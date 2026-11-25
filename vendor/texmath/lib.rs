// TeX 数学子集的 Unicode 排版：上游是 openai/codex（https://github.com/openai/codex）提交 c3d3b142d 的
// `codex-rs/tui/src/markdown_render/math/render.rs` 与 `render/structured.rs`，Apache License 2.0，Copyright 2025 OpenAI。
// 上游文件在 upstream/ 下（fetch.mjs 生成，不手改）；这一层是本工程写的：上游在它的 crate 里用 `crate::width::display_width`，
// 这里给出同样的函数，再把上游的 `render` 交出去。
//! TeX 数学子集排成 Unicode 文字（docs/specs/rust-cli/tui-kit.md §3.5「数学公式与 Mermaid 图」）。

#[path = "upstream/codex-rs/tui/src/markdown_render/math/render.rs"]
mod render;

/// 上游的宽度函数：终端里的列数。同上游：unicode-width，再把两个半角浊音符号（U+FF9E、U+FF9F）各算一列。
pub(crate) mod width {
    use unicode_width::UnicodeWidthStr;

    pub(crate) fn display_width(text: &str) -> usize {
        UnicodeWidthStr::width(text)
            + text
                .chars()
                .filter(|ch| matches!(ch, '\u{FF9E}' | '\u{FF9F}'))
                .count()
    }
}

/// `source`（不含定界符的 TeX）排成 Unicode。`display` 为真时是显示公式（分式上下叠、上下限，可以有几行），否则是行内的
/// 一行写法。不支持或超限时是 `None`；几行之间用 `\n` 连起来。
pub fn render(source: &str, display: bool) -> Option<String> {
    render::render(source, display)
}
