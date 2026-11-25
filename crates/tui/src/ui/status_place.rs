//! 状态栏第 2 行（docs/specs/rust-cli/tui-display.md §2.6「第 2 行」，用户要求 2026-11-12）：左边是工作目录、分支与未提交的
//! 增删行数，右边是会话标题（翻历史时 `History k/n`，查看子代理时它的描述）。
//!
//! 标题以前在板上面任务行的右边，输入框上方那一句有字、板上开着层、对话还没有正文时都不画（用户报告“莫名其妙的，输入框
//! 右上角的 title 就不显示了，冷启动的时候不会显示”）。移到这一行之后，它只看有没有字。
//!
//! 分支与行数只来自内核（`Kernel::working_tree`）：runner 问，回执按序号写进 [`WorkingTrees`]；这里只排，不另算。
use rust_cli_kernel_api::env_git::WorkingTree;
use rust_cli_tui_kit::app::widgets::key_hints::edge;
use rust_cli_tui_kit::app::widgets::status_bar::SEPARATOR;
use rust_cli_tui_kit::render::width::string_width;
use rust_cli_tui_kit::render::{Span, Style, StyledLine};
use rust_cli_tui_kit::theme::theme::Theme;

use super::app::App;

/// 左边从第 3 列起（同第 1 行，kit `status_bar` 的 `LEAD`）。
const LEAD: usize = 3;
/// 左右两边至少隔这么多列（同第 1 行）。
const GAP: usize = 3;
/// 标题至少要这么多列（比它短的标题要它自己的宽），不然不写。
const TITLE_MIN: usize = 10;
/// 目录缩短时中间的记号。
const ELIDED: &str = "…";
/// 少于这么多段的目录不缩（`~/a/b` 缩不短）。
const SHORT_PARTS: usize = 4;

/// runner 写下的最后一份工作树，带问的时候的工作区根。根换了时旧的不画：不用在每个换目录的地方各清一次（启动、切换会话、
/// `/cd`、EnterWorktree、草稿换文件夹都只是根变了）。回执带序号，晚到的旧回执丢掉，不用超时。
#[derive(Debug, Default)]
pub struct WorkingTrees {
    seq: u64,
    asked: Option<String>,
    shown: Option<(String, WorkingTree)>,
}

impl WorkingTrees {
    /// 现在的根还没问过。
    pub fn stale(&self, root: &str) -> bool {
        self.asked.as_deref() != Some(root)
    }

    /// 发一次新查询：记下根，序号加一并返回。
    pub fn begin(&mut self, root: &str) -> u64 {
        self.seq += 1;
        self.asked = Some(root.to_owned());
        self.seq
    }

    /// 查询的回执：序号是最新的才收下（返回是否收下）。`tree` 为 None（内核不支持）时只画目录。
    pub fn arrived(&mut self, seq: u64, root: String, tree: Option<WorkingTree>) -> bool {
        if seq != self.seq {
            return false;
        }
        self.shown = tree.map(|tree| (root, tree));
        true
    }

    /// 根 `root` 的分支与行数（没有回执或回执属于别的根时没有）。
    pub fn of(&self, root: &str) -> Option<&WorkingTree> {
        self.shown
            .as_ref()
            .filter(|(at, _)| at == root)
            .map(|(_, tree)| tree)
    }
}

/// 右边写的字（[`title`]）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Title {
    pub text: String,
    pub style: Style,
    /// 子代理的描述：窄时比分支后走（看的是谁要一直知道）。
    pub stays: bool,
}

/// 右边写的字（tui-tasks.md §2.3）：查看子代理时它的描述（`agent` 墨），浏览输入历史时 `History k/n`，否则会话标题（fg3）；
/// 没有字时没有。
pub fn title(app: &App) -> Option<Title> {
    let fg3 = app.ink.theme.fg3;
    let (text, style, stays) = if let Some(viewing) = &app.viewing {
        let words: Vec<&str> = viewing.description.split_whitespace().collect();
        (words.join(" "), app.ink.subagent_ink(), true)
    } else if let Some((index, total)) = app.composer.history_position() {
        (format!("History {index}/{total}"), fg3, false)
    } else {
        (app.view.title().trim().to_owned(), fg3, false)
    };
    (!text.is_empty()).then_some(Title { text, style, stays })
}

