//! 回合结束那一行（docs/specs/rust-cli/tui-display.md §2.7，修复线 F118）：`✻ <动词> for <用时> · done <时刻>`，后台还有
//! 在跑的命令时接 ` · <pill> still running`（pill 由调用方每次画时现取）；等后台子代理时整行换成
//! `Waiting for N background agent(s) to finish`。整行 fg3（kit 主题里安静的字；上游是 ratatui 的
//! DIM，搬进 kit 的 transcript 时换成主题的 fg3，深浅主题各取各的）。位置照 transcript 的网格：`✻` 在第 1 列，文字与续行从
//! 第 3 列起，按正文宽度折行。
//!
//! 回合在跑时同一个位置是它的现在时（回合的页脚，docs/specs/rust-cli/tui-kit.md §4.0「尾巴」）：[`footer`]。
use rust_cli_base::units;
use rust_cli_kernel_api::turn_duration::TurnDuration;
use rust_cli_tui_kit::app::widgets::{fade, lattice};
use rust_cli_tui_kit::render::{Span, Style, StyledLine, attr};

use super::{Ctx, duration, look};
use crate::ui::i18n::Lang;
use crate::ui::lines::wrap;

/// 动词（与对齐对象的清单相同）与它的现在分词（回合的页脚用，同一个种子挑同一对）。
const VERBS: [(&str, &str); 8] = [
    ("Baked", "Baking"),
    ("Brewed", "Brewing"),
    ("Churned", "Churning"),
    ("Cogitated", "Cogitating"),
    ("Cooked", "Cooking"),
    ("Crunched", "Crunching"),
    ("Sautéed", "Sautéing"),
    ("Worked", "Working"),
];

/// 字符串哈希的乘数（`h = h·31 + 字符`）。
const STRING_HASH_MULTIPLIER: i32 = 31;
/// 12 小时制半天的小时数。
const HOURS_PER_HALF_DAY: i64 = 12;

/// 按种子的字符串哈希挑动词：`h = h·31 + 字符`（UTF-16 单元，32 位有符号回绕），取无符号后对动词数（8）取余。种子是
/// 回合 ID，同一个回合每次画都一样。
fn pick(seed: &str) -> (&'static str, &'static str) {
    let hash = seed.encode_utf16().fold(0_i32, |hash, unit| {
        hash.wrapping_mul(STRING_HASH_MULTIPLIER)
            .wrapping_add(i32::from(unit))
    });
    VERBS[u32::from_ne_bytes(hash.to_ne_bytes()) as usize % VERBS.len()]
}

/// 结束那一行的动词（过去时）。
pub(crate) fn verb(seed: &str) -> &'static str {
    pick(seed).0
}

/// 回合的页脚的动词：同一个种子挑的那个动词的现在分词。
pub(crate) fn verb_ing(seed: &str) -> &'static str {
    pick(seed).1
}

/// `at_ms` 加上 UTC 偏移之后的时刻：英文 12 小时制（`4:52 AM`），中文 24 小时制（`16:52`），小时不补零。
pub(crate) fn clock(at_ms: u64, offset_minutes: i32, lang: Lang) -> String {
    let minutes =
        i64::try_from(at_ms / units::MS_PER_MINUTE).unwrap_or(i64::MAX) + i64::from(offset_minutes);
    let of_day = minutes.rem_euclid(units::MINUTES_PER_DAY as i64);
    let per_hour = units::MINUTES_PER_HOUR as i64;
    let (hour, minute) = (of_day / per_hour, of_day % per_hour);
    match lang {
        Lang::En => {
            let half = if hour < HOURS_PER_HALF_DAY {
                "AM"
            } else {
                "PM"
            };
            let hour = match hour % HOURS_PER_HALF_DAY {
                0 => HOURS_PER_HALF_DAY,
                hour => hour,
            };
            format!("{hour}:{minute:02} {half}")
        }
        Lang::Zh => format!("{hour}:{minute:02}"),
    }
}

/// 这一行写的用时：向上取整到整秒，至少 1 秒（tui-display.md §2.7）。
///
/// 修 bug（2026-11-12，用户报告“有的时候是 0 秒，思考就得 1 秒”）：以前向下取整，同一屏里工作段的思考用时
/// （kit `fit::thought_time`）却向上取整、至少 1 秒，0.8 秒的回合写“思考了 1s”而“用时 0s”。两处取整一致后，回合的用时
/// 不会小于它里面的思考。
fn whole_seconds_up(ms: u64) -> u64 {
    ms.div_ceil(units::MS_PER_SECOND).max(1) * units::MS_PER_SECOND
}

