//! 设置的 Stats 段（tui-panels.md §4.6；以前是 `/usage all` 的「This computer」页）：`/stats` 打开它并要读本机的统计；行是热力图
//! 与图例、范围、两列的数（窄时一列）、token 的分项与这次会话；⇥ 换范围，回执按序号收；设置关上之后到的统计丢掉；滚动夹到
//! 最后一屏。
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rust_cli_base::models::GLM_5_3;
use rust_cli_kernel_api::metering::{
    ActivityDay, ActivityRange, ActivityStats, ModelTokens, SessionMetering,
};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::testing::selection;
use rust_cli_tui_kit::render::StyledLine;

use super::lines;
use crate::ui::app::{Action, App};
use crate::ui::i18n::Lang;
use crate::ui::manage::Manage;
use crate::ui::probe::{ColorLevel, Colors};
use crate::view::Transcript;

fn app() -> App {
    App::new(
        Colors::new(ColorLevel::TrueColor, Some((0, 0, 0))),
        Lang::En,
        Transcript::new(
            selection(),
            PermissionMode::Default,
            crate::view::test_workspace(),
        ),
        Some("/Users/dev".into()),
        "0.1.0".into(),
    )
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn stats() -> ActivityStats {
    ActivityStats {
        range: Some(ActivityRange::All),
        today: "2026-10-18".into(),
        days: vec![
            ActivityDay {
                date: "2026-10-16".into(),
                messages: 1,
            },
            ActivityDay {
                date: "2026-10-17".into(),
                messages: 4,
            },
            ActivityDay {
                date: "2026-10-18".into(),
                messages: 2,
            },
        ],
        models: vec![ModelTokens {
            model: GLM_5_3.into(),
            input: 1_200_000,
            output: 300_000,
            cache_read: 50_000,
            cache_write: 0,
        }],
        sessions: 3,
        longest_session_ms: Some(65 * 60_000),
        ..ActivityStats::default()
    }
}

/// `/stats` 打开 Stats 段，runner 读到的统计到了。
fn opened() -> App {
    let mut app = app();
    app.composer.set_text("draft");
    assert_eq!(app.slash("/stats"), Some(Action::Manage(Manage::Stats)));
    app.stats_opened(
        SessionMetering {
            api_duration_ms: 12_000,
        },
        Ok(stats()),
    );
    app
}

fn text(app: &App, width: usize) -> Vec<String> {
    let stats = app.settings.as_ref().and_then(|ui| ui.stats.as_ref());
    lines(app.lang, &app.ink.theme, stats, width)
        .iter()
        .map(|l| l.plain().trim_end().to_owned())
        .collect()
}

fn range(app: &App) -> ActivityRange {
    app.settings.as_ref().unwrap().stats.as_ref().unwrap().range
}

/// 段的行（100 列，从第 3 列起）：热力图与图例、范围、两列的数、token 的分项、这次会话。
#[test]
fn stats_is_the_heat_map_the_ranges_and_two_columns() {
    let app = opened();
    let lines = text(&app, 100);
    let all = lines.join("\n");
    assert!(
        lines[0].starts_with("   Messages, last 52 weeks") && lines[0].ends_with("7 total"),
        "{all}"
    );
    assert!(all.contains("   Mon  "), "the heat map: {all}");
    // 图例在星期名之后（第 8 列），四级的格子（plain 主题是 `▪ ▪ ■ ■`）。
    assert!(
        lines.iter().any(|l| l.starts_with("        Less ")
            && l.ends_with(" More")
            && l.chars().count() == 25),
        "{all}"
    );
    assert!(
        all.contains("   All time  ·  Last 7 days  ·  Last 30 days"),
        "{all}"
    );
    // 两列：第二列从第 35 列起，名字补到 17 列。
    let row = |name: &str| -> &String {
        lines
            .iter()
            .find(|l| l.trim_start().starts_with(name))
            .unwrap_or_else(|| panic!("{name}: {all}"))
    };
    let first = row("Favorite model");
    assert_eq!(
        first,
        &format!(
            "   {:<17}{:<15}{:<17}1.6M",
            "Favorite model", GLM_5_3, "Total tokens"
        )
    );
    assert!(
        row("Sessions").ends_with("Longest session  1h 05m"),
        "{all}"
    );
    assert!(
        row("Active days").starts_with("   Active days      3/3"),
        "{all}"
    );
    assert!(
        row("Active days").ends_with("Longest streak   3 days"),
        "{all}"
    );
    assert!(
        row("Most active day").starts_with("   Most active day  Oct 17"),
        "{all}"
    );
    assert!(
        row("Most active day").ends_with("Current streak   3 days"),
        "{all}"
    );
    assert!(
        all.contains("   In 1.2M  ·  Out 300K  ·  Cache read 50K"),
        "{all}"
    );
    assert!(
        !all.contains("Cache write"),
        "zero parts are left out: {all}"
    );
    let session = row("This session");
    assert!(session.ends_with("12s API time  ·  0s wall time"), "{all}");
}

/// 窄时（窗格 < 64 列）两列的数排成一列，左列的在前。
#[test]
fn a_narrow_pane_puts_the_figures_in_one_column() {
    let app = opened();
    let lines = text(&app, 50);
    let names: Vec<&str> = lines
        .iter()
        .map(|l| l.trim_start())
        .filter(|l| {
            [
                "Favorite model",
                "Sessions",
                "Active days",
                "Most active day",
                "Total tokens",
                "Longest session",
                "Longest streak",
                "Current streak",
            ]
            .iter()
            .any(|n| l.starts_with(n))
        })
        .map(|l| l.split("  ").next().unwrap_or_default())
        .collect();
    assert_eq!(
        names,
        [
            "Favorite model",
            "Sessions",
            "Active days",
            "Most active day",
            "Total tokens",
            "Longest session",
            "Longest streak",
            "Current streak"
        ]
    );
}

/// ⇥ 换范围：7 天与 30 天问内核（序号加一），旧的回执丢掉，问的时候说在读；回到 All time 不用问。
#[test]
fn tab_changes_the_range_and_stale_answers_are_dropped() {
    let mut app = opened();
    assert_eq!(
        app.settings_key(key(KeyCode::Tab)),
        Some(Action::Manage(Manage::StatsRange {
            range: ActivityRange::Week,
            seq: 1
        }))
    );
    assert!(
        text(&app, 100)
            .iter()
            .any(|l| l.contains("Reading this computer's history")),
    );
    // 又换了一次：序号 1 的回执晚到，丢掉。
    app.settings_key(key(KeyCode::Tab));
    assert_eq!(range(&app), ActivityRange::Month);
    app.stats_range(1, Ok(ActivityStats::default()));
    assert_eq!(
        app.settings
            .as_ref()
            .unwrap()
            .stats
            .as_ref()
            .unwrap()
            .ranged,
        None
    );
    app.stats_range(2, Err("offline".into()));
    assert!(
        text(&app, 100)
            .iter()
            .any(|l| l.contains("Could not read the history: offline"))
    );
    assert_eq!(app.settings_key(key(KeyCode::Tab)), None);
    assert_eq!(range(&app), ActivityRange::All);
}

/// 设置关上之后到的统计丢掉（下次打开再读）；没有记录时是 `No activity yet.`，热力图没有图例。
#[test]
fn late_answers_are_dropped_and_an_empty_history_says_so() {
    let mut app = app();
    app.stats_opened(SessionMetering::default(), Ok(stats()));
    assert!(app.settings.is_none());
    let _ = app.slash("/stats");
    app.stats_opened(
        SessionMetering::default(),
        Ok(ActivityStats {
            today: "2026-10-18".into(),
            ..ActivityStats::default()
        }),
    );
    let lines = text(&app, 100);
    assert_eq!(
        lines
            .iter()
            .filter(|l| l.trim() == "No activity yet.")
            .count(),
        2,
        "{lines:#?}"
    );
    assert!(!lines.iter().any(|l| l.contains("Less")), "{lines:#?}");
}

/// 状态栏右边是 Stats 的键（`⇥ range`）；↓ 与翻页滚动、夹到最后一屏（上一帧窗格能放的行）。
#[test]
fn the_keys_say_range_and_the_page_scrolls() {
    let mut app = opened();
    let frame = app
        .settings_frame(Instant::now(), 0, 100, 20)
        .expect("the settings");
    // 状态栏第 1 行是倒数第 2 行（最后一行是目录与标题，tui-display.md §2.6）。
    let keys = StyledLine::plain(&frame[frame.len() - 2])
        .trim_end()
        .to_owned();
    assert!(keys.contains("⇥ range"), "{keys}");
    assert!(keys.contains("↑↓ scroll"), "{keys}");
    app.settings_key(key(KeyCode::End));
    let bottom = app.settings.as_ref().unwrap().top();
    assert!(bottom > 0);
    app.settings_key(key(KeyCode::Down));
    assert_eq!(app.settings.as_ref().unwrap().top(), bottom, "clamped");
    app.settings_key(key(KeyCode::Up));
    assert_eq!(app.settings.as_ref().unwrap().top(), bottom - 1);
    app.settings_key(key(KeyCode::Home));
    assert_eq!(app.settings.as_ref().unwrap().top(), 0);
}
