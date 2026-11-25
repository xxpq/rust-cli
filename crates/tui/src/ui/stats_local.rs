//! 设置的 Stats 段（docs/specs/rust-cli/tui-panels.md §4.6，2026-11-12 起；以前是 `/usage all` 的「This computer」页）：这台电脑上
//! 的使用记录，样子是概览——消息的热力图与图例、范围、两列的数、token 的分项，最后是这次会话的数字。
//!
//! 数据是内核的（用量表是唯一来源）：段显示时 runner 一起问 API 时长与全时段的统计，⇥ 换范围时再问一次，回执带段的请求序号，
//! 过时的丢掉（`FuturesUnordered` 里回执的先后不定），不用超时。读到的放在设置的界面状态里（`Settings.stats`），关上设置就丢。
//! 行只用 kit 的构件，从第 3 列起（页框的左边距，设置的窗格排好之后去掉，`settings/live.rs`），句子是英文（同设置页）。
use rust_cli_kernel_api::metering::{
    ActivityRange, ActivityStats, ModelTokens, SessionMetering, SessionSummary,
};
use rust_cli_tui_kit::app::element::{Element, br, by_width, measure, single_line, stack};
use rust_cli_tui_kit::app::layout::Layout;
use rust_cli_tui_kit::app::widgets::stats::working;
use rust_cli_tui_kit::app::widgets::usage::view::{
    LEAD, SETTLED, asking, commas, compact, facts, failed, joined, pad_to, spread,
};
use rust_cli_tui_kit::app::widgets::usage::{PlanDetail, page};
use rust_cli_tui_kit::render::{Span, Style, StyledLine, attr};
use rust_cli_tui_kit::theme::theme::Theme;

use super::app::{Action, App};
use super::i18n::Lang;
use super::manage::Manage;
use super::pick::MEASURE_MAX;
use super::stats_local_data as data;

/// 范围与它们的名字，⇥ 按这个次序换。
const RANGES: [(ActivityRange, &str, &str); 3] = [
    (ActivityRange::All, "All time", "全部时间"),
    (ActivityRange::Week, "Last 7 days", "最近 7 天"),
    (ActivityRange::Month, "Last 30 days", "最近 30 天"),
];

/// 名字补到这么宽（两列的数与 This session 的行）。
const NAME: usize = 17;
/// 两列时第二列从这一列起：左边距之后 32 列（两格各 28 列、隔 4 列）。
const SECOND_AT: usize = LEAD.len() + 32;
/// 窄于这个宽（含左边距）时一列：第二列放不下 [`NAME`] 与 14 列的值。
const TWO_COLUMNS: usize = SECOND_AT + NAME + 14;
/// 两列之间至少空这么多列（第一列的值长过第二列的起点时）。
const COLUMN_GAP: usize = 2;
/// 段标题与右端的数之间至少空这么多列（kit `spread`）。
const HEADING_GAP: i64 = 2;
/// 图例对齐热力图的格子：在 5 列宽的星期名之后（kit `page::heat_rows`）。
const LEGEND_AT: &str = "     ";

/// 读到的统计：会话的数字（读到时算好）、全时段的统计、所选的范围与它的统计（`None` 时还在问）、请求序号。
#[derive(Debug, Clone, PartialEq)]
pub struct LocalStats {
    pub session: SessionSummary,
    /// `Err` 是给用户看的原因。
    pub all: Result<ActivityStats, String>,
    pub range: ActivityRange,
    pub ranged: Option<Result<ActivityStats, String>>,
    pub seq: u64,
}

impl LocalStats {
    /// 所选范围的统计：全时段用读到的；其余在回执到之前为 `None`。
    fn shown(&self) -> Option<&Result<ActivityStats, String>> {
        match self.range {
            ActivityRange::All => Some(&self.all),
            _ => self.ranged.as_ref(),
        }
    }
}

impl App {
    /// 会话的数字：墙钟时长从本次 TUI 启动起算，改动来自 `FileChanges`，用量按模型累计。
    pub(crate) fn session_summary(&self, metering: SessionMetering) -> SessionSummary {
        let wall_duration_ms =
            u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let (lines_added, lines_removed) = self
            .view
            .turn_changes()
            .iter()
            .flat_map(|turn| &turn.files)
            .fold((0, 0), |(a, d), f| (a + f.additions, d + f.deletions));
        SessionSummary {
            api_duration_ms: metering.api_duration_ms,
            wall_duration_ms,
            lines_added,
            lines_removed,
            models: self
                .view
                .usage()
                .iter()
                .map(|(model, usage)| (model.clone(), *usage))
                .collect(),
        }
    }

