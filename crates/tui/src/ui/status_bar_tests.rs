//! 状态栏的用例（docs/specs/rust-cli/tui-display.md §2.6）：旧界面的数据经 kit 排出的样子。排版本身的用例在 kit
//! （`list_rows_tests.rs`）。
use rust_cli_tui_kit::app::widgets::live_speed::LiveSpeed;
use rust_cli_tui_kit::render::{Span, Style, StyledLine};
use rust_cli_tui_kit::theme::scheme::ThemeChoice;
use rust_cli_tui_kit::theme::theme::Theme;

use super::{Figures, Part, Window, figures, lines, machine_parts, mode_parts, model_parts};

/// 早已落定的淡入（tick 数）。
const SETTLED: i64 = 1_000_000;

fn plain_theme() -> Theme {
    Theme::resolve(None, None, false, &ThemeChoice::default(), None)
}

fn text(lines: &[StyledLine]) -> Vec<String> {
    lines.iter().map(StyledLine::plain).collect()
}

/// 右边从左到右：速度、缓存、上下文、模型、档位（用户要求 2026-11-12）；两个百分数带名字；没有表与 `用掉/窗口`；没有项目路径。
fn right(theme: &Theme, window: &Window) -> Vec<Part> {
    // 速度是 kit 的现场估算画的（调用之间：量到的数）。
    let speed = LiveSpeed {
        shown: Some(182.0),
        ..LiveSpeed::default()
    }
    .spans(theme, 0, false);
    let mut right = figures(
        theme,
        &Figures {
            speed,
            cache: Some(0.853),
            context: Some(window),
            context_age: SETTLED,
            cache_word: "缓存",
            context_word: "上下文",
        },
    );
    right.extend(model_parts(theme, "deepseek-flash", "max", SETTLED));
    right
}

#[test]
fn the_left_says_who_and_the_right_says_how_much_and_which_model() {
    let theme = plain_theme();
    let left = mode_parts(&theme, vec![("完全访问".to_owned(), theme.fg3)], SETTLED);
    let window = Window {
        used: 17_000,
        max: 980_000,
        compact_at: Some(967_000),
        ..Window::default()
    };
    let shown = text(&lines(&theme, &left, &right(&theme, &window), &[], 120));
    // 左边从第 3 列起，右边的最后一格后面留一格；项之间是 ` · `。
    let start = "   完全访问 ⇧⇥";
    let end = "182 tok/s · 缓存 85.3% · 上下文 1.7% · deepseek-flash · max ";
    assert_eq!(shown.len(), 1, "always one line");
    assert!(shown[0].starts_with(start), "{shown:?}");
    assert!(shown[0].ends_with(end), "{shown:?}");
    // 窄时名次低的先走：`⇧⇥`、速度、缓存、档位……上下文与模型留着，仍是一行。
    let narrow = text(&lines(&theme, &left, &right(&theme, &window), &[], 50));
    assert_eq!(narrow.len(), 1);
    assert!(narrow[0].contains("上下文 1.7%"), "{narrow:?}");
    assert!(narrow[0].contains("deepseek-flash"), "{narrow:?}");
    assert!(!narrow[0].contains("tok/s"), "{narrow:?}");
    assert!(!narrow[0].contains("⇧⇥"), "{narrow:?}");
}

/// 槽的说明句（note）要写在状态栏时，它占右边，右边的那一组不写；不分成两行。
#[test]
fn a_note_takes_the_right_instead_of_the_figures() {
    let theme = plain_theme();
    let left = mode_parts(&theme, vec![("Ask".to_owned(), theme.fg3)], SETTLED);
    let window = Window {
        used: 10,
        max: 100,
        ..Window::default()
    };
    let note = [Part {
        right: true,
        ..Part::new("note", vec![Span::new("Copied", Style::DEFAULT)], 95)
    }];
    let shown = text(&lines(&theme, &left, &right(&theme, &window), &note, 40));
    assert_eq!(shown.len(), 1, "{shown:?}");
    assert!(shown[0].ends_with("Copied "), "{shown:?}");
    assert!(!shown[0].contains("上下文"), "{shown:?}");
}

#[test]
fn ready_machines_follow_the_mode_and_leave_first_when_narrow() {
    let theme = plain_theme();
    let mut left = mode_parts(&theme, vec![("Ask".to_owned(), theme.fg3)], SETTLED);
    left.extend(machine_parts(&theme, Some("@dev-a +1")));
    let wide = text(&lines(&theme, &left, &[], &[], 80));
    assert_eq!(wide[0].trim_end(), "   Ask ⇧⇥ · @dev-a +1");
    let narrow = text(&lines(&theme, &left, &[], &[], 12));
    assert!(!narrow[0].contains("@dev-a"), "{narrow:?}");
    assert!(narrow[0].contains("Ask"), "{narrow:?}");
}