/// 第 2 行，宽 `width`。窄时（§2.6）：先丢行数，再把目录缩成第一段与最后一个文件夹，再丢标题，再丢分支，目录最后走；子代理
/// 的描述比分支后走。
pub fn row(app: &App, width: usize) -> StyledLine {
    let theme = &app.ink.theme;
    let end = edge(width);
    let full = app.workspace_label();
    let short = shorten(&full);
    let tree = app.working_tree.of(app.workspace());
    let branch = tree.and_then(|tree| tree.branch.as_deref());
    let lines = tree.map(|tree| (tree.added, tree.removed));
    let title = title(app);
    let stays = title.as_ref().is_some_and(|title| title.stays);
    // 标题至少要的列数：比 TITLE_MIN 短的标题要它自己的宽。
    let least = title
        .as_ref()
        .map_or(0, |title| string_width(&title.text).clamp(1, TITLE_MIN));
    // 从最想要的写法往下试：（左边，标题至少要的列数；None 是这一种不带标题）。子代理的描述在分支走了之后还留着，有一列
    // 就写。
    let tries = [
        (left(theme, &full, branch, lines), Some(least)),
        (left(theme, &full, branch, None), Some(least)),
        (left(theme, &short, branch, None), Some(least)),
        if stays {
            (left(theme, &short, None, None), Some(1))
        } else {
            (left(theme, &short, branch, None), None)
        },
        (left(theme, &short, None, None), None),
    ];
    for (spans, least) in tries {
        let used = LEAD + width_of(&spans);
        match (title.as_ref(), least) {
            (Some(title), Some(least)) => {
                let room = end.saturating_sub(used + GAP);
                if room >= least {
                    let shown = super::lines::truncate(&title.text, room);
                    let pad = end - used - string_width(&shown);
                    return line(spans, Some((pad, Span::new(shown, title.style))));
                }
            }
            _ if used <= end => return line(spans, None),
            _ => {}
        }
    }
    // 缩短的目录也放不下：截掉。
    let spans = left(theme, &short, None, None);
    super::lines::truncate_line(line(spans, None), end)
}

/// 一行：第 3 列起的左边，与右边的（空格数，标题）。
fn line(left: Vec<Span>, right: Option<(usize, Span)>) -> StyledLine {
    let mut spans = vec![Span::plain(" ".repeat(LEAD))];
    spans.extend(left);
    if let Some((pad, title)) = right {
        spans.push(Span::plain(" ".repeat(pad)));
        spans.push(title);
    }
    StyledLine::new(spans)
}

/// 左边的一种写法：目录（fg3）、` · ` 分支（fg3，有时）、行数（有时；有分支时隔一格，否则隔 ` · `）。
fn left(theme: &Theme, dir: &str, branch: Option<&str>, lines: Option<(u64, u64)>) -> Vec<Span> {
    let mut out = vec![Span::new(dir.to_owned(), theme.fg3)];
    if let Some(branch) = branch {
        out.push(Span::new(SEPARATOR, theme.line));
        out.push(Span::new(branch.to_owned(), theme.fg3));
    }
    let counts = lines.map(|(added, removed)| counts(theme, added, removed));
    if let Some(counts) = counts.filter(|counts| !counts.is_empty()) {
        out.push(if branch.is_some() {
            Span::plain(" ")
        } else {
            Span::new(SEPARATOR, theme.line)
        });
        out.extend(counts);
    }
    out
}

/// `+181 −31`：增 ok 色、删 err 色；一边为 0 时不写那一边，都为 0 时没有。
fn counts(theme: &Theme, added: u64, removed: u64) -> Vec<Span> {
    let mut out = Vec::new();
    if added > 0 {
        out.push(Span::new(format!("+{added}"), Style::fg(theme.ok)));
    }
    if removed > 0 {
        if !out.is_empty() {
            out.push(Span::plain(" "));
        }
        out.push(Span::new(format!("−{removed}"), Style::fg(theme.err)));
    }
    out
}

/// `~/a/b/c` → `~/…/c`、`C:\Users\dev\x` → `C:\…\x`：第一段与最后一个文件夹。段少时不变。
fn shorten(path: &str) -> String {
    let sep = if path.contains('/') { '/' } else { '\\' };
    let parts: Vec<&str> = path.split(sep).collect();
    match (parts.first(), parts.last()) {
        (Some(first), Some(last)) if parts.len() >= SHORT_PARTS => {
            format!("{first}{sep}{ELIDED}{sep}{last}")
        }
        _ => path.to_owned(),
    }
}

fn width_of(spans: &[Span]) -> usize {
    spans.iter().map(|span| string_width(&span.text)).sum()
}

#[cfg(test)]
#[path = "status_place_tests.rs"]
mod tests;
