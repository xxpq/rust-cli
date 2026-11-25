//! 状态栏的「多少」（docs/specs/rust-cli/tui-display.md §2.6：速度、缓存命中、上下文占用）与上下文的页用的几样：组成的
//! 合并与档（[`groups`]、[`step`]），模型没有窗口时的 Context 一行（[`context_rows`]），工作的时长（[`working`]）。纯的；
//! 入参是中性的 [`Window`] 与 [`Usage`]。最早移植自 Puffin 的 shell/stats.rs；`/stats` 的数字卡 2026-11-12 删掉（三个页面
//! 改成设置的段，tui-panels.md §4），只留还有人用的部分。
use crate::markdown::render::INDENT;
use crate::render::width::string_width;
use crate::render::{Color, Span, Style};
use crate::theme::rgb::Rgb;
use crate::theme::theme::Theme;

use super::fade;
use super::fit::{MEASURE_MARGIN, MEASURE_MAX, count, pad, round};
use super::status_bar::Part;

/// 上下文窗口：用掉的与窗口的 token，自动压缩的位置（没有时为 `None`），组成（来源与字符数，只在 `/stats` 打开时有）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Window {
    pub used: u64,
    pub max: u64,
    pub compact_at: Option<u64>,
    pub breakdown: Vec<(String, u64)>,
}

/// Context 一行要的：窗口（模型不说时为 `None`）与最近一次调用的 token。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Usage {
    pub context: Option<Window>,
    pub last_call_tokens: Option<u64>,
}

// ---- 状态栏 ----

/// 状态栏右边「多少」要的（docs/specs/rust-cli/tui-display.md §2.6）：画好的速度（为空时不显示）、缓存命中率（0–1）、上下文
/// 窗口，上下文的数变了多少 tick（从那时淡入），两个百分数的名字（壳按语言给：`cache` / `缓存`、`ctx` / `上下文`）。
#[derive(Clone, Debug)]
pub struct Figures<'a> {
    pub speed: Vec<Span>,
    pub cache: Option<f64>,
    pub context: Option<&'a Window>,
    pub context_age: i64,
    pub cache_word: &'a str,
    pub context_word: &'a str,
}

/// 「多少」的部分，从左到右：速度（名次 30）、缓存命中（40）、上下文占用（85）。2026-11-12 起（用户要求）：上下文只有带名字的
/// 百分数，没有表（横条）与 `用掉/窗口`；缓存命中回到状态栏，也带名字，两个百分数分得清。百分数一位小数（F115）。
pub fn parts(theme: &Theme, f: &Figures<'_>) -> Vec<Part> {
    let mut out = Vec::new();
    if !f.speed.is_empty() {
        out.push(Part {
            right: true,
            ..Part::new("speed", f.speed.clone(), 30)
        });
    }
    if let Some(rate) = f.cache {
        out.push(Part {
            right: true,
            ..Part::new(
                "cache",
                vec![
                    Span::new(format!("{} ", f.cache_word), theme.fg3),
                    Span::new(tenths(rate * 100.0), theme.fg2),
                ],
                40,
            )
        });
    }
    if let Some(c) = f.context.filter(|c| c.max > 0) {
        // 离压缩的位置近时琥珀色（同以前的表）。
        let ink = if near(c) {
            Style::fg(theme.code)
        } else {
            theme.fg2
        };
        let spans = vec![
            Span::new(format!("{} ", f.context_word), theme.fg3),
            Span::new(context_percent(c), ink),
        ];
        out.push(Part {
            right: true,
            ..Part::new(
                "context",
                fade::fade_in(theme, spans, f.context_age, fade::TICKS, None),
                85,
            )
        });
    }
    out
}

/// 状态栏写的上下文占用：`min(100, used ÷ max · 100)`，一位小数（tui-display.md §2.6 的 ctx 一行）。壳按它判断数变了没有
/// （从变的那一刻淡入）。
pub fn context_percent(c: &Window) -> String {
    tenths((c.used as f64 / c.max as f64).min(1.0) * 100.0)
}

/// 百分数，四舍五入到一位小数（整数也写 `.0`）。
fn tenths(percent: f64) -> String {
    format!("{percent:.1}%")
}

/// 离 agent 压缩的位置不到十个百分点（没有时离整个窗口）：表与它的数变成琥珀色。
pub fn near(c: &Window) -> bool {
    let max = c.max as f64;
    c.used as f64 >= c.compact_at.map_or(max, |a| a as f64) - 0.1 * max
}

