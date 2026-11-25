//! 正文里别处的网址（docs/specs/rust-cli/tui-select.md §10「别处的网址也能点」，用户决定 2026-11-12：“哪里的网址都能点”）：
//! 块排好之后，把认出的网址那几格的 `Decor.link` 设成它。于是单击、悬停与终端自己的 Ctrl / Cmd + 点击都同回答里的
//! 链接（回答的链接由 Markdown 设，不过这一步）。
//!
//! 认法是 kit 的 [`plain_urls`]（与回答里的裸网址共用 `url_end`）。软折行（`StyledLine.cont`）的几行先接成一行再认，
//! 跨行的网址每一段都带整条网址；画不下、截成 `…` 的网址不算（不知道整条是什么）。
use std::borrow::Cow;

use rust_cli_tui_kit::markdown::inline_parser::plain_urls;
use rust_cli_tui_kit::render::width::display_width;
use rust_cli_tui_kit::render::{Decor, Span, StyledLine};

/// 截断的记号（kit `lay_line`、`lines::truncate`）：网址带着它或紧接着它时是截断的。
const CUT: char = '…';

/// 接起来的一行里一个字符的来处：第几行、第几段、段里的字节起止。折行处补的空格没有来处。
#[derive(Clone, Copy)]
struct At {
    line: usize,
    span: usize,
    from: usize,
    to: usize,
}

/// 一处要设链接的字：第几行、第几段、段里的字节起止与网址。
struct Mark {
    line: usize,
    span: usize,
    from: usize,
    to: usize,
    url: String,
}

/// 给 `lines` 里认出的网址设 `Decor.link`。已经是链接的字（回答里的）不动。
pub fn mark(lines: &mut [StyledLine]) {
    // 多数块没有网址：不接行、不复制。
    let any = lines
        .iter()
        .any(|line| line.spans.iter().any(|span| span.text.contains("http")));
    if !any {
        return;
    }
    let mut marks = Vec::new();
    let mut start = 0;
    while start < lines.len() {
        let mut end = start + 1;
        while end < lines.len() && lines[end].cont.is_some() {
            end += 1;
        }
        find(lines, start, end, &mut marks);
        start = end;
    }
    for (index, line) in lines.iter_mut().enumerate() {
        let here: Vec<&Mark> = marks.iter().filter(|m| m.line == index).collect();
        if !here.is_empty() {
            split(line, &here);
        }
    }
}

/// 第 `start..end` 行（一行与它的软折行）里的网址。
fn find(lines: &[StyledLine], start: usize, end: usize, marks: &mut Vec<Mark>) {
    let mut text = String::new();
    let mut from: Vec<Option<At>> = Vec::new();
    for (index, line) in lines.iter().enumerate().take(end).skip(start) {
        // 续行从格子 `at` 起是接着上一行的字，前面是折行的缩进；折行处丢了空格时补一个（网址到那里为止）。
        let skip = match line.cont.filter(|_| index > start) {
            Some(cont) => {
                if cont.space {
                    text.push(' ');
                    from.push(None);
                }
                cont.at
            }
            None => 0,
        };
        let mut col = 0;
        for (span_index, span) in line.spans.iter().enumerate() {
            for (at, c) in span.text.char_indices() {
                if col >= skip {
                    text.push(c);
                    from.push(Some(At {
                        line: index,
                        span: span_index,
                        from: at,
                        to: at + c.len_utf8(),
                    }));
                }
                col += display_width(c);
            }
        }
    }
    let starts: Vec<usize> = text.char_indices().map(|(at, _)| at).collect();
    for range in plain_urls(&text) {
        let url = &text[range.clone()];
        let first = starts.partition_point(|&at| at < range.start);
        let last = starts.partition_point(|&at| at < range.end);
        let cut = url.contains(CUT) || text[range.end..].starts_with(CUT);
        let chars = &from[first..last];
        let linked = chars
            .iter()
            .flatten()
            .any(|at| !lines[at.line].spans[at.span].decor.link.is_empty());
        if cut || linked {
            continue;
        }
        for at in chars.iter().flatten() {
            match marks.last_mut() {
                Some(m)
                    if m.line == at.line
                        && m.span == at.span
                        && m.to == at.from
                        && m.url == url =>
                {
                    m.to = at.to;
                }
                _ => marks.push(Mark {
                    line: at.line,
                    span: at.span,
                    from: at.from,
                    to: at.to,
                    url: url.to_owned(),
                }),
            }
        }
    }
}

/// 把一行里带链接的字切成自己的段，带上网址；别的字、样式与下划线的颜色不变。
fn split(line: &mut StyledLine, marks: &[&Mark]) {
    let mut spans = Vec::with_capacity(line.spans.len() + marks.len() * 2);
    for (index, span) in line.spans.iter().enumerate() {
        let mut here: Vec<&&Mark> = marks.iter().filter(|m| m.span == index).collect();
        if here.is_empty() {
            spans.push(span.clone());
            continue;
        }
        here.sort_by_key(|m| m.from);
        let text = span.text.as_ref();
        let piece = |part: &str, decor: Decor| Span {
            text: Cow::Owned(part.to_owned()),
            style: span.style,
            decor,
        };
        let mut at = 0;
        for m in here {
            if m.from > at {
                spans.push(piece(&text[at..m.from], span.decor.clone()));
            }
            let decor = Decor {
                link: m.url.clone(),
                ..span.decor.clone()
            };
            spans.push(piece(&text[m.from..m.to], decor));
            at = m.to;
        }
        if at < text.len() {
            spans.push(piece(&text[at..], span.decor.clone()));
        }
    }
    line.spans = spans;
}

#[cfg(test)]
#[path = "links_tests.rs"]
mod tests;