    /// Stats 段读到了：API 时长与全时段的统计。设置已经关上时丢掉（下次打开再读）。
    pub fn stats_opened(&mut self, metering: SessionMetering, all: Result<ActivityStats, String>) {
        let session = self.session_summary(metering);
        if let Some(ui) = self.settings.as_mut() {
            ui.stats = Some(LocalStats {
                session,
                all,
                range: ActivityRange::All,
                ranged: None,
                seq: 0,
            });
        }
    }

    /// 范围的回执：段的统计还在、序号是最新的才收下。
    pub fn stats_range(&mut self, seq: u64, result: Result<ActivityStats, String>) {
        if let Some(stats) = self.settings.as_mut().and_then(|ui| ui.stats.as_mut())
            && stats.seq == seq
        {
            stats.ranged = Some(result);
        }
    }

    /// ⇥：换到下一个范围（All → 7 天 → 30 天 → All）。All time 用读到的；其余问内核，序号加一，旧的回执丢掉。
    pub(super) fn stats_next_range(&mut self) -> Option<Action> {
        let stats = self.settings.as_mut()?.stats.as_mut()?;
        let at = RANGES
            .iter()
            .position(|(r, _, _)| *r == stats.range)
            .unwrap_or(0);
        let range = RANGES[(at + 1) % RANGES.len()].0;
        stats.range = range;
        stats.seq += 1;
        stats.ranged = None;
        (range != ActivityRange::All).then_some(Action::Manage(Manage::StatsRange {
            range,
            seq: stats.seq,
        }))
    }
}

/// 段标题：第 3 列起 fg2 粗体，右端是 `right`。
fn headed(theme: &Theme, text: &str, right: Vec<Span>) -> Element {
    let left = vec![
        Span::plain(LEAD),
        Span::new(text.to_owned(), theme.fg2.with_attr(attr::BOLD)),
    ];
    by_width(move |w| single_line(spread(left.clone(), right.clone(), w, HEADING_GAP), false))
}

/// fg3 斜体的一句（没有数据时）。
fn quiet(theme: &Theme, text: &str) -> Element {
    single_line(
        vec![
            Span::plain(LEAD),
            Span::new(text.to_owned(), theme.fg3.with_attr(attr::ITALIC)),
        ],
        false,
    )
}

/// 一格：fg3 的名字补到 [`NAME`]，然后是值。
fn cell(theme: &Theme, name: &str, value: Vec<Span>) -> Vec<Span> {
    let mut spans = vec![Span::new(pad_to(name, NAME), theme.fg3)];
    spans.extend(value);
    spans
}

/// `This session` 下面的一行：第 3 列起一格；值是空的时不写这一行。
fn row(theme: &Theme, name: &str, value: Vec<Span>) -> Option<Element> {
    (!value.is_empty()).then(|| {
        let mut spans = vec![Span::plain(LEAD)];
        spans.extend(cell(theme, name, value));
        single_line(spans, false)
    })
}

fn chosen(theme: &Theme) -> Style {
    if theme.rich {
        Style::fg(theme.brand)
    } else {
        Style::fg(theme.brand).with_attr(attr::BOLD)
    }
}

/// `1 day`、`3 days`；中文 `3 天`。
fn days(l: Lang, n: u64) -> String {
    match l {
        Lang::En => format!("{n} day{}", if n == 1 { "" } else { "s" }),
        Lang::Zh => format!("{n} 天"),
    }
}

fn reading(l: Lang) -> &'static str {
    l.pick("Reading this computer's history", "正在读这台电脑的记录")
}

fn no_activity(l: Lang) -> &'static str {
    l.pick("No activity yet.", "还没有活动。")
}

/// Stats 段的行，排在 `width` 列里（从第 3 列起）。还没读到时是 kit `asking` 的一行。字按界面语言（tui-kit.md §4.5「语言」）。
pub fn lines(l: Lang, theme: &Theme, stats: Option<&LocalStats>, width: usize) -> Vec<StyledLine> {
    let rows = match stats {
        None => vec![asking(theme, reading(l))],
        Some(stats) => rows(l, theme, stats),
    };
    Layout::lay(&measure(stack(rows), MEASURE_MAX, 0), width.max(1))
}