/// 这一行。`still_running` 是 footer 的 pill（没有在跑的命令时为 None）；等后台子代理的行不接它。`age`：这一行到达之后的
/// tick（刚到达、还在落定时）；None 是落定的样子。
pub(crate) fn lines(
    line: &TurnDuration,
    still_running: Option<&str>,
    age: Option<i64>,
    ctx: &Ctx<'_>,
) -> Vec<StyledLine> {
    let theme = &ctx.ink.theme;
    let style = theme.fg3;
    let spans = if line.waiting_agents > 0 {
        let count = line.waiting_agents;
        let (before, after) = match ctx.lang {
            Lang::En => (
                "Waiting for ",
                if count == 1 {
                    " background agent to finish"
                } else {
                    " background agents to finish"
                },
            ),
            Lang::Zh => ("等待 ", " 个后台代理完成"),
        };
        vec![
            Span::new(before, style),
            Span::new(count.to_string(), style.with_attr(attr::BOLD)),
            Span::new(after, style),
        ]
    } else {
        let took = duration(whole_seconds_up(line.duration_ms));
        let at = clock(line.finished_at_ms, line.utc_offset_minutes, ctx.lang);
        // 前一半是页脚换成过去时的那句，后一半（`· done …`）是结束时才有的，到达时从底色升起。
        let (head, mut rest) = match ctx.lang {
            Lang::En => (
                format!("{} for {took}", verb(line.turn.as_str())),
                format!(" · done {at}"),
            ),
            Lang::Zh => (format!("用时 {took}"), format!(" · 完成于 {at}")),
        };
        if let Some(still) = still_running {
            rest.push_str(&match ctx.lang {
                Lang::En => format!(" · {still} still running"),
                Lang::Zh => format!(" · {still} 仍在运行"),
            });
        }
        let mut spans = vec![Span::new(head, style)];
        spans.extend(fade::fade_in(
            theme,
            vec![Span::new(rest, style)],
            age.unwrap_or(fade::TICKS),
            fade::TICKS,
            None,
        ));
        spans
    };
    // 修复原因：这一行来自没有网格的修复线（F118），合进来时只换了墨色，前缀还是自己写的 `✻ `：记号在第 0 列、文字在第 2 列，
    // 比工具行靠左一格；折行也按整个屏幕宽，不按正文宽度。这里改用与工具行同一份网格，不再自己写前缀。
    // 到达时 `✻` 从光标最亮的 brand 落到 fg3（回合的页脚的 `✻` 借的是光标的呼吸，tui-kit.md §4.0「尾巴」）。
    let star = age.map_or(style, |age| {
        fade::ink(theme, theme.brand, style, age, fade::TICKS)
    });
    wrap(
        &spans,
        look::measure(ctx.width),
        &look::marked(Span::new("✻", star)),
        &[Span::plain(look::LEAD)],
    )
}

/// 回合的页脚（docs/specs/rust-cli/tui-kit.md §4.0「尾巴」，2026-11-13）：回合在跑时尾巴的最后一行，是结束那一行的现在时，
/// `✻ Cooking for 2m 5s`（中文 `✻ 用时 2m 5s`）。`seed` 挑动词（回合 ID，同结束那一行），`elapsed_ms` 是回合开始以来的毫秒，
/// 按整秒向下取，不满 1 秒不写；`star` 是 `✻` 的墨（借光标的呼吸，光标不在时 fg3）。
///
/// 修 bug（2026-11-13，用户报告“为什么有时候有两个 Working”）：以前这里是 `Working  2m 05s`，点阵在工作段里时与工作段
/// 等模型的 `└ ⋮⋮⋮ Working` 成了两行 `Working`。`Working` 只是点阵后面的词；总用时是回合的事实，放在回合的页脚。
///
/// `dots`：点阵出现了多久（点阵在时）。点阵的亮光离开第三列之后接着走过动词（[`lattice::band`]，同一个相位），以前走在点阵
/// 后面的 `Working` 上；`Working` 去掉之后（2026-11-13，用户要求）改走这里。只走动词，不走 `for <用时>`。
pub(crate) fn footer(
    seed: &str,
    elapsed_ms: u64,
    star: Style,
    dots: Option<i64>,
    ctx: &Ctx<'_>,
) -> Vec<StyledLine> {
    let theme = &ctx.ink.theme;
    let style = theme.fg3;
    let word = match ctx.lang {
        Lang::En => verb_ing(seed),
        Lang::Zh => "用时",
    };
    let word = Span::new(word, style);
    let mut spans = match dots {
        Some(age) => lattice::band(theme, age, word),
        None => vec![word],
    };
    if elapsed_ms >= units::MS_PER_SECOND {
        let took = duration(elapsed_ms / units::MS_PER_SECOND * units::MS_PER_SECOND);
        spans.push(Span::new(
            match ctx.lang {
                Lang::En => format!(" for {took}"),
                Lang::Zh => format!(" {took}"),
            },
            style,
        ));
    }
    wrap(
        &spans,
        look::measure(ctx.width),
        &look::marked(Span::new("✻", star)),
        &[Span::plain(look::LEAD)],
    )
}

#[cfg(test)]
#[path = "duration_tests.rs"]
mod tests;
