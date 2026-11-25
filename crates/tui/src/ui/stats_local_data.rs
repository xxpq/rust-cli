//! 设置的 Stats 段的数据（docs/specs/rust-cli/tui-panels.md §4.6、usage.md §5）：内核给的本机活动统计（[`ActivityStats`]）在这里
//! 换成 kit 的入参——热力图的周与级别（Puffin 投影 `Activity` 的形状：第一周周日的日期、逐周逐天的 0–4）——与两列的数：连续的
//! 天数、活跃的天数、消息最多的一天。纯函数；画法在 `stats_local.rs`。
use std::collections::{BTreeMap, BTreeSet};

// 一天的毫秒数只定义在 base（F58）。
use rust_cli_base::js::date::DAY_MS;
use rust_cli_kernel_api::clock::civil_date;
use rust_cli_kernel_api::metering::ActivityRange;
use rust_cli_kernel_api::metering::{ActivityDay, ActivityStats};
use rust_cli_tui_kit::app::widgets::usage::Activity;
use rust_cli_tui_kit::app::widgets::usage::view::MONTHS;

/// 热力图最多几周（Puffin `heat_rows` 的上限）。
pub const WEEKS: usize = 52;

/// 一周的天数（热力图一列的格子数）。
const WEEK_DAYS: usize = rust_cli_base::units::DAYS_PER_WEEK as usize;

/// 级别 2、3、4 的分界：有消息的日子里第 25、50、75 百分位的消息数。
const QUARTILES: [f64; 3] = [0.25, 0.5, 0.75];

/// `YYYY-MM-DD` → 自 1970-01-01 的天数。修改原因（2026-11-12，数字门禁）：以前这里手写了一份 days_from_civil，与
/// base `civil` 重复（`quantities_tests.rs` 逐日比对过两者相同），现在直接用 base 的。
pub fn day_number(date: &str) -> Option<i64> {
    rust_cli_base::civil::parse_date(date)
}

fn date_of(day: i64) -> String {
    civil_date(u64::try_from(day).unwrap_or(0) * DAY_MS)
}

/// 连续的天数：以今天为终点往前数的（今天没有消息时是 0）与最长的一段。
pub fn streaks(stats: &ActivityStats) -> (u64, u64) {
    let set: BTreeSet<i64> = stats
        .days
        .iter()
        .filter_map(|d| day_number(&d.date))
        .collect();
    let mut current = 0;
    if let Some(mut day) = day_number(&stats.today) {
        while set.contains(&day) {
            current += 1;
            day -= 1;
        }
    }
    let (mut longest, mut run, mut previous) = (0, 0, None);
    for day in &set {
        run = if previous == Some(day - 1) {
            run + 1
        } else {
            1
        };
        longest = longest.max(run);
        previous = Some(*day);
    }
    (current, longest)
}

/// 活跃的天数与范围的天数（`Active days: N/<7|30|totalDays>`）：7 天与 30 天是范围的天数；All time 是第一天到今天（含两头），
/// 读不出日期时没有分母。没有活跃的一天时为 None。
pub fn active_days(stats: &ActivityStats) -> Option<(u64, Option<u64>)> {
    let active = stats.days.len() as u64;
    if active == 0 {
        return None;
    }
    let of = match stats.range.and_then(ActivityRange::days) {
        Some(days) => Some(days),
        None => {
            let first = stats.days.first().and_then(|d| day_number(&d.date));
            // 含第一天与今天两头。
            first
                .zip(day_number(&stats.today))
                .map(|(first, today)| u64::try_from((first..=today).count()).unwrap_or(active))
        }
    };
    Some((active, of))
}

/// 消息最多的一天（同样多时最早的）。
pub fn most_active(stats: &ActivityStats) -> Option<&ActivityDay> {
    stats
        .days
        .iter()
        .filter(|d| d.messages > 0)
        .fold(None, |best: Option<&ActivityDay>, day| match best {
            Some(b) if b.messages >= day.messages => Some(b),
            _ => Some(day),
        })
}

/// 日期里的月份从 1 数起；月名的表（kit `MONTHS`）从 0 数起。
const FIRST_MONTH: usize = 1;

/// `YYYY-MM-DD` → `Oct 3`（月份的缩写同热力图）。
pub fn short_date(date: &str) -> Option<String> {
    let mut parts = date.split('-').skip(1).map(str::parse::<usize>);
    let (month, day) = (parts.next()?.ok()?, parts.next()?.ok()?);
    let name = MONTHS.get(month.checked_sub(FIRST_MONTH)?)?;
    Some(format!("{name} {day}"))
}

/// 有消息的日子的四分位（级别 1–4 的分界）。
fn quartiles(days: &[ActivityDay]) -> Option<[u64; 3]> {
    let mut counts: Vec<u64> = days.iter().map(|d| d.messages).filter(|&n| n > 0).collect();
    if counts.is_empty() {
        return None;
    }
    counts.sort_unstable();
    let at = |fraction: f64| {
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss
        )]
        let index = (counts.len() as f64 * fraction).floor() as usize;
        counts[index.min(counts.len() - 1)]
    };
    Some(QUARTILES.map(at))
}

/// 一天的级别：没有消息 0，其余按四分位 1–4（1 加上达到的分界数）。
fn level(messages: u64, quartiles: Option<[u64; 3]>) -> i8 {
    match quartiles {
        None => 0,
        Some(_) if messages == 0 => 0,
        Some(bounds) => 1 + bounds.iter().map(|&b| i8::from(messages >= b)).sum::<i8>(),
    }
}

/// 热力图的数据（kit `Activity`）：[`WEEKS`] 周，最新的一周在最后、含今天，今天之后的日子是 -1；`total` 是消息数，
/// `best_day` 是消息最多的一天的数，`streak` 是到今天为止连续的天数。今天读不出来时没有格子。
pub fn activity(stats: &ActivityStats) -> Activity {
    let total = stats.days.iter().map(|d| d.messages).sum();
    let best_day = stats.days.iter().map(|d| d.messages).max().unwrap_or(0);
    let streak = streaks(stats).0;
    let Some(today) = day_number(&stats.today) else {
        return Activity {
            total,
            best_day,
            streak,
            ..Activity::default()
        };
    };
    let counts: BTreeMap<i64, u64> = stats
        .days
        .iter()
        .filter_map(|d| day_number(&d.date).map(|n| (n, d.messages)))
        .collect();
    let quartiles = quartiles(&stats.days);
    // 周日为 0（base `civil::weekday`）。
    let weekday = i64::from(rust_cli_base::civil::weekday(today));
    let start = today - weekday - (WEEKS as i64 - 1) * WEEK_DAYS as i64;
    let levels = (0..WEEKS)
        .map(|week| {
            (0..WEEK_DAYS)
                .map(|row| {
                    let day = start + (week * WEEK_DAYS + row) as i64;
                    if day > today {
                        -1
                    } else {
                        level(counts.get(&day).copied().unwrap_or(0), quartiles)
                    }
                })
                .collect()
        })
        .collect();
    Activity {
        total,
        best_day,
        streak,
        start: date_of(start),
        levels,
    }
}

#[cfg(test)]
#[path = "stats_local_data_tests.rs"]
mod tests;
