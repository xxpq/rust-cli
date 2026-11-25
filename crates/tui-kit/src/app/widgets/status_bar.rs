//! 状态栏（docs/specs/rust-cli/tui-display.md §2.6；最早照 docs/puffin-tui.md §4.2「状态栏」）：部分（[`Part`]）、按名次丢掉与
//! 回来的排法（[`fit`]）、一行（[`line`]），与左边的部分怎样接起来（模式与 `⇧⇥`、目标的标记与字、`@machine`，[`left`]）、右边的
//! 模型与档位（[`model_parts`]）。纯的；入参是中性的 [`Left`] 与段。
//!
//! 2026-11-12 起（用户要求）：总是一行，窄时只丢次要的项、不分两行；项之间是 ` · `；不写项目路径；模型与档位在右端。以前照
//! Puffin 的 shell/status_line.rs：终端够高时分成两行（`SPLIT_ROWS`）、`  ·  ` 两边各两格、cwd 与分支在左边、缩短的 cwd。
use std::collections::BTreeSet;

use crate::render::width::string_width;
use crate::render::{Span, Style};
use crate::theme::theme::Theme;

use super::fade;
use super::key_hints;
use super::modes::{self, Mode};

/// 状态栏的一个部分。
///
/// - `glue`：在它那一侧把它接到前一个部分的；`None` 是 line 墨的 ` · `。
/// - `after`：只与这个部分一起显示（模式后的 `⇧⇥`、模型后的档位）。
/// - `right`：它在右端。
#[derive(Clone, Debug, PartialEq)]
pub struct Part {
    pub key: String,
    pub spans: Vec<Span>,
    pub rank: i32,
    pub right: bool,
    pub glue: Option<Vec<Span>>,
    pub after: Option<String>,
}

impl Part {
    /// 只有键、样式段与名次的部分，其余为缺省。
    pub fn new(key: impl Into<String>, spans: Vec<Span>, rank: i32) -> Part {
        Part {
            key: key.into(),
            spans,
            rank,
            right: false,
            glue: None,
            after: None,
        }
    }
}

/// 右边一组键（[`key_hints::spans`] 排出的段，例如页的键）：一对一个部分，对与对之间照旧是两格（[`key_hints::PAIR_GAP`]）。
/// 越靠后的对名次越高，最后一对（页的 esc 与它回到哪里）的名次是 `rank`：放不下时从前面一对一对地走，esc 那一对最后走
/// （docs/specs/rust-cli/tui-kit.md §4.0「页的键」，用户决定 2026-11-12）。以前整组是一个部分，窄时一起走，连怎么回去都
/// 看不到。一句说明（没有对与对之间的空白）是一个部分。
pub fn key_parts(key: &str, spans: Vec<Span>, rank: i32) -> Vec<Part> {
    let pairs = key_hints::split_pairs(spans);
    let last = pairs.len().saturating_sub(1);
    pairs
        .into_iter()
        .enumerate()
        .map(|(i, spans)| Part {
            right: true,
            glue: (i > 0).then(|| vec![Span::new(key_hints::PAIR_GAP, Style::DEFAULT)]),
            ..Part::new(
                format!("{key}{i}"),
                spans,
                rank - i32::try_from(last - i).unwrap_or(i32::MAX),
            )
        })
        .collect()
}

/// 项与项之间的分隔（2026-11-12 起一边一格；以前一边两格，用户说“点的左右空白大了，额外占空间”）。
pub const SEPARATOR: &str = " · ";
const LEAD: &str = "   ";

fn cells(spans: &[Span]) -> usize {
    spans.iter().map(|sp| string_width(&sp.text)).sum()
}

/// 宽 `width` 时的状态栏：总是一行。`group` 是右边的部分（多少、模型与档位），`note` 是槽的那句话或设置页的键：有 `note`
/// 时它占右边，`group` 不写。
pub fn line(left: &[Part], group: &[Part], note: &[Part], width: usize, sep: Style) -> Vec<Span> {
    let right = if note.is_empty() { group } else { note };
    // 右边的一组与左边至少隔三格；说明句与设置的键照旧隔两格。
    let gap = if note.is_empty() { 3 } else { 2 };
    let all: Vec<Part> = left.iter().chain(right).cloned().collect();
    fit(&all, width, gap, sep)
}