/// 压缩的位置在 `cells` 格里的哪一格；不在格里时为 `None`。
fn tick_at(c: &Window, cells: usize) -> Option<usize> {
    c.compact_at
        .map(|a| round(a as f64 / c.max as f64 * cells as f64))
        .filter(|t| *t >= 0 && (*t as usize) < cells)
        .map(|t| t as usize)
}

/// `cells` 格里用掉的格数：`used ÷ max`，四舍五入。
fn filled(c: &Window, cells: usize) -> usize {
    round(c.used as f64 / c.max as f64 * cells as f64).clamp(0, cells as i64) as usize
}

/// 相邻的同墨格并成一段。
fn runs(cells: &[(&str, Style)]) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::new();
    for (t, st) in cells {
        match out.last_mut() {
            Some(last) if last.style == *st => last.text.to_mut().push_str(t),
            _ => out.push(Span::new((*t).to_owned(), *st)),
        }
    }
    out
}

// ---- 卡 ----

/// `   Context    `：卡上一行的内容从哪里开始。
const LABEL: usize = 11;
/// 组成的名字，按合并的次序。
const SOURCES: [(&str, &[&str]); 5] = [
    ("system", &["system_prompt", "meta_user_context"]),
    ("tools", &["tool_prompt", "system_tool_schemas"]),
    ("MCP", &["mcp_tool_schemas"]),
    ("skills", &["skills"]),
    ("messages", &["messages"]),
];
/// 条的每一级从 brand 向板混合多少。
const STEPS: [f64; 5] = [0.0, 0.3, 0.5, 0.66, 0.8];
/// 组成的第 `i` 档（按大小排的名次，[`groups`]）的墨：brand 向板的底色混合 [`STEPS`]；plain 模式是 brand、brand、fg3、fg3、
/// line。卡的条与图例、上下文的页的方格图与大纲同一档（docs/specs/rust-cli/tui-panels.md §4.2）。
pub fn step(theme: &Theme, i: usize) -> Style {
    let i = i.min(STEPS.len() - 1);
    match (theme.brand, theme.panel) {
        (Color::TrueColor(r, g, b), Some(p)) if theme.rich => {
            Style::fg(mixed(Rgb::new(r, g, b), p, STEPS[i]))
        }
        _ => [
            Style::fg(theme.brand),
            Style::fg(theme.brand),
            theme.fg3,
            theme.fg3,
            theme.line,
        ][i],
    }
}

/// 组成按 [`SOURCES`] 的名字合并，最大的在前（稳定排序，同量时按 `SOURCES` 的次序，同 Scala 的 sortBy）；为 0 的不列。
/// 名次就是 [`step`] 的档。
pub fn groups(breakdown: &[(String, u64)]) -> Vec<(&'static str, u64)> {
    let mut sums: Vec<(&'static str, u64)> = SOURCES
        .iter()
        .map(|(name, sources)| {
            let n = breakdown
                .iter()
                .filter(|(source, _)| sources.contains(&source.as_str()))
                .map(|(_, n)| *n)
                .sum();
            (*name, n)
        })
        .filter(|(_, n)| *n > 0)
        .collect();
    sums.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    sums
}

fn width(spans: &[Span]) -> usize {
    spans.iter().map(|sp| string_width(&sp.text)).sum()
}

/// Context 行（条、用量与压缩的位置、图例），落定的墨：上下文的页在计数到达之前与模型没有窗口时画它
/// （docs/specs/rust-cli/tui-panels.md §4.2）。`cols` 是可用的宽度，至多 101 列（同回答）。
pub fn context_rows(theme: &Theme, u: &Usage, cols: usize) -> Vec<Vec<Span>> {
    Rows {
        theme,
        cols: cols.min(MEASURE_MAX + MEASURE_MARGIN),
    }
    .context_rows(u)
}

/// 在一个宽度上画 Context 行要的。
struct Rows<'a> {
    theme: &'a Theme,
    cols: usize,
}