/// 从上到下：热力图与图例、范围、两列的数与 token 的分项、这次会话（§4.6）。
fn rows(l: Lang, theme: &Theme, stats: &LocalStats) -> Vec<Element> {
    let theme = *theme;
    let mut out = heat(l, &theme, &stats.all);
    out.push(br());
    // 范围：选中的 chosen 的墨，其余 fg3，点是 line。
    let mut ranges = vec![Span::plain(LEAD)];
    for (i, (range, en, zh)) in RANGES.iter().enumerate() {
        let label = l.pick(en, zh);
        if i > 0 {
            ranges.push(Span::new("  ·  ", theme.line));
        }
        let ink = if *range == stats.range {
            chosen(&theme)
        } else {
            theme.fg3
        };
        ranges.push(Span::new(label, ink));
    }
    out.push(single_line(ranges, false));
    out.push(br());
    match stats.shown() {
        None => out.push(asking(&theme, reading(l))),
        Some(Err(why)) => out.push(failed(
            &theme,
            &match l {
                Lang::En => format!("Could not read the history: {why}"),
                Lang::Zh => format!("没能读到记录：{why}"),
            },
        )),
        Some(Ok(range)) => {
            let current = stats.all.as_ref().ok().map(|all| data::streaks(all).0);
            out.extend(figures(l, &theme, range, current));
        }
    }
    out.push(br());
    out.extend(session(l, &theme, &stats.session));
    out
}

/// 消息的热力图：总是全时段（不随范围）。
fn heat(l: Lang, theme: &Theme, all: &Result<ActivityStats, String>) -> Vec<Element> {
    let theme = *theme;
    let all = match all {
        Err(why) => {
            return vec![failed(
                &theme,
                &match l {
                    Lang::En => format!("Could not read this computer's history: {why}"),
                    Lang::Zh => format!("没能读到这台电脑的记录：{why}"),
                },
            )];
        }
        Ok(all) => all,
    };
    let detail = PlanDetail {
        activity: data::activity(all),
        ..PlanDetail::default()
    };
    let total = detail.activity.total;
    let right = if total > 0 {
        vec![
            Span::new(compact(total as f64), theme.fg2),
            Span::new(l.pick(" total", " 条"), theme.fg3),
        ]
    } else {
        Vec::new()
    };
    let mut out = vec![headed(
        &theme,
        l.pick("Messages, last 52 weeks", "消息，最近 52 周"),
        right,
    )];
    if detail.activity.levels.iter().flatten().all(|n| *n <= 0) {
        out.push(quiet(&theme, no_activity(l)));
        return out;
    }
    out.push(by_width(move |w| {
        stack(page::heat_rows(&theme, &detail, w, SETTLED))
    }));
    let mut legend = vec![Span::plain(LEAD), Span::plain(LEGEND_AT)];
    legend.extend(page::heat_legend(&theme));
    out.push(single_line(legend, false));
    out
}