/// 宽 `width` 时留下的部分排成的一行：名次低的先走，一次一个名次；然后走掉的在放得下时回来，名次高的先回来。左边的部分从
/// 第 3 列起；右边的部分停在边前一格，与左边至少隔 `gap` 格。
pub fn fit(parts: &[Part], width: usize, gap: usize, sep: Style) -> Vec<Span> {
    let present: BTreeSet<String> = parts.iter().map(|p| p.key.clone()).collect();
    let shown = |gone: &BTreeSet<String>| -> Vec<&Part> {
        parts
            .iter()
            .filter(|p| {
                !gone.contains(&p.key)
                    && p.after
                        .as_ref()
                        .is_none_or(|a| present.contains(a) && !gone.contains(a))
            })
            .collect()
    };
    let build = |gone: &BTreeSet<String>| -> Option<Vec<Span>> {
        let kept = shown(gone);
        let side = |right: bool| -> Vec<Span> {
            let mut out = Vec::new();
            for (i, p) in kept.iter().filter(|p| p.right == right).enumerate() {
                if i > 0 {
                    match &p.glue {
                        Some(g) => out.extend(g.iter().cloned()),
                        None => out.push(Span::new(SEPARATOR, sep)),
                    }
                }
                out.extend(p.spans.iter().cloned());
            }
            out
        };
        let l = side(false);
        let r = side(true);
        let left = if l.is_empty() {
            Vec::new()
        } else {
            let mut v = vec![Span::new(LEAD, Style::DEFAULT)];
            v.extend(l);
            v
        };
        let (lw, rw) = (cells(&left), cells(&r));
        if r.is_empty() {
            (lw == 0 || lw + 2 <= width).then_some(left)
        } else if lw + gap + rw < width {
            let mut v = left;
            v.push(Span::new(" ".repeat(width - lw - rw - 1), Style::DEFAULT));
            v.extend(r);
            v.push(Span::new(" ", Style::DEFAULT));
            Some(v)
        } else {
            None
        }
    };
    let mut thresholds: Vec<i32> = parts.iter().map(|p| p.rank).collect();
    thresholds.sort_unstable();
    thresholds.dedup();
    // 门槛 t：名次小于 t 的都走。第一个门槛什么都不丢。
    let first = std::iter::once(None)
        .chain(thresholds.iter().map(|t| Some(t + 1)))
        .map(|t| -> BTreeSet<String> {
            match t {
                None => BTreeSet::new(),
                Some(t) => parts
                    .iter()
                    .filter(|p| p.rank < t)
                    .map(|p| p.key.clone())
                    .collect(),
            }
        })
        .find(|g| build(g).is_some())
        .unwrap_or_else(|| present.clone());
    // 早丢掉的部分在放得下时回来，名次高的先回来；一个回不来，名次更低的也不回来。修改原因（2026-11-12）：以前照 Puffin
    // 跳过回不来的、继续试更低的，窄时短的 `182 tok/s` 回来了而更重要的缓存命中没有，隐藏的不按次要的次序。
    let mut back: Vec<&Part> = parts.iter().filter(|p| first.contains(&p.key)).collect();
    back.sort_by_key(|p| std::cmp::Reverse(p.rank));
    let mut gone = first;
    for p in back {
        let mut g = gone.clone();
        g.remove(&p.key);
        if build(&g).is_none() {
            break;
        }
        gone = g;
    }
    build(&gone).unwrap_or_default()
}

/// 带墨色的几段字（目标的标记与字）。
pub type Inked = Vec<(String, Style)>;

/// 左边要说的，每个带它变了多少 tick（从那时淡入）。
///
/// - `mode`：在用的模式与计划模式开没开；`goal`：目标的标记与字（壳按目标写好）。
/// - `machines`：就绪的远端机器（`@dev-box +1`）。
#[derive(Clone, Debug, Default)]
pub struct Left<'a> {
    pub mode: Option<(Mode, bool, i64)>,
    pub goal: Option<(Inked, Inked, i64)>,
    pub machines: Option<&'a str>,
}

/// 模式之后换模式的键 `⇧⇥`（line 墨）：挨着它换的东西，名次最低之一，窄时先走。
pub fn tab_part(theme: &Theme) -> Part {
    Part {
        glue: Some(Vec::new()),
        after: Some("mode".to_owned()),
        ..Part::new(
            "tab",
            vec![Span::new(" ", Style::DEFAULT), Span::new("⇧⇥", theme.line)],
            10,
        )
    }
}

/// 左边的部分：权限模式带 line 墨色的 `⇧⇥` 领头、最后走；目标的标记跟着模式，字先于模式丢掉；就绪的远端机器窄时最先走。
pub fn left(theme: &Theme, l: &Left<'_>) -> Vec<Part> {
    let mut out = Vec::new();
    if let Some((mode, plan, age)) = l.mode {
        let ms = fade::parts(theme, modes::parts(theme, mode, plan), age, None);
        out.push(Part::new("mode", ms, 100));
        out.push(tab_part(theme));
    }
    // 目标的标记跟着模式，字在等你时才有，先于模式丢掉。
    if let Some((m, w, age)) = &l.goal {
        let (mark, words) = (
            fade::parts(theme, m.clone(), *age, None),
            fade::parts(theme, w.clone(), *age, None),
        );
        if !mark.is_empty() {
            out.push(Part {
                glue: Some(vec![Span::new(" ", Style::DEFAULT)]),
                ..Part::new("goal", mark, 100)
            });
        }
        if !words.is_empty() {
            out.push(Part {
                glue: Some(Vec::new()),
                after: Some("goal".to_owned()),
                ..Part::new("goalWords", words, 70)
            });
        }
    }
    if let Some(at) = l.machines {
        out.push(Part::new(
            "machines",
            vec![Span::new(at.to_owned(), theme.fg3)],
            5,
        ));
    }
    out
}

/// 右端的模型与档位（2026-11-12 起在右端；以前在左边模式之后）：模型 fg2、名次 90；档位 fg3、名次 50，只与模型一起显示。
/// `/model` 选了之后 `age` 个 tick 从底色淡入。没有档位时只有模型。
pub fn model_parts(theme: &Theme, model: &str, effort: &str, age: i64) -> Vec<Part> {
    let fade =
        |text: &str, style: Style| fade::parts(theme, vec![(text.to_owned(), style)], age, None);
    let mut out = vec![Part {
        right: true,
        ..Part::new("model", fade(model, theme.fg2), 90)
    }];
    if !effort.is_empty() {
        out.push(Part {
            right: true,
            after: Some("model".to_owned()),
            ..Part::new("effort", fade(effort, theme.fg3), 50)
        });
    }
    out
}

/// shell 模式的左边（§4.2「Shell 命令（!）」）：`Shell`（琥珀色，进入 shell 模式 `age` tick 之后淡入完），没有 `⇧⇥`。
pub fn shell_left(theme: &Theme, age: i64) -> Vec<Part> {
    vec![Part::new(
        "mode",
        fade::parts(
            theme,
            vec![("Shell".to_owned(), Style::fg(theme.code))],
            age,
            None,
        ),
        100,
    )]
}

#[cfg(test)]
#[path = "status_bar_tests.rs"]
mod tests;