impl Rows<'_> {
    fn label(&self, t: &str) -> Vec<Span> {
        vec![
            Span::new(INDENT, Style::DEFAULT),
            Span::new(pad(t, LABEL), self.theme.fg2),
        ]
    }

    fn context_rows(&self, u: &Usage) -> Vec<Vec<Span>> {
        let theme = self.theme;
        let cols = self.cols as i64;
        let Some(c) = &u.context else {
            let mut known: Vec<Span> = u.last_call_tokens.map_or_else(Vec::new, |n| {
                vec![
                    Span::new(count(n as f64), Style::DEFAULT),
                    Span::new(" tokens", theme.fg3),
                    Span::new("   ", Style::DEFAULT),
                ]
            });
            known.push(Span::new("the model does not say its window", theme.fg3));
            let mut row = self.label("Context");
            row.extend(known);
            return vec![row];
        };
        let edge = cols - 1;
        let bw = (cols - 52).clamp(10, 32) as usize;
        let used_cells = filled(c, bw);
        let tick = tick_at(c, bw);
        // 组成按名字合并，最大的在前；不到 1 % 的不进图例。
        let sums = groups(&c.breakdown);
        let total: f64 = sums.iter().map(|(_, n)| *n as f64).sum();
        let shares: Vec<(&str, f64)> = sums
            .into_iter()
            .map(|(name, n)| (name, if total > 0.0 { n as f64 / total } else { 0.0 }))
            .collect();
        let step = |i: usize| step(theme, i);
        let mut bar: Vec<(&str, Style)> = Vec::new();
        let mut at = 0;
        for (i, (_, sh)) in shares.iter().enumerate() {
            let n = round(sh * used_cells as f64).max(0) as usize;
            let mut k = 0;
            while k < n && at < used_cells {
                bar.push(("█", step(i)));
                k += 1;
                at += 1;
            }
        }
        while at < used_cells {
            bar.push(("█", step(0)));
            at += 1;
        }
        for i in used_cells..bw {
            bar.push(if tick == Some(i) {
                ("│", theme.fg3)
            } else {
                ("─", theme.line)
            });
        }
        let mut head = self.label("Context");
        head.push(Span::new(" ".repeat(bw), Style::DEFAULT));
        let amount = vec![
            Span::new("   ", Style::DEFAULT),
            Span::new(count(c.used as f64), Style::DEFAULT),
            Span::new(" of ", theme.fg3),
            Span::new(count(c.max as f64), theme.fg2),
        ];
        let compacts = c.compact_at.map_or_else(Vec::new, |a| {
            vec![
                Span::new("   compacts at ", theme.fg3),
                Span::new(count(a as f64), theme.fg2),
            ]
        });
        let (hw, aw, cw) = (
            width(&head) as i64,
            width(&amount) as i64,
            width(&compacts) as i64,
        );
        let tail: Vec<Span> = if hw + aw + cw <= edge {
            amount.into_iter().chain(compacts).collect()
        } else if hw + aw <= edge {
            amount
        } else {
            Vec::new()
        };
        let legend: Vec<Vec<Span>> = shares
            .iter()
            .enumerate()
            .filter(|(_, (_, sh))| *sh >= 0.01)
            .map(|(i, (name, sh))| {
                vec![
                    Span::new("■", step(i)),
                    Span::new(format!(" {name}"), theme.fg3),
                    Span::new(format!(" {}%", round(sh * 100.0)), theme.fg2),
                ]
            })
            .collect();
        let room = edge - INDENT.len() as i64 - LABEL as i64;
        let mut legend_rows: Vec<Vec<Span>> = Vec::new();
        for it in legend {
            match legend_rows.last_mut() {
                Some(last) if width(last) as i64 + 3 + width(&it) as i64 <= room => {
                    last.push(Span::new("   ", Style::DEFAULT));
                    last.extend(it);
                }
                _ => legend_rows.push(it),
            }
        }
        let mut first = self.label("Context");
        let mut body = runs(&bar);
        body.extend(tail);
        first.extend(body);
        let pad = format!("{INDENT}{}", " ".repeat(LABEL));
        std::iter::once(first)
            .chain(legend_rows.into_iter().map(|r| {
                let mut row = vec![Span::new(pad.clone(), Style::DEFAULT)];
                row.extend(r);
                row
            }))
            .collect()
    }
}

/// `ms` 的工作：一分钟以内是秒，一小时以内是整分钟，再长是时与分。
pub fn working(ms: f64) -> String {
    let secs = round(ms / 1000.0).max(0);
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else {
        format!("{}h {:02}m", secs / 3600, secs % 3600 / 60)
    }
}

fn mixed(from: Rgb, to: Color, t: f64) -> Color {
    match to {
        Color::TrueColor(r, g, b) => from.mix(Rgb::new(r, g, b), t).color(),
        _ => from.color(),
    }
}