/// 所选范围的两列的数与 token 的分项；`current` 是到今天为止的连续天数（全时段）。
fn figures(l: Lang, theme: &Theme, stats: &ActivityStats, current: Option<u64>) -> Vec<Element> {
    let theme = *theme;
    let tokens: u64 = stats.models.iter().map(ModelTokens::total).sum();
    if stats.days.is_empty() && tokens == 0 && stats.sessions == 0 {
        return vec![quiet(&theme, no_activity(l))];
    }
    let value = |text: String| vec![Span::new(text, Style::DEFAULT)];
    let (_, longest) = data::streaks(stats);
    let left: Vec<Option<(&str, Vec<Span>)>> = vec![
        stats.models.first().map(|m| {
            (
                l.pick("Favorite model", "最常用的模型"),
                value(m.model.clone()),
            )
        }),
        (stats.sessions > 0).then(|| {
            (
                l.pick("Sessions", "会话"),
                value(commas(stats.sessions as f64)),
            )
        }),
        data::active_days(stats).map(|(active, of)| {
            let text = match of {
                Some(of) => format!("{active}/{of}"),
                None => active.to_string(),
            };
            (l.pick("Active days", "活跃天数"), value(text))
        }),
        data::most_active(stats)
            .and_then(|day| data::short_date(&day.date))
            .map(|date| (l.pick("Most active day", "最活跃的一天"), value(date))),
    ];
    let right: Vec<Option<(&str, Vec<Span>)>> = vec![
        (tokens > 0).then(|| {
            (
                l.pick("Total tokens", "总 token"),
                value(compact(tokens as f64)),
            )
        }),
        stats.longest_session_ms.map(|ms| {
            (
                l.pick("Longest session", "最长的会话"),
                value(working(ms as f64)),
            )
        }),
        (longest > 0).then(|| {
            (
                l.pick("Longest streak", "最长连续"),
                value(days(l, longest)),
            )
        }),
        current
            .filter(|n| *n > 0)
            .map(|n| (l.pick("Current streak", "当前连续"), value(days(l, n)))),
    ];
    let cells = |side: &[Option<(&str, Vec<Span>)>]| -> Vec<Option<Vec<Span>>> {
        side.iter()
            .map(|c| c.as_ref().map(|(name, v)| cell(&theme, name, v.clone())))
            .collect()
    };
    let (left, right) = (cells(&left), cells(&right));
    let mut out = vec![by_width(move |w| {
        let mut lines = Vec::new();
        if w >= TWO_COLUMNS {
            for (lc, r) in left.iter().zip(&right) {
                if lc.is_none() && r.is_none() {
                    continue;
                }
                let mut spans = vec![Span::plain(LEAD)];
                let used: usize = LEAD.len()
                    + lc.iter()
                        .flatten()
                        .map(|s| rust_cli_tui_kit::render::width::string_width(&s.text))
                        .sum::<usize>();
                spans.extend(lc.iter().flatten().cloned());
                if let Some(r) = r {
                    spans.push(Span::plain(
                        " ".repeat(SECOND_AT.saturating_sub(used).max(COLUMN_GAP)),
                    ));
                    spans.extend(r.iter().cloned());
                }
                lines.push(single_line(spans, false));
            }
        } else {
            for c in left.iter().chain(&right).flatten() {
                let mut spans = vec![Span::plain(LEAD)];
                spans.extend(c.iter().cloned());
                lines.push(single_line(spans, false));
            }
        }
        stack(lines)
    })];
    // token 的分项：名字 fg3、数 fg2，为 0 的不写（`Input · Output · Cache read · Cache write`）。
    let sum = |f: fn(&ModelTokens) -> u64| stats.models.iter().map(f).sum::<u64>();
    let parts = [
        (l.pick("In", "输入"), sum(|m| m.input)),
        (l.pick("Out", "输出"), sum(|m| m.output)),
        (l.pick("Cache read", "缓存读"), sum(|m| m.cache_read)),
        (l.pick("Cache write", "缓存写"), sum(|m| m.cache_write)),
    ];
    let mut split = vec![Span::plain(LEAD)];
    for (name, n) in parts.iter().filter(|(_, n)| *n > 0) {
        if split.len() > 1 {
            split.push(Span::new("  ·  ", theme.line));
        }
        split.push(Span::new(format!("{name} "), theme.fg3));
        split.push(Span::new(compact(*n as f64), theme.fg2));
    }
    if split.len() > 1 {
        out.push(single_line(split, false));
    }
    out
}

/// 这次会话（usage.md §4）：API 时长与墙钟时长、改动的行、各模型的 token。
fn session(l: Lang, theme: &Theme, s: &SessionSummary) -> Vec<Element> {
    let mut out = vec![headed(
        theme,
        l.pick("This session", "这个会话"),
        facts(
            theme,
            vec![
                (
                    working(s.api_duration_ms as f64),
                    l.pick("API time", "API 用时").to_owned(),
                ),
                (
                    working(s.wall_duration_ms as f64),
                    l.pick("wall time", "总用时").to_owned(),
                ),
            ],
        ),
    )];
    let changes = if s.lines_added + s.lines_removed == 0 {
        Vec::new()
    } else {
        vec![
            Span::new(
                format!("+{}", commas(s.lines_added as f64)),
                Style::fg(theme.ok),
            ),
            Span::plain(" "),
            Span::new(
                format!("−{}", commas(s.lines_removed as f64)),
                Style::fg(theme.err),
            ),
            Span::new(l.pick(" lines", " 行"), theme.fg3),
        ]
    };
    let models = s
        .models
        .iter()
        .map(|(name, u)| {
            let tokens = u.input_tokens
                + u.output_tokens
                + u.cache_creation_input_tokens
                + u.cache_read_input_tokens;
            (name.clone(), compact(tokens as f64))
        })
        .collect();
    out.extend(row(theme, l.pick("Changes", "改动"), changes));
    out.extend(row(theme, l.pick("Models", "模型"), joined(theme, models)));
    out
}

#[cfg(test)]
#[path = "stats_local_tests.rs"]
mod tests;
